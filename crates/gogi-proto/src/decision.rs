//! `POST /council/v2/decisions`: one case read after each context, with the reads pooled.

use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::context::{Context, Tool, Usage};
use crate::error::Invalid;
use crate::identity::Class;
use crate::ids::{SnapshotId, SpecId};
use crate::required_nullable;

pub type QuestionId = String;

/// The most contexts one decision may read.
pub const MAX_CONTEXTS: usize = 8;

/// How reads are pooled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PoolMethod {
    /// The weighted mean of probabilities.
    Linear,
    /// The normalized weighted product.
    Loglinear,
}

/// How reads are weighted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PoolWeights {
    Uniform,
    /// Each read's `exp(mass)` for that question.
    Mass,
    /// The request's `values`.
    Given,
}

/// Pool settings in a request. Absent fields take `linear` and `uniform`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pool {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<PoolMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weights: Option<PoolWeights>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<f64>>,
}

/// The thinking before a decision's answers. `tokens: 0` turns it off; above 0 needs the `think`
/// capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Think {
    pub tokens: u64,
}

/// The body of `POST /council/v2/decisions`. Each context is sent whole; each read names its
/// context by its index in `contexts`, as do the keys of `leave_one_out`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionRequest {
    /// The served model: its `model` or an alias.
    pub model: String,
    /// What the decision judges: a string, object, or array.
    pub case: Value,
    /// A held spec's id. Text: an id the server does not hold is a `404`, not a decode error.
    pub spec_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask: Option<Vec<QuestionId>>,
    /// Question id to the options to narrow it to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contexts: Vec<Context>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<Pool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub think: Option<Think>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<Class>,
    /// Higher runs first within its class. Absent: 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
}

impl DecisionRequest {
    /// Checks the rules a client can check without the server: a string, object, or array
    /// `case`; at most 8 valid contexts, all of them queue contexts or none of them; a
    /// positive `timeout_ms`; and pool `values` that go with `given` weights, one per read,
    /// finite, at least 0, summing above 0.
    pub fn validate(&self) -> Result<(), Invalid> {
        if !matches!(self.case, Value::String(_) | Value::Object(_) | Value::Array(_)) {
            return Err(Invalid::param("case", "case is a string, object, or array"));
        }
        if self.timeout_ms == Some(0) {
            return Err(Invalid::param("timeout_ms", "timeout_ms is at least 1"));
        }
        if self.contexts.len() > MAX_CONTEXTS {
            return Err(Invalid::param("contexts", format!("at most {} contexts", MAX_CONTEXTS)));
        }
        for (i, c) in self.contexts.iter().enumerate() {
            c.check().map_err(|e| Invalid::param("contexts", format!("contexts[{}]: {}", i, e.message)))?;
        }
        let queued = self.contexts.iter().filter(|c| queue_call(c).is_some()).count();
        if queued > 0 && queued != self.contexts.len() {
            return Err(Invalid::param("contexts", "a decision's contexts all end with a call to their queue tool, or none do"));
        }
        let pool = self.pool.clone().unwrap_or_default();
        let weights = pool.weights.unwrap_or(PoolWeights::Uniform);
        match (&pool.values, weights) {
            (Some(_), w) if w != PoolWeights::Given => return Err(Invalid::param("pool.values", "values go with weights given")),
            (None, PoolWeights::Given) => return Err(Invalid::param("pool.values", "weights given needs values")),
            (Some(v), _) => {
                let reads = self.contexts.len().max(1);
                if v.len() != reads {
                    return Err(Invalid::param("pool.values", format!("{} values for {} reads", v.len(), reads)));
                }
                if v.iter().any(|x| !x.is_finite() || *x < 0.0) || !(v.iter().sum::<f64>() > 0.0) {
                    return Err(Invalid::param("pool.values", "values must be finite, at least 0, and sum above 0"));
                }
            }
            (None, _) => {}
        }
        Ok(())
    }

    /// What `math::verify` needs of this request: its pool settings and the names the reads
    /// carry, which are the indexes of `contexts`.
    pub fn expect(&self) -> Expect {
        Expect { pool: self.pool.clone().unwrap_or_default(), contexts: (0..self.contexts.len()).map(|i| i.to_string()).collect() }
    }
}

/// The last call of a context's last message when it calls the queue tool: the call whose
/// result a decision gives as the case.
pub fn queue_call(c: &Context) -> Option<&crate::message::ToolCall> {
    let q = c.queue.as_ref()?;
    let last = c.messages.last().filter(|m| m.role == crate::message::Role::Assistant)?;
    last.tool_calls.last().filter(|t| &t.name == q)
}

/// The queue tool of a context, when `queue` names one.
pub fn queue_tool(c: &Context) -> Option<&Tool> {
    c.tool(c.queue.as_deref()?)
}

/// Checks that the queue tool declares, for each asked question, an `answers` property whose
/// `enum` is the letters `A`, `B`, ... of its options in order. `asked` is each question's id
/// and option count.
pub fn check_queue_schema(c: &Context, asked: &[(&str, usize)]) -> Result<(), Invalid> {
    let q = c.queue.as_deref().ok_or_else(|| Invalid::param("queue", "the context has no queue"))?;
    let tool = c.tool(q).ok_or_else(|| Invalid::param("queue", "the queue tool is gone"))?;
    let props = &tool.input_schema.get("properties").unwrap_or(&Value::Null)["answers"]["properties"];
    for (id, n) in asked {
        let want: Vec<Value> = (0..*n).map(|j| Value::String(((b'A' + j as u8) as char).to_string())).collect();
        if props[*id]["enum"] != Value::Array(want.clone()) {
            let letters = want.iter().map(|w| w.as_str().unwrap()).collect::<Vec<_>>().join(", ");
            return Err(Invalid::param("queue", format!("the queue tool's answers.{} is not an enum of {}", id, letters)));
        }
    }
    Ok(())
}

/// What a decision's numbers are checked against: the request's pool settings and the names its
/// reads carry, in order. Empty `contexts` means one read with no context.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Expect {
    pub pool: Pool,
    pub contexts: Vec<String>,
}

// ---- answers

/// A `choice` answer from one read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReadChoice {
    pub choice: String,
    pub probabilities: IndexMap<String, f64>,
    pub confidence: f64,
    pub logprobs: IndexMap<String, f64>,
    pub mass: f64,
}

/// A `score` answer from one read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReadScore {
    pub score: f64,
    pub legend: IndexMap<String, String>,
    pub probabilities: IndexMap<String, f64>,
    pub confidence: f64,
    pub logprobs: IndexMap<String, f64>,
    pub mass: f64,
}

/// A `noul` answer from one read; `logprobs` is keyed `yes` and `no`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReadNoul {
    pub noul: f64,
    pub logprobs: IndexMap<String, f64>,
    pub mass: f64,
}

/// One context's answer to one question.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ReadAnswer {
    Choice(ReadChoice),
    Score(ReadScore),
    Noul(ReadNoul),
}

pub type Loo<T> = IndexMap<String, Option<T>>;

/// A pooled `choice` answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PooledChoice {
    pub choice: String,
    pub probabilities: IndexMap<String, f64>,
    pub confidence: f64,
    pub agree: bool,
    pub spread: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leave_one_out: Option<Loo<IndexMap<String, f64>>>,
}

/// A pooled `score` answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PooledScore {
    pub score: f64,
    pub legend: IndexMap<String, String>,
    pub probabilities: IndexMap<String, f64>,
    pub confidence: f64,
    pub agree: bool,
    pub spread: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leave_one_out: Option<Loo<IndexMap<String, f64>>>,
}

/// A pooled `noul` answer: the pooled probability of yes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PooledNoul {
    pub noul: f64,
    pub agree: bool,
    pub spread: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leave_one_out: Option<Loo<f64>>,
}

/// An answer pooled over the reads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PooledAnswer {
    Choice(PooledChoice),
    Score(PooledScore),
    Noul(PooledNoul),
}

/// A read's required `context`: `null`, a v1 context id, or a council v2 index. An index is
/// parsed as its decimal text.
fn context_name<'de, D: Deserializer<'de>>(de: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Name {
        Id(String),
        Index(u64),
    }
    Ok(Option::<Name>::deserialize(de)?.map(|n| match n {
        Name::Id(id) => id,
        Name::Index(i) => i.to_string(),
    }))
}

/// Writes a read's `context` as an index when its text is one, else as text, else `null`.
fn write_context_name<S: serde::Serializer>(name: &Option<String>, ser: S) -> Result<S::Ok, S::Error> {
    match name {
        None => ser.serialize_none(),
        Some(n) => match n.parse::<u64>() {
            Ok(i) if i.to_string() == *n => ser.serialize_u64(i),
            _ => ser.serialize_str(n),
        },
    }
}

/// One context's read of the case.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Read {
    /// The context read, or null when the decision sends no contexts. A council v2 response names
    /// it by its index in the request's `contexts`, parsed here as that index's decimal text.
    #[serde(deserialize_with = "context_name", serialize_with = "write_context_name")]
    pub context: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub snapshot: Option<SnapshotId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub described: Option<IndexMap<QuestionId, String>>,
    pub answers: IndexMap<QuestionId, ReadAnswer>,
    /// sha256 of the rendered text this read was conditioned on.
    pub rendered_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ms: Option<f64>,
}

/// A control token that some text spelled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlTextHit {
    pub r#where: String,
    pub token: String,
}

/// Signals the server reports beside the answers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signals {
    #[serde(default)]
    pub control_text: Vec<ControlTextHit>,
}

/// The pool settings the server used.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PoolEcho {
    pub method: PoolMethod,
    pub weights: PoolWeights,
    /// By question id, the weight each read pooled with, in read order.
    pub normalized: IndexMap<QuestionId, Vec<f64>>,
}

/// The identity fields a decision's numbers depend on, so a client fits its thresholds per
/// identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionIdentity {
    pub model: String,
    pub weight_hash: String,
    pub tokenizer_hash: String,
    pub template: String,
    pub engine: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_id: Option<SpecId>,
}

/// The response to `POST /council/v2/decisions`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecisionResponse {
    pub model: String,
    pub answers: IndexMap<QuestionId, PooledAnswer>,
    pub reads: Vec<Read>,
    pub pool: PoolEcho,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signals: Option<Signals>,
    pub identity: DecisionIdentity,
    /// Summed over the reads: `prompt` is every token a read was conditioned on, and
    /// `completion` the `text` answers and thoughts the model wrote.
    pub usage: Usage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ms: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Message, ToolCall};
    use serde_json::json;

    fn spec_id() -> String {
        format!("sha256:{}", "a".repeat(64))
    }

    fn chat() -> Context {
        Context { messages: vec![Message::user("u")], ..Default::default() }
    }

    fn queue_context() -> Context {
        let call = ToolCall { id: "c".into(), name: "next".into(), input: Default::default() };
        Context {
            tools: vec![Tool::new("next", json!({"type": "object", "properties": {"answers": {"type": "object", "properties": {
                "verdict": {"type": "string", "enum": ["A", "B"]}}}}}))],
            queue: Some("next".into()),
            messages: vec![Message::user("u"), Message { tool_calls: vec![call], ..Message::assistant("") }],
            ..Default::default()
        }
    }

    fn req() -> DecisionRequest {
        DecisionRequest { model: "m".into(), case: json!("git push"), spec_id: spec_id(), contexts: vec![chat()], ..Default::default() }
    }

    fn param(r: DecisionRequest) -> String {
        let e = r.validate().expect_err("should be refused");
        e.param.unwrap_or(e.message)
    }

    #[test]
    fn a_plain_request_is_valid_and_each_rule_refuses() {
        req().validate().unwrap();
        DecisionRequest { contexts: vec![], ..req() }.validate().unwrap();
        assert_eq!(param(DecisionRequest { case: json!(3), ..req() }), "case");
        assert_eq!(param(DecisionRequest { timeout_ms: Some(0), ..req() }), "timeout_ms");
        assert_eq!(param(DecisionRequest { contexts: vec![chat(); 9], ..req() }), "contexts");
        assert_eq!(param(DecisionRequest { contexts: vec![Context::default()], ..req() }), "contexts");
        DecisionRequest { contexts: vec![chat(); 8], ..req() }.validate().unwrap();
    }

    #[test]
    fn queue_contexts_all_end_with_their_call_or_none_do() {
        DecisionRequest { contexts: vec![queue_context(), queue_context()], ..req() }.validate().unwrap();
        assert_eq!(param(DecisionRequest { contexts: vec![queue_context(), chat()], ..req() }), "contexts");
        assert!(queue_call(&queue_context()).is_some() && queue_call(&chat()).is_none());
    }

    #[test]
    fn given_weights_need_one_good_value_per_read() {
        let given = |values: Option<Vec<f64>>, n: usize| DecisionRequest {
            contexts: vec![chat(); n],
            pool: Some(Pool { weights: Some(PoolWeights::Given), values, ..Default::default() }),
            ..req()
        };
        given(Some(vec![1.0, 0.0]), 2).validate().unwrap();
        for bad in [None, Some(vec![1.0]), Some(vec![1.0, -1.0]), Some(vec![0.0, 0.0]), Some(vec![f64::NAN, 1.0])] {
            assert_eq!(param(given(bad.clone(), 2)), "pool.values", "{bad:?}");
        }
        let stray = DecisionRequest { pool: Some(Pool { values: Some(vec![1.0]), ..Default::default() }), ..req() };
        assert_eq!(param(stray), "pool.values");
        let none = DecisionRequest { contexts: vec![], pool: Some(Pool { weights: Some(PoolWeights::Given), values: Some(vec![2.0]), ..Default::default() }), ..req() };
        none.validate().unwrap();
    }

    #[test]
    fn a_request_decodes_strictly_and_names_reads_by_index() {
        let v = json!({"model": "m", "case": {"b": 1, "a": 2}, "spec_id": spec_id(), "contexts": [{"messages": [{"role": "user", "content": "u"}]}], "think": {"tokens": 0}});
        let r: DecisionRequest = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(serde_json::to_value(&r).unwrap(), v);
        assert_eq!(serde_json::to_string(&r.case).unwrap(), r#"{"b":1,"a":2}"#);
        assert_eq!(r.expect().contexts, ["0"]);
        let v1 = json!({"model": "m", "spec_id": spec_id(), "case": "s", "contexts": [{"id": "old-style"}]});
        assert!(serde_json::from_value::<DecisionRequest>(v1).is_err(), "a v1 context reference is not a v2 context");
        assert!(serde_json::from_value::<DecisionRequest>(json!({"model": "m", "spec_id": spec_id(), "case": "s", "contxts": []})).is_err());
        for gone in [json!({"state": "s"}), json!({"case": "s", "questions": {}}), json!({"case": "s", "provider": {}}), json!({"case": "s", "session_id": "x"})] {
            let mut v = json!({"model": "m", "spec_id": spec_id()});
            v.as_object_mut().unwrap().extend(gone.as_object().unwrap().clone());
            assert!(serde_json::from_value::<DecisionRequest>(v.clone()).is_err(), "{v}");
        }
        assert!(serde_json::from_value::<DecisionRequest>(json!({"spec_id": spec_id(), "case": "s"})).is_err(), "a decision names its model");
    }

    #[test]
    fn the_queue_schema_names_each_asked_questions_letters() {
        let c = queue_context();
        check_queue_schema(&c, &[("verdict", 2)]).unwrap();
        let e = check_queue_schema(&c, &[("verdict", 3)]).unwrap_err();
        assert_eq!(e.message, "the queue tool's answers.verdict is not an enum of A, B, C");
        assert!(check_queue_schema(&c, &[("undo", 2)]).is_err());
    }

    #[test]
    fn a_read_names_its_context_by_text_or_index() {
        let r = |c: Value| json!({"context": c, "snapshot": null, "answers": {}, "rendered_sha256": "x"});
        assert_eq!(serde_json::from_value::<Read>(r(json!(3))).unwrap().context.as_deref(), Some("3"));
        assert_eq!(serde_json::from_value::<Read>(r(json!("abc"))).unwrap().context.as_deref(), Some("abc"));
        assert_eq!(serde_json::from_value::<Read>(r(json!(null))).unwrap().context, None);
        let named = |c: Value| serde_json::to_value(serde_json::from_value::<Read>(r(c)).unwrap()).unwrap()["context"].clone();
        assert_eq!((named(json!(3)), named(json!("abc")), named(json!(null))), (json!(3), json!("abc"), json!(null)));
        let mut missing = r(json!(0));
        missing.as_object_mut().unwrap().remove("snapshot");
        assert!(serde_json::from_value::<Read>(missing).is_err());
    }
}
