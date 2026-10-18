//! Messages in the model's chat shape, and the rules a list of them must meet.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::Invalid;
use crate::is_false;

/// Who wrote a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

/// A tool call in an assistant message. `id` is what a `tool` message's `tool_call_id` names.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// A JSON object, in the member order the model wrote.
    pub arguments: Map<String, Value>,
}

/// A council v2 message. `snap` marks a boundary at its end.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub role: Role,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// An assistant message's thinking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// A `tool` message names the call whose result it gives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub snap: bool,
}

impl Message {
    fn text(role: Role, content: impl Into<String>) -> Self {
        Message { role, content: Some(content.into()), reasoning: None, tool_calls: Vec::new(), tool_call_id: None, snap: false }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self::text(Role::System, content)
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::text(Role::User, content)
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::text(Role::Assistant, content)
    }

    /// The result of the call `call_id`.
    pub fn tool(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Message { tool_call_id: Some(call_id.into()), ..Self::text(Role::Tool, content) }
    }

    /// The same message with a boundary at its end.
    pub fn snapped(self) -> Self {
        Message { snap: true, ..self }
    }
}

/// A call or tool-call id. Printable text of 1 to 256 bytes: the template matches ids and
/// never renders them.
pub fn is_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)
}

/// Checks the rules for messages, and that each tool message names a call of the assistant
/// message before its run of tool messages. `before` is the messages that precede `messages`,
/// such as a context's messages when checking a generation's reply. A system message after the
/// first is system text at that point in the context.
pub fn validate(before: &[Message], messages: &[Message]) -> Result<(), Invalid> {
    let mut calls: Vec<&str> =
        before.iter().rev().find(|m| m.role == Role::Assistant).map(|m| m.tool_calls.iter().map(|c| c.id.as_str()).collect()).unwrap_or_default();
    let mut prev_role = before.last().map(|m| m.role.as_str()).unwrap_or("");
    for (i, m) in messages.iter().enumerate() {
        let bad = |s: &str| Err(Invalid::new(format!("messages[{}]: {}", i, s)));
        if m.role != Role::Assistant && (m.reasoning.is_some() || !m.tool_calls.is_empty()) {
            return bad("only an assistant message carries reasoning or tool_calls");
        }
        for c in &m.tool_calls {
            if !is_id(&c.id) || c.name.is_empty() {
                return bad("a tool call needs a printable id and a name");
            }
        }
        match (m.role, &m.tool_call_id) {
            (Role::Tool, Some(id)) => {
                if !matches!(prev_role, "assistant" | "tool") || !calls.contains(&id.as_str()) {
                    return bad(&format!("tool_call_id {} names no call of the assistant message before it", id));
                }
            }
            (Role::Tool, None) => return bad("a tool message needs a tool_call_id"),
            (_, Some(_)) => return bad("only a tool message carries tool_call_id"),
            _ => {}
        }
        if m.role == Role::Assistant {
            calls = m.tool_calls.iter().map(|c| c.id.as_str()).collect();
        }
        prev_role = m.role.as_str();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(id: &str) -> ToolCall {
        ToolCall { id: id.into(), name: "shell".into(), arguments: Map::new() }
    }

    fn asks(calls: &[&str]) -> Message {
        Message { tool_calls: calls.iter().map(|c| call(c)).collect(), ..Message::assistant("") }
    }

    fn refused(before: &[Message], messages: &[Message]) -> String {
        validate(before, messages).expect_err("should be refused").message
    }

    #[test]
    fn a_tool_result_answers_a_call_of_the_message_before_it() {
        let ok = [Message::system("s"), Message::user("u"), asks(&["a", "b"]), Message::tool("a", "r"), Message::tool("b", "r")];
        validate(&[], &ok).unwrap();
        let wrong = [Message::user("u"), asks(&["a"]), Message::tool("z", "r")];
        assert_eq!(refused(&[], &wrong), "messages[2]: tool_call_id z names no call of the assistant message before it");
    }

    #[test]
    fn a_tool_result_after_a_user_message_answers_nothing() {
        let m = [asks(&["a"]), Message::user("u"), Message::tool("a", "r")];
        assert!(refused(&[], &m).starts_with("messages[2]: tool_call_id a names no call"));
    }

    #[test]
    fn a_tool_result_answers_the_latest_assistant_message_only() {
        let m = [asks(&["a"]), Message::tool("a", "r"), asks(&["b"]), Message::tool("a", "late")];
        assert!(refused(&[], &m).starts_with("messages[3]"));
    }

    #[test]
    fn appended_messages_answer_calls_in_the_context_they_follow() {
        let before = [Message::user("u"), asks(&["a"])];
        validate(&before, &[Message::tool("a", "r")]).unwrap();
        assert!(refused(&before, &[Message::tool("b", "r")]).starts_with("messages[0]"));
    }

    #[test]
    fn only_an_assistant_carries_reasoning_or_calls() {
        let r = Message { reasoning: Some("r".into()), ..Message::user("u") };
        assert_eq!(refused(&[], &[r]), "messages[0]: only an assistant message carries reasoning or tool_calls");
        let c = Message { tool_calls: vec![call("a")], ..Message::system("s") };
        assert!(refused(&[], &[c]).contains("only an assistant"));
    }

    #[test]
    fn tool_call_id_goes_on_tool_messages_only() {
        let m = Message { tool_call_id: Some("a".into()), ..Message::user("u") };
        assert_eq!(refused(&[], &[m]), "messages[0]: only a tool message carries tool_call_id");
        let t = Message { tool_call_id: None, ..Message::tool("a", "r") };
        assert_eq!(refused(&[], &[t]), "messages[0]: a tool message needs a tool_call_id");
    }

    #[test]
    fn a_call_needs_a_printable_id_and_a_name() {
        for id in ["", "a\nb", &"x".repeat(257)] {
            assert!(!is_id(id), "{id:?}");
            assert!(refused(&[], &[asks(&[id])]).contains("printable id"));
        }
        assert!(is_id(&"x".repeat(256)) && is_id("call_12_0") && is_id("héllo"));
        let nameless = Message { tool_calls: vec![ToolCall { name: String::new(), ..call("a") }], ..Message::assistant("") };
        assert!(refused(&[], &[nameless]).contains("a name"));
    }

    #[test]
    fn a_later_system_message_is_accepted_with_no_reasoning_or_calls() {
        validate(&[], &[Message::system("s"), Message::user("u"), Message::assistant("a"), Message::system("nudge")]).unwrap();
    }

    #[test]
    fn the_error_names_the_message_index() {
        let m = [Message::user("u"), Message::user("v"), Message::tool("a", "r")];
        assert!(refused(&[], &m).starts_with("messages[2]:"));
    }

    #[test]
    fn messages_decode_strictly_and_encode_without_absent_members() {
        let m: Message = serde_json::from_value(json!({"role": "user", "content": "hi", "snap": true})).unwrap();
        assert_eq!(serde_json::to_value(&m).unwrap(), json!({"role": "user", "content": "hi", "snap": true}));
        assert_eq!(serde_json::to_value(Message::user("x")).unwrap(), json!({"role": "user", "content": "x"}));
        assert!(serde_json::from_value::<Message>(json!({"role": "user", "extra": 1})).is_err());
        assert!(serde_json::from_value::<Message>(json!({"role": "wizard", "content": "x"})).is_err());
        assert!(serde_json::from_value::<ToolCall>(json!({"id": "a", "name": "n", "arguments": "x"})).is_err());
    }

    #[test]
    fn arguments_keep_the_member_order_the_model_wrote() {
        let m: Message = serde_json::from_str(r#"{"role":"assistant","tool_calls":[{"id":"a","name":"n","arguments":{"z":1,"a":2}}]}"#).unwrap();
        assert_eq!(serde_json::to_string(&m.tool_calls[0].arguments).unwrap(), r#"{"z":1,"a":2}"#);
    }
}
