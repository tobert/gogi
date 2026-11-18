//! A context's layer loop: a span of a served model's layers that runs more than once.
//!
//! The model runs layers `0..start` once, then `start..end` `count` times, then `end..` once.
//! Each run of a layer has model state of its own. A served model with `layer_loops` lists the
//! spans it runs in its [`LayerLoops`] member.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Invalid;
use crate::identity::{Capability, ServedModel};

/// A context's `loop`: layers `start` to `end - 1` run `count` times.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct Loop {
    pub start: u32,
    pub end: u32,
    pub count: u32,
}

/// What a served model with `layer_loops` runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerLoops {
    /// The count of layers of the model.
    pub layers: u32,
    /// The layer numbers where a span may start or end, in ascending order.
    pub bounds: Vec<u32>,
    /// The most runs of a span, at least 2.
    pub count: u32,
    /// The most runs a loop adds to each token position.
    pub extra_runs: u64,
}

impl Loop {
    /// The runs a loop adds to each token position: `(end - start) × (count - 1)`.
    pub fn extra_runs(&self) -> u64 {
        u64::from(self.end.saturating_sub(self.start)) * u64::from(self.count.saturating_sub(1))
    }

    /// The layer of each run, in the order the model runs them, for a model of `layers` layers. A
    /// loop that [`Loop::check`] passed has `end` at most `layers`.
    pub fn runs(&self, layers: u32) -> Vec<u32> {
        let mut out: Vec<u32> = (0..self.start).collect();
        for _ in 0..self.count {
            out.extend(self.start..self.end);
        }
        out.extend(self.end..layers);
        out
    }

    /// Checks the loop against served model `model`, in the contract's order: the capability, then
    /// `start`, `end`, `count`, and the extra runs. An error names `loop` or its member.
    pub fn check(&self, model: &ServedModel) -> Result<(), Invalid> {
        let spans = match (&model.layer_loops, model.has(Capability::LayerLoops)) {
            (Some(spans), true) => spans,
            _ => return Err(Invalid::param("loop", format!("nothing ran: {} does not declare layer_loops; send no loop", model.model))),
        };
        if !spans.bounds.contains(&self.start) {
            return Err(Invalid::param("loop.start", format!("nothing ran: loop.start is {}; {} starts a span at one of {:?}", self.start, model.model, spans.bounds)));
        }
        if self.end <= self.start || !spans.bounds.contains(&self.end) {
            return Err(Invalid::param("loop.end", format!("nothing ran: loop.end is {}; {} ends a span at one of {:?}, above loop.start {}", self.end, model.model, spans.bounds, self.start)));
        }
        if self.count == 0 || self.count > spans.count {
            return Err(Invalid::param("loop.count", format!("nothing ran: loop.count is {}; {} runs a span 1 to {} times", self.count, model.model, spans.count)));
        }
        if self.extra_runs() > spans.extra_runs {
            return Err(Invalid::param(
                "loop",
                format!("nothing ran: the loop adds {} runs of a layer to each token, and {} adds at most {}; send a shorter span or a lower count", self.extra_runs(), model.model, spans.extra_runs),
            ));
        }
        Ok(())
    }
}

/// Decodes a context's `loop`: `{start, end, count}`, each a whole number from 0. An error names
/// the member at fault, such as `loop.count`, as `invalid field` does for `thinking`.
pub(crate) fn loop_member<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Option<Loop>, D::Error> {
    let v = Value::deserialize(de)?;
    decode(&v).map(Some).map_err(|(param, message)| serde::de::Error::custom(format!("invalid field `{}`: {}", param, message)))
}

fn decode(v: &Value) -> Result<Loop, (String, String)> {
    let Value::Object(m) = v else {
        return Err(("loop".into(), format!("loop is an object {{start, end, count}}, not {}", v)));
    };
    if let Some(k) = m.keys().find(|k| !["start", "end", "count"].contains(&k.as_str())) {
        return Err((format!("loop.{}", k), format!("loop has the unknown member {}; a loop has start, end, and count only", k)));
    }
    let whole = |name: &str| -> Result<u32, (String, String)> {
        match m.get(name).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()) {
            Some(n) => Ok(n),
            None => Err((format!("loop.{}", name), format!("loop.{} is a whole number from 0, not {}", name, m.get(name).unwrap_or(&Value::Null)))),
        }
    };
    Ok(Loop { start: whole("start")?, end: whole("end")?, count: whole("count")? })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Context;
    use crate::error::decode_param;
    use serde_json::json;

    /// Decodes a context with `loop` set to `member`. An error is the member it names, and its text.
    fn context(member: Value) -> Result<Context, (Option<String>, String)> {
        serde_json::from_value::<Context>(json!({"thinking": false, "loop": member, "messages": [{"role": "user", "content": "u"}]})).map_err(|e| (decode_param(&e.to_string()), e.to_string()))
    }

    /// A served model with 24 layers in groups of four from layer 2, as LFM3 has them.
    fn served(declares: bool) -> ServedModel {
        let mut m: ServedModel = serde_json::from_value(json!({
            "model": "m", "status": "ready", "weight_hash": "w", "tokenizer_hash": "t", "template": "p", "engine": "e",
            "limits": {"context_tokens": 4096, "decision_context_tokens": 2048, "snapshot_bytes": 1, "contexts_per_decision": 8, "default_timeout_ms": 1000},
            "capabilities": ["generate"],
            "layer_loops": {"layers": 24, "bounds": [2, 6, 10, 14, 18, 22], "count": 4, "extra_runs": 16}}))
        .unwrap();
        if declares {
            m.capabilities.push(Capability::LayerLoops);
        }
        m
    }

    #[test]
    fn a_loop_decodes_and_is_left_out_when_absent() {
        let c = context(json!({"start": 10, "end": 14, "count": 2})).unwrap();
        assert_eq!(c.layer_loop, Some(Loop { start: 10, end: 14, count: 2 }));
        assert_eq!(serde_json::to_value(&c).unwrap()["loop"], json!({"start": 10, "end": 14, "count": 2}));
        let plain: Context = serde_json::from_value(json!({"thinking": false, "messages": [{"role": "user", "content": "u"}]})).unwrap();
        assert!(plain.layer_loop.is_none() && serde_json::to_value(&plain).unwrap().get("loop").is_none());
    }

    #[test]
    fn a_bad_loop_names_its_member() {
        for (member, param) in [
            (json!([10, 14, 2]), "loop"),
            (json!(null), "loop"),
            (json!({"start": 10, "end": 14}), "loop.count"),
            (json!({"start": -1, "end": 14, "count": 2}), "loop.start"),
            (json!({"start": 10, "end": 14.5, "count": 2}), "loop.end"),
            (json!({"start": 10, "end": 14, "count": "2"}), "loop.count"),
            (json!({"start": 10, "end": 14, "count": 2, "mode": "layer"}), "loop.mode"),
        ] {
            let (got, text) = context(member.clone()).unwrap_err();
            assert_eq!(got.as_deref(), Some(param), "{member}: {text}");
        }
    }

    #[test]
    fn a_check_names_the_first_rule_that_fails() {
        let m = served(true);
        let at = |start, end, count| Loop { start, end, count }.check(&m).map_err(|e| e.param.unwrap());
        assert_eq!(at(10, 14, 2), Ok(()));
        assert_eq!(at(2, 22, 1), Ok(()), "count 1 runs each layer once");
        assert_eq!(at(6, 14, 3), Ok(()), "16 extra runs is the limit");
        assert_eq!(at(11, 14, 2).unwrap_err(), "loop.start");
        assert_eq!(at(10, 13, 2).unwrap_err(), "loop.end");
        assert_eq!(at(10, 10, 2).unwrap_err(), "loop.end", "an empty span");
        assert_eq!(at(14, 10, 2).unwrap_err(), "loop.end", "a span that runs back");
        assert_eq!(at(10, 14, 0).unwrap_err(), "loop.count");
        assert_eq!(at(10, 14, 5).unwrap_err(), "loop.count");
        assert_eq!(at(2, 22, 2).unwrap_err(), "loop", "20 extra runs pass 16");
        assert_eq!(at(11, 13, 9).unwrap_err(), "loop.start", "start comes before count");
        let e = Loop { start: 10, end: 14, count: 2 }.check(&served(false)).unwrap_err();
        assert_eq!(e.param.as_deref(), Some("loop"));
        assert!(e.message.contains("does not declare layer_loops"), "{}", e.message);
    }

    #[test]
    fn a_loop_beside_parent_replaces_the_parents_and_null_drops_it() {
        use crate::record::{context_id, resolve};
        let parent = context(json!({"start": 10, "end": 14, "count": 2})).unwrap();
        let id = context_id(&parent);
        let lookup = |want: &crate::ids::ContextId| (*want == id).then(|| parent.clone());
        let read = |sent: Value| {
            let mut sent = sent;
            resolve(&mut sent, "context", false, &lookup).unwrap();
            serde_json::from_value::<Context>(sent).unwrap().layer_loop
        };
        assert_eq!(read(json!({"parent": id.as_str()})), Some(Loop { start: 10, end: 14, count: 2 }));
        assert_eq!(read(json!({"parent": id.as_str(), "loop": {"start": 6, "end": 10, "count": 3}})), Some(Loop { start: 6, end: 10, count: 3 }));
        assert_eq!(read(json!({"parent": id.as_str(), "loop": null})), None);
        // The whole context and the context by parent have one id, with the loop in its record.
        let mut sent = json!({"parent": id.as_str()});
        resolve(&mut sent, "context", false, &lookup).unwrap();
        assert_eq!(context_id(&serde_json::from_value(sent).unwrap()), id);
        let plain = Context { layer_loop: None, ..parent.clone() };
        assert_ne!(context_id(&plain), id, "the loop is part of the record");
    }

    #[test]
    fn a_control_vector_request_reads_no_loop() {
        use crate::control::ControlVectorRequest;
        let c = json!({"thinking": false, "messages": [{"role": "user", "content": "u"}]});
        let mut looped = c.clone();
        looped["loop"] = json!({"start": 10, "end": 14, "count": 2});
        let req: ControlVectorRequest = serde_json::from_value(json!({"model": "m", "positive": [c], "negative": [c, looped]})).unwrap();
        assert_eq!(req.validate().unwrap_err().param.as_deref(), Some("negative[1].loop"));
        let mut bad = c.clone();
        bad["loop"] = json!({"start": 10, "end": 14});
        let e = serde_json::from_value::<ControlVectorRequest>(json!({"model": "m", "positive": [bad], "negative": [c]})).unwrap_err();
        assert_eq!(decode_param(&e.to_string()).as_deref(), Some("positive[0].loop.count"), "{e}");
    }

    #[test]
    fn a_loop_error_of_a_reply_names_its_context() {
        assert_eq!(crate::replies::param_in("loop.count", 2), "contexts[2].loop.count");
        assert_eq!(crate::replies::param_in("loop", 0), "contexts[0].loop");
    }

    #[test]
    fn the_runs_repeat_the_span_in_order() {
        let l = Loop { start: 1, end: 3, count: 3 };
        assert_eq!(l.runs(5), vec![0, 1, 2, 1, 2, 1, 2, 3, 4]);
        assert_eq!(l.extra_runs(), 4);
        assert_eq!(Loop { start: 1, end: 3, count: 1 }.runs(5), vec![0, 1, 2, 3, 4]);
    }
}
