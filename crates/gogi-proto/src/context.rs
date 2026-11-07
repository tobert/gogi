//! A context as a request carries it, and the response to building one.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::control::Control;
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
    /// The objects the request read and the reply wrote. Absent when it read and wrote text only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<MediaUsage>,
}

/// What a request read and wrote that is not text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaUsage {
    pub read: MediaCount,
    pub written: MediaCount,
}

/// A count of objects and their bytes, with the measures a served model reports for its media:
/// milliseconds of audio, pixels of images, frames of video, and denoising steps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaCount {
    pub objects: u64,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pixels: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<u64>,
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    /// Whether the model thinks before its reply. Required: a context without it is a `400`
    /// naming `thinking`. `true` needs the served model's `thinking` capability.
    #[serde(deserialize_with = "thinking_flag")]
    pub thinking: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
    /// The control vectors that change the served model's residual while it reads this context,
    /// applied in order. Empty: the plain model. A non-empty list needs `control_vectors`.
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "crate::control::control_list")]
    pub control: Vec<Control>,
    pub messages: Vec<Message>,
}

/// Decodes `thinking`, which is `true` or `false`. Any other value is an error that names
/// `thinking`.
fn thinking_flag<'de, D: serde::Deserializer<'de>>(de: D) -> Result<bool, D::Error> {
    match serde_json::Value::deserialize(de)? {
        serde_json::Value::Bool(b) => Ok(b),
        other => Err(serde::de::Error::custom(format!("invalid field `thinking`: thinking is true or false, not {}", other))),
    }
}

impl Context {
    /// A context with no tools and no queue. Each caller says whether the model thinks.
    pub fn new(thinking: bool, messages: Vec<Message>) -> Context {
        Context { thinking, tools: vec![], queue: None, control: vec![], messages }
    }

    /// Checks the messages, that each tool has a distinct name, that `queue` names a tool, and that
    /// each control item has a finite strength. An open message with reasoning needs
    /// `thinking: true`.
    pub fn check(&self) -> Result<(), Invalid> {
        if self.messages.is_empty() {
            return Err(Invalid::new("a context has at least one message"));
        }
        for (i, c) in self.control.iter().enumerate() {
            c.check(&format!("control[{}]", i))?;
        }
        message::check_opens(&self.messages)?;
        if let Some(m) = self.open()
            && !m.reasoning.as_deref().unwrap_or("").is_empty()
            && !self.thinking
        {
            return Err(Invalid::param("continue", format!("messages[{}]: an open message with reasoning needs thinking: true in the context", self.messages.len() - 1)));
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

    /// The last message when it is open (`continue: true`): the assistant message a
    /// generation continues.
    pub fn open(&self) -> Option<&Message> {
        self.messages.last().filter(|m| m.open)
    }

    /// Checks a context that a decision or a raw read reads, the `i`th of a request: an error
    /// names `continue` when the context is open or breaks a rule of an open message, the member
    /// under `contexts[i]` when the error names one, such as `contexts[0].messages[2].content[1]`,
    /// and `contexts` otherwise. `what` names the request and its verb, such as "a decision reads".
    pub fn check_read(&self, i: usize, what: &str) -> Result<(), Invalid> {
        self.check_in("contexts", i, what)
    }

    /// Checks a closed context, the `i`th of the request's list `list`, such as `contexts` or
    /// `positive`, as [`Context::check_read`] does.
    pub fn check_in(&self, list: &str, i: usize, what: &str) -> Result<(), Invalid> {
        let at = format!("{}[{}]", list, i);
        let named = |e: Invalid| {
            let param = match e.param.as_deref() {
                Some("continue") => "continue".to_string(),
                Some(p) if p.starts_with("messages[") || p.starts_with("control") => format!("{}.{}", at, p),
                _ => list.to_string(),
            };
            Invalid::param(param, format!("{}: {}", at, e.message))
        };
        self.check().map_err(named)?;
        if self.open().is_some() {
            return Err(Invalid::param("continue", format!("{}: {} no open message; send continue: true only to a generation", at, what)));
        }
        Ok(())
    }

    /// Checks that served model `model` declares `control_vectors` when the context has a
    /// `control` list.
    pub fn check_control(&self, model: &crate::identity::ServedModel) -> Result<(), Invalid> {
        if !self.control.is_empty() && !model.has(crate::identity::Capability::ControlVectors) {
            return Err(Invalid::param("control", format!("nothing ran: {} does not declare control_vectors; send no control", model.model)));
        }
        Ok(())
    }

    /// Checks that served model `model` declares `thinking` when the context sets `thinking: true`.
    pub fn check_thinking(&self, model: &crate::identity::ServedModel) -> Result<(), Invalid> {
        if self.thinking && !model.has(crate::identity::Capability::Thinking) {
            return Err(Invalid::param("thinking", format!("nothing ran: {} does not declare thinking; send thinking: false", model.model)));
        }
        Ok(())
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

impl ContextBuild {
    /// Checks the context. A build reads an open message only with `dry_run: true`.
    pub fn validate(&self) -> Result<(), Invalid> {
        self.context.check()?;
        if self.context.open().is_some() && self.dry_run != Some(true) {
            return Err(Invalid::param("continue", "nothing was built: a build reads an open message only with dry_run: true; send dry_run: true to test a continuation, or send it to a generation"));
        }
        Ok(())
    }
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
        format!("blake3:{p}{}", "0".repeat(64 - p.len()))
    }

    #[test]
    fn a_read_names_a_part_under_its_context() {
        use crate::message::{Content, Part};
        let bad = Message { content: Some(Content::Parts(vec![Part::Object { id: "blake3:ab".into(), mime: "image/png".into() }])), ..Message::user("") };
        let c = Context::new(false, vec![Message::user("u"), bad]);
        let e = c.check_read(3, "a decision reads").unwrap_err();
        assert_eq!(e.param.as_deref(), Some("contexts[3].messages[1].content[0]"), "{e:?}");
    }

    #[test]
    fn a_context_needs_a_message() {
        assert_eq!(Context::new(false, vec![]).check().unwrap_err().message, "a context has at least one message");
    }

    #[test]
    fn queue_names_a_tool() {
        let mut c = Context { thinking: false, tools: vec![tool("next")], queue: Some("next".into()), control: vec![], messages: vec![Message::user("u")] };
        c.check().unwrap();
        c.queue = Some("missing".into());
        assert_eq!(c.check().unwrap_err().message, "queue missing names no tool in tools");
        c.tools.clear();
        assert!(c.check().is_err());
    }

    #[test]
    fn tool_names_are_distinct_and_not_empty() {
        let mut c = Context { thinking: false, tools: vec![tool("a"), tool("b")], queue: None, control: vec![], messages: vec![Message::user("u")] };
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
        Usage { prompt: 10, kept: 4, fed: 6, completion: 3, reasoning: 2, media: None }.check().unwrap();
        assert!(Usage { prompt: 10, kept: 4, fed: 5, ..Default::default() }.check().is_err());
        assert!(Usage { completion: 1, reasoning: 2, ..Default::default() }.check().is_err());
        assert!(serde_json::from_value::<Usage>(json!({"prompt": 1, "kept": 0, "fed": 1, "completion": 0})).is_err());
    }

    #[test]
    fn the_messages_are_checked_from_the_start() {
        let c = Context::new(false, vec![Message::tool("a", "r")]);
        assert!(c.check().unwrap_err().message.starts_with("messages[0]"));
        let c = Context::new(false, vec![Message { tool_calls: vec![ToolCall { id: "a".into(), name: "n".into(), input: Default::default() }], ..Message::assistant("") }, Message::tool("a", "r")]);
        c.check().unwrap();
    }

    #[test]
    fn a_context_without_thinking_is_refused_and_names_thinking() {
        let e = serde_json::from_value::<Context>(json!({"messages": [{"role": "user", "content": "u"}]})).unwrap_err();
        assert_eq!(crate::error::decode_param(&e.to_string()).as_deref(), Some("thinking"), "{e}");
        for v in [json!("yes"), json!(null), json!(1)] {
            let e = serde_json::from_value::<Context>(json!({"thinking": v, "messages": [{"role": "user", "content": "u"}]})).unwrap_err();
            assert_eq!(crate::error::decode_param(&e.to_string()).as_deref(), Some("thinking"), "{v}: {e}");
        }
        let c: Context = serde_json::from_value(json!({"thinking": false, "messages": [{"role": "user", "content": "u"}]})).unwrap();
        assert_eq!(serde_json::to_value(&c).unwrap()["thinking"], json!(false), "false is written, not left out");
    }

    fn served(capabilities: Vec<crate::identity::Capability>) -> crate::identity::ServedModel {
        serde_json::from_value(json!({"model": "m", "status": "ready", "weight_hash": "w", "tokenizer_hash": "t", "template": "p", "engine": "e",
            "limits": {"context_tokens": 8, "decision_context_tokens": 8, "snapshot_bytes": 1, "contexts_per_decision": 1, "default_timeout_ms": 1},
            "capabilities": capabilities}))
        .unwrap()
    }

    #[test]
    fn thinking_true_needs_the_thinking_capability() {
        use crate::identity::Capability;
        let on = Context { thinking: true, tools: vec![], queue: None, control: vec![], messages: vec![Message::user("u")] };
        let off = Context { thinking: false, ..on.clone() };
        let plain = served(vec![Capability::Generate]);
        let e = on.check_thinking(&plain).unwrap_err();
        assert_eq!((e.param.as_deref(), e.message.as_str()), (Some("thinking"), "nothing ran: m does not declare thinking; send thinking: false"));
        off.check_thinking(&plain).unwrap();
        let thinker = served(vec![Capability::Generate, Capability::Thinking]);
        on.check_thinking(&thinker).unwrap();
        off.check_thinking(&thinker).unwrap();
    }

    fn open_context(thinking: bool, open: Message) -> Context {
        Context::new(thinking, vec![Message::user("u"), open])
    }

    #[test]
    fn an_open_message_with_reasoning_needs_thinking_sent_true() {
        let thought = Message { reasoning: Some("I should".into()), content: None, ..Message::assistant("").opened() };
        open_context(true, thought.clone()).check().unwrap();
        let e = open_context(false, thought.clone()).check().unwrap_err();
        assert_eq!((e.param.as_deref(), e.message.as_str()), (Some("continue"), "messages[1]: an open message with reasoning needs thinking: true in the context"));
        open_context(false, Message::assistant("The plan:").opened()).check().unwrap();
        let mut stray = open_context(false, thought.clone());
        stray.messages.insert(0, Message::tool("nope", "r"));
        assert_eq!(stray.check().unwrap_err().param.as_deref(), Some("continue"), "the open message's rule comes first");
        open_context(false, Message { reasoning: Some(String::new()), ..Message::assistant("x").opened() }).check().unwrap();
    }

    #[test]
    fn open_names_the_open_last_message() {
        let c = open_context(false, Message::assistant("The plan:").opened());
        assert_eq!(c.open().and_then(|m| m.text()), Some("The plan:"));
        assert_eq!(open_context(false, Message::assistant("done")).open(), None);
    }

    #[test]
    fn a_build_reads_an_open_message_only_as_a_dry_run() {
        let build = |dry_run| ContextBuild { model: "m".into(), context: open_context(false, Message::assistant("The plan:").opened()), dry_run, class: None, priority: None };
        build(Some(true)).validate().unwrap();
        for dry_run in [None, Some(false)] {
            let e = build(dry_run).validate().unwrap_err();
            assert_eq!(e.param.as_deref(), Some("continue"), "{dry_run:?}");
            assert!(e.message.contains("dry_run: true"), "{}", e.message);
        }
        let closed = ContextBuild { context: open_context(false, Message::assistant("done")), ..build(None) };
        closed.validate().unwrap();
        assert!(ContextBuild { context: Context::new(false, vec![]), ..build(Some(true)) }.validate().is_err());
    }

    #[test]
    fn a_context_with_a_stray_member_is_refused() {
        assert!(serde_json::from_value::<Context>(json!({"thinking": false, "messages": [], "system": "x"})).is_err());
        assert!(serde_json::from_value::<ContextBuild>(json!({"model": "m", "context": {"thinking": false, "messages": []}, "persist": true})).is_err());
        assert!(serde_json::from_value::<ContextBuild>(json!({"context": {"thinking": false, "messages": []}})).is_err(), "a build names its model");
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
