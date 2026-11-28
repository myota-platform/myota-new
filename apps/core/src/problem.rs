//! RFC 7807 problem responses.
//!
//! One error type, one rendering. Every failure a caller can see is
//! `application/problem+json` with a stable `type`, because a client that has to
//! parse five error shapes writes five parsers and tests none of them.
//!
//! The `type` values are the shared contract's, not invented here: the contract
//! lists them under `Problem`, and `tools/check_contracts` composes the documents
//! that the playground shows. A type the contract does not declare would be an
//! undocumented error shape.

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// The problem classes this service can return.
///
/// The variants are the decisions, not the status codes: `MfaNotEnrolled` and
/// `Forbidden` are both 403 and a client must be able to tell them apart, because
/// one means "enrol a second factor" and the other means "ask an administrator for
/// a role".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProblemType {
    /// The request body or a parameter was not acceptable.
    ValidationFailed,
    /// No credential, an invalid token, or a session that is gone.
    Unauthorized,
    /// Authenticated, but not an administrator.
    Forbidden,
    /// The password was right and there is no confirmed second factor.
    MfaNotEnrolled,
    /// The second factor was wrong or already used.
    MfaFailed,
    /// A valid credential belonging to the other audience.
    WrongAudience,
    /// The actor does not hold a scope the request would confer on someone else.
    ScopeNotHeld,
    /// Nothing at that address.
    NotFound,
    /// The operation conflicts with the current state.
    Conflict,
    /// A challenge or enrolment that has expired.
    Gone,
    /// Too many attempts.
    RateLimited,
    /// This service failed. Should be rare enough to be a bug report.
    Internal,
}

impl ProblemType {
    /// The `type` URI. These strings are asserted against the contract by the
    /// contract checks, so changing one is a contract change, not a refactor.
    #[must_use]
    pub fn uri(self) -> &'static str {
        match self {
            Self::ValidationFailed => "about:blank#validation-failed",
            Self::Unauthorized => "about:blank#unauthorized",
            Self::Forbidden => "about:blank#forbidden",
            Self::MfaNotEnrolled => "about:blank#mfa-not-enrolled",
            Self::MfaFailed => "about:blank#mfa-failed",
            Self::WrongAudience => "about:blank#wrong-audience",
            Self::ScopeNotHeld => "about:blank#scope-not-held",
            Self::NotFound => "about:blank#not-found",
            Self::Conflict => "about:blank#conflict",
            // The contract's 410 example carries `about:blank#not-found`. Repeated
            // here rather than invented, so a client that switches on `type` does
            // not meet a value the document never mentions.
            Self::Gone => "about:blank#not-found",
            Self::RateLimited => "about:blank#rate-limited",
            Self::Internal => "about:blank#internal",
        }
    }

    /// The status code.
    #[must_use]
    pub fn status(self) -> StatusCode {
        match self {
            Self::ValidationFailed => StatusCode::BAD_REQUEST,
            Self::Unauthorized | Self::MfaFailed | Self::WrongAudience => StatusCode::UNAUTHORIZED,
            Self::Forbidden | Self::MfaNotEnrolled | Self::ScopeNotHeld => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Conflict => StatusCode::CONFLICT,
            Self::Gone => StatusCode::GONE,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// The stable title for the class.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::ValidationFailed => "Request validation failed",
            Self::Unauthorized => "Not authenticated",
            Self::Forbidden => "Forbidden",
            Self::MfaNotEnrolled => "Second factor not enrolled",
            Self::MfaFailed => "Second factor rejected",
            Self::WrongAudience => "Wrong audience for this route",
            Self::ScopeNotHeld => "Scope not held",
            Self::NotFound => "Not found",
            Self::Conflict => "Conflict",
            Self::Gone => "Challenge expired",
            Self::RateLimited => "Too many requests",
            Self::Internal => "Internal error",
        }
    }
}

/// A problem response.
#[derive(Debug, Serialize)]
pub struct Problem {
    /// The class URI.
    #[serde(rename = "type")]
    pub problem_type: &'static str,
    /// The stable title.
    pub title: &'static str,
    /// The status code.
    pub status: u16,
    /// What went wrong, in a sentence a support person can read.
    pub detail: String,
    /// The request id, so a log line and a report can be joined.
    #[serde(rename = "requestId", skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Seconds until the caller may retry, on a 429.
    #[serde(rename = "retryAfterSeconds", skip_serializing_if = "Option::is_none")]
    pub retry_after_seconds: Option<i64>,
}

impl Problem {
    /// Builds a problem of `kind` with a detail message.
    #[must_use]
    pub fn new(kind: ProblemType, detail: impl Into<String>) -> Self {
        Self {
            problem_type: kind.uri(),
            title: kind.title(),
            status: kind.status().as_u16(),
            detail: detail.into(),
            request_id: None,
            retry_after_seconds: None,
        }
    }

    /// Attaches a request id.
    #[must_use]
    pub fn with_request_id(mut self, request_id: Option<String>) -> Self {
        self.request_id = request_id;
        self
    }

    /// Attaches a retry hint. The header and the body say the same thing, because
    /// a browser reads one and a client library reads the other.
    #[must_use]
    pub fn with_retry_after(mut self, seconds: i64) -> Self {
        self.retry_after_seconds = Some(seconds);
        self
    }

    /// The status code, for callers that need to branch on it.
    #[must_use]
    pub fn status_code(&self) -> StatusCode {
        StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let retry_after = self.retry_after_seconds;

        match serde_json::to_vec(&self) {
            Ok(body) => {
                let mut response = (status, body).into_response();
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/problem+json"),
                );
                if let Some(seconds) = retry_after {
                    if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
                        response.headers_mut().insert(header::RETRY_AFTER, value);
                    }
                }
                response
            }
            Err(error) => {
                // Serialising a struct of strings and integers cannot fail in
                // practice, but a panic in an error path would turn a 400 into a
                // dropped connection. Fall back to the status alone.
                tracing::error!(%error, "rendering a problem response failed");
                status.into_response()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_class_has_a_status_matching_its_purpose() {
        assert_eq!(
            ProblemType::ValidationFailed.status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(ProblemType::Unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(ProblemType::Forbidden.status(), StatusCode::FORBIDDEN);
        assert_eq!(ProblemType::MfaNotEnrolled.status(), StatusCode::FORBIDDEN);
        assert_eq!(ProblemType::MfaFailed.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            ProblemType::WrongAudience.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(ProblemType::Conflict.status(), StatusCode::CONFLICT);
        assert_eq!(ProblemType::Gone.status(), StatusCode::GONE);
        assert_eq!(
            ProblemType::RateLimited.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            ProblemType::Internal.status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn the_rendered_body_carries_the_type_and_status() {
        let problem = Problem::new(ProblemType::MfaNotEnrolled, "Enrol a second factor first.");
        let rendered = serde_json::to_value(&problem).expect("serialisable");
        assert_eq!(rendered["type"], "about:blank#mfa-not-enrolled");
        assert_eq!(rendered["status"], 403);
        assert_eq!(rendered["title"], "Second factor not enrolled");
        assert_eq!(rendered["detail"], "Enrol a second factor first.");
    }

    #[test]
    fn absent_optional_fields_are_omitted_rather_than_null() {
        let problem = Problem::new(ProblemType::NotFound, "no");
        let rendered = serde_json::to_value(&problem).expect("serialisable");
        assert!(rendered.get("requestId").is_none());
        assert!(rendered.get("retryAfterSeconds").is_none());
    }

    #[test]
    fn a_rate_limit_carries_the_retry_hint_in_body_and_header() {
        use axum::response::IntoResponse;

        let problem = Problem::new(ProblemType::RateLimited, "slow down").with_retry_after(900);
        let response = problem.into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some("900")
        );
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/problem+json")
        );
    }
}
