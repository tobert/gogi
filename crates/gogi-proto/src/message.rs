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
    pub input: Map<String, Value>,
}

/// A message of a context. `snap` marks a boundary at its end.
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
    /// A `tool` message's result is the call's failure. The template renders it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub snap: bool,
    /// `continue: true` opens the last message: an assistant message whose text the model
    /// continues in a generation. `continue` is a Rust keyword, so the field is `open`.
    #[serde(rename = "continue", default, skip_serializing_if = "is_false", deserialize_with = "continue_flag")]
    pub open: bool,
}

/// Decodes `continue`, which is `true` or `false`. Any other value is an error that names
/// `continue`.
fn continue_flag<'de, D: serde::Deserializer<'de>>(de: D) -> Result<bool, D::Error> {
    match Value::deserialize(de)? {
        Value::Bool(b) => Ok(b),
        other => Err(serde::de::Error::custom(format!("invalid field `continue`: continue is true or false, not {}; true opens the last message", other))),
    }
}

impl Message {
    fn text(role: Role, content: impl Into<String>) -> Self {
        Message { role, content: Some(content.into()), reasoning: None, tool_calls: Vec::new(), tool_call_id: None, is_error: false, snap: false, open: false }
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

    /// The failed result of the call `call_id`.
    pub fn tool_error(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Message { is_error: true, ..Self::tool(call_id, content) }
    }

    /// The same message with a boundary at its end.
    pub fn snapped(self) -> Self {
        Message { snap: true, ..self }
    }

    /// The same message, open: `continue: true`.
    pub fn opened(self) -> Self {
        Message { open: true, ..self }
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
/// first is system text at that point in the context. Only the last of `messages` may be open
/// (`continue: true`); a broken rule of an open message is an error naming `continue`.
pub fn validate(before: &[Message], messages: &[Message]) -> Result<(), Invalid> {
    check_opens(messages)?;
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
        if m.is_error && m.role != Role::Tool {
            return bad("only a tool message carries is_error");
        }
        if m.role == Role::Assistant {
            calls = m.tool_calls.iter().map(|c| c.id.as_str()).collect();
        }
        prev_role = m.role.as_str();
    }
    Ok(())
}

/// Checks each open message before any other rule, so a broken rule of an open message is an
/// error naming `continue` even when another rule also fails.
pub fn check_opens(messages: &[Message]) -> Result<(), Invalid> {
    for (i, m) in messages.iter().enumerate().filter(|(_, m)| m.open) {
        check_open(m, i + 1 == messages.len()).map_err(|s| Invalid::param("continue", format!("messages[{}]: {}", i, s)))?;
    }
    Ok(())
}

/// Checks an open message: the last message, an assistant message, no `tool_calls`, and a
/// `content` member, even an empty one, or `reasoning` that is not empty.
fn check_open(m: &Message, last: bool) -> Result<(), &'static str> {
    if !last {
        return Err("only the last message may carry continue: true");
    }
    if m.role != Role::Assistant {
        return Err("only an assistant message carries continue: true");
    }
    if !m.tool_calls.is_empty() {
        return Err("an open message carries no tool_calls; send the reply back closed, or drop the call");
    }
    if m.content.is_none() && m.reasoning.as_deref().unwrap_or("").is_empty() {
        return Err("an open message needs a content member, even an empty one, or reasoning that is not empty");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(id: &str) -> ToolCall {
        ToolCall { id: id.into(), name: "shell".into(), input: Map::new() }
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
    fn is_error_goes_on_tool_messages_only() {
        validate(&[], &[asks(&["a"]), Message::tool_error("a", "exit 1")]).unwrap();
        let m = Message { is_error: true, ..Message::user("u") };
        assert_eq!(refused(&[], &[m]), "messages[0]: only a tool message carries is_error");
        let v = serde_json::to_value(Message::tool_error("a", "exit 1")).unwrap();
        assert_eq!(v, json!({"role": "tool", "content": "exit 1", "tool_call_id": "a", "is_error": true}));
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
        assert!(serde_json::from_value::<ToolCall>(json!({"id": "a", "name": "n", "input": "x"})).is_err());
        assert!(serde_json::from_value::<ToolCall>(json!({"id": "a", "name": "n", "arguments": {}})).is_err());
    }

    fn refused_open(messages: &[Message]) -> String {
        let e = validate(&[], messages).expect_err("should be refused");
        assert_eq!(e.param.as_deref(), Some("continue"), "{}", e.message);
        e.message
    }

    #[test]
    fn continue_opens_only_a_last_assistant_message_with_text_and_no_calls() {
        validate(&[], &[Message::user("u"), Message::assistant("The plan:").opened()]).unwrap();
        let thinking = Message { reasoning: Some("I should".into()), content: None, ..Message::assistant("").opened() };
        validate(&[], &[Message::user("u"), thinking]).unwrap();
        let closed_thinking = Message { reasoning: Some("I should".into()), ..Message::assistant("").opened() };
        validate(&[], &[Message::user("u"), closed_thinking]).unwrap();
        assert_eq!(refused_open(&[Message::user("u"), Message::assistant("a").opened(), Message::user("v")]), "messages[1]: only the last message may carry continue: true");
        assert_eq!(refused_open(&[Message::user("u").opened()]), "messages[0]: only an assistant message carries continue: true");
        assert_eq!(refused_open(&[Message::user("u"), asks(&["a"]).opened()]), "messages[1]: an open message carries no tool_calls; send the reply back closed, or drop the call");
        let empty = "messages[1]: an open message needs a content member, even an empty one, or reasoning that is not empty";
        assert_eq!(refused_open(&[Message::user("u"), Message { content: None, reasoning: Some(String::new()), ..Message::assistant("").opened() }]), empty);
        assert_eq!(refused_open(&[Message::user("u"), Message { content: None, ..Message::assistant("").opened() }]), empty);
    }

    #[test]
    fn an_empty_content_member_opens_the_content_after_an_empty_thinking() {
        validate(&[], &[Message::user("u"), Message::assistant("").opened()]).unwrap();
        validate(&[], &[Message::user("u"), Message { reasoning: Some(String::new()), ..Message::assistant("").opened() }]).unwrap();
    }

    #[test]
    fn an_open_messages_errors_name_continue_even_when_another_rule_fails() {
        let open_user = Message { reasoning: Some("r".into()), ..Message::user("u").opened() };
        assert_eq!(refused_open(&[open_user]), "messages[0]: only an assistant message carries continue: true");
        let stray = Message::tool("nope", "r");
        assert_eq!(refused_open(&[stray.clone(), Message::user("u").opened()]), "messages[1]: only an assistant message carries continue: true");
        assert_eq!(refused_open(&[Message::assistant("a").opened(), stray]), "messages[0]: only the last message may carry continue: true");
    }

    #[test]
    fn continue_false_is_allowed_on_any_message_and_changes_nothing() {
        let v = json!([{"role": "user", "content": "u", "continue": false}, {"role": "assistant", "content": "a", "continue": false}, {"role": "user", "content": "v"}]);
        let m: Vec<Message> = serde_json::from_value(v).unwrap();
        validate(&[], &m).unwrap();
        assert_eq!(m, [Message::user("u"), Message::assistant("a"), Message::user("v")]);
        assert_eq!(serde_json::to_value(&m[1]).unwrap(), json!({"role": "assistant", "content": "a"}));
    }

    #[test]
    fn continue_is_a_boolean_and_any_other_value_names_continue() {
        let m: Message = serde_json::from_value(json!({"role": "assistant", "content": "a", "continue": true})).unwrap();
        assert!(m.open);
        assert_eq!(serde_json::to_value(&m).unwrap(), json!({"role": "assistant", "content": "a", "continue": true}));
        for bad in [json!("yes"), json!(1), json!(null), json!({"suffix": "x"})] {
            let e = serde_json::from_value::<Message>(json!({"role": "assistant", "content": "a", "continue": bad})).unwrap_err().to_string();
            assert!(e.starts_with("invalid field `continue`"), "{e}");
        }
    }

    #[test]
    fn input_keeps_the_member_order_the_model_wrote() {
        let m: Message = serde_json::from_str(r#"{"role":"assistant","tool_calls":[{"id":"a","name":"n","input":{"z":1,"a":2}}]}"#).unwrap();
        assert_eq!(serde_json::to_string(&m.tool_calls[0].input).unwrap(), r#"{"z":1,"a":2}"#);
    }
}
