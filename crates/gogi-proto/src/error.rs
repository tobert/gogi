//! The error object and the reason a request is refused.

use serde::{Deserialize, Serialize};

use crate::identity::{ModelStatus, ServedModel};

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

/// The member a body's decode error names, for `error.param`: from serde's "missing field `x`"
/// and "unknown field `x`", and from "invalid field `x`", which this crate's own decoders write,
/// such as for a `continue` that is not a boolean.
pub fn decode_param(text: &str) -> Option<String> {
    ["missing field `", "unknown field `", "invalid field `"].iter().find_map(|p| text.split_once(p).and_then(|(_, rest)| rest.split_once('`')).map(|(f, _)| f.to_string()))
}

/// The `type` of an error body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorType {
    /// `400`.
    InvalidRequest,
    /// `404`.
    NotFound,
    /// `413`: past a limit `param` names.
    TooLarge,
    /// `429`.
    Busy,
    /// `503`: the server is stopping or stopped.
    Unavailable,
    /// `504`.
    Timeout,
    /// `500`.
    Internal,
    /// A generation's prompt leaves no room for a reply.
    ContextFull,
    /// The served model a request names is not `ready`. A server responds with a `503`.
    ModelUnavailable,
}

/// The inside of an error body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub r#type: ErrorType,
    pub message: String,
    /// The request field or limit at fault.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    /// For `context_full`: the prompt's tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    /// For `context_full`: `limits.context_tokens`, the most tokens a prompt may have.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    /// For `model_unavailable`: the served model's `model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// For `model_unavailable`: the served model's status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ModelStatus>,
}

/// An error body: `{"error": {...}, "request_id": ...}`. A server sends `request_id`, the same id
/// as its `request-id` header, in every error body and `error` event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

impl ErrorBody {
    pub fn new(r#type: ErrorType, message: impl Into<String>) -> Self {
        ErrorBody {
            error: ErrorDetail { r#type, message: message.into(), param: None, prompt_tokens: None, context_tokens: None, model: None, status: None },
            request_id: None,
        }
    }

    /// The `503` body for a request whose served model is not `ready`.
    pub fn unavailable(m: &ServedModel) -> Self {
        let mut e = ErrorBody::new(ErrorType::ModelUnavailable, format!("nothing ran: model {} is {}, not ready; retry when identity reports it ready", m.model, status_word(m.status)));
        e.error.model = Some(m.model.clone());
        e.error.status = Some(m.status);
        e
    }

    pub fn with_param(mut self, param: impl Into<String>) -> Self {
        self.error.param = Some(param.into());
        self
    }

    pub fn with_request_id(mut self, id: impl Into<String>) -> Self {
        self.request_id = Some(id.into());
        self
    }
}

fn status_word(s: ModelStatus) -> &'static str {
    match s {
        ModelStatus::Ready => "ready",
        ModelStatus::Loading => "loading",
        ModelStatus::Draining => "draining",
        ModelStatus::Unloaded => "unloaded",
        ModelStatus::Unknown => "in an unknown status",
    }
}

impl From<Invalid> for ErrorBody {
    fn from(e: Invalid) -> Self {
        ErrorBody { error: ErrorDetail { param: e.param, ..ErrorBody::new(ErrorType::InvalidRequest, e.message).error }, request_id: None }
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
    fn model_unavailable_names_the_model_and_its_status() {
        let v = json!({"error": {"type": "model_unavailable", "message": "m", "model": "qwen-3.8-mutant-636", "status": "loading"}});
        let e: ErrorBody = serde_json::from_value(v.clone()).unwrap();
        assert_eq!((e.error.model.as_deref(), e.error.status), (Some("qwen-3.8-mutant-636"), Some(ModelStatus::Loading)));
        assert_eq!(serde_json::to_value(&e).unwrap(), v);
    }

    #[test]
    fn a_decode_error_names_the_member_at_fault() {
        for (text, want) in [
            ("missing field `model` at line 1 column 2", Some("model")),
            ("unknown field `persist`, expected one of `model`, `context` at line 1 column 9", Some("persist")),
            ("invalid field `continue`: continue is true or false, not \"yes\"", Some("continue")),
            ("expected value at line 1 column 1", None),
        ] {
            assert_eq!(decode_param(text).as_deref(), want, "{text}");
        }
        let e = serde_json::from_value::<crate::Message>(json!({"role": "assistant", "content": "a", "continue": "yes"})).unwrap_err();
        assert_eq!(decode_param(&e.to_string()).as_deref(), Some("continue"));
    }

    #[test]
    fn an_invalid_request_becomes_a_400_body_with_its_param() {
        let e: ErrorBody = Invalid::param("ttl_s", "ttl_s is 1 to 86400").into();
        assert_eq!(serde_json::to_value(&e).unwrap(), json!({"error": {"type": "invalid_request", "message": "ttl_s is 1 to 86400", "param": "ttl_s"}}));
    }
}
