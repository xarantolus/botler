//! Model call failures: what a failed turn's error means for retrying it.
//!
//! pi retries transient provider errors itself (a few attempts, seconds apart) and honours
//! `Retry-After` while doing so; the error the hub sees is what is left after that. pi reports it
//! only as text (`errorMessage` on the assistant message), usually starting with the HTTP status,
//! so classification is by status code and well-known phrases.

use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What kind of failure ended a turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    /// Connection problems, timeouts, rate limits, overload, server errors: retried later.
    Transient,
    /// The conversation no longer fits the model's context: compacted once, then retried once.
    ContextLength,
    /// The request itself is wrong (bad request, unknown model): retrying cannot help.
    Permanent,
    /// The provider rejected the key (401, 403).
    Auth,
    /// Out of credits (402).
    Billing,
}

impl ErrorClass {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Transient => "transient provider error",
            Self::ContextLength => "context length exceeded",
            Self::Permanent => "permanent model error",
            Self::Auth => "provider rejected the API key",
            Self::Billing => "provider account is out of credits",
        }
    }
}

/// Classify a failed model call by its error text.
pub fn classify(error: &str) -> ErrorClass {
    let lower = error.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));
    // Overflow messages vary by provider and may come with a 400 or 413.
    if has(&[
        "context length",
        "context_length",
        "context window",
        "maximum context",
        "too many tokens",
        "prompt is too long",
    ]) {
        return ErrorClass::ContextLength;
    }
    match status(&lower) {
        Some(401 | 403) => return ErrorClass::Auth,
        Some(402) => return ErrorClass::Billing,
        Some(408 | 409 | 425 | 429 | 500 | 502 | 503 | 504 | 529) => return ErrorClass::Transient,
        Some(413) => return ErrorClass::ContextLength,
        Some(400..=499) => return ErrorClass::Permanent,
        Some(500..=599) => return ErrorClass::Transient,
        _ => {}
    }
    if has(&["insufficient credits", "insufficient_quota", "payment required"]) {
        return ErrorClass::Billing;
    }
    if has(&["unauthorized", "invalid api key", "forbidden"]) {
        return ErrorClass::Auth;
    }
    if has(&["model not found", "invalid request", "not a valid model"]) {
        return ErrorClass::Permanent;
    }
    // Connection errors, timeouts, "overloaded", "terminated", and anything unrecognised: retrying
    // later costs little, and the attempts are bounded.
    ErrorClass::Transient
}

/// The HTTP status at the start of the message (`429 Too Many Requests`, `Error: 503 ...`), or
/// after `status`/`code` (`status code 502`).
fn status(lower: &str) -> Option<u16> {
    let numbers = lower.split(|c: char| !c.is_ascii_digit()).filter(|w| w.len() == 3);
    let leading = lower.trim_start().trim_start_matches("error").trim_start_matches(':').trim_start();
    if let Some(n) = leading.get(..3).and_then(|s| s.parse::<u16>().ok())
        && (100..600).contains(&n)
    {
        return Some(n);
    }
    if lower.contains("status") || lower.contains("code") || lower.contains("http") {
        return numbers.filter_map(|w| w.parse::<u16>().ok()).find(|n| (400..600).contains(n));
    }
    None
}

/// Delay before the hub's `attempt`-th retry of a turn that failed with a transient error
/// (1-based: after the first failure, attempt 1). `None` once the attempts are used up.
pub fn retry_delay(attempt: u32) -> Option<Duration> {
    const DELAYS: [u64; 4] = [30, 2 * 60, 10 * 60, 30 * 60];
    let i = usize::try_from(attempt).ok()?.checked_sub(1)?;
    DELAYS.get(i).map(|s| Duration::from_secs(*s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_status_and_phrase() {
        use ErrorClass::*;
        let cases = [
            ("Connection error.", Transient),
            ("Request timed out.", Transient),
            ("429 Too Many Requests", Transient),
            ("Error: 503 Service Unavailable", Transient),
            ("529 overloaded_error: Overloaded", Transient),
            ("The model is overloaded, try later", Transient),
            ("408 Request Timeout", Transient),
            ("409 conflict", Transient),
            ("425 Too Early", Transient),
            ("500 internal server error", Transient),
            ("upstream returned status code 502", Transient),
            ("504 Gateway Timeout", Transient),
            ("400 Invalid request: tools[0] is malformed", Permanent),
            ("404 No endpoints found for model foo/bar", Permanent),
            ("model not found", Permanent),
            ("400 This model's maximum context length is 128000 tokens", ContextLength),
            ("prompt is too long: 210000 tokens > 200000 maximum", ContextLength),
            ("413 Payload Too Large", ContextLength),
            ("401 Unauthorized", Auth),
            ("403 Forbidden: key disabled", Auth),
            ("402 Insufficient credits. Add more using https://openrouter.ai/settings/credits", Billing),
            ("Insufficient credits", Billing),
        ];
        for (msg, want) in cases {
            assert_eq!(classify(msg), want, "{msg}");
        }
    }

    #[test]
    fn backoff_is_bounded() {
        let delays: Vec<u64> = (1..=5).map_while(retry_delay).map(|d| d.as_secs()).collect();
        assert_eq!(delays, [30, 120, 600, 1800]);
        assert_eq!(retry_delay(0), None);
        assert_eq!(retry_delay(5), None);
    }
}
