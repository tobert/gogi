//! `POST /council/v2/reads`: raw reads, letter log probabilities after a suffix.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::context::{Context, Usage};
use crate::decision::Signals;
use crate::error::Invalid;
use crate::identity::Class;
use crate::ids::SnapshotId;

/// The most contexts one request may name.
pub const MAX_CONTEXTS: usize = 8;

/// The body of `POST /council/v2/reads`: each suffix read after each context's end.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadsRequest {
    /// The served model: its `model` or an alias.
    pub model: String,
    pub contexts: Vec<Context>,
    /// Text the client formats itself; special-token text in it becomes one token id each.
    pub suffixes: Vec<String>,
    pub letters: Vec<String>,
    /// How many of the most likely tokens each read reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<Class>,
    /// Higher runs first within its class. Absent: 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
}

impl ReadsRequest {
    /// Checks 1 to 8 valid contexts, a suffix, and 2 or more distinct non-empty letters.
    pub fn validate(&self) -> Result<(), Invalid> {
        if self.contexts.is_empty() || self.contexts.len() > MAX_CONTEXTS {
            return Err(Invalid::param("contexts", "reads name 1 to 8 contexts"));
        }
        for (i, c) in self.contexts.iter().enumerate() {
            c.check_read(i, "raw reads read")?;
        }
        if self.suffixes.is_empty() {
            return Err(Invalid::param("suffixes", "reads need a suffix"));
        }
        if let Some(i) = self.suffixes.iter().position(|s| s.is_empty()) {
            return Err(Invalid::param("suffixes", format!("suffix {} is empty", i)));
        }
        let l = &self.letters;
        if l.len() < 2 || l.iter().enumerate().any(|(i, x)| x.is_empty() || l[..i].contains(x)) {
            return Err(Invalid::param("letters", "reads need 2 or more distinct, non-empty letters"));
        }
        Ok(())
    }
}

/// One of the most likely tokens after a suffix.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TopToken {
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u32>,
    pub logprob: f64,
}

/// One context's read of one suffix.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RawRead {
    /// The index of the context read, in `contexts`.
    pub context: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotId>,
    /// The index of the suffix read.
    pub suffix: u64,
    /// Each letter's full-vocabulary log probability, in the request's order.
    pub logprobs: IndexMap<String, f64>,
    /// `log Σ exp(logprobs)`.
    pub mass: f64,
    pub top: Vec<TopToken>,
}

/// The response to `POST /council/v2/reads`: one read per context and suffix, context-major.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReadsResponse {
    /// The served model's `model`, never an alias.
    pub model: String,
    pub reads: Vec<RawRead>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signals: Option<Signals>,
    /// Summed over the reads: `prompt` is each context and suffix as read. `completion` is 0.
    pub usage: Usage,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Message;
    use serde_json::json;

    fn req() -> ReadsRequest {
        ReadsRequest {
            model: "m".into(),
            contexts: vec![Context { messages: vec![Message::user("u")], ..Default::default() }],
            suffixes: vec!["x".into()],
            letters: vec!["A".into(), "B".into()],
            top: None,
            class: None,
            priority: None,
        }
    }

    #[test]
    fn the_request_rules_each_refuse() {
        req().validate().unwrap();
        let cases: Vec<(&str, ReadsRequest)> = vec![
            ("contexts", ReadsRequest { contexts: vec![], ..req() }),
            ("contexts", ReadsRequest { contexts: vec![req().contexts[0].clone(); 9], ..req() }),
            ("contexts", ReadsRequest { contexts: vec![Context::default()], ..req() }),
            ("suffixes", ReadsRequest { suffixes: vec![], ..req() }),
            ("suffixes", ReadsRequest { suffixes: vec![String::new()], ..req() }),
            ("letters", ReadsRequest { letters: vec!["A".into()], ..req() }),
            ("letters", ReadsRequest { letters: vec!["A".into(), "A".into()], ..req() }),
            ("letters", ReadsRequest { letters: vec!["A".into(), String::new()], ..req() }),
        ];
        for (param, r) in cases {
            assert_eq!(r.validate().unwrap_err().param.as_deref(), Some(param), "{r:?}");
        }
        ReadsRequest { contexts: vec![req().contexts[0].clone(); 8], ..req() }.validate().unwrap();
    }

    #[test]
    fn reads_never_read_an_open_message() {
        let open = Context { messages: vec![Message::user("u"), Message::assistant("The plan:").opened()], ..Default::default() };
        let e = ReadsRequest { contexts: vec![req().contexts[0].clone(), open], ..req() }.validate().unwrap_err();
        assert_eq!((e.param.as_deref(), e.message.as_str()), (Some("continue"), "contexts[1]: raw reads read no open message; send continue: true only to a generation"));
        let broken = Context { messages: vec![Message::user("u").opened()], ..Default::default() };
        assert_eq!(ReadsRequest { contexts: vec![broken], ..req() }.validate().unwrap_err().param.as_deref(), Some("continue"));
    }

    #[test]
    fn a_response_keeps_letter_order_and_the_token_ids() {
        let r: ReadsResponse = serde_json::from_value(json!({"model": "m", "reads": [{"context": 0, "snapshot": format!("blake3:{}", "0".repeat(64)), "suffix": 1,
            "logprobs": {"B": -1.0, "A": -2.0}, "mass": -0.7, "top": [{"token": "A", "id": 65, "logprob": -2.0}]}], "signals": {"control_text": []},
            "usage": {"prompt": 20, "kept": 18, "fed": 2, "completion": 0, "reasoning": 0}})).unwrap();
        assert_eq!(r.reads[0].logprobs.keys().collect::<Vec<_>>(), ["B", "A"]);
        assert_eq!(r.reads[0].top[0].id, Some(65));
    }
}
