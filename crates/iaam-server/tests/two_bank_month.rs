//! One synthetic month, imported from two institutions, asserted through the
//! HTTP surface (iaam-tk2j).
//!
//! The scenario an external agent hit: a month from two banks imported into an
//! empty database, every verdict positive, every fact in the journal, and half
//! the money outside the contour the report was computed over. Six beads came
//! out of that report. This file is the month they are all measured against,
//! written once so each fix is judged by the same fixture.
//!
//! Several assertions here cannot pass yet. They carry `#[ignore]` naming the
//! bead that will make them pass; each is a target, not a decoration, and each
//! was run with `--ignored` and read to confirm it fails for the reason its
//! bead describes rather than because the fixture is wrong.
//!
//! Nothing in this file is derived from any real export: the institutions, the
//! accounts and every amount are invented (CLAUDE.md, "Conventions & Patterns").

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::{Clock, UnavailableOutboundHttp};
use iaam_app::storage::SqliteStore;
use iaam_app::storage::{TokenRecord, TokenScope};
use iaam_core::ids::OwnerId;
use iaam_server::auth::hash_token;
use iaam_server::rate_limit::RateLimiter;
use iaam_server::{ServerState, build};
use serde_json::{Value, json};
use time::Date;
use time::macros::date;
use tower::ServiceExt;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A clock fixed after the imported month: a report «as at today» is otherwise
/// not reproducible.
struct FixedClock(Date);

impl Clock for FixedClock {
    fn today(&self) -> Date {
        self.0
    }
}

struct Harness {
    router: Router,
    owner_token: String,
}

/// An owner with tokens and **no accounts**: the scenario starts from an empty
/// database, which is where the reported failure started.
fn harness() -> Harness {
    let store = SqliteStore::open_in_memory().expect("in-memory database");
    let owner = OwnerId::new_random();
    let owner_token = "owner-secret-token";
    store
        .insert_token(
            &TokenRecord {
                id: Uuid::new_v4(),
                owner,
                label: "owner".into(),
                scope: TokenScope::Owner,
                revoked: false,
            },
            &hash_token(owner_token),
        )
        .expect("owner token");

    let adapter = Arc::new(SqliteAdapter::new(store));
    let services = Arc::new(AppServices {
        store: adapter.clone(),
        directory: adapter.clone(),
        broker: adapter.clone(),
        tokens: adapter.clone(),
        clock: Arc::new(FixedClock(date!(2025 - 04 - 01))),
        channels: adapter.clone(),
        rules: adapter.clone(),
        categories: adapter.clone(),
        http: Arc::new(UnavailableOutboundHttp),
        broker_dictionary: adapter.clone(),
        market_store: Arc::new(tokio::sync::Mutex::new(
            SqliteStore::open_in_memory().expect("market store"),
        )),
    });
    let (router, _api) = build(ServerState::new(
        services,
        Arc::new(RateLimiter::new(1_000, Duration::from_secs(60))),
    ))
    .expect("build");

    Harness {
        router,
        owner_token: owner_token.to_owned(),
    }
}

async fn call(router: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("handler responded");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response body")
        .to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

fn get(path: &str, token: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .method("GET")
        .header("Authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("request")
}

fn post(path: &str, token: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .uri(path)
        .method("POST")
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

// ---------------------------------------------------------------------------
// The fixture: one month, two institutions, invented from nothing
// ---------------------------------------------------------------------------

/// The month the whole file reports on.
const MONTH_FROM: &str = "2025-03-01";
const MONTH_TO: &str = "2025-03-31";

/// The two institutions. Invented names: nothing here is a bank that exists.
const NORTHLINE: &str = "Northline";
const SOUTHGATE: &str = "Southgate";

/// The five accounts, once the API has minted their identifiers.
///
/// Three at one institution and two at the other, because the failure being
/// reproduced needs a second institution created **after** the first contour
/// was drawn — with one bank the owner does it by hand and never notices.
#[derive(Debug, Clone, Copy)]
struct Accounts {
    /// Northline, current.
    main: Uuid,
    /// Northline, savings.
    savings: Uuid,
    /// Northline, the term deposit closed during the month.
    term: Uuid,
    /// Southgate, current.
    everyday: Uuid,
    /// Southgate, the deposit opened during the month.
    reserve: Uuid,
}

impl Accounts {
    /// Every account, in creation order.
    const fn all(self) -> [Uuid; 5] {
        [
            self.main,
            self.savings,
            self.term,
            self.everyday,
            self.reserve,
        ]
    }

    const fn northline(self) -> [Uuid; 3] {
        [self.main, self.savings, self.term]
    }

    const fn southgate(self) -> [Uuid; 2] {
        [self.everyday, self.reserve]
    }
}

/// The amount of the transfer that crosses the two institutions.
///
/// One economic event, printed by each bank as its own row: an outgoing one at
/// Northline and an incoming one at Southgate. Named because three assertions
/// turn on it.
const CROSS_BANK_AMOUNT: &str = "12000.00";
/// The month's one expense.
const EXPENSE_AMOUNT: &str = "1500.00";
/// The month's one income — pay arriving from outside the perimeter.
const INCOME_AMOUNT: &str = "40000.00";
/// The month's one interest accrual, credited by the closing term deposit.
const INTEREST_AMOUNT: &str = "350.00";
/// The internal transfer inside Northline.
const INTERNAL_AMOUNT: &str = "5000.00";
/// The principal and interest the closing term deposit returns to `Main`.
const DEPOSIT_CLOSED_AMOUNT: &str = "20350.00";
/// What opens the deposit at Southgate.
const DEPOSIT_OPENED_AMOUNT: &str = "30000.00";
/// The row whose direction the source did not give.
const AMBIGUOUS_AMOUNT: &str = "2500.00";

async fn create_account(harness: &Harness, title: &str, institution: &str) -> Uuid {
    let (status, body) = call(
        &harness.router,
        post(
            "/v1/accounts",
            &harness.owner_token,
            &json!({ "title": title, "institution": institution }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok())
        .expect("account identifier")
}

/// Create the five accounts. No operation has been imported at this point.
async fn create_accounts(harness: &Harness) -> Accounts {
    Accounts {
        main: create_account(harness, "Main", NORTHLINE).await,
        savings: create_account(harness, "Savings", NORTHLINE).await,
        term: create_account(harness, "Term Deposit", NORTHLINE).await,
        everyday: create_account(harness, "Everyday", SOUTHGATE).await,
        reserve: create_account(harness, "Reserve Deposit", SOUTHGATE).await,
    }
}

/// One import: the rows of one account, under a source the caller declares.
///
/// Declared rather than left to the server, because an import that names itself
/// is the one `POST /v1/corrections/imports` can retract, and a month imported
/// against the wrong account map is the failure this scenario came from.
fn import(account: Uuid, label: &str, operations: Vec<Value>) -> Value {
    json!({
        "source": { "account": account, "channel": "file", "label": label },
        "operations": operations,
    })
}

/// Northline's statement for `Main`.
fn main_rows(accounts: Accounts) -> Value {
    import(
        accounts.main,
        "northline-main-2025-03",
        vec![
            // The internal transfer inside one bank.
            json!({
                "account": accounts.main,
                "type": "transfer",
                "to_account": accounts.savings,
                "amount": INTERNAL_AMOUNT,
                "currency": "RUB",
                "dates": { "cash_posted": "2025-03-12" },
                "idempotency_key": "main-to-savings",
            }),
            // Leg one of the transfer between the two banks. Northline prints an
            // outgoing row and knows nothing about the account on the other side.
            json!({
                "account": accounts.main,
                "type": "withdrawal",
                "amount": CROSS_BANK_AMOUNT,
                "currency": "RUB",
                "dates": { "cash_posted": "2025-03-15" },
                "description": "Transfer to Southgate",
                "idempotency_key": "main-cross-bank-out",
            }),
            // The month's expense.
            json!({
                "account": accounts.main,
                "type": "withdrawal",
                "amount": EXPENSE_AMOUNT,
                "currency": "RUB",
                "dates": { "cash_posted": "2025-03-20" },
                "description": "Shop One",
                "source_category": "Shops",
                "idempotency_key": "main-shop-one",
            }),
        ],
    )
}

/// Northline's statement for the term deposit that closes during the month.
fn term_rows(accounts: Accounts) -> Value {
    import(
        accounts.term,
        "northline-term-2025-03",
        vec![
            // The month's interest accrual.
            json!({
                "account": accounts.term,
                "type": "income",
                "amount": INTEREST_AMOUNT,
                "currency": "RUB",
                "kind": "deposit_interest",
                "dates": { "cash_posted": "2025-03-10" },
                "idempotency_key": "term-interest",
            }),
            // The deposit closes: principal and interest return to `Main`.
            json!({
                "account": accounts.term,
                "type": "transfer",
                "to_account": accounts.main,
                "amount": DEPOSIT_CLOSED_AMOUNT,
                "currency": "RUB",
                "dates": { "cash_posted": "2025-03-10" },
                "description": "Deposit closed",
                "idempotency_key": "term-closed",
            }),
        ],
    )
}

/// Southgate's statement for `Everyday`.
fn everyday_rows(accounts: Accounts) -> Value {
    import(
        accounts.everyday,
        "southgate-everyday-2025-03",
        vec![
            // The month's income, arriving from outside the perimeter.
            json!({
                "account": accounts.everyday,
                "type": "deposit",
                "amount": INCOME_AMOUNT,
                "currency": "RUB",
                "dates": { "cash_posted": "2025-03-05" },
                "description": "Pay",
                "idempotency_key": "everyday-pay",
            }),
            // Leg two of the transfer between the two banks.
            json!({
                "account": accounts.everyday,
                "type": "deposit",
                "amount": CROSS_BANK_AMOUNT,
                "currency": "RUB",
                "dates": { "cash_posted": "2025-03-15" },
                "description": "Transfer from Northline",
                "idempotency_key": "everyday-cross-bank-in",
            }),
            // The deposit that opens during the month.
            json!({
                "account": accounts.everyday,
                "type": "transfer",
                "to_account": accounts.reserve,
                "amount": DEPOSIT_OPENED_AMOUNT,
                "currency": "RUB",
                "dates": { "cash_posted": "2025-03-25" },
                "description": "Deposit opened",
                "idempotency_key": "everyday-opens-reserve",
            }),
        ],
    )
}

/// The row whose direction the source did not give.
///
/// The shape a bank export takes when it prints `INNER` with an amount and
/// nothing else: not which side of it this account was on, and not which
/// account was the other side. It is kept out of [`import_northline`] and
/// [`import_southgate`] on purpose — there is no wire shape for it today, so
/// every other assertion in this file would have to carry the guess the
/// importer was forced to make, which is exactly what iaam-6qsa is about.
///
/// `type` here is the word iaam-6qsa's reporter used for the concept, not a
/// word this API publishes. Whatever the fix names it, the assertion the test
/// makes about it does not change: a row with no direction comes back needing
/// an answer, and moves no balance until it has one.
fn ambiguous_row(accounts: Accounts) -> Value {
    import(
        accounts.everyday,
        "southgate-everyday-2025-03-inner",
        vec![json!({
            "account": accounts.everyday,
            "type": "unresolved_direction",
            "amount": AMBIGUOUS_AMOUNT,
            "currency": "RUB",
            "dates": { "cash_posted": "2025-03-18" },
            "source_category": "INNER",
            "idempotency_key": "everyday-inner",
        })],
    )
}

async fn submit(harness: &Harness, batch: &Value) -> Value {
    let (status, verdicts) = call(
        &harness.router,
        post("/v1/ingest/operations", &harness.owner_token, batch),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    for verdict in verdicts.as_array().expect("array of verdicts") {
        assert_ne!(
            verdict["verdict"], "rejected",
            "the fixture must be submittable: {verdicts}"
        );
    }
    verdicts
}

async fn import_northline(harness: &Harness, accounts: Accounts) {
    submit(harness, &main_rows(accounts)).await;
    submit(harness, &term_rows(accounts)).await;
}

async fn import_southgate(harness: &Harness, accounts: Accounts) {
    submit(harness, &everyday_rows(accounts)).await;
}

/// Draw a contour over the accounts named, and return its identifier.
async fn draw_contour(harness: &Harness, title: &str, accounts: &[Uuid]) -> String {
    let (status, body) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({ "title": title, "accounts": accounts }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["contour"].as_str().expect("contour id").to_owned()
}

async fn flow_report(harness: &Harness, contour: &str) -> Value {
    let (status, body) = call(
        &harness.router,
        get(
            &format!("/v1/reports/flow?contour={contour}&from={MONTH_FROM}&to={MONTH_TO}"),
            &harness.owner_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn actions(harness: &Harness) -> Vec<Value> {
    let (status, body) = call(&harness.router, get("/v1/actions", &harness.owner_token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body.as_array().expect("action items").clone()
}

/// The one currency the month is denominated in.
fn rub(report: &Value) -> Value {
    report["currencies"]
        .as_array()
        .expect("currencies")
        .iter()
        .find(|entry| entry["currency"] == "RUB")
        .cloned()
        .unwrap_or_else(|| panic!("no RUB in {report}"))
}

// ---------------------------------------------------------------------------
// 1. No operation leaves the report because of which contour was chosen
// ---------------------------------------------------------------------------

/// The month's figures, stripped of every identifier.
///
/// Two runs of the same month mint different account identifiers, so the
/// reports cannot be compared whole. These are the numbers the owner reads, and
/// they are the thing that must not depend on the order the setup was done in.
fn figures(report: &Value) -> Value {
    let rub = rub(report);
    json!({
        "came_in": rub["came_in"],
        "went_out": rub["went_out"],
        "earned_by_capital": rub["earned_by_capital"],
        "moved_into_assets": rub["moved_into_assets"],
        "fees": rub["fees"],
        "taxes": rub["taxes"],
        "internal_transfers": rub["internal_transfers"],
        "cash_delta": rub["cash_delta"],
        "residual": rub["residual"],
        "not_decomposed_count": rub["not_decomposed"]["count"],
        "not_decomposed_amount": rub["not_decomposed"]["amount"],
    })
}

/// The month as the owner reads it, whichever order the setup was done in.
///
/// Absolute values, not only the agreement of two runs: two runs of one bug
/// agree with each other. Every figure below is the fixture added up by hand.
fn expected_figures() -> Value {
    json!({
        // The pay, plus the incoming leg of the cross-bank transfer, which is
        // an arrival from outside only because nothing relates it to its own
        // outgoing leg (iaam-3ul2).
        "came_in": "52000.00",
        // The expense, plus the outgoing leg of the same transfer.
        "went_out": "13500.00",
        // The deposit interest.
        "earned_by_capital": "350.00",
        "moved_into_assets": "0.00",
        "fees": "0.00",
        "taxes": "0.00",
        // Reported one-sided: each account's net position in the transfer
        // ledger, positive entries only. `Main` sends 5000.00 to `Savings` and
        // receives 20350.00 from the closing term deposit, netting 15350.00;
        // `Savings` receives 5000.00; `Reserve Deposit` receives 30000.00 as it
        // opens. The accounts that only sent contribute nothing.
        "internal_transfers": "50350.00",
        // 52000.00 in, 13500.00 out, 350.00 earned. Every internal transfer
        // nets to nothing.
        "cash_delta": "38850.00",
        // The six quantities explain every account's cash change.
        "residual": "0.00",
        // The two outflows, neither of which any category rule covers.
        "not_decomposed_count": 2,
        "not_decomposed_amount": "13500.00",
    })
}

/// Both sources imported, in either order, with the contour drawn before or
/// after: the report states the same month.
#[tokio::test]
async fn no_operation_leaves_the_report_because_of_the_order_the_setup_was_done_in() {
    // Contour first, then Northline, then Southgate.
    let first = harness();
    let accounts = create_accounts(&first).await;
    let contour = draw_contour(&first, "Household", &accounts.all()).await;
    import_northline(&first, accounts).await;
    import_southgate(&first, accounts).await;
    let contour_drawn_first = flow_report(&first, &contour).await;

    // Southgate first, then Northline, and only then the contour.
    let second = harness();
    let accounts = create_accounts(&second).await;
    import_southgate(&second, accounts).await;
    import_northline(&second, accounts).await;
    let contour = draw_contour(&second, "Household", &accounts.all()).await;
    let contour_drawn_last = flow_report(&second, &contour).await;

    assert_eq!(
        figures(&contour_drawn_first),
        expected_figures(),
        "{contour_drawn_first}"
    );
    assert_eq!(
        figures(&contour_drawn_last),
        expected_figures(),
        "{contour_drawn_last}"
    );

    // And the population is the same five accounts either way: an operation
    // that left the report would leave its account out of `covered` with it.
    for report in [&contour_drawn_first, &contour_drawn_last] {
        assert_eq!(report["population"]["completeness"], "whole", "{report}");
        assert_eq!(
            report["population"]["covered"]
                .as_array()
                .expect("covered")
                .len(),
            5,
            "{report}"
        );
        assert_eq!(report["population"]["outside"], json!([]), "{report}");
    }
}

// ---------------------------------------------------------------------------
// 2. A second contour cannot be created by omitting an identifier (iaam-j9oi)
// ---------------------------------------------------------------------------

/// The second bank is added to the contour the owner already has.
///
/// The reported failure exactly: a contour was drawn for the first bank, the
/// same route was called again for the second, and the caller received a second
/// contour holding only the second bank's accounts. Both exist, every operation
/// is in the journal, and the report for the newer contour shows one bank.
///
/// Under the split this is no longer an omission the caller can make: the create
/// route refuses a contour identifier outright and names the versions route in
/// its refusal, so "create a perimeter" and "write into the one I have" are two
/// calls that cannot be confused. What is asserted instead is the bead's other
/// criterion — the response says which contour was written and whether it was
/// new — and that adding the second bank reaches the first contour rather than
/// minting a second.
#[tokio::test]
async fn a_second_bank_joins_the_contour_instead_of_minting_another() {
    let harness = harness();
    let accounts = create_accounts(&harness).await;
    let first = draw_contour(&harness, "Household", &accounts.northline()).await;

    // The call that used to mint a second perimeter in silence is now refused,
    // and the refusal names the route that does what the caller meant.
    let (status, refused) = call(
        &harness.router,
        post(
            "/v1/contours",
            &harness.owner_token,
            &json!({
                "contour": first,
                "title": "Household",
                "accounts": accounts.southgate()
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert!(
        refused["message"]
            .as_str()
            .is_some_and(|text| text.contains("/v1/contours/{contour}/versions")),
        "the refusal must name the route that adds a version: {refused}"
    );

    // The whole month's accounts, written into the contour that already exists.
    let (status, added) = call(
        &harness.router,
        post(
            &format!("/v1/contours/{first}/versions"),
            &harness.owner_token,
            &json!({ "title": "Household", "accounts": accounts.all() }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{added}");
    assert_eq!(
        added["contour"], first,
        "a version belongs to the contour it was written into: {added}"
    );
    assert_eq!(
        added["created"], false,
        "writing into a contour that exists did not create one: {added}"
    );

    // And only one perimeter exists, which is the thing the owner was never
    // able to check before: the read route did not exist.
    let (status, listed) = call(&harness.router, get("/v1/contours", &harness.owner_token)).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let contours = listed.as_array().map_or_else(
        || {
            listed["contours"]
                .as_array()
                .expect("a list of contours")
                .clone()
        },
        Clone::clone,
    );
    assert_eq!(contours.len(), 1, "one bank joined the other: {listed}");
}

// ---------------------------------------------------------------------------
// 3. An ambiguous row does not silently become a deposit (iaam-6qsa)
// ---------------------------------------------------------------------------

/// A row the source gave no direction for is answered, not guessed.
///
/// `Verdict::NeedsClassification` is published in this API's vocabulary and
/// constructed by nothing in production. This is the row that should construct
/// it: `INNER` with an amount, and neither the side this account was on nor the
/// account on the other side.
#[tokio::test]
async fn an_ambiguous_row_does_not_silently_become_a_deposit() {
    let harness = harness();
    let accounts = create_accounts(&harness).await;
    let contour = draw_contour(&harness, "Household", &accounts.all()).await;
    import_northline(&harness, accounts).await;
    import_southgate(&harness, accounts).await;
    let before = flow_report(&harness, &contour).await;

    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/ingest/operations",
            &harness.owner_token,
            &ambiguous_row(accounts),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a row with no direction must be submittable: {verdicts}"
    );
    assert_eq!(
        verdicts[0]["verdict"], "needs_classification",
        "a row with no direction must come back as a question, not a movement: {verdicts}"
    );
    let detail = verdicts[0]["detail"]
        .as_str()
        .unwrap_or_else(|| panic!("the verdict must carry the question: {verdicts}"));
    // Which question, not merely that there is one. A row whose direction the
    // source withheld must be asked about its direction; asking "is this
    // income?" about it would settle the direction by assuming it, one step
    // further along, and the owner's answer would record the guess as a rule.
    // Asserting only that some question came back does not catch that.
    assert!(
        detail.contains("INNER"),
        "the question must quote what the source did state: {verdicts}"
    );

    // And nothing moved on the guess: the balance does not change until the
    // owner has answered.
    let after = flow_report(&harness, &contour).await;
    assert_eq!(
        figures(&after),
        figures(&before),
        "an unanswered row must not move any figure: {after}"
    );
}

// ---------------------------------------------------------------------------
// 4. The two legs of the cross-bank transfer can be related (iaam-3ul2)
// ---------------------------------------------------------------------------

/// Money that moved between the owner's own banks did not leave the perimeter.
///
/// One transfer, printed by Northline as an outgoing row and by Southgate as an
/// incoming one, with the same amount and the same date. Recorded as two
/// unrelated movements it makes the report count an external outflow and an
/// external inflow that never happened — for a contour spanning both banks,
/// wrong twice over.
///
/// The confirmation is part of the test, and it has to be: the system proposes
/// the pair on its evidence and the owner relates it. Test 1 above imports the
/// same month, confirms nothing, and asserts the two legs are still counted
/// separately — so a build that related them by itself would fail that test.
/// The two assertions are compatible only because the relating is an act.
#[tokio::test]
async fn the_two_legs_of_the_cross_bank_transfer_can_be_related() {
    let harness = harness();
    let accounts = create_accounts(&harness).await;
    let contour = draw_contour(&harness, "Household", &accounts.all()).await;
    import_northline(&harness, accounts).await;
    import_southgate(&harness, accounts).await;

    // The pair is proposed, with the evidence it rests on, and nothing else is.
    let (status, proposals) = call(
        &harness.router,
        get("/v1/transfer-pairings", &harness.owner_token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{proposals}");
    let candidates = proposals["candidates"]
        .as_array()
        .expect("candidate pairs")
        .clone();
    assert_eq!(
        candidates.len(),
        1,
        "one pair, and it is the cross-bank transfer: {proposals}"
    );
    let candidate = &candidates[0];
    assert_eq!(
        candidate["evidence"]["amount"], CROSS_BANK_AMOUNT,
        "{proposals}"
    );
    assert_eq!(candidate["evidence"]["days_apart"], 0, "{proposals}");
    assert_eq!(candidate["evidence"]["sole_candidate"], true, "{proposals}");
    assert_eq!(
        candidate["outgoing"]["account"],
        accounts.main.to_string(),
        "{proposals}"
    );
    assert_eq!(
        candidate["incoming"]["account"],
        accounts.everyday.to_string(),
        "{proposals}"
    );
    // The month's other one-sided movements are reported as having no
    // counterpart rather than dropped: a leg that vanished from the answer is a
    // leg the owner reads as external flow by default.
    let without_counterpart = proposals["without_counterpart"]
        .as_array()
        .expect("legs with no counterpart");
    assert_eq!(
        without_counterpart.len(),
        2,
        "the pay and the expense pair with nothing: {proposals}"
    );

    // Nothing is related until the owner says so.
    let untouched = flow_report(&harness, &contour).await;
    assert_eq!(
        rub(&untouched)["came_in"],
        "52000.00",
        "a proposal is not a decision: {untouched}"
    );

    let (status, confirmed) = call(
        &harness.router,
        post(
            "/v1/transfer-pairings",
            &harness.owner_token,
            &json!({
                "outgoing": candidate["outgoing"]["event"],
                "incoming": candidate["incoming"]["event"],
                "acknowledge_retraction": true,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{confirmed}");
    assert!(
        confirmed["transfer"].is_string(),
        "the confirmation records one transfer: {confirmed}"
    );

    let report = flow_report(&harness, &contour).await;
    let rub = rub(&report);
    assert_eq!(
        rub["came_in"], INCOME_AMOUNT,
        "only the pay arrived from outside the perimeter: {report}"
    );
    assert_eq!(
        rub["went_out"], EXPENSE_AMOUNT,
        "only the expense left the perimeter: {report}"
    );
    // The month's arithmetic is untouched by the relation: the same money moved,
    // and relating its two legs moves it between quantities rather than
    // creating or destroying any of it.
    assert_eq!(rub["cash_delta"], "38850.00", "{report}");
    assert_eq!(rub["residual"], "0.00", "{report}");
    // `internal_transfers` is deliberately not pinned to a figure. Its value
    // depends on whether the fix records one movement between the two accounts
    // or two events carrying a relation, and iaam-3ul2 leaves that open. What
    // the report must not say is that the money left and separately arrived,
    // and the two assertions above state exactly that.
}

// ---------------------------------------------------------------------------
// 5. The queue asks about structure before it asks for an import (iaam-7xh3)
// ---------------------------------------------------------------------------

/// The stage iaam-7xh3 names, by the names its reporter proposed.
///
/// Pinned to those names rather than matched loosely: the queue already
/// contains items that are about structure in a broad sense — creating the
/// first contour, placing an account that belongs to none — and a loose match
/// would pass today while the question the reporter could not get an answer to
/// ("which accounts are the two sides of this transfer?") is still unasked.
const DISCOVERY_KINDS: [&str; 5] = [
    "discover_institutions",
    "discover_accounts",
    "map_source_accounts",
    "select_contour_membership",
    "resolve_transfer_relationships",
];

/// With two banks and nothing imported, the queue's first item is about
/// structure and not about importing.
#[tokio::test]
async fn the_queue_asks_about_structure_before_it_asks_for_an_import() {
    let harness = harness();
    let _accounts = create_accounts(&harness).await;

    let items = actions(&harness).await;
    let kinds: Vec<&str> = items
        .iter()
        .map(|item| item["kind"].as_str().expect("action kind"))
        .collect();

    let discovery = kinds
        .iter()
        .position(|kind| DISCOVERY_KINDS.contains(kind))
        .unwrap_or_else(|| panic!("the queue asks nothing about structure: {kinds:?}"));
    let importing = kinds
        .iter()
        .position(|kind| *kind == "start_account_import")
        .unwrap_or(usize::MAX);
    assert!(
        discovery < importing,
        "structure must be asked about before importing: {kinds:?}"
    );
    assert!(
        kinds.contains(&"resolve_transfer_relationships"),
        "the queue must ask which accounts are the two sides of a transfer: {kinds:?}"
    );
}

// ---------------------------------------------------------------------------
// 6. The report names which accounts it covers
// ---------------------------------------------------------------------------

/// A report that does not cover everything says so, and names what it left out.
///
/// This is the mechanism by which the reported import produced a silently
/// incomplete report: the second bank's accounts were in no contour, every
/// verdict was positive, and the report said nothing.
#[tokio::test]
async fn the_report_names_the_accounts_it_covers_and_the_ones_it_does_not() {
    let harness = harness();
    let accounts = create_accounts(&harness).await;
    // Northline only — the shape the reported failure left behind.
    let contour = draw_contour(&harness, "Northline only", &accounts.northline()).await;
    import_northline(&harness, accounts).await;
    import_southgate(&harness, accounts).await;

    let report = flow_report(&harness, &contour).await;
    let population = &report["population"];
    assert_eq!(
        population["contour"], contour,
        "the report names the scope it was computed over: {report}"
    );

    let covered: Vec<&str> = population["covered"]
        .as_array()
        .expect("covered")
        .iter()
        .map(|entry| entry["title"].as_str().expect("title"))
        .collect();
    assert_eq!(covered, ["Main", "Savings", "Term Deposit"], "{report}");

    let outside: Vec<&str> = population["outside"]
        .as_array()
        .expect("outside")
        .iter()
        .map(|entry| entry["title"].as_str().expect("title"))
        .collect();
    assert_eq!(outside, ["Everyday", "Reserve Deposit"], "{report}");

    // Nobody has ruled on the two accounts left out, so the report says the
    // part of the owner's money it answers about is undelimited.
    assert_eq!(population["completeness"], "undecided", "{report}");
    for entry in population["outside"].as_array().expect("outside") {
        assert_eq!(entry["standing"], "outside_undecided", "{report}");
    }
}

// ---------------------------------------------------------------------------
// 7. An opening balance is asked for, per account
// ---------------------------------------------------------------------------

/// Every account the month moved money in is asked for its opening balance.
///
/// Opening before closing: a closing balance compared against a sum accumulated
/// from an unasserted start yields a discrepancy that is not one.
#[tokio::test]
async fn an_opening_balance_is_asked_for_per_account() {
    let harness = harness();
    let accounts = create_accounts(&harness).await;
    draw_contour(&harness, "Household", &accounts.all()).await;
    import_northline(&harness, accounts).await;
    import_southgate(&harness, accounts).await;

    let items = actions(&harness).await;
    let asked: Vec<Uuid> = items
        .iter()
        .filter(|item| item["kind"] == "provide_control_assertion")
        .filter(|item| item["target"]["request"]["preset"]["at"] == "opening")
        .filter_map(|item| item["subject"]["id"].as_str())
        .filter_map(|id| Uuid::parse_str(id).ok())
        .collect();

    for account in [accounts.main, accounts.term, accounts.everyday] {
        assert!(
            asked.contains(&account),
            "no opening balance was asked for {account}: {items:#?}"
        );
    }
    // Asked once each, and the closing balance is not asked for beside it.
    //
    // Five, not three: the month moved money into all five accounts, and since
    // iaam-8axt the two whose whole content arrived by internal transfer are
    // counted as having facts too. The number this assertion protects is «once
    // each», and it was three only while two accounts were invisible.
    assert_eq!(asked.len(), 5, "{items:#?}");
    assert!(
        !items
            .iter()
            .any(|item| item["target"]["request"]["preset"]["at"] == "closing"),
        "the closing balance must not be asked for before the opening one: {items:#?}"
    );
}

/// The two accounts that only ever received a leg are asked for nothing.
///
/// `Savings` and `Reserve Deposit` each hold money at the end of the month —
/// the internal transfer put it there — and neither is asked for an opening
/// balance, because activity is counted from the account an event is recorded
/// against and a transfer is recorded against the account it leaves. So the two
/// accounts whose whole content arrived by transfer are the two the queue never
/// asks about, and a contour spanning both banks is full of them.
///
/// Found while building this scenario and filed as iaam-8axt:
/// `list_account_activity` joins on the account an event is recorded against,
/// and a transfer is recorded against one side only.
#[tokio::test]
async fn an_opening_balance_is_asked_for_an_account_that_only_received_transfers() {
    let harness = harness();
    let accounts = create_accounts(&harness).await;
    draw_contour(&harness, "Household", &accounts.all()).await;
    import_northline(&harness, accounts).await;
    import_southgate(&harness, accounts).await;

    let items = actions(&harness).await;
    let asked: Vec<Uuid> = items
        .iter()
        .filter(|item| item["kind"] == "provide_control_assertion")
        .filter_map(|item| item["subject"]["id"].as_str())
        .filter_map(|id| Uuid::parse_str(id).ok())
        .collect();

    for account in [accounts.savings, accounts.reserve] {
        assert!(
            asked.contains(&account),
            "no opening balance was asked for {account}, which the month moved money into: {items:#?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 8. A correction changes the reports and leaves the journal auditable
// ---------------------------------------------------------------------------

async fn journal_by_key(harness: &Harness, key: &str) -> Value {
    let (status, page) = call(
        &harness.router,
        get(
            &format!("/v1/journal/events?idempotency_key={key}"),
            &harness.owner_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    page
}

/// The expense is corrected: the report changes, and the journal still holds
/// what was first recorded.
#[tokio::test]
async fn a_correction_changes_the_reports_and_leaves_the_journal_auditable() {
    let harness = harness();
    let accounts = create_accounts(&harness).await;
    let contour = draw_contour(&harness, "Household", &accounts.all()).await;
    import_northline(&harness, accounts).await;
    import_southgate(&harness, accounts).await;

    let before = flow_report(&harness, &contour).await;
    assert_eq!(rub(&before)["went_out"], "13500.00", "{before}");

    let recorded = journal_by_key(&harness, "main-shop-one").await;
    let original = recorded["rows"][0]["event"]
        .as_str()
        .expect("the expense in the journal")
        .to_owned();

    // The bank printed the expense at one amount and the receipt says another.
    let (status, verdicts) = call(
        &harness.router,
        post(
            "/v1/corrections",
            &harness.owner_token,
            &json!({
                "acknowledge_retraction": true,
                "corrections": [{
                    "relation": "replacement",
                    "target": original,
                    "operation": {
                        "account": accounts.main,
                        "type": "withdrawal",
                        "amount": "1750.00",
                        "currency": "RUB",
                        "dates": { "cash_posted": "2025-03-20" },
                        "description": "Shop One",
                        "source_category": "Shops",
                    },
                }],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdicts}");
    assert_ne!(verdicts[0]["verdict"], "rejected", "{verdicts}");
    let replacement = verdicts[0]["event_id"]
        .as_str()
        .expect("the correcting event")
        .to_owned();

    // The report changes.
    let after = flow_report(&harness, &contour).await;
    assert_eq!(
        rub(&after)["went_out"],
        "13750.00",
        "the correction must reach the report: {after}"
    );

    // And the journal is still auditable: the original stands where it was, the
    // correcting fact names it, and nothing was mutated or deleted.
    let still_there = journal_by_key(&harness, "main-shop-one").await;
    assert_eq!(
        still_there["rows"][0]["event"], original,
        "a correction must not remove what was first recorded: {still_there}"
    );
    assert_eq!(
        still_there["rows"][0]["legs"][0]["amount"], "-1500.00",
        "the retracted fact keeps the value it was submitted with: {still_there}"
    );

    let (status, page) = call(
        &harness.router,
        get(
            &format!(
                "/v1/journal/events?account={}&from={MONTH_FROM}&to={MONTH_TO}",
                accounts.main
            ),
            &harness.owner_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let correcting = page["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .find(|row| row["event"] == replacement)
        .unwrap_or_else(|| panic!("the correcting fact is not in the journal: {page}"));
    assert_eq!(
        correcting["relation"]["kind"], "replacement",
        "{correcting}"
    );
    assert_eq!(correcting["relation"]["target"], original, "{correcting}");
}
