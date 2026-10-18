//! `POST /council/v2/generations`: a sampled reply after a context.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::context::{Context, SnapshotInfo};
use crate::error::Invalid;
use crate::ids::SnapshotId;
use crate::required_nullable;

/// Sampling settings. `temperature: 0` is greedy and reproducible.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sample {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
}

impl Sample {
    pub fn validate(&self) -> Result<(), Invalid> {
        let bad = Invalid::new("sample out of range: temperature >= 0, 0 < top_p <= 1, 0 <= min_p < 1");
        if self.temperature.is_some_and(|t| t.is_nan() || t < 0.0)
            || self.top_p.is_some_and(|p| !(p > 0.0 && p <= 1.0))
            || self.min_p.is_some_and(|p| !(0.0..1.0).contains(&p))
        {
            return Err(bad);
        }
        Ok(())
    }
}

/// The body of `POST /council/v2/generations`. Capability `generate`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationRequest {
    pub context: Context,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample: Option<Sample>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// Decisions that ride with the generation. Capability `ride`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<RidingDecision>,
}

/// When a riding decision is asked: `before`, after the context and before the reply, or `after`,
/// after the reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RideWhen {
    Before,
    After,
}

/// A held spec's questions, asked in a system message after the context or the reply during a
/// generation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RidingDecision {
    pub spec_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask: Option<Vec<String>>,
    pub when: RideWhen,
}

/// A riding decision's answers, in a decision read's shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RideAnswers {
    pub spec_id: String,
    pub when: RideWhen,
    pub answers: serde_json::Map<String, Value>,
}

impl GenerationRequest {
    /// Checks the context, a positive `max_tokens`, and the sample.
    pub fn validate(&self) -> Result<(), Invalid> {
        self.context.check()?;
        if self.max_tokens == Some(0) {
            return Err(Invalid::param("max_tokens", "max_tokens is at least 1"));
        }
        if let Some(s) = &self.sample {
            s.validate()?;
        }
        Ok(())
    }
}

/// A tool call the reply made. One that did not parse is kept as written, never repaired and
/// never dropped. The server names each call with the `id` a later `tool` message names.
#[derive(Clone, Debug, PartialEq)]
pub enum ReplyToolCall {
    Parsed { id: String, name: String, arguments: Map<String, Value>, stats: Option<TokenStats> },
    Unparsed { id: String, name: Option<String>, raw: String, error: String, stats: Option<TokenStats> },
}

/// The model's untempered log probabilities for the sampled tokens of one part of a reply, and
/// the entropy, in nats, of each distribution they came from. Diagnostics: a server may compute
/// them from logits whose last bits depend on how the prompt was fed, so they agree across
/// replays within 1e-6, not bit for bit.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenStats {
    pub tokens: u64,
    pub mean_logprob: f64,
    pub min_logprob: f64,
    pub mean_entropy: f64,
    pub max_entropy: f64,
}

/// The stats of a reply's thinking and its text; `None` for a part with no tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplyStats {
    #[serde(default)]
    pub reasoning: Option<TokenStats>,
    #[serde(default)]
    pub content: Option<TokenStats>,
}

impl ReplyToolCall {
    pub fn id(&self) -> &str {
        match self {
            ReplyToolCall::Parsed { id, .. } | ReplyToolCall::Unparsed { id, .. } => id,
        }
    }
}

impl<'de> Deserialize<'de> for ReplyToolCall {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        use serde::de::Error;

        #[derive(Deserialize)]
        struct Raw {
            id: String,
            #[serde(default)]
            name: Option<String>,
            arguments: Option<Value>,
            raw: Option<String>,
            error: Option<String>,
            #[serde(default)]
            stats: Option<TokenStats>,
        }
        let r = Raw::deserialize(de)?;
        if r.id.is_empty() {
            return Err(D::Error::custom("a reply tool call has an empty id"));
        }
        match (r.arguments, r.raw, r.error) {
            (Some(Value::Object(arguments)), None, None) => match r.name {
                Some(name) if !name.is_empty() => Ok(ReplyToolCall::Parsed { id: r.id, name, arguments, stats: r.stats }),
                _ => Err(D::Error::custom("a parsed tool call has no name")),
            },
            (Some(_), None, None) => Err(D::Error::custom("tool call arguments are not an object")),
            (None, Some(raw), Some(error)) => Ok(ReplyToolCall::Unparsed { id: r.id, name: r.name, raw, error, stats: r.stats }),
            _ => Err(D::Error::custom("a tool call has either `arguments` or both `raw` and `error`")),
        }
    }
}

impl Serialize for ReplyToolCall {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let v = match self {
            ReplyToolCall::Parsed { id, name, arguments, stats } => serde_json::json!({"id": id, "name": name, "arguments": arguments, "stats": stats}),
            ReplyToolCall::Unparsed { id, name, raw, error, stats } => serde_json::json!({"id": id, "name": name, "raw": raw, "error": error, "stats": stats}),
        };
        v.serialize(ser)
    }
}

/// Why the reply ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Finish {
    Stop,
    Length,
    Cancelled,
}

/// A generation's token counts: `kept` came from held snapshots, `fed` is the prompt tokens this
/// request ran, and the two make `prompt`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationUsage {
    pub prompt: u64,
    pub kept: u64,
    pub fed: u64,
    pub completion: u64,
}

/// The response to a generation: the reply, its token counts, and the head after it. The client
/// appends the reply to its context's messages for the next request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GenerationDone {
    pub content: String,
    /// Empty when the model did not think.
    #[serde(default)]
    pub reasoning: String,
    #[serde(default)]
    pub tool_calls: Vec<ReplyToolCall>,
    pub finish: Finish,
    pub usage: GenerationUsage,
    /// The snapshot after the reply when the server holds it, else `null`.
    #[serde(deserialize_with = "required_nullable")]
    pub head: Option<SnapshotId>,
    /// The snapshots of the context with the reply appended.
    pub snapshots: Vec<SnapshotInfo>,
    /// Absent when the server does not report them.
    #[serde(default)]
    pub stats: Option<ReplyStats>,
    /// The answers of each riding decision, in request order.
    #[serde(default)]
    pub decisions: Vec<RideAnswers>,
}

impl GenerationDone {
    /// The reply as the assistant message a client appends to its context.
    /// `None` when a call did not parse: a client sends that as text, not as a call.
    pub fn as_message(&self) -> Option<crate::message::Message> {
        let mut calls = Vec::new();
        for c in &self.tool_calls {
            match c {
                ReplyToolCall::Parsed { id, name, arguments, .. } => {
                    calls.push(crate::message::ToolCall { id: id.clone(), name: name.clone(), arguments: arguments.clone() })
                }
                ReplyToolCall::Unparsed { .. } => return None,
            }
        }
        Some(crate::message::Message {
            role: crate::message::Role::Assistant,
            content: Some(self.content.clone()).filter(|c| !c.is_empty()),
            reasoning: Some(self.reasoning.clone()).filter(|r| !r.is_empty()),
            tool_calls: calls,
            tool_call_id: None,
            snap: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snap(p: &str) -> String {
        format!("snap:{p}{}", "0".repeat(64 - p.len()))
    }

    fn done() -> Value {
        json!({"content": "", "reasoning": "list it",
               "tool_calls": [{"id": "c2", "name": "ls", "arguments": {"path": "/tmp", "all": true}}],
               "finish": "stop", "usage": {"prompt": 1852, "kept": 1840, "fed": 12, "completion": 30},
               "head": snap("77aa"), "snapshots": [{"id": snap("9a1e"), "message": 1, "tokens": 412}]})
    }

    #[test]
    fn a_done_keeps_call_ids_and_argument_order() {
        let text = done().to_string();
        let d: GenerationDone = serde_json::from_str(&text).unwrap();
        let [ReplyToolCall::Parsed { id, arguments, .. }] = d.tool_calls.as_slice() else { panic!("{:?}", d.tool_calls) };
        assert_eq!(id, "c2");
        assert_eq!(serde_json::to_string(arguments).unwrap(), r#"{"path":"/tmp","all":true}"#);
        let m = d.as_message().unwrap();
        assert_eq!((m.content.as_deref(), m.reasoning.as_deref(), m.tool_calls[0].id.as_str()), (None, Some("list it"), "c2"));
    }

    #[test]
    fn done_needs_its_members_and_a_head_that_may_be_null() {
        for missing in ["head", "snapshots", "usage", "finish", "content"] {
            let mut v = done();
            v.as_object_mut().unwrap().remove(missing);
            assert!(serde_json::from_value::<GenerationDone>(v).is_err(), "{missing}");
        }
        let mut v = done();
        v["head"] = json!(null);
        assert_eq!(serde_json::from_value::<GenerationDone>(v).unwrap().head, None);
    }

    #[test]
    fn a_call_that_did_not_parse_is_kept_and_has_no_message_form() {
        let mut v = done();
        v["tool_calls"] = json!([{"id": "c3", "name": null, "raw": "<function=", "error": "no name"}]);
        let d: GenerationDone = serde_json::from_value(v).unwrap();
        assert_eq!(d.tool_calls[0].id(), "c3");
        assert!(d.as_message().is_none());
        assert_eq!(serde_json::to_value(&d.tool_calls[0]).unwrap()["raw"], "<function=");
    }

    #[test]
    fn malformed_reply_calls_are_refused() {
        for bad in [
            json!({"name": "ls", "arguments": {}}),
            json!({"id": "", "name": "ls", "arguments": {}}),
            json!({"id": "c", "name": "ls", "arguments": "x"}),
            json!({"id": "c", "arguments": {}}),
            json!({"id": "c", "name": "ls", "raw": "x"}),
        ] {
            assert!(serde_json::from_value::<ReplyToolCall>(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_request_rules_each_refuse() {
        let ctx = Context { messages: vec![crate::message::Message::user("u")], ..Default::default() };
        let r = GenerationRequest { context: ctx, max_tokens: Some(8), sample: Some(Sample { temperature: Some(0.0), ..Default::default() }), stream: None, decisions: Vec::new() };
        r.validate().unwrap();
        assert!(GenerationRequest { max_tokens: Some(0), ..r.clone() }.validate().is_err());
        assert!(GenerationRequest { context: Context::default(), ..r.clone() }.validate().is_err());
        for s in [Sample { temperature: Some(-1.0), ..Default::default() }, Sample { top_p: Some(0.0), ..Default::default() }, Sample { min_p: Some(1.0), ..Default::default() }] {
            assert!(GenerationRequest { sample: Some(s), ..r.clone() }.validate().is_err());
        }
    }
}
