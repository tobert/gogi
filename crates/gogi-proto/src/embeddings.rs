//! `POST /council/v2/embeddings`: a vector for each input, for comparing inputs by meaning.

use serde::{Deserialize, Serialize};

use crate::context::Usage;
use crate::decision::DecisionIdentity;
use crate::error::Invalid;
use crate::identity::{Capability, Class, ServedModel};
use crate::message::{Content, Part};
use crate::objects::{Named, ObjectRef};

/// What a served model embeds: the member `embeddings` of a served model that declares `embed`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Embeddings {
    /// The task names a request may send, such as `query` and `document`.
    pub tasks: Vec<String>,
    /// The vector lengths it offers, largest first.
    pub dimensions: Vec<u32>,
    /// The most inputs one request may send, at least 1.
    pub inputs_per_request: u32,
}

/// The body of `POST /council/v2/embeddings`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingsRequest {
    /// The served model: its `model` or an alias.
    pub model: String,
    /// One of the served model's `embeddings.tasks`. The server renders its prompt.
    pub task: String,
    /// Each input is content, as a message's content is.
    pub inputs: Vec<Content>,
    /// One of the served model's `embeddings.dimensions`. Absent: the largest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<u32>,
    /// Counts the tokens of each input and embeds nothing.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub dry_run: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<Class>,
    /// Higher runs first within its class. Absent: 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
}

impl EmbeddingsRequest {
    /// Checks the request against the served model it names: `embed`, the task, the count of
    /// inputs, each input not empty with whole object ids, and the dimensions. Returns the
    /// dimensions to embed with. The server then checks the objects with
    /// `objects::check_reads(model, &self.objects())`.
    pub fn check(&self, model: &ServedModel) -> Result<u32, Invalid> {
        let e = match &model.embeddings {
            Some(e) if model.has(Capability::Embed) => e,
            _ => return Err(Invalid::param("model", format!("nothing ran: model {} does not declare embed; send embeddings to a served model whose capabilities list embed", model.model))),
        };
        if !e.tasks.contains(&self.task) {
            return Err(Invalid::param("task", format!("nothing ran: model {} has no task {:?}; its tasks are {}", model.model, self.task, e.tasks.join(", "))));
        }
        if self.inputs.is_empty() || self.inputs.len() > e.inputs_per_request as usize {
            return Err(Invalid::param("inputs", format!("nothing ran: the request has {} inputs; model {} takes 1 to {}", self.inputs.len(), model.model, e.inputs_per_request)));
        }
        for (i, input) in self.inputs.iter().enumerate() {
            let empty = match input {
                Content::Text(s) => s.is_empty(),
                Content::Parts(p) => p.is_empty(),
            };
            if empty {
                return Err(Invalid::param(format!("inputs[{}]", i), format!("nothing ran: inputs[{}] is empty; send text or one part or more, or leave the input out", i)));
            }
        }
        for n in self.objects() {
            n.object.check(&n.at)?;
        }
        let largest = e.dimensions.iter().copied().max();
        match (self.dimensions, largest) {
            (_, None) => Err(Invalid::param("model", format!("nothing ran: model {} lists no dimensions", model.model))),
            (None, Some(d)) => Ok(d),
            (Some(d), Some(_)) if e.dimensions.contains(&d) => Ok(d),
            (Some(d), Some(_)) => {
                let offered: Vec<String> = e.dimensions.iter().map(u32::to_string).collect();
                Err(Invalid::param("dimensions", format!("nothing ran: model {} has no {} dimensions; it offers {}", model.model, d, offered.join(", "))))
            }
        }
    }

    /// Each object the inputs name, as `inputs[i][j]`.
    pub fn objects(&self) -> Vec<Named> {
        let mut out = Vec::new();
        for (i, input) in self.inputs.iter().enumerate() {
            for (j, p) in input.parts().into_iter().enumerate() {
                if let Part::Object { id, mime } = p {
                    out.push(Named { at: format!("inputs[{}][{}]", i, j), object: ObjectRef { id, mime }, assistant: false });
                }
            }
        }
        out
    }
}

/// One input's embedding.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Embedding {
    /// The index of the input, in `inputs`.
    pub index: u64,
    /// The input's tokens as the served model reads it, with its task prompt and special tokens.
    pub tokens: u64,
    /// `dimensions` float32 values with an L2 norm of 1. A dry run has none.
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "short_floats")]
    pub vector: Option<Vec<f32>>,
}

/// Writes each float32 as the shortest decimal that reads back as the same float32, also through
/// a `serde_json::Value`, which holds a float64. Without this, 0.6f32 is written as
/// 0.6000000238418579.
fn short_floats<S: serde::Serializer>(v: &Option<Vec<f32>>, s: S) -> Result<S::Ok, S::Error> {
    let wide: Option<Vec<f64>> = v.as_ref().map(|v| v.iter().map(|x| x.to_string().parse::<f64>().expect("a float32 prints as a decimal")).collect());
    wide.serialize(s)
}

/// The response to `POST /council/v2/embeddings`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingsResponse {
    /// The served model's `model`, never an alias.
    pub model: String,
    pub task: String,
    pub dimensions: u32,
    /// One for each input, in input order.
    pub embeddings: Vec<Embedding>,
    /// The identity fields the vectors depend on. A vector of one identity does not compare with
    /// a vector of another.
    pub identity: DecisionIdentity,
    /// `prompt` and `fed` count every input's tokens; the rest are 0. A dry run feeds nothing.
    pub usage: Usage,
}

/// The L2 norm of a vector, summed in float64.
pub fn norm(v: &[f32]) -> f64 {
    v.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>().sqrt()
}

/// The first `d` components of `v`, normalized again to an L2 norm of 1: what a request with
/// `dimensions: d` gets, from the whole vector.
pub fn cut(v: &[f32], d: usize) -> Vec<f32> {
    let head = &v[..d.min(v.len())];
    let n = norm(head);
    head.iter().map(|x| (f64::from(*x) / n) as f32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{Media, ModelLimits, ModelStatus};
    use serde_json::json;

    fn model() -> ServedModel {
        ServedModel {
            model: "embeddinggemma-2".into(),
            aliases: vec![],
            status: ModelStatus::Ready,
            weight_hash: "w".into(),
            tokenizer_hash: "t".into(),
            template: "p".into(),
            engine: "e".into(),
            device: None,
            text_stop: None,
            limits: ModelLimits { context_tokens: 2048, decision_context_tokens: 0, snapshot_bytes: 0, contexts_per_decision: 0, default_timeout_ms: 30000, text_tokens: None },
            capabilities: vec![Capability::Embed, Capability::DryRun],
            media: Some(Media { inputs: vec![], outputs: vec![], text_replies: false }),
            embeddings: Some(Embeddings { tasks: vec!["query".into(), "document".into()], dimensions: vec![768, 512, 256, 128], inputs_per_request: 4 }),
        }
    }

    fn req() -> EmbeddingsRequest {
        serde_json::from_value(json!({"model": "embeddinggemma-2", "task": "query", "inputs": ["a", "b"]})).unwrap()
    }

    #[test]
    fn the_dimensions_default_to_the_largest_and_take_an_offered_cut() {
        assert_eq!(req().check(&model()), Ok(768));
        assert_eq!(EmbeddingsRequest { dimensions: Some(256), ..req() }.check(&model()), Ok(256));
    }

    #[test]
    fn the_request_rules_each_refuse_naming_the_member() {
        let parts = |v: serde_json::Value| serde_json::from_value::<Content>(v).unwrap();
        let cases: Vec<(&str, EmbeddingsRequest)> = vec![
            ("task", EmbeddingsRequest { task: "rerank".into(), ..req() }),
            ("inputs", EmbeddingsRequest { inputs: vec![], ..req() }),
            ("inputs", EmbeddingsRequest { inputs: vec!["x".into(); 5], ..req() }),
            ("inputs[1]", EmbeddingsRequest { inputs: vec!["x".into(), "".into()], ..req() }),
            ("inputs[0]", EmbeddingsRequest { inputs: vec![parts(json!([]))], ..req() }),
            ("inputs[0][1]", EmbeddingsRequest { inputs: vec![parts(json!([{"type": "text", "text": "x"}, {"type": "object", "id": "blake3:12", "mime": "image/png"}]))], ..req() }),
            ("dimensions", EmbeddingsRequest { dimensions: Some(300), ..req() }),
        ];
        for (param, r) in cases {
            assert_eq!(r.check(&model()).unwrap_err().param.as_deref(), Some(param), "{r:?}");
        }
        EmbeddingsRequest { inputs: vec!["x".into(); 4], ..req() }.check(&model()).unwrap();
    }

    #[test]
    fn a_model_without_embed_refuses_naming_model() {
        let chat = ServedModel { capabilities: vec![Capability::Generate], embeddings: None, ..model() };
        assert_eq!(req().check(&chat).unwrap_err().param.as_deref(), Some("model"));
        let half = ServedModel { capabilities: vec![], ..model() };
        assert_eq!(req().check(&half).unwrap_err().param.as_deref(), Some("model"), "embeddings without the capability is not enough");
    }

    #[test]
    fn an_object_part_is_named_by_input_and_part() {
        let id = format!("blake3:{}", "a".repeat(64));
        let r: EmbeddingsRequest = serde_json::from_value(json!({"model": "m", "task": "query",
            "inputs": ["x", [{"type": "text", "text": "y"}, {"type": "object", "id": id, "mime": "image/png"}]]})).unwrap();
        let o = r.objects();
        assert_eq!((o.len(), o[0].at.as_str(), o[0].object.id.as_str()), (1, "inputs[1][1]", id.as_str()));
        assert_eq!(crate::objects::check_reads(&model(), &o).unwrap_err().param.as_deref(), Some("inputs[1][1]"), "a text-only model refuses the part");
    }

    #[test]
    fn an_unknown_member_is_refused() {
        let e = serde_json::from_value::<EmbeddingsRequest>(json!({"model": "m", "task": "query", "inputs": ["a"], "encoding_format": "float"})).unwrap_err();
        assert_eq!(crate::error::decode_param(&e.to_string()).as_deref(), Some("encoding_format"));
        let e = serde_json::from_value::<EmbeddingsRequest>(json!({"model": "m", "inputs": ["a"]})).unwrap_err();
        assert_eq!(crate::error::decode_param(&e.to_string()).as_deref(), Some("task"));
    }

    #[test]
    fn a_cut_is_the_prefix_normalized_again() {
        let v = [0.6f32, 0.0, 0.8, 0.0];
        assert!((norm(&v) - 1.0).abs() < 1e-7);
        assert_eq!(cut(&v, 2), vec![1.0, 0.0]);
        let c = cut(&[3.0, 4.0, 12.0], 2);
        assert!((c[0] - 0.6).abs() < 1e-7 && (c[1] - 0.8).abs() < 1e-7, "{c:?}");
    }

    #[test]
    fn a_response_and_a_dry_run_round_trip() {
        let v = json!({"model": "embeddinggemma-2", "task": "query", "dimensions": 2,
            "embeddings": [{"index": 0, "tokens": 9, "vector": [0.6, 0.8]}, {"index": 1, "tokens": 7}],
            "identity": {"model": "embeddinggemma-2", "weight_hash": "w", "tokenizer_hash": "t", "template": "p", "engine": "e"},
            "usage": {"prompt": 16, "kept": 0, "fed": 16, "completion": 0, "reasoning": 0}});
        let r: EmbeddingsResponse = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(r.embeddings[0].vector.as_deref(), Some(&[0.6f32, 0.8][..]));
        assert!(r.embeddings[1].vector.is_none());
        assert_eq!(serde_json::to_value(&r).unwrap(), v);
        assert!(serde_json::to_string(&r).unwrap().contains("[0.6,0.8]"), "each float32 is its shortest decimal");
    }

    #[test]
    fn identity_lists_what_a_model_embeds() {
        let m = serde_json::to_value(model()).unwrap();
        assert_eq!(m["embeddings"], json!({"tasks": ["query", "document"], "dimensions": [768, 512, 256, 128], "inputs_per_request": 4}));
        assert_eq!(m["capabilities"], json!(["embed", "dry_run"]));
        let back: ServedModel = serde_json::from_value(m).unwrap();
        assert_eq!(back, model());
    }
}
