//! `POST /council/v2/generations`: a sampled reply after a context.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::context::{Context, SnapshotInfo, Usage};
use crate::error::{ErrorBody, Invalid};
use crate::identity::Class;
use crate::ids::SnapshotId;
use crate::required_nullable;

/// Sampling settings. `temperature: 0` is greedy and reproducible. With a `seed`, a server
/// usually samples the same reply again, but not always: logits whose last bits depend on how the
/// prompt was fed can change a sample.
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
    /// The served model: its `model` or an alias.
    pub model: String,
    pub context: Context,
    /// The most tokens the reply may have, 1 to the served model's `context_tokens`.
    pub max_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample: Option<Sample>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// Decisions that ride with the generation. Capability `ride`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<RidingDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<Class>,
    /// Higher runs first within its class. Absent: 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
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
    /// Checks the context, a positive `max_tokens`, the sample, and each riding decision's `ask`.
    pub fn validate(&self) -> Result<(), Invalid> {
        self.context.check()?;
        if self.max_tokens == 0 {
            return Err(Invalid::param("max_tokens", "max_tokens is at least 1"));
        }
        if let Some(s) = &self.sample {
            s.validate()?;
        }
        for (i, d) in self.decisions.iter().enumerate() {
            if let Some(ask) = &d.ask {
                crate::decision::check_ask(ask).map_err(|m| Invalid::param("decisions", format!("decisions[{}]: {}", i, m)))?;
            }
        }
        Ok(())
    }
}

/// A tool call the reply made. One that did not parse is kept as written, never repaired and
/// never dropped. The server names each call with the `id` a later `tool` message names.
#[derive(Clone, Debug, PartialEq)]
pub enum ReplyToolCall {
    Parsed { id: String, name: String, input: Map<String, Value>, stats: Option<TokenStats> },
    Unparsed { id: String, name: Option<String>, raw: String, error: String, stats: Option<TokenStats> },
}

/// The model's untempered log probabilities for the sampled tokens of one part of a reply, and
/// the entropy, in nats, of each distribution they came from. Diagnostics: a server may compute
/// them from logits whose last bits depend on how the prompt was fed, so they agree across
/// replays within 1e-4, not bit for bit.
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
            input: Option<Value>,
            raw: Option<String>,
            error: Option<String>,
            #[serde(default)]
            stats: Option<TokenStats>,
        }
        let r = Raw::deserialize(de)?;
        if r.id.is_empty() {
            return Err(D::Error::custom("a reply tool call has an empty id"));
        }
        match (r.input, r.raw, r.error) {
            (Some(Value::Object(input)), None, None) => match r.name {
                Some(name) if !name.is_empty() => Ok(ReplyToolCall::Parsed { id: r.id, name, input, stats: r.stats }),
                _ => Err(D::Error::custom("a parsed tool call has no name")),
            },
            (Some(_), None, None) => Err(D::Error::custom("a tool call's input is not an object")),
            (None, Some(raw), Some(error)) => Ok(ReplyToolCall::Unparsed { id: r.id, name: r.name, raw, error, stats: r.stats }),
            _ => Err(D::Error::custom("a tool call has either `input` or both `raw` and `error`")),
        }
    }
}

impl Serialize for ReplyToolCall {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let v = match self {
            ReplyToolCall::Parsed { id, name, input, stats } => serde_json::json!({"id": id, "name": name, "input": input, "stats": stats}),
            ReplyToolCall::Unparsed { id, name, raw, error, stats } => serde_json::json!({"id": id, "name": name, "raw": raw, "error": error, "stats": stats}),
        };
        v.serialize(ser)
    }
}

/// Why the reply ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Finish {
    /// The model ended its turn with no tool call.
    EndTurn,
    /// The model ended its turn after one or more tool calls.
    ToolUse,
    /// The reply reached `max_tokens`.
    MaxTokens,
    /// The prompt and the reply reached the served model's `context_tokens`.
    ContextFull,
}

/// The response to a generation: the reply, its token counts, and the head after it. The client
/// appends the reply to its context's messages for the next request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GenerationDone {
    /// The served model's `model`, never an alias.
    pub model: String,
    pub content: String,
    /// Empty when the model did not think.
    #[serde(default)]
    pub reasoning: String,
    #[serde(default)]
    pub tool_calls: Vec<ReplyToolCall>,
    pub finish: Finish,
    /// `prompt` counts the prompt; `completion` the reply, with its thinking in `reasoning`.
    pub usage: Usage,
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
    /// The seed the server sampled with: the request's, or one it chose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
}

impl GenerationDone {
    /// The reply as the assistant message a client appends to its context.
    /// `None` when a call did not parse: a client sends that as text, not as a call.
    pub fn as_message(&self) -> Option<crate::message::Message> {
        let mut calls = Vec::new();
        for c in &self.tool_calls {
            match c {
                ReplyToolCall::Parsed { id, name, input, .. } => {
                    calls.push(crate::message::ToolCall { id: id.clone(), name: name.clone(), input: input.clone() })
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
            is_error: false,
            snap: false,
        })
    }
}

/// Which part of the reply a streamed token belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Reasoning,
    Content,
    Call,
    /// A phase this crate does not know.
    #[serde(other)]
    Unknown,
}

/// A `token` event: one sampled token of the reply. `piece` is display text, which may be empty
/// while a character is incomplete; the reply is `done`'s fields, never the joined pieces.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TokenEvent {
    /// The token's id.
    pub id: u32,
    pub piece: String,
    pub phase: Phase,
    /// The token's untempered probability.
    pub p: f64,
    /// The entropy, in nats, of the distribution it came from.
    pub h: f64,
    /// The reply's tokens so far, this one included.
    pub n: u64,
    /// Reply tokens per second so far.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tps: Option<f64>,
}

/// Whether `c` is whitespace that a part's joined pieces and `done`'s text may differ by at either
/// end: Unicode White_Space, and U+001C to U+001F, which Python's `str.strip` also removes.
pub fn is_edge_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `s` without edge whitespace at either end.
pub fn trim_edges(s: &str) -> &str {
    s.trim_matches(is_edge_space)
}

/// Checks that the pieces of a stream's `reasoning` tokens, joined, equal `done.reasoning`, and its
/// `content` pieces `done.content`, each up to edge whitespace at either end. `call` pieces are
/// display text only; the calls are `done.tool_calls`.
pub fn check_pieces(tokens: &[TokenEvent], done: &GenerationDone) -> Result<(), String> {
    for (phase, name, text) in [(Phase::Reasoning, "reasoning", &done.reasoning), (Phase::Content, "content", &done.content)] {
        let joined: String = tokens.iter().filter(|t| t.phase == phase).map(|t| t.piece.as_str()).collect();
        if trim_edges(&joined) != trim_edges(text) {
            return Err(format!("the {} pieces join to {:?}, but done.{} is {:?}", name, joined, name, text));
        }
    }
    Ok(())
}

/// One server-sent event of a streamed generation. The stream ends after `done` or `error`.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamEvent {
    Token(TokenEvent),
    /// Sent while nothing else is; it carries `{}`.
    Ping,
    Done(Box<GenerationDone>),
    /// A failure after the stream began, in an error response's shape.
    Error(ErrorBody),
}

impl StreamEvent {
    /// Decodes one event from its `event` name and `data`. `Ok(None)` is an event this crate does
    /// not know, which a client ignores.
    pub fn parse(name: &str, data: &str) -> Result<Option<StreamEvent>, String> {
        let bad = |e: serde_json::Error| format!("a {} event does not decode: {}", name, e);
        Ok(Some(match name {
            "token" => StreamEvent::Token(serde_json::from_str(data).map_err(bad)?),
            "ping" => StreamEvent::Ping,
            "done" => StreamEvent::Done(Box::new(serde_json::from_str(data).map_err(bad)?)),
            "error" => StreamEvent::Error(serde_json::from_str(data).map_err(bad)?),
            _ => return Ok(None),
        }))
    }

    /// The `event` name.
    pub fn name(&self) -> &'static str {
        match self {
            StreamEvent::Token(_) => "token",
            StreamEvent::Ping => "ping",
            StreamEvent::Done(_) => "done",
            StreamEvent::Error(_) => "error",
        }
    }

    /// The `data`, as JSON text.
    pub fn data(&self) -> String {
        match self {
            StreamEvent::Token(t) => serde_json::to_string(t),
            StreamEvent::Ping => Ok("{}".to_string()),
            StreamEvent::Done(d) => serde_json::to_string(d),
            StreamEvent::Error(e) => serde_json::to_string(e),
        }
        .expect("event data encodes")
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
        json!({"model": "m", "content": "", "reasoning": "list it",
               "tool_calls": [{"id": "c2", "name": "ls", "input": {"path": "/tmp", "all": true}}],
               "finish": "tool_use", "usage": {"prompt": 1852, "kept": 1840, "fed": 12, "completion": 30, "reasoning": 4},
               "head": snap("77aa"), "snapshots": [{"id": snap("9a1e"), "message": 1, "tokens": 412}]})
    }

    #[test]
    fn a_done_keeps_call_ids_and_input_order() {
        let text = done().to_string();
        let d: GenerationDone = serde_json::from_str(&text).unwrap();
        let [ReplyToolCall::Parsed { id, input, .. }] = d.tool_calls.as_slice() else { panic!("{:?}", d.tool_calls) };
        assert_eq!(id, "c2");
        assert_eq!(serde_json::to_string(input).unwrap(), r#"{"path":"/tmp","all":true}"#);
        let m = d.as_message().unwrap();
        assert_eq!((m.content.as_deref(), m.reasoning.as_deref(), m.tool_calls[0].id.as_str()), (None, Some("list it"), "c2"));
    }

    #[test]
    fn done_needs_its_members_and_a_head_that_may_be_null() {
        for missing in ["model", "head", "snapshots", "usage", "finish", "content"] {
            let mut v = done();
            v.as_object_mut().unwrap().remove(missing);
            assert!(serde_json::from_value::<GenerationDone>(v).is_err(), "{missing}");
        }
        let mut v = done();
        v["head"] = json!(null);
        assert_eq!(serde_json::from_value::<GenerationDone>(v).unwrap().head, None);
        for old in ["stop", "length", "cancelled"] {
            let mut v = done();
            v["finish"] = json!(old);
            assert!(serde_json::from_value::<GenerationDone>(v).is_err(), "{old}");
        }
    }

    #[test]
    fn a_parts_pieces_join_to_dones_text_up_to_edge_whitespace() {
        let tok = |piece: &str, phase: Phase| TokenEvent { id: 1, piece: piece.into(), phase, p: 1.0, h: 0.0, n: 1, tps: None };
        let mut d: GenerationDone = serde_json::from_value(done()).unwrap();
        d.content = "ls -la".into();
        let tokens = [tok("list", Phase::Reasoning), tok(" it\n", Phase::Reasoning), tok("\n", Phase::Content), tok("ls", Phase::Content), tok(" -la", Phase::Content), tok("<call>", Phase::Call)];
        check_pieces(&tokens, &d).unwrap();
        let shown_twice = [tok("list it", Phase::Reasoning), tok("ls -la", Phase::Content), tok("ls", Phase::Content)];
        assert!(check_pieces(&shown_twice, &d).unwrap_err().starts_with("the content pieces join"));
        let hidden = [tok("", Phase::Reasoning), tok("ls -la", Phase::Content)];
        assert!(check_pieces(&hidden, &d).unwrap_err().starts_with("the reasoning pieces join"));
        assert_eq!(trim_edges("\u{1f}\u{a0} x \u{2029}"), "x");
        assert!(!is_edge_space('\u{200b}'));
    }

    #[test]
    fn stream_events_decode_by_name_and_skip_unknown_ones() {
        let t = r#"{"id": 42, "piece": "ls", "phase": "content", "p": 0.9, "h": 0.3, "n": 1, "tps": 50.0}"#;
        let Some(StreamEvent::Token(e)) = StreamEvent::parse("token", t).unwrap() else { panic!() };
        assert_eq!((e.id, e.phase, e.n), (42, Phase::Content, 1));
        assert_eq!(StreamEvent::parse("ping", "{}").unwrap(), Some(StreamEvent::Ping));
        assert_eq!(StreamEvent::parse("progress", "{}").unwrap(), None);
        let err = r#"{"error": {"type": "internal", "message": "the engine failed; nothing more is sent"}, "request_id": "req_1"}"#;
        let Some(StreamEvent::Error(b)) = StreamEvent::parse("error", err).unwrap() else { panic!() };
        assert_eq!(b.request_id.as_deref(), Some("req_1"));
        let d = StreamEvent::parse("done", &done().to_string()).unwrap().unwrap();
        assert_eq!(StreamEvent::parse(d.name(), &d.data()).unwrap(), Some(d));
        assert!(StreamEvent::parse("token", "{}").is_err());
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
            json!({"name": "ls", "input": {}}),
            json!({"id": "", "name": "ls", "input": {}}),
            json!({"id": "c", "name": "ls", "input": "x"}),
            json!({"id": "c", "input": {}}),
            json!({"id": "c", "name": "ls", "raw": "x"}),
        ] {
            assert!(serde_json::from_value::<ReplyToolCall>(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_request_rules_each_refuse() {
        let ctx = Context { messages: vec![crate::message::Message::user("u")], ..Default::default() };
        let r = GenerationRequest { model: "m".into(), context: ctx, max_tokens: 8, sample: Some(Sample { temperature: Some(0.0), ..Default::default() }), stream: None, decisions: Vec::new(), class: None, priority: None };
        r.validate().unwrap();
        assert!(GenerationRequest { max_tokens: 0, ..r.clone() }.validate().is_err());
        let v = json!({"model": "m", "context": {"messages": [{"role": "user", "content": "u"}]}});
        assert!(serde_json::from_value::<GenerationRequest>(v).is_err(), "a generation sends max_tokens");
        assert!(GenerationRequest { context: Context::default(), ..r.clone() }.validate().is_err());
        let ride = |ask: Option<Vec<String>>| GenerationRequest {
            decisions: vec![RidingDecision { spec_id: format!("sha256:{}", "a".repeat(64)), ask, when: RideWhen::After }],
            ..r.clone()
        };
        ride(Some(vec!["q".into()])).validate().unwrap();
        assert_eq!(ride(Some(vec![])).validate().unwrap_err().param.as_deref(), Some("decisions"));
        assert_eq!(ride(Some(vec!["q".into(), "q".into()])).validate().unwrap_err().param.as_deref(), Some("decisions"));
        for s in [Sample { temperature: Some(-1.0), ..Default::default() }, Sample { top_p: Some(0.0), ..Default::default() }, Sample { min_p: Some(1.0), ..Default::default() }] {
            assert!(GenerationRequest { sample: Some(s), ..r.clone() }.validate().is_err());
        }
    }
}
