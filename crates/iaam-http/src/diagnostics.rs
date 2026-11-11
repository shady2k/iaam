//! Bounded, allowlisted diagnostics for a broker's refused response.

use crate::gateway::without_secrets;
use crate::{Destination, HttpRequest, HttpResponse};

fn millis(wait: std::time::Duration) -> u64 {
    u64::try_from(wait.as_millis()).unwrap_or(u64::MAX)
}

/// Broker refusals expose only selected error fields, never an arbitrary body.
/// Redact before bounding a field so a token crossing the cut cannot leak.
fn refusal_field(value: &str, request: &HttpRequest) -> String {
    without_secrets(
        Some(value),
        request
            .bearer()
            .into_iter()
            .chain(request.cache_identity())
            .chain(request.redaction_words().iter()),
    )
    .unwrap_or_default()
    .chars()
    .map(|character| {
        if character.is_control() {
            ' '
        } else {
            character
        }
    })
    .scan(0, |bytes, character| {
        *bytes += character.len_utf8();
        (*bytes <= 128).then_some(character)
    })
    .collect()
}

fn refusal_fields(body: &[u8], request: &HttpRequest) -> [Option<String>; 3] {
    let parsed = serde_json::from_slice::<serde_json::Value>(body).ok();
    ["code", "message", "description"].map(|name| match parsed.as_ref()?.get(name)? {
        serde_json::Value::String(value) => Some(refusal_field(value, request)),
        serde_json::Value::Number(value) => Some(refusal_field(&value.to_string(), request)),
        _ => None,
    })
}

pub(crate) fn log_broker_refusal(request: &HttpRequest, response: &HttpResponse, attempt: u32) {
    if !matches!(
        request.destination(),
        Destination::TinkoffProd | Destination::TinkoffSandbox | Destination::FinamApi
    ) || (200..300).contains(&response.status)
    {
        return;
    }
    let body = refusal_fields(&response.body, request);
    let field = |index: usize| body[index].as_deref().unwrap_or("-");
    let header = |value: Option<&str>| value.map(|value| refusal_field(value, request));
    tracing::warn!(
        destination = ?request.destination(),
        method = request.path(),
        attempt,
        status = response.status,
        request_id = header(response.request_id.as_deref()).as_deref().unwrap_or("-"),
        content_type = header(response.content_type.as_deref()).as_deref().unwrap_or("-"),
        retry_after_ms = response.retry_after_header.map(millis),
        rate_limit = response.rate_limit,
        rate_remaining = response.rate_remaining,
        rate_reset = response.rate_reset,
        error_code = field(0),
        error_message = field(1),
        error_description = field(2),
        "broker response refused"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn request() -> HttpRequest {
        HttpRequest::get(Destination::TinkoffProd, "/UsersService/GetAccounts")
            .with_bearer("presented-secret")
            .with_cache_identity("identity-secret")
            .with_redaction_word("body-secret")
    }

    #[test]
    fn selected_fields_redact_all_credentials_before_utf8_safe_byte_bounds() {
        let request = request();
        let payload = serde_json::json!({
            "code": 80002,
            "message": format!("presented-secret identity-secret body-secret\n{}", "я".repeat(200)),
            "description": "permission denied",
            "account": "private-account",
            "token": "unknown-token"
        });
        let fields = refusal_fields(&serde_json::to_vec(&payload).unwrap(), &request);
        assert_eq!(fields[0].as_deref(), Some("80002"));
        assert_eq!(fields[2].as_deref(), Some("permission denied"));
        let message = fields[1].as_deref().unwrap();
        assert!(message.starts_with("<redacted> <redacted> <redacted> "));
        assert!(message.len() <= 128);
        assert!(!message.chars().any(char::is_control));
        assert!(!message.contains("secret"));
        assert!(
            !fields
                .iter()
                .flatten()
                .any(|field| field.contains("private-account") || field.contains("unknown-token"))
        );
        // A secret straddling the boundary is cut before truncation.
        let crossing = format!("{}presented-secret", "a".repeat(120));
        assert!(!refusal_field(&crossing, &request).contains("presented"));
    }

    #[test]
    fn malformed_non_json_and_nested_values_do_not_become_body_excerpts() {
        for payload in [
            b"<html>private-account</html>".as_slice(),
            b"{",
            &[0xff],
            br#"{"message":{"token":"private-account"},"code":true,"description":null}"#,
        ] {
            assert_eq!(refusal_fields(payload, &request()), [None, None, None]);
        }
        // Parse the full document before limiting fields, even with unrelated data first.
        let payload = format!(r#"{{"ignored":"{}","message":"denied"}}"#, "x".repeat(2000));
        assert_eq!(
            refusal_fields(payload.as_bytes(), &request())[1].as_deref(),
            Some("denied")
        );
    }

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn refusal_warning_has_safe_metadata_and_only_allowlisted_error_fields() {
        let capture = Capture(Arc::default());
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .without_time()
            .finish();
        let response = HttpResponse {
            status: 429,
            body: br#"{"code":80002,"message":"presented-secret denied","description":"limit","account":"private-account"}"#.to_vec(),
            request_id: Some("trace-body-secret".to_owned()),
            rate_limit: Some(200),
            rate_remaining: Some(0),
            rate_reset: Some(17),
            retry_after: Some(Duration::from_secs(99)),
            retry_after_header: Some(Duration::from_secs(12)),
            ..Default::default()
        };
        tracing::subscriber::with_default(subscriber, || {
            log_broker_refusal(&request(), &response, 2);
            log_broker_refusal(
                &request(),
                &HttpResponse {
                    status: 200,
                    ..response.clone()
                },
                3,
            );
            log_broker_refusal(
                &HttpRequest::get(Destination::MoexIss, "/test"),
                &response,
                1,
            );
        });
        let text = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
        for expected in [
            "WARN",
            "broker response refused",
            "status=429",
            "attempt=2",
            "rate_limit=200",
            "rate_remaining=0",
            "rate_reset=17",
            "retry_after_ms=12000",
            "80002",
            "<redacted> denied",
            "trace-<redacted>",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        for excluded in [
            "private-account",
            "presented-secret",
            "body-secret",
            "99000",
        ] {
            assert!(!text.contains(excluded), "{text}");
        }
    }
}
