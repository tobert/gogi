//! The error object and the reason a request is refused.

use serde::{Deserialize, Serialize};

use crate::ids::SnapshotId;

/// Why a request breaks the contract. A server responds with a `400`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct Invalid {
    /// The request field at fault, when one is.
    pub param: Option<String>,
    pub message: String,
}

impl Invalid {
    pub fn new(message: impl Into<String>) -> Self {
        Invalid { param: None, message: message.into() }
    }

    pub fn param(param: impl Into<String>, message: impl Into<String>) -> Self {
        Invalid { param: Some(param.into()), message: message.into() }
    }
}

/// The `type` of an error body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorType {
    InvalidRequest,
    NotFound,
    SnapshotGone,
    HeadMismatch,
    TooLarge,
    Busy,
    Unavailable,
    Timeout,
    PinBudget,
    Internal,
    /// A generation's prompt leaves no room for a reply.
    ContextFull,
}

/// The inside of an error body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub r#type: ErrorType,
    pub message: String,
    /// The request field or limit at fault.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    /// For `snapshot_gone` and `head_mismatch`, v1's errors: the context record's current head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<SnapshotId>,
    /// For `context_full`: the prompt's tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    /// For `context_full`: `limits.context_tokens`, the most tokens a prompt may have.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
}

/// An error body: `{"error": {...}}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

impl ErrorBody {
    pub fn new(r#type: ErrorType, message: impl Into<String>) -> Self {
        ErrorBody {
            error: ErrorDetail { r#type, message: message.into(), param: None, head: None, prompt_tokens: None, context_tokens: None },
        }
    }

    pub fn with_param(mut self, param: impl Into<String>) -> Self {
        self.error.param = Some(param.into());
        self
    }
}

impl From<Invalid> for ErrorBody {
    fn from(e: Invalid) -> Self {
        ErrorBody { error: ErrorDetail { param: e.param, ..ErrorBody::new(ErrorType::InvalidRequest, e.message).error } }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn context_full_carries_both_counts() {
        let v = json!({"error": {"type": "context_full", "message": "m", "prompt_tokens": 70000, "context_tokens": 65535}});
        let e: ErrorBody = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(e.error.r#type, ErrorType::ContextFull);
        assert_eq!((e.error.prompt_tokens, e.error.context_tokens), (Some(70000), Some(65535)));
        assert_eq!(serde_json::to_value(&e).unwrap(), v);
    }

    #[test]
    fn an_invalid_request_becomes_a_400_body_with_its_param() {
        let e: ErrorBody = Invalid::param("ttl_s", "ttl_s is 1 to 86400").into();
        assert_eq!(serde_json::to_value(&e).unwrap(), json!({"error": {"type": "invalid_request", "message": "ttl_s is 1 to 86400", "param": "ttl_s"}}));
    }
}
