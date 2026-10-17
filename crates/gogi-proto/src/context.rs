//! A context as a request carries it, and the reply to building one.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Invalid;
use crate::ids::SnapshotId;
use crate::message::{self, Message};
use crate::required_nullable;

/// A chat sent whole with every request that reads it: messages, tools, whether the model
/// thinks, and the tool a decision delivers its case through.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    /// Absent: the server's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
    pub messages: Vec<Message>,
}

impl Context {
    /// Checks the messages and that `queue` names a tool.
    pub fn check(&self) -> Result<(), Invalid> {
        if self.messages.is_empty() {
            return Err(Invalid::new("a context has at least one message"));
        }
        message::validate(&[], &self.messages)?;
        if let Some(q) = &self.queue {
            if !self.tool_names().contains(&q.as_str()) {
                return Err(Invalid::new(format!("queue {} names no tool in tools", q)));
            }
        }
        Ok(())
    }

    /// Whether the model thinks: on unless the context says otherwise.
    pub fn thinks(&self) -> bool {
        self.thinking.unwrap_or(true)
    }

    /// The `function.name` of each tool that has one.
    pub fn tool_names(&self) -> Vec<&str> {
        self.tools.iter().filter_map(|t| t["function"]["name"].as_str()).collect()
    }
}

/// The body of `POST /council/v2/contexts`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextBuild {
    pub context: Context,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
}

/// Where a snapshot's state lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Device,
    Host,
    Disk,
    /// A tier this crate does not know.
    #[serde(other)]
    Unknown,
}

/// One held snapshot of a context: the boundary at the end of message `message`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotInfo {
    pub id: SnapshotId,
    /// The index of the message whose end it holds, the system message counted.
    pub message: u64,
    pub tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<Tier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

/// The reply to `POST /council/v2/contexts`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBuilt {
    /// The snapshot at the context's end.
    pub head: SnapshotId,
    pub tokens: u64,
    /// Tokens not fed.
    pub kept: u64,
    /// Tokens fed now, or that would be under `dry_run`.
    pub fed: u64,
    /// The snapshot the build started from, or `null`.
    #[serde(deserialize_with = "required_nullable")]
    pub from: Option<SnapshotId>,
    pub dry_run: bool,
    pub snapshots: Vec<SnapshotInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ToolCall;
    use serde_json::json;

    fn tool(name: &str) -> Value {
        json!({"type": "function", "function": {"name": name, "parameters": {}}})
    }

    fn snap(p: &str) -> String {
        format!("snap:{p}{}", "0".repeat(64 - p.len()))
    }

    #[test]
    fn a_context_needs_a_message() {
        assert_eq!(Context::default().check().unwrap_err().message, "a context has at least one message");
    }

    #[test]
    fn queue_names_a_tool() {
        let mut c = Context { tools: vec![tool("next")], queue: Some("next".into()), messages: vec![Message::user("u")], ..Default::default() };
        c.check().unwrap();
        c.queue = Some("missing".into());
        assert_eq!(c.check().unwrap_err().message, "queue missing names no tool in tools");
        c.tools.clear();
        assert!(c.check().is_err());
    }

    #[test]
    fn the_messages_are_checked_from_the_start() {
        let c = Context { messages: vec![Message::tool("a", "r")], ..Default::default() };
        assert!(c.check().unwrap_err().message.starts_with("messages[0]"));
        let c = Context {
            messages: vec![Message { tool_calls: vec![ToolCall { id: "a".into(), name: "n".into(), arguments: Default::default() }], ..Message::assistant("") }, Message::tool("a", "r")],
            ..Default::default()
        };
        c.check().unwrap();
    }

    #[test]
    fn thinking_is_on_unless_the_context_says_otherwise() {
        assert!(Context::default().thinks());
        assert!(!Context { thinking: Some(false), ..Default::default() }.thinks());
    }

    #[test]
    fn a_context_with_a_stray_member_is_refused() {
        assert!(serde_json::from_value::<Context>(json!({"messages": [], "system": "x"})).is_err());
        assert!(serde_json::from_value::<ContextBuild>(json!({"context": {"messages": []}, "persist": true})).is_err());
    }

    #[test]
    fn a_build_reply_round_trips_and_needs_from() {
        let v = json!({"head": snap("51d0"), "tokens": 1840, "kept": 0, "fed": 1840, "from": null, "dry_run": false,
            "snapshots": [{"id": snap("9a1e"), "message": 1, "tokens": 412, "tier": "host"}, {"id": snap("51d0"), "message": 4, "tokens": 1840, "tier": "host"}]});
        let r: ContextBuilt = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(serde_json::to_value(&r).unwrap(), v);
        let mut no_from = v.clone();
        no_from.as_object_mut().unwrap().remove("from");
        assert!(serde_json::from_value::<ContextBuilt>(no_from).is_err());
        let mut newer = v;
        newer["snapshots"][0]["tier"] = json!("nvram");
        let r: ContextBuilt = serde_json::from_value(newer).unwrap();
        assert_eq!(r.snapshots[0].tier, Some(Tier::Unknown));
    }
}
