//! A context as a request carries it, and the response to building one.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::Invalid;
use crate::identity::Class;
use crate::ids::SnapshotId;
use crate::message::{self, Message};
use crate::required_nullable;

/// A tool the model may call: its name, what it does, and the JSON Schema of its input.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// A JSON Schema object, in the member order the client wrote.
    pub input_schema: Map<String, Value>,
}

impl Tool {
    pub fn new(name: impl Into<String>, input_schema: Value) -> Self {
        let input_schema = match input_schema {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        Tool { name: name.into(), description: None, input_schema }
    }
}

/// Token counts, in the same shape for every request. `kept` came from held snapshots and `fed`
/// is what this request ran; the two make `prompt`. `completion` is what the server wrote, and
/// `reasoning` is the part of `completion` the model wrote as thinking.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt: u64,
    pub kept: u64,
    pub fed: u64,
    pub completion: u64,
    pub reasoning: u64,
}

impl Usage {
    /// Checks `kept + fed = prompt` and `reasoning <= completion`.
    pub fn check(&self) -> Result<(), String> {
        if self.kept + self.fed != self.prompt {
            return Err(format!("usage: kept {} + fed {} is not prompt {}", self.kept, self.fed, self.prompt));
        }
        if self.reasoning > self.completion {
            return Err(format!("usage: reasoning {} is more than completion {}", self.reasoning, self.completion));
        }
        Ok(())
    }
}

/// A chat sent whole with every request that reads it: messages, tools, whether the model
/// thinks, and the tool a decision delivers its case through.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    /// Absent: the server's default. mkviolet's is `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
    pub messages: Vec<Message>,
}

impl Context {
    /// Checks the messages, that each tool has a distinct name, and that `queue` names a tool.
    pub fn check(&self) -> Result<(), Invalid> {
        if self.messages.is_empty() {
            return Err(Invalid::new("a context has at least one message"));
        }
        message::validate(&[], &self.messages)?;
        let names = self.tool_names();
        for (i, n) in names.iter().enumerate() {
            if n.is_empty() || names[..i].contains(n) {
                return Err(Invalid::new(format!("tools[{}]: a tool needs a name no other tool has", i)));
            }
        }
        if let Some(q) = &self.queue
            && !self.tool_names().contains(&q.as_str())
        {
            return Err(Invalid::new(format!("queue {} names no tool in tools", q)));
        }
        Ok(())
    }

    /// Whether the model thinks, taking an absent `thinking` as `true`, mkviolet's default.
    pub fn thinks(&self) -> bool {
        self.thinking.unwrap_or(true)
    }

    /// The name of each tool, in order.
    pub fn tool_names(&self) -> Vec<&str> {
        self.tools.iter().map(|t| t.name.as_str()).collect()
    }

    /// The tool named `name`.
    pub fn tool(&self, name: &str) -> Option<&Tool> {
        self.tools.iter().find(|t| t.name == name)
    }
}

/// The body of `POST /council/v2/contexts`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextBuild {
    /// The served model: its `model` or an alias.
    pub model: String,
    pub context: Context,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<Class>,
    /// Higher runs first within its class. Absent: 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
}

/// Where a snapshot's model state is stored.
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
    /// The index of the message the boundary ends, counting from the first message as 0.
    pub message: u64,
    pub tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<Tier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

/// The response to `POST /council/v2/contexts`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBuilt {
    /// The served model's `model`, never an alias.
    pub model: String,
    /// The snapshot at the context's end.
    pub head: SnapshotId,
    /// `prompt` is the context's tokens; `fed` is what this request feeds, or would feed under
    /// `dry_run`.
    pub usage: Usage,
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

    fn tool(name: &str) -> Tool {
        Tool::new(name, json!({"type": "object"}))
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
    fn tool_names_are_distinct_and_not_empty() {
        let mut c = Context { tools: vec![tool("a"), tool("b")], messages: vec![Message::user("u")], ..Default::default() };
        c.check().unwrap();
        c.tools.push(tool("a"));
        assert_eq!(c.check().unwrap_err().message, "tools[2]: a tool needs a name no other tool has");
        c.tools = vec![tool("")];
        assert!(c.check().is_err());
    }

    #[test]
    fn a_tool_is_a_name_a_description_and_an_input_schema() {
        let v = json!({"name": "next_command", "description": "The next command.", "input_schema": {"type": "object", "properties": {}}});
        let t: Tool = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(serde_json::to_value(&t).unwrap(), v);
        let openai = json!({"type": "function", "function": {"name": "n", "parameters": {}}});
        assert!(serde_json::from_value::<Tool>(openai).is_err());
        assert!(serde_json::from_value::<Tool>(json!({"name": "n", "input_schema": []})).is_err());
    }

    #[test]
    fn usage_relations() {
        Usage { prompt: 10, kept: 4, fed: 6, completion: 3, reasoning: 2 }.check().unwrap();
        assert!(Usage { prompt: 10, kept: 4, fed: 5, ..Default::default() }.check().is_err());
        assert!(Usage { completion: 1, reasoning: 2, ..Default::default() }.check().is_err());
        assert!(serde_json::from_value::<Usage>(json!({"prompt": 1, "kept": 0, "fed": 1, "completion": 0})).is_err());
    }

    #[test]
    fn the_messages_are_checked_from_the_start() {
        let c = Context { messages: vec![Message::tool("a", "r")], ..Default::default() };
        assert!(c.check().unwrap_err().message.starts_with("messages[0]"));
        let c = Context {
            messages: vec![Message { tool_calls: vec![ToolCall { id: "a".into(), name: "n".into(), input: Default::default() }], ..Message::assistant("") }, Message::tool("a", "r")],
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
        assert!(serde_json::from_value::<ContextBuild>(json!({"model": "m", "context": {"messages": []}, "persist": true})).is_err());
        assert!(serde_json::from_value::<ContextBuild>(json!({"context": {"messages": []}})).is_err(), "a build names its model");
    }

    #[test]
    fn a_build_response_round_trips_and_needs_from() {
        let v = json!({"model": "m", "head": snap("51d0"), "usage": {"prompt": 1840, "kept": 0, "fed": 1840, "completion": 0, "reasoning": 0}, "from": null, "dry_run": false,
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
