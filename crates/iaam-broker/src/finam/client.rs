use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::credentials::BrokerToken;
use iaam_http::gateway::{Clock, SystemClock};
use iaam_http::{Destination, GatewayError, HttpRequest, Outbound, RequestBody, Secret};
use serde_json::Value;
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::{Date, OffsetDateTime, Time};

/// Budget key of `AccountsService/GetAccount`. Finam states its limit per
/// method, so each method draws on a budget of its own.
const GET_ACCOUNT: &str = "AccountsService.GetAccount";

/// Budget key of `AccountsService/Transactions`.
const TRANSACTIONS: &str = "AccountsService.Transactions";

/// The limit one transactions request names (`TransactionsRequest.limit`
/// in the published contract). The contract publishes no maximum and no
/// continuation: an answer carrying exactly this many transactions may
/// continue past the page, so the fetched interval narrows until an
/// answer stays under the limit, and only a single day that still
/// reaches it is refused — never silently truncated.
const TRANSACTIONS_LIMIT: i32 = 1_000;

/// Budget key of the session methods: the exchange of the secret for a
/// session token (`POST /v1/sessions`) and the details of that token
/// (`POST /v1/sessions/details`, `TokenDetails` in Finam's REST docs). One
/// row budgets both: Finam documents 200 requests a minute for each
/// method, and the row grants the conservative half of that to the two of
/// them together.
const SESSIONS: &str = "AuthService.Sessions";

/// What Finam answers with when it names no other lifetime (it names none
/// today): the portal's FAQ states a session token lives 15 minutes
/// (https://api.finam.ru/getting-started/).
const SESSION_LIFETIME: Duration = Duration::from_secs(15 * 60);

/// A token with more than this left is reused; one at or inside the margin
/// is exchanged anew, so a call never starts on a token that dies under it.
const RENEW_BEFORE: Duration = Duration::from_secs(30);

/// HTTP access errors for the Finam Trade API.
///
/// No variant carries the token: a rejected body is kept with it hidden.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FinamError {
    /// The transport could not be set up — a client or a trust root this
    /// build could not construct. A fault of this build, not of Finam or the
    /// network: retrying meets the same fault.
    #[error("the transport to Finam could not be built: {reason}")]
    TransportNotBuilt { reason: String },
    /// Finam kept answering 429 through every retry.
    #[error("Finam gateway rate-limited the request; retry after {retry_after:?}")]
    RateLimited { retry_after: Duration },
    /// Finam kept failing transiently, or the circuit to it is open; the same
    /// request may succeed after `retry_after`.
    #[error(
        "Finam is unavailable after {attempts} attempts (last status {status:?}); retry after {retry_after:?}"
    )]
    Unavailable {
        status: Option<u16>,
        attempts: u32,
        retry_after: Duration,
    },
    #[error("Finam token is invalid")]
    InvalidToken,
    #[error("unexpected HTTP status {status}: {body}")]
    UnexpectedStatus { status: u16, body: String },
    /// The gateway refused the call before sending it: its budget table has
    /// no row for Finam, which is a fault of this build, not of the request.
    #[error("the outbound gateway refused the Finam call: {reason}")]
    Gateway { reason: String },
    /// A single day's transactions answer reached the request's limit, and
    /// a single day cannot be split further: whether more transactions lie
    /// past the page cannot be proven from the wire, so the interval is
    /// refused rather than fetched truncated.
    #[error(
        "Finam transactions answer for a single day reached the request limit; the interval cannot be proven complete"
    )]
    PartialResponse,
    #[error("successful response does not match the JSON schema")]
    MalformedResponse,
}

/// A session token of the Trade API, the moment it stops working, and the
/// moment it was issued — all read against the client's own clock.
#[derive(Clone)]
struct Session {
    token: Secret,
    expires_at: Instant,
    issued_at: Instant,
}

/// Finam HTTP client returning raw response bodies.
///
/// Every request goes through the process's one outbound gateway, which owns
/// the budget, the retries and the breaker; this client only describes the
/// request and classifies what comes back. The owner's secret travels once,
/// in the body of the exchange for a session token; every other call carries
/// that token, and never the secret.
pub struct FinamClient {
    token: BrokerToken,
    gateway: Arc<dyn Outbound>,
    /// What a session's `expires_at` reads against. The system clock in
    /// production; a fake a test can turn forward, so no test sleeps a
    /// session old.
    clock: Arc<dyn Clock>,
    /// The live session, while one is worth reusing.
    session: Mutex<Option<Session>>,
    /// The limit a transactions request names; the contract's `limit`,
    /// fixed for this build and narrowed around in tests.
    transactions_limit: i32,
}

impl FinamClient {
    /// Create a client over the shared gateway; the secret remains in a
    /// zeroizing wrapper.
    #[must_use]
    pub fn new(token: BrokerToken, gateway: Arc<dyn Outbound>) -> Self {
        Self::with_clock(token, gateway, Arc::new(SystemClock), TRANSACTIONS_LIMIT)
    }

    fn with_clock(
        token: BrokerToken,
        gateway: Arc<dyn Outbound>,
        clock: Arc<dyn Clock>,
        transactions_limit: i32,
    ) -> Self {
        Self {
            token,
            gateway,
            clock,
            session: Mutex::new(None),
            transactions_limit,
        }
    }

    /// Return the raw body of the account's current portfolio.
    pub async fn get_portfolio(&self, account_id: &str) -> Result<String, FinamError> {
        self.get(GET_ACCOUNT, format!("/v1/accounts/{account_id}"), &[])
            .await
    }

    /// Return the raw body of the account's transactions for a whole
    /// interval, under the published contract: the request names a limit,
    /// and the answer is a bare repeated list with no continuation to
    /// follow. Completeness is proven by narrowing: an answer that reaches
    /// the limit may continue past the page, so the interval splits at its
    /// middle day and each half is asked again until every answer stays
    /// under the limit. A single day whose answer still reaches it has no
    /// smaller interval to ask, and is refused rather than truncated.
    pub async fn get_transactions(
        &self,
        account_id: &str,
        from: Date,
        to: Date,
    ) -> Result<String, FinamError> {
        let transactions = self.transactions_interval(account_id, from, to).await?;
        Ok(serde_json::json!({ "transactions": transactions }).to_string())
    }

    /// The transactions of one date interval, whole. The recursion is
    /// boxed because it splits itself in two; its depth is the interval's
    /// day count halved down to one.
    fn transactions_interval<'a>(
        &'a self,
        account_id: &'a str,
        from: Date,
        to: Date,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Value>, FinamError>> + Send + 'a>> {
        Box::pin(async move {
            if from > to {
                return Ok(Vec::new());
            }
            let transactions = self.transactions_page(account_id, from, to).await?;
            let limit = usize::try_from(self.transactions_limit).unwrap_or(usize::MAX);
            if transactions.len() < limit {
                return Ok(transactions);
            }
            let whole_days = (to - from).whole_days();
            if whole_days == 0 {
                return Err(FinamError::PartialResponse);
            }
            let middle = from + time::Duration::days(whole_days / 2);
            let mut merged = self.transactions_interval(account_id, from, middle).await?;
            let after_middle = middle.next_day().ok_or(FinamError::PartialResponse)?;
            let rest = self
                .transactions_interval(account_id, after_middle, to)
                .await?;
            merged.extend(rest);
            Ok(merged)
        })
    }

    /// One request of the interval fetch: the page the contract answers
    /// with, its transactions as the wire printed them.
    async fn transactions_page(
        &self,
        account_id: &str,
        from: Date,
        to: Date,
    ) -> Result<Vec<Value>, FinamError> {
        let query = [
            ("interval.start_time", rfc3339_midnight(from)),
            ("interval.end_time", rfc3339_midnight(to)),
            ("limit", self.transactions_limit.to_string()),
        ];
        let body = self
            .get(
                TRANSACTIONS,
                format!("/v1/accounts/{account_id}/transactions"),
                &query,
            )
            .await?;
        let value: Value =
            serde_json::from_str(&body).map_err(|_| FinamError::MalformedResponse)?;
        let transactions = value
            .get("transactions")
            .and_then(Value::as_array)
            .ok_or(FinamError::MalformedResponse)?;
        Ok(transactions.clone())
    }

    /// The account ids the access sees, from `POST /v1/sessions/details`.
    pub async fn get_account_ids(&self) -> Result<Vec<String>, FinamError> {
        let (body, token) = self
            .authorized(SESSIONS, |token| {
                // The token rides the body, as the method's page passes it,
                // and the bearer, as the API's authentication asks of every
                // method. Reading twice changes nothing.
                HttpRequest::post(
                    Destination::FinamApi,
                    "/v1/sessions/details",
                    RequestBody::Json(serde_json::json!({ "token": token }).to_string()),
                )
                .with_bearer(token)
                .idempotent()
            })
            .await?;
        let value: Value =
            serde_json::from_str(&body).map_err(|_| FinamError::MalformedResponse)?;
        let ids = value
            .get("account_ids")
            .and_then(Value::as_array)
            .ok_or(FinamError::MalformedResponse)?;
        let ids: Vec<String> = ids
            .iter()
            .map(|id| {
                id.as_str()
                    .map(str::to_owned)
                    .ok_or(FinamError::MalformedResponse)
            })
            .collect::<Result<Vec<_>, _>>()?;
        // The answer describes the very token the call carried, so its
        // created/expires span is that token's true lifetime — applied only
        // after the answer has fully checked out, and only to the session
        // that still holds that token.
        if let Some(lifetime) = lifetime_of(&value) {
            self.correct_session_lifetime(token.expose(), lifetime);
        }
        Ok(ids)
    }

    /// Send a reading call over the session token.
    async fn get(
        &self,
        method: &'static str,
        path: String,
        query: &[(&str, String)],
    ) -> Result<String, FinamError> {
        let query = query.to_vec();
        self.authorized(method, move |token| {
            let mut request = HttpRequest::get(Destination::FinamApi, &path).with_bearer(token);
            for (key, value) in &query {
                request = request.with_query(key, value);
            }
            request
        })
        .await
        .map(|(body, _)| body)
    }

    /// Send an authorized call: the session token is the bearer. A 401 to
    /// that token is answered with one fresh exchange and one retry; the
    /// second 401 is a refusal. Returns the body and the token that
    /// finally carried it.
    async fn authorized(
        &self,
        method: &'static str,
        build: impl Fn(&str) -> HttpRequest,
    ) -> Result<(String, Secret), FinamError> {
        let session = self.session().await?;
        let step = match self.raw(method, &build(session.token.expose())).await {
            Ok(body) => return Ok((body, session.token.clone())),
            Err(step) => step,
        };
        if !step.is_unauthorized() {
            return Err(step.into_error(self.token.expose(), Some(session.token.expose())));
        }
        let fresh = self.exchange().await?;
        self.keep(fresh.clone());
        match self.raw(method, &build(fresh.token.expose())).await {
            Ok(body) => Ok((body, fresh.token.clone())),
            Err(step) => Err(step.into_error(self.token.expose(), Some(fresh.token.expose()))),
        }
    }

    /// The session to carry: the remembered one while it lives, else a
    /// fresh exchange.
    async fn session(&self) -> Result<Session, FinamError> {
        if let Some(live) = self.live_session() {
            return Ok(live);
        }
        let fresh = self.exchange().await?;
        self.keep(fresh.clone());
        Ok(fresh)
    }

    /// The remembered session while it has more than the renewal margin
    /// left. The lock guards a read only: the exchange never happens under
    /// it.
    fn live_session(&self) -> Option<Session> {
        let guard = self.session.lock().expect("session lock");
        let session = guard.as_ref()?;
        let left = session
            .expires_at
            .checked_duration_since(self.clock.now())?;
        (left > RENEW_BEFORE).then(|| session.clone())
    }

    /// Remember the fresh session unless a concurrent call already put a
    /// longer-lived one there.
    fn keep(&self, fresh: Session) {
        let mut guard = self.session.lock().expect("session lock");
        if guard
            .as_ref()
            .is_none_or(|current| current.expires_at <= fresh.expires_at)
        {
            *guard = Some(fresh);
        }
    }
    /// Re-anchor the live session's end on the lifetime its details answer
    /// stated. The anchor stays the session's own issuance moment, so a
    /// correction moves only the end, and only on the client's clock. The
    /// answer names the token it described: a concurrent renewal may have
    /// replaced that session since, and a stranger's end is not ours to
    /// write.
    fn correct_session_lifetime(&self, token: &str, lifetime: Duration) {
        let mut guard = self.session.lock().expect("session lock");
        if let Some(session) = guard.as_mut() {
            if session.token.expose() == token {
                session.expires_at = session.issued_at + lifetime;
            }
        }
    }

    /// Exchange the secret for a session token (`POST /v1/sessions`): the
    /// one call that carries the secret, in the body alone, as the method's
    /// page shows (https://api.finam.ru/docs/rest/authservice_auth.md/).
    /// The answer names the token and nothing else, so its lifetime is the
    /// documented one until a details answer corrects it. Minting a
    /// session has no effect on the account, so the gateway may repeat it
    /// like any read.
    async fn exchange(&self) -> Result<Session, FinamError> {
        // The anchor precedes the send: Finam creates the token between
        // the request and the answer, so measuring its life from the
        // request can only renew early, never late.
        let issued_at = self.clock.now();
        let request = HttpRequest::post(
            Destination::FinamApi,
            "/v1/sessions",
            RequestBody::Json(serde_json::json!({ "secret": self.token.expose() }).to_string()),
        )
        .idempotent();
        let response = self
            .gateway
            .send(SESSIONS, &request, None)
            .await
            .map_err(|error| classify_refusal(error, self.token.expose(), None))?;
        let value: Value =
            serde_json::from_slice(&response.body).map_err(|_| FinamError::MalformedResponse)?;
        let token = value
            .get("token")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if token.is_empty() {
            return Err(FinamError::MalformedResponse);
        }
        Ok(Session {
            token: Secret::new(token),
            issued_at,
            expires_at: issued_at + SESSION_LIFETIME,
        })
    }

    /// Send the request through the gateway and return its body.
    async fn raw(&self, method: &'static str, request: &HttpRequest) -> Result<String, Step> {
        let response = self
            .gateway
            .send(method, request, None)
            .await
            .map_err(Step::Refused)?;
        String::from_utf8(response.body).map_err(|_| Step::Malformed)
    }
}

/// What a sent call came back with, before Finam's meaning is laid over it.
enum Step {
    /// The gateway did not complete the call.
    Refused(GatewayError),
    /// The answer was not text; no meaning can be read from it.
    Malformed,
}

impl Step {
    /// Finam's meaning of the step, with every secret cut out of a kept body.
    fn into_error(self, secret: &str, token: Option<&str>) -> FinamError {
        match self {
            Self::Refused(error) => classify_refusal(error, secret, token),
            Self::Malformed => FinamError::MalformedResponse,
        }
    }

    /// Whether Finam refused the session token the call carried.
    fn is_unauthorized(&self) -> bool {
        matches!(
            self,
            Self::Refused(GatewayError::Rejected { status: 401, .. })
        )
    }
}

fn rfc3339_midnight(date: Date) -> String {
    OffsetDateTime::new_utc(date, Time::MIDNIGHT)
        .format(&Rfc3339)
        .unwrap_or_else(|_| format!("{date}T00:00:00Z"))
}

/// The lifetime a details answer states for the token it describes: the
/// span from `created_at` to `expires_at`, both RFC 3339. `None` when the
/// answer names either end unreadably — the documented lifetime stands.
fn lifetime_of(value: &Value) -> Option<Duration> {
    let created = OffsetDateTime::parse(value.get("created_at")?.as_str()?, &Rfc3339).ok()?;
    let expires = OffsetDateTime::parse(value.get("expires_at")?.as_str()?, &Rfc3339).ok()?;
    Duration::try_from(expires - created).ok()
}

/// Finam's meaning of a call the gateway did not complete. The refused body
/// is kept with every secret cut out: the session token (the gateway has
/// usually cut it already), then the owner's secret, which no layer before
/// this one knows to look for.
fn classify_refusal(error: GatewayError, secret: &str, token: Option<&str>) -> FinamError {
    if let Some(retry_after) = error.retry_after() {
        return match error.status() {
            Some(429) => FinamError::RateLimited { retry_after },
            status => FinamError::Unavailable {
                status,
                attempts: error.attempts(),
                retry_after,
            },
        };
    }
    match error {
        GatewayError::Rejected { status, body, .. } => {
            classify_rejection(status, body.as_bytes(), secret, token)
        }
        GatewayError::Transport { error, .. } => FinamError::TransportNotBuilt {
            reason: error.to_string(),
        },
        other => FinamError::Gateway {
            reason: other.to_string(),
        },
    }
}

/// Finam's meaning of a status the gateway would not retry.
fn classify_rejection(status: u16, body: &[u8], secret: &str, token: Option<&str>) -> FinamError {
    match status {
        401 | 403 => FinamError::InvalidToken,
        status => {
            let kept = String::from_utf8_lossy(body);
            let kept = redact_token(&kept, secret);
            let kept = redact_token(&kept, token.unwrap_or_default());
            FinamError::UnexpectedStatus { status, body: kept }
        }
    }
}
fn redact_token(body: &str, token: &str) -> String {
    if token.is_empty() {
        body.to_owned()
    } else {
        body.replace(token, "<token hidden>")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FinamClient, FinamError, RENEW_BEFORE, SESSION_LIFETIME, TRANSACTIONS_LIMIT,
        classify_rejection,
    };
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use iaam_http::gateway::{BUDGETS, Budget, Clock, MethodScope, Sleeper, Transport};
    use iaam_http::{Destination, Gateway, HttpError, HttpRequest, HttpResponse};
    use time::macros::date;

    use crate::credentials::{Key, open, seal};

    /// The owner's secret, invented; the session tokens are invented too.
    const SECRET: &str = "finam-invented-secret";
    const JWT_ONE: &str = "invented.jwt.one";
    const JWT_TWO: &str = "invented.jwt.two";
    const JWT_THREE: &str = "invented.jwt.three";

    /// An exchange answer naming the given session token.
    fn session_answer(token: &str) -> String {
        format!(r#"{{"token":"{token}"}}"#)
    }

    /// A clock that moves only when something sleeps on it or a test turns
    /// it forward: no test sleeps for a session to grow old.
    struct FakeTime {
        now: Mutex<Instant>,
        slept: Mutex<Vec<Duration>>,
    }

    impl FakeTime {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                now: Mutex::new(Instant::now()),
                slept: Mutex::new(Vec::new()),
            })
        }

        fn slept(&self) -> Vec<Duration> {
            self.slept.lock().expect("sleeps").clone()
        }

        fn advance(&self, by: Duration) {
            *self.now.lock().expect("clock") += by;
        }
    }

    impl Clock for FakeTime {
        fn now(&self) -> Instant {
            *self.now.lock().expect("clock")
        }
    }

    impl Sleeper for FakeTime {
        fn sleep(&self, delay: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            self.slept.lock().expect("sleeps").push(delay);
            *self.now.lock().expect("clock") += delay;
            Box::pin(async {})
        }
    }

    /// An endpoint that answers from a script, then with a default status,
    /// and remembers each request it received.
    struct Scripted {
        script: Mutex<VecDeque<Result<HttpResponse, HttpError>>>,
        default_status: u16,
        received: Mutex<Vec<HttpRequest>>,
    }

    impl Scripted {
        fn answering(default_status: u16) -> Self {
            Self {
                script: Mutex::new(VecDeque::new()),
                default_status,
                received: Mutex::new(Vec::new()),
            }
        }

        fn then(self, status: u16, body: &str) -> Self {
            self.script
                .lock()
                .expect("script")
                .push_back(Ok(response(status, body)));
            self
        }

        fn then_fault(self, fault: HttpError) -> Self {
            self.script.lock().expect("script").push_back(Err(fault));
            self
        }
    }

    /// The gateway owns its transport; the test keeps a second handle to see
    /// what reached the endpoint.
    struct Shared(Arc<Scripted>);

    impl Transport for Shared {
        async fn send(&self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
            let endpoint = &self.0;
            endpoint
                .received
                .lock()
                .expect("received")
                .push(request.clone());
            endpoint
                .script
                .lock()
                .expect("script")
                .pop_front()
                .unwrap_or_else(|| Ok(response(endpoint.default_status, "")))
        }
    }

    fn response(status: u16, body: &str) -> HttpResponse {
        HttpResponse {
            status,
            body: body.as_bytes().to_vec(),
            retry_after: None,
        }
    }

    fn client_over(
        budgets: &'static [Budget],
        endpoint: &Arc<Scripted>,
    ) -> (FinamClient, Arc<FakeTime>) {
        client_with_limit(budgets, endpoint, TRANSACTIONS_LIMIT)
    }

    /// The same client, naming a smaller transactions limit: the contract's
    /// `limit` is a request field, and a scripted "page full" answer needs
    /// a page small enough to fill.
    fn client_with_limit(
        budgets: &'static [Budget],
        endpoint: &Arc<Scripted>,
        transactions_limit: i32,
    ) -> (FinamClient, Arc<FakeTime>) {
        let time = FakeTime::new();
        let clock = Arc::clone(&time) as Arc<dyn Clock>;
        let gateway = Gateway::with_parts(
            Shared(Arc::clone(endpoint)),
            budgets,
            Arc::clone(&time) as Arc<dyn Clock>,
            Arc::clone(&time) as Arc<dyn Sleeper>,
        )
        .expect("the budget table is valid");
        let key = Key::from_bytes([7; 32]);
        let secret = open(&key, &seal(&key, SECRET)).expect("an invented secret");
        // The private constructor, so the client reads the same fake clock
        // the gateway does; `new` keeps the system clock.
        (
            FinamClient::with_clock(secret, Arc::new(gateway), clock, transactions_limit),
            time,
        )
    }

    /// No refusal names a secret: neither the owner's, nor a session token.
    fn assert_no_secret(error: &FinamError) {
        for hidden in [SECRET, JWT_ONE, JWT_TWO] {
            assert!(!error.to_string().contains(hidden), "{error}");
            assert!(!format!("{error:?}").contains(hidden), "{error:?}");
        }
    }

    #[tokio::test]
    async fn calls_carry_the_session_token_and_only_the_exchange_carries_the_secret() {
        let endpoint = Arc::new(
            Scripted::answering(200)
                .then(200, &session_answer(JWT_ONE))
                .then(200, "{}")
                .then(200, r#"{"transactions":[]}"#),
        );
        let (client, _) = client_over(BUDGETS, &endpoint);

        client.get_portfolio("Main").await.expect("portfolio");
        client
            .get_transactions("Main", date!(2024 - 01 - 01), date!(2024 - 02 - 01))
            .await
            .expect("transactions");

        let received = endpoint.received.lock().expect("received");
        assert_eq!(received.len(), 3);
        let exchange = &received[0];
        assert_eq!(exchange.url(), "https://api.finam.ru/v1/sessions");
        assert_eq!(exchange.bearer(), None);
        assert_eq!(
            exchange.body().map(iaam_http::RequestBody::payload),
            Some(r#"{"secret":"finam-invented-secret"}"#),
        );
        assert_eq!(received[1].url(), "https://api.finam.ru/v1/accounts/Main");
        assert_eq!(
            received[2].url(),
            "https://api.finam.ru/v1/accounts/Main/transactions\
             ?interval%2Estart%5Ftime=2024%2D01%2D01T00%3A00%3A00Z\
             &interval%2Eend%5Ftime=2024%2D02%2D01T00%3A00%3A00Z\
             &limit=1000"
        );
        for request in received.iter().skip(1) {
            assert_eq!(request.bearer().map(|token| token.expose()), Some(JWT_ONE));
        }
    }

    #[tokio::test]
    async fn the_session_is_exchanged_once_and_reused() {
        let endpoint = Arc::new(
            Scripted::answering(200)
                .then(200, &session_answer(JWT_ONE))
                .then(200, "{}")
                .then(200, "{}")
                .then(200, "{}"),
        );
        let (client, time) = client_over(BUDGETS, &endpoint);

        for _ in 0..3 {
            client.get_portfolio("Main").await.expect("portfolio");
        }

        assert_eq!(endpoint.received.lock().expect("received").len(), 4);
        assert!(time.slept().is_empty(), "the clock renews, nothing sleeps");
    }

    #[tokio::test]
    async fn a_session_past_its_expiry_is_exchanged_again() {
        let endpoint = Arc::new(
            Scripted::answering(200)
                .then(200, &session_answer(JWT_ONE))
                .then(200, "{}")
                .then(200, &session_answer(JWT_TWO))
                .then(200, "{}"),
        );
        let (client, time) = client_over(BUDGETS, &endpoint);
        client
            .get_portfolio("Main")
            .await
            .expect("the first session");

        time.advance(SESSION_LIFETIME);

        client
            .get_portfolio("Main")
            .await
            .expect("the renewed session");

        let received = endpoint.received.lock().expect("received");
        assert_eq!(received.len(), 4);
        assert_eq!(received[2].url(), "https://api.finam.ru/v1/sessions");
        assert_eq!(
            received[3].bearer().map(|token| token.expose()),
            Some(JWT_TWO)
        );
    }

    #[tokio::test]
    async fn a_session_inside_the_renewal_margin_is_exchanged_again() {
        let endpoint = Arc::new(
            Scripted::answering(200)
                .then(200, &session_answer(JWT_ONE))
                .then(200, "{}")
                .then(200, &session_answer(JWT_TWO))
                .then(200, "{}"),
        );
        let (client, time) = client_over(BUDGETS, &endpoint);
        client
            .get_portfolio("Main")
            .await
            .expect("the first session");

        // One second into the margin: the token still works, but a call
        // starting now gets a fresh one.
        time.advance(SESSION_LIFETIME - RENEW_BEFORE + Duration::from_secs(1));

        client
            .get_portfolio("Main")
            .await
            .expect("the renewed session");

        let received = endpoint.received.lock().expect("received");
        assert_eq!(received.len(), 4);
        assert_eq!(
            received[3].bearer().map(|token| token.expose()),
            Some(JWT_TWO)
        );
    }

    #[tokio::test]
    async fn a_session_at_the_margin_itself_is_exchanged_again() {
        let endpoint = Arc::new(
            Scripted::answering(200)
                .then(200, &session_answer(JWT_ONE))
                .then(200, "{}")
                .then(200, &session_answer(JWT_TWO))
                .then(200, "{}"),
        );
        let (client, time) = client_over(BUDGETS, &endpoint);
        client
            .get_portfolio("Main")
            .await
            .expect("the first session");

        // Exactly the margin left: renewed. Only a session with more than
        // the margin left is reused.
        time.advance(SESSION_LIFETIME - RENEW_BEFORE);

        client
            .get_portfolio("Main")
            .await
            .expect("the renewed session");

        let received = endpoint.received.lock().expect("received");
        assert_eq!(received.len(), 4);
        assert_eq!(
            received[3].bearer().map(|token| token.expose()),
            Some(JWT_TWO)
        );
    }

    #[tokio::test]
    async fn a_longer_lived_session_survives_a_shorter_one_kept_later() {
        let endpoint = Arc::new(
            Scripted::answering(200)
                .then(200, &session_answer(JWT_ONE))
                .then(200, "{}"),
        );
        let (client, time) = client_over(BUDGETS, &endpoint);
        client
            .get_portfolio("Main")
            .await
            .expect("the first session");

        client.keep(super::Session {
            token: iaam_http::Secret::new(JWT_TWO),
            issued_at: time.now(),
            expires_at: time.now() + Duration::from_secs(60),
        });

        client.get_portfolio("Main").await.expect("portfolio");

        let received = endpoint.received.lock().expect("received");
        assert_eq!(received.len(), 3);
        assert_eq!(
            received[2].bearer().map(|token| token.expose()),
            Some(JWT_ONE)
        );
    }

    #[test]
    fn the_session_lives_the_documented_fifteen_minutes() {
        // The portal's FAQ states the number (15 minutes); the renewal
        // pace follows it, not the other way round.
        assert_eq!(SESSION_LIFETIME, Duration::from_secs(15 * 60));
    }

    #[tokio::test]
    async fn a_401_on_a_data_call_is_answered_with_one_renewal_and_one_retry() {
        let endpoint = Arc::new(
            Scripted::answering(200)
                .then(200, &session_answer(JWT_ONE))
                .then(401, "")
                .then(200, &session_answer(JWT_TWO))
                .then(200, r#"{"id":"Main"}"#),
        );
        let (client, _) = client_over(BUDGETS, &endpoint);

        let body = client
            .get_portfolio("Main")
            .await
            .expect("the retry succeeds");

        assert_eq!(body, r#"{"id":"Main"}"#);
        let received = endpoint.received.lock().expect("received");
        assert_eq!(received.len(), 4);
        assert_eq!(
            received[1].bearer().map(|token| token.expose()),
            Some(JWT_ONE)
        );
        assert_eq!(
            received[3].bearer().map(|token| token.expose()),
            Some(JWT_TWO)
        );
    }

    #[tokio::test]
    async fn a_second_401_is_a_refusal_and_stops_the_retry() {
        let endpoint = Arc::new(
            Scripted::answering(200)
                .then(200, &session_answer(JWT_ONE))
                .then(401, "")
                .then(200, &session_answer(JWT_TWO))
                .then(401, ""),
        );
        let (client, time) = client_over(BUDGETS, &endpoint);

        let error = client
            .get_portfolio("Main")
            .await
            .expect_err("refused after the renewed call is refused too");

        assert_eq!(error, FinamError::InvalidToken);
        // Two exchanges, two data calls: one renewal, one retry, then the
        // refusal. No wait: a 401 is never retried by the gateway itself.
        assert_eq!(endpoint.received.lock().expect("received").len(), 4);
        assert!(time.slept().is_empty());
        assert_no_secret(&error);
    }

    #[tokio::test]
    async fn a_403_names_the_access_and_sends_nothing_again() {
        let endpoint = Arc::new(
            Scripted::answering(200)
                .then(200, &session_answer(JWT_ONE))
                .then(403, ""),
        );
        let (client, time) = client_over(BUDGETS, &endpoint);

        let error = client
            .get_transactions("Main", date!(2024 - 01 - 01), date!(2024 - 02 - 01))
            .await
            .expect_err("403 is refused");

        assert_eq!(error, FinamError::InvalidToken);
        assert_eq!(endpoint.received.lock().expect("received").len(), 2);
        assert!(time.slept().is_empty());
        assert_no_secret(&error);
    }

    #[tokio::test]
    async fn a_503_is_retried_through_the_gateway() {
        let endpoint = Arc::new(
            Scripted::answering(200)
                .then(200, &session_answer(JWT_ONE))
                .then(503, "")
                .then(200, r#"{"id":"Main"}"#),
        );
        let (client, time) = client_over(BUDGETS, &endpoint);

        let body = client
            .get_portfolio("Main")
            .await
            .expect("the retry succeeds");

        assert_eq!(body, r#"{"id":"Main"}"#);
        assert_eq!(endpoint.received.lock().expect("received").len(), 3);
        assert_eq!(time.slept(), vec![iaam_http::gateway::FIRST_BACKOFF]);
    }

    #[tokio::test]
    async fn a_503_to_the_end_says_when_to_try_again_without_the_token() {
        let endpoint = Arc::new(Scripted::answering(503).then(200, &session_answer(JWT_ONE)));
        let (client, _) = client_over(BUDGETS, &endpoint);

        let error = client
            .get_portfolio("Main")
            .await
            .expect_err("every attempt fails");

        assert_eq!(
            endpoint.received.lock().expect("received").len(),
            1 + iaam_http::gateway::ATTEMPTS as usize,
        );
        match &error {
            FinamError::Unavailable {
                status,
                attempts,
                retry_after,
            } => {
                assert_eq!(*status, Some(503));
                assert_eq!(*attempts, iaam_http::gateway::ATTEMPTS);
                assert!(!retry_after.is_zero());
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
        assert_no_secret(&error);
    }

    #[tokio::test]
    async fn a_429_to_the_end_is_rate_limited_with_its_wait() {
        let endpoint = Arc::new(Scripted::answering(429).then(200, &session_answer(JWT_ONE)));
        let (client, _) = client_over(BUDGETS, &endpoint);

        let error = client
            .get_portfolio("Main")
            .await
            .expect_err("every attempt is throttled");

        match error {
            FinamError::RateLimited { retry_after } => assert!(!retry_after.is_zero()),
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    /// The gateway has already cut the session token out of a rejected
    /// body by the time the client sees it; the client cuts the owner's
    /// secret, which no layer before this one knows to look for.
    #[tokio::test]
    async fn a_rejected_body_is_kept_with_the_secret_hidden() {
        let endpoint = Arc::new(
            Scripted::answering(400)
                .then(200, &session_answer(JWT_ONE))
                .then(400, &format!("bad {SECRET}")),
        );
        let (client, _) = client_over(BUDGETS, &endpoint);

        let error = client
            .get_portfolio("Main")
            .await
            .expect_err("400 is refused");

        assert_eq!(
            error,
            FinamError::UnexpectedStatus {
                status: 400,
                body: "bad <token hidden>".to_owned(),
            }
        );
        assert_no_secret(&error);
    }

    #[tokio::test]
    async fn an_exchange_that_cannot_complete_is_the_call_s_refusal() {
        let endpoint = Arc::new(Scripted::answering(503));
        let (client, _) = client_over(BUDGETS, &endpoint);

        let error = client
            .get_portfolio("Main")
            .await
            .expect_err("every attempt of the exchange fails");

        assert!(
            matches!(
                error,
                FinamError::Unavailable { status: Some(503), attempts, .. }
                    if attempts == iaam_http::gateway::ATTEMPTS
            ),
            "{error:?}"
        );
        assert_no_secret(&error);
    }

    #[tokio::test]
    async fn a_401_to_the_exchange_itself_is_a_refusal_not_a_renewal() {
        let endpoint = Arc::new(Scripted::answering(401));
        let (client, _) = client_over(BUDGETS, &endpoint);

        let error = client
            .get_portfolio("Main")
            .await
            .expect_err("the secret itself is refused");

        assert_eq!(error, FinamError::InvalidToken);
        assert_eq!(endpoint.received.lock().expect("received").len(), 1);
        assert_no_secret(&error);
    }

    #[tokio::test]
    async fn an_exchange_answer_without_a_token_is_malformed() {
        for body in ["{}", r#"{"token":""}"#] {
            let endpoint = Arc::new(Scripted::answering(200).then(200, body));
            let (client, _) = client_over(BUDGETS, &endpoint);

            let error = client
                .get_portfolio("Main")
                .await
                .expect_err("no token, no session");

            assert_eq!(error, FinamError::MalformedResponse);
        }
    }

    /// A client or trust root that could not be built is this build's
    /// fault: retrying later meets it again, so it is neither "unavailable"
    /// nor a network refusal.
    #[tokio::test]
    async fn a_client_that_cannot_be_built_is_a_fault_of_this_build_not_retried() {
        for fault in [
            HttpError::ClientNotBuilt("invented".to_owned()),
            HttpError::TrustAnchorNotParsed("invented".to_owned()),
        ] {
            let expected = FinamError::TransportNotBuilt {
                reason: fault.to_string(),
            };
            let endpoint = Arc::new(Scripted::answering(200).then_fault(fault));
            let (client, time) = client_over(BUDGETS, &endpoint);

            let error = client
                .get_portfolio("Main")
                .await
                .expect_err("no client, no request");

            assert_eq!(error, expected);
            assert_eq!(endpoint.received.lock().expect("received").len(), 1);
            assert!(time.slept().is_empty());
            assert_no_secret(&error);
        }
    }

    #[tokio::test]
    async fn a_network_fault_to_the_end_is_unavailable_with_no_status() {
        let endpoint = Arc::new(Scripted::answering(200));
        for _ in 0..iaam_http::gateway::ATTEMPTS {
            endpoint
                .script
                .lock()
                .expect("script")
                .push_back(Err(HttpError::Network));
        }
        let (client, _) = client_over(BUDGETS, &endpoint);

        let error = client
            .get_portfolio("Main")
            .await
            .expect_err("every attempt faults");

        assert!(
            matches!(
                error,
                FinamError::Unavailable { status: None, attempts, .. }
                    if attempts == iaam_http::gateway::ATTEMPTS
            ),
            "{error:?}"
        );
    }

    /// A table with no Finam row: a build fault the gateway catches.
    static NO_FINAM: &[Budget] = &[Budget {
        destination: Destination::MoexIss,
        scope: MethodScope::Shared,
        documented: None,
        used: 1,
        window: Duration::from_secs(1),
    }];

    #[tokio::test]
    async fn a_missing_budget_row_is_a_gateway_refusal_and_sends_nothing() {
        let endpoint = Arc::new(Scripted::answering(200));
        let (client, _) = client_over(NO_FINAM, &endpoint);

        let error = client
            .get_portfolio("Main")
            .await
            .expect_err("no budget, no request");

        assert!(matches!(error, FinamError::Gateway { .. }), "{error:?}");
        assert!(endpoint.received.lock().expect("received").is_empty());
        assert_no_secret(&error);
    }

    /// One Finam request a minute per method: tight enough to show which
    /// budget a call draws on.
    static ONE_PER_METHOD: &[Budget] = &[
        Budget {
            destination: Destination::FinamApi,
            scope: MethodScope::Named("AccountsService.GetAccount"),
            documented: Some(200),
            used: 1,
            window: Duration::from_secs(60),
        },
        Budget {
            destination: Destination::FinamApi,
            scope: MethodScope::Named("AccountsService.Transactions"),
            documented: Some(200),
            used: 1,
            window: Duration::from_secs(60),
        },
        Budget {
            destination: Destination::FinamApi,
            scope: MethodScope::Named("AuthService.Sessions"),
            documented: Some(200),
            used: 1,
            window: Duration::from_secs(60),
        },
    ];

    #[tokio::test]
    async fn each_method_draws_on_its_own_budget() {
        let endpoint = Arc::new(
            Scripted::answering(200)
                .then(200, &session_answer(JWT_ONE))
                .then(200, "{}")
                .then(200, r#"{"transactions":[]}"#)
                .then(200, "{}"),
        );
        let (client, time) = client_over(ONE_PER_METHOD, &endpoint);

        client.get_portfolio("Main").await.expect("portfolio");
        client
            .get_transactions("Main", date!(2024 - 01 - 01), date!(2024 - 02 - 01))
            .await
            .expect("transactions");
        assert!(time.slept().is_empty(), "three methods, three budgets");

        client.get_portfolio("Main").await.expect("portfolio again");
        assert_eq!(time.slept(), vec![Duration::from_secs(60)]);
        // One exchange for four calls: the session rides its own budget.
        assert_eq!(endpoint.received.lock().expect("received").len(), 4);
    }

    #[tokio::test]
    async fn account_ids_come_from_the_details_answer() {
        let endpoint = Arc::new(
            Scripted::answering(200)
                .then(200, &session_answer(JWT_ONE))
                .then(200, r#"{"account_ids":["One","Two"],"readonly":true}"#),
        );
        let (client, _) = client_over(BUDGETS, &endpoint);

        let ids = client.get_account_ids().await.expect("the accounts");

        assert_eq!(ids, vec!["One".to_owned(), "Two".to_owned()]);
        let received = endpoint.received.lock().expect("received");
        assert_eq!(received.len(), 2);
        let details = &received[1];
        assert_eq!(details.url(), "https://api.finam.ru/v1/sessions/details");
        assert_eq!(details.bearer().map(|token| token.expose()), Some(JWT_ONE));
        assert_eq!(
            details.body().map(iaam_http::RequestBody::payload),
            Some(r#"{"token":"invented.jwt.one"}"#),
        );
    }

    #[tokio::test]
    async fn a_details_answer_without_account_ids_is_malformed() {
        let endpoint = Arc::new(
            Scripted::answering(200)
                .then(200, &session_answer(JWT_ONE))
                .then(200, r#"{"readonly":true}"#),
        );
        let (client, _) = client_over(BUDGETS, &endpoint);

        let error = client
            .get_account_ids()
            .await
            .expect_err("no ids, no answer");

        assert_eq!(error, FinamError::MalformedResponse);
    }

    /// The details answer describes the very token the call carried: a
    /// ten-minute span there means the session dies ten minutes in, not
    /// the documented fifteen.
    #[tokio::test]
    async fn the_details_answer_corrects_the_session_s_lifetime() {
        let endpoint = Arc::new(
            Scripted::answering(200)
                .then(200, &session_answer(JWT_ONE))
                .then(200, "{}")
                .then(
                    200,
                    r#"{"account_ids":["One"],"created_at":"2026-01-01T00:00:00Z",
                       "expires_at":"2026-01-01T00:10:00Z"}"#,
                )
                .then(200, &session_answer(JWT_TWO))
                .then(200, "{}"),
        );
        let (client, time) = client_over(BUDGETS, &endpoint);

        client
            .get_portfolio("Main")
            .await
            .expect("the first session");
        client.get_account_ids().await.expect("the accounts");

        // One second inside the corrected margin: the ten-minute token is
        // exchanged anew, though the uncorrected fifteen-minute assumption
        // would still call it good for four more minutes and reuse it.
        let corrected = Duration::from_secs(10 * 60);
        time.advance(corrected - RENEW_BEFORE + Duration::from_secs(1));

        client
            .get_portfolio("Main")
            .await
            .expect("the renewed session");

        let received = endpoint.received.lock().expect("received");
        assert_eq!(received.len(), 5);
        assert_eq!(received[3].url(), "https://api.finam.ru/v1/sessions");
        assert_eq!(
            received[4].bearer().map(|token| token.expose()),
            Some(JWT_TWO)
        );
    }

    /// The answer naming the documented lifetime leaves the session exactly
    /// as alive as the default did: no early renewal may creep in.
    #[tokio::test]
    async fn a_full_lifetime_details_answer_leaves_the_session_alive() {
        let endpoint = Arc::new(
            Scripted::answering(200)
                .then(200, &session_answer(JWT_ONE))
                .then(200, "{}")
                .then(
                    200,
                    r#"{"account_ids":["One"],"created_at":"2026-01-01T00:00:00Z",
                       "expires_at":"2026-01-01T00:15:00Z"}"#,
                )
                .then(200, "{}"),
        );
        let (client, time) = client_over(BUDGETS, &endpoint);

        client
            .get_portfolio("Main")
            .await
            .expect("the first session");
        client.get_account_ids().await.expect("the accounts");

        time.advance(Duration::from_secs(60));
        client.get_portfolio("Main").await.expect("portfolio");

        let received = endpoint.received.lock().expect("received");
        assert_eq!(received.len(), 4);
        assert_eq!(
            received[3].bearer().map(|token| token.expose()),
            Some(JWT_ONE)
        );
    }

    /// A correction names the token its answer described: a stale answer
    /// for the long-gone first token must not rewrite a newer session's
    /// end, even with a span that expired long ago.
    #[tokio::test]
    async fn a_stale_correction_leaves_a_newer_session_alone() {
        let endpoint = Arc::new(
            Scripted::answering(200)
                .then(200, &session_answer(JWT_ONE))
                .then(200, "{}")
                .then(200, &session_answer(JWT_TWO))
                .then(200, "{}")
                .then(200, &session_answer(JWT_THREE))
                .then(200, "{}"),
        );
        let (client, time) = client_over(BUDGETS, &endpoint);

        client
            .get_portfolio("Main")
            .await
            .expect("the first session");

        // Past the margin: the renewal puts the second token in the cache.
        time.advance(SESSION_LIFETIME - RENEW_BEFORE + Duration::from_secs(1));
        client
            .get_portfolio("Main")
            .await
            .expect("the renewed session");

        // A stale answer for the long-gone first token names that token's
        // long-over span. It is not this session's to inherit: left
        // unguarded it would drag the second token's end to now.
        client.correct_session_lifetime(JWT_ONE, Duration::from_secs(60));

        time.advance(Duration::from_secs(60));
        client.get_portfolio("Main").await.expect("portfolio");

        let received = endpoint.received.lock().expect("received");
        assert_eq!(received.len(), 5);
        assert_eq!(
            received[4].bearer().map(|token| token.expose()),
            Some(JWT_TWO)
        );
    }

    #[tokio::test]
    async fn a_401_before_the_details_is_renewed_and_retried_once() {
        let endpoint = Arc::new(
            Scripted::answering(200)
                .then(200, &session_answer(JWT_ONE))
                .then(401, "")
                .then(200, &session_answer(JWT_TWO))
                .then(200, r#"{"account_ids":["Three"]}"#),
        );
        let (client, _) = client_over(BUDGETS, &endpoint);

        let ids = client.get_account_ids().await.expect("the retry succeeds");

        assert_eq!(ids, vec!["Three".to_owned()]);
        let received = endpoint.received.lock().expect("received");
        assert_eq!(received.len(), 4);
        assert_eq!(
            received[3].bearer().map(|token| token.expose()),
            Some(JWT_TWO)
        );
        assert_eq!(
            received[3].body().map(iaam_http::RequestBody::payload),
            Some(r#"{"token":"invented.jwt.two"}"#),
        );
    }

    #[test]
    fn classifies_auth_and_unexpected_statuses() {
        assert!(matches!(
            classify_rejection(401, b"", "secret", None),
            FinamError::InvalidToken
        ));
        assert!(matches!(
            classify_rejection(403, b"", "secret", Some("token")),
            FinamError::InvalidToken
        ));
        assert!(matches!(
            classify_rejection(404, b"failure", "secret", None),
            FinamError::UnexpectedStatus { status: 404, .. }
        ));
    }

    #[test]
    fn unexpected_status_never_prints_a_secret() {
        let secret = "owner-secret-value";
        let token = "session-jwt-value";
        let error = classify_rejection(
            404,
            b"upstream owner-secret-value and session-jwt-value",
            secret,
            Some(token),
        );
        for hidden in [secret, token] {
            assert!(!error.to_string().contains(hidden));
            assert!(!format!("{error:?}").contains(hidden));
        }
    }

    /// An answer carrying exactly the limit may continue past the page:
    /// the interval splits at its middle day and each half is asked again,
    /// and the merged answer keeps the wire's order.
    #[tokio::test]
    async fn a_response_that_reaches_the_limit_splits_the_interval() {
        let endpoint = Arc::new(
            Scripted::answering(200)
                .then(200, &session_answer(JWT_ONE))
                .then(
                    200,
                    r#"{"transactions":[{"id":"first-one"},{"id":"first-two"}]}"#,
                )
                .then(200, r#"{"transactions":[{"id":"left"}]}"#)
                .then(200, r#"{"transactions":[{"id":"right"}]}"#),
        );
        let (client, _) = client_with_limit(BUDGETS, &endpoint, 2);

        let body = client
            .get_transactions("Main", date!(2024 - 01 - 01), date!(2024 - 02 - 01))
            .await
            .expect("the whole interval is fetched");

        let received = endpoint.received.lock().expect("received");
        assert_eq!(received.len(), 4, "one page per split, one exchange");
        let full = "https://api.finam.ru/v1/accounts/Main/transactions\
             ?interval%2Estart%5Ftime=2024%2D01%2D01T00%3A00%3A00Z\
             &interval%2Eend%5Ftime=2024%2D02%2D01T00%3A00%3A00Z&limit=2";
        let left = "https://api.finam.ru/v1/accounts/Main/transactions\
             ?interval%2Estart%5Ftime=2024%2D01%2D01T00%3A00%3A00Z\
             &interval%2Eend%5Ftime=2024%2D01%2D16T00%3A00%3A00Z&limit=2";
        let right = "https://api.finam.ru/v1/accounts/Main/transactions\
             ?interval%2Estart%5Ftime=2024%2D01%2D17T00%3A00%3A00Z\
             &interval%2Eend%5Ftime=2024%2D02%2D01T00%3A00%3A00Z&limit=2";
        assert_eq!(received[1].url(), full, "the asked interval goes first");
        assert_eq!(
            received[2].url(),
            left,
            "the first half narrows the interval"
        );
        assert_eq!(
            received[3].url(),
            right,
            "the second half takes over the middle day"
        );

        let merged: serde_json::Value = serde_json::from_str(&body).expect("merged body");
        let ids: Vec<&str> = merged["transactions"]
            .as_array()
            .expect("merged transactions")
            .iter()
            .map(|transaction| transaction["id"].as_str().expect("id"))
            .collect();
        // The halves re-cover the full page's interval, so its rows come
        // back inside them: the merge appends only the halves, and no
        // transaction is carried twice.
        assert_eq!(ids, ["left", "right"]);
    }

    /// A single day has no smaller interval to ask: an answer that reaches
    /// the limit there is refused, never fetched truncated.
    #[tokio::test]
    async fn an_answer_for_one_day_that_reaches_the_limit_is_refused() {
        let endpoint = Arc::new(
            Scripted::answering(200)
                .then(200, &session_answer(JWT_ONE))
                .then(200, r#"{"transactions":[{"id":"one"},{"id":"two"}]}"#),
        );
        let (client, _) = client_with_limit(BUDGETS, &endpoint, 2);

        let error = client
            .get_transactions("Main", date!(2024 - 01 - 01), date!(2024 - 01 - 01))
            .await
            .expect_err("a full single day cannot be proven complete");

        assert!(
            matches!(error, FinamError::PartialResponse),
            "the refusal names the unprovable interval: {error}"
        );
        assert_eq!(
            endpoint.received.lock().expect("received").len(),
            2,
            "one exchange, one refused page, no further request"
        );
    }

    /// An interval that names no days holds no transactions: nothing is
    /// asked, and the empty answer is the whole truth.
    #[tokio::test]
    async fn an_interval_that_names_no_days_sends_no_request() {
        let endpoint = Arc::new(Scripted::answering(200));
        let (client, _) = client_over(BUDGETS, &endpoint);

        let body = client
            .get_transactions("Main", date!(2024 - 02 - 01), date!(2024 - 01 - 01))
            .await
            .expect("an empty interval is an empty answer");

        assert_eq!(body, r#"{"transactions":[]}"#);
        assert!(
            endpoint.received.lock().expect("received").is_empty(),
            "no request may be spent on an interval with no days"
        );
    }
}
