//! A context's cycles: the count of cycles a model built to cycle runs for each token.
//!
//! A cycle is one walk through the span of layers `start..end`. Each cycle after the first reads a
//! message that the cycle before it wrote. A served model with `cycles` lists the counts it runs in
//! its [`Cycles`] member. A context with no `cycles` runs the model's `default`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Invalid;
use crate::identity::{Capability, ServedModel};

/// What a served model with `cycles` runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cycles {
    /// The fewest cycles the server runs, at least 1.
    pub min: u32,
    /// The most cycles the server runs.
    pub max: u32,
    /// The count of a context with no `cycles`. The weights fix it.
    pub default: u32,
    /// The first layer of the span.
    pub start: u32,
    /// The layer after the last layer of the span.
    pub end: u32,
}

impl Cycles {
    /// Whether the member is sane: `1 <= min <= default <= max`, and a span that is not empty.
    pub fn is_sane(&self) -> bool {
        1 <= self.min && self.min <= self.default && self.default <= self.max && self.start < self.end
    }

    /// The count that runs for a context's `cycles`: the member, or `default` when it is absent.
    pub fn resolve(&self, cycles: Option<u32>) -> u32 {
        cycles.unwrap_or(self.default)
    }

    /// The runs of a layer that a token at count `count` holds beyond a token at `default`:
    /// `(count - default) × (end - start)`. It is negative below `default`.
    pub fn extra_runs(&self, count: u32) -> i64 {
        (i64::from(count) - i64::from(self.default)) * i64::from(self.end.saturating_sub(self.start))
    }
}

/// Checks a context's `cycles`, `count`, against served model `model`, in the contract's order:
/// the capability, then the range. An error names `cycles`. The first check of the contract, that
/// `cycles` is a whole number from 0, is the decoder's.
pub fn check(count: u32, model: &ServedModel) -> Result<(), Invalid> {
    let range = match (&model.cycles, model.has(Capability::Cycles)) {
        (Some(range), true) => range,
        _ => return Err(Invalid::param("cycles", format!("nothing ran: {} does not declare cycles; send no cycles", model.model))),
    };
    if count < range.min || count > range.max {
        return Err(Invalid::param("cycles", format!("nothing ran: cycles is {}; {} runs {} to {} cycles", count, model.model, range.min, range.max)));
    }
    Ok(())
}

/// Decodes a context's `cycles`: a whole number from 0. An error names `cycles`, as `invalid field`
/// does for `thinking`. A `null` is an error here: `cycles: null` has a meaning only beside
/// `parent`, where `resolve` reads it before the context is decoded.
pub(crate) fn cycles_member<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Option<u32>, D::Error> {
    let v = Value::deserialize(de)?;
    match v.as_u64().and_then(|n| u32::try_from(n).ok()) {
        Some(n) => Ok(Some(n)),
        None => Err(serde::de::Error::custom(format!("invalid field `cycles`: cycles is a whole number from 0, not {}", v))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Context;
    use crate::error::decode_param;
    use serde_json::json;

    /// Decodes a context with `cycles` set to `member`. An error is the member it names, and its text.
    fn context(member: Value) -> Result<Context, (Option<String>, String)> {
        serde_json::from_value::<Context>(json!({"thinking": false, "cycles": member, "messages": [{"role": "user", "content": "u"}]})).map_err(|e| (decode_param(&e.to_string()), e.to_string()))
    }

    /// A served model with 1 to 5 cycles of layers 6 to 17, three by default, as Inkling 2 Mini has them.
    fn served(declares: bool) -> ServedModel {
        let mut m: ServedModel = serde_json::from_value(json!({
            "model": "m", "status": "ready", "weight_hash": "w", "tokenizer_hash": "t", "template": "p", "engine": "e",
            "limits": {"context_tokens": 4096, "decision_context_tokens": 2048, "snapshot_bytes": 1, "contexts_per_decision": 8, "default_timeout_ms": 1000},
            "capabilities": ["generate"],
            "cycles": {"min": 1, "max": 5, "default": 3, "start": 6, "end": 18}}))
        .unwrap();
        if declares {
            m.capabilities.push(Capability::Cycles);
        }
        m
    }

    #[test]
    fn cycles_decode_and_are_left_out_when_absent() {
        let c = context(json!(5)).unwrap();
        assert_eq!(c.cycles, Some(5));
        assert_eq!(serde_json::to_value(&c).unwrap()["cycles"], json!(5));
        let zero = context(json!(0)).unwrap();
        assert_eq!(zero.cycles, Some(0), "0 decodes; the range check refuses it");
        let plain: Context = serde_json::from_value(json!({"thinking": false, "messages": [{"role": "user", "content": "u"}]})).unwrap();
        assert!(plain.cycles.is_none() && serde_json::to_value(&plain).unwrap().get("cycles").is_none());
    }

    #[test]
    fn bad_cycles_name_their_member() {
        for member in [json!(-1), json!(2.5), json!("3"), json!(null), json!({"count": 3}), json!([3]), json!(4294967296u64)] {
            let (got, text) = context(member.clone()).unwrap_err();
            assert_eq!(got.as_deref(), Some("cycles"), "{member}: {text}");
        }
    }

    #[test]
    fn a_check_names_the_first_rule_that_fails() {
        let m = served(true);
        let at = |n| check(n, &m).map_err(|e| e.param.unwrap());
        assert_eq!(at(1), Ok(()), "min");
        assert_eq!(at(3), Ok(()), "default");
        assert_eq!(at(5), Ok(()), "max");
        assert_eq!(at(0).unwrap_err(), "cycles", "0 is below min");
        assert_eq!(at(6).unwrap_err(), "cycles", "above max");
        let range = check(6, &m).unwrap_err().message;
        assert!(range.contains("1 to 5"), "{range}");
        let e = check(3, &served(false)).unwrap_err();
        assert_eq!(e.param.as_deref(), Some("cycles"));
        assert_eq!(e.message, "nothing ran: m does not declare cycles; send no cycles");
        let mut no_member = served(true);
        no_member.cycles = None;
        assert!(check(3, &no_member).unwrap_err().message.contains("does not declare cycles"), "a capability with no member");
        let mut no_cap = served(false);
        no_cap.capabilities.retain(|c| *c != Capability::Cycles);
        assert!(check(0, &no_cap).unwrap_err().message.contains("does not declare cycles"), "the capability comes before the range");
    }

    #[test]
    fn a_context_with_a_loop_and_cycles_is_refused_after_each_member_passes() {
        let mut m = served(true);
        m.capabilities.push(Capability::LayerLoops);
        m.layer_loops = Some(crate::layer_loop::LayerLoops { layers: 24, bounds: vec![2, 6, 10], count: 4, extra_runs: 16 });
        let body = |extra: Value| {
            let mut c = json!({"thinking": false, "messages": [{"role": "user", "content": "u"}]});
            for (k, v) in extra.as_object().unwrap() {
                c[k] = v.clone();
            }
            serde_json::from_value::<Context>(c).unwrap()
        };
        let both = body(json!({"loop": {"start": 2, "end": 6, "count": 2}, "cycles": 3}));
        both.check_loop(&m).unwrap();
        both.check_cycles(&m).unwrap();
        let e = both.check_members().unwrap_err();
        assert_eq!(e.param.as_deref(), Some("cycles"));
        assert!(e.message.contains("loop") && e.message.contains("cycles"), "{}", e.message);
        // Each member's own check fails before the check of the two together.
        let bad_loop = body(json!({"loop": {"start": 3, "end": 6, "count": 2}, "cycles": 3}));
        assert_eq!(bad_loop.check_loop(&m).unwrap_err().param.as_deref(), Some("loop.start"));
        let bad_cycles = body(json!({"loop": {"start": 2, "end": 6, "count": 2}, "cycles": 9}));
        assert_eq!(bad_cycles.check_cycles(&m).unwrap_err().param.as_deref(), Some("cycles"));
        assert_eq!(bad_cycles.check_members().unwrap_err().param.as_deref(), Some("cycles"));
        // One member alone passes.
        body(json!({"cycles": 3})).check_members().unwrap();
        body(json!({"loop": {"start": 2, "end": 6, "count": 2}})).check_members().unwrap();
        body(json!({})).check_cycles(&m).unwrap();
    }

    #[test]
    fn cycles_beside_parent_replace_the_parents_and_null_drops_them() {
        use crate::record::{context_id, resolve};
        let parent = context(json!(5)).unwrap();
        let id = context_id(&parent);
        let lookup = |want: &crate::ids::ContextId| (*want == id).then(|| parent.clone());
        let read = |sent: Value| {
            let mut sent = sent;
            resolve(&mut sent, "context", false, &lookup).unwrap();
            serde_json::from_value::<Context>(sent).unwrap().cycles
        };
        assert_eq!(read(json!({"parent": id.as_str()})), Some(5));
        assert_eq!(read(json!({"parent": id.as_str(), "cycles": 2})), Some(2));
        assert_eq!(read(json!({"parent": id.as_str(), "cycles": null})), None);
        // The whole context and the context by parent have one id, with the count in its record.
        let mut sent = json!({"parent": id.as_str()});
        resolve(&mut sent, "context", false, &lookup).unwrap();
        assert_eq!(context_id(&serde_json::from_value(sent).unwrap()), id);
        let plain = Context { cycles: None, ..parent.clone() };
        assert_ne!(context_id(&plain), id, "the count is part of the record");
        // A record with a count and the same context with the model's default as a count are two records.
        let default = Context { cycles: Some(3), ..parent.clone() };
        assert_ne!(context_id(&default), context_id(&plain));
        // The member stands in the record text between `loop` and `messages`.
        let text = crate::record::record_text(&parent);
        assert!(text.contains(r#""cycles":5,"messages""#), "{text}");
    }

    #[test]
    fn a_context_with_no_cycles_keeps_its_record_text() {
        let plain: Context = serde_json::from_value(json!({"thinking": false, "messages": [{"role": "user", "content": "u"}]})).unwrap();
        assert_eq!(crate::record::record_text(&plain), r#"{"thinking":false,"messages":[{"role":"user","content":"u"}]}"#);
    }

    #[test]
    fn a_control_vector_request_reads_no_cycles() {
        use crate::control::ControlVectorRequest;
        let c = json!({"thinking": false, "messages": [{"role": "user", "content": "u"}]});
        let mut counted = c.clone();
        counted["cycles"] = json!(3);
        let req: ControlVectorRequest = serde_json::from_value(json!({"model": "m", "positive": [counted], "negative": [c]})).unwrap();
        assert_eq!(req.validate().unwrap_err().param.as_deref(), Some("positive[0].cycles"), "a count equal to the default too");
        let req: ControlVectorRequest = serde_json::from_value(json!({"model": "m", "positive": [c], "negative": [c, counted]})).unwrap();
        assert_eq!(req.validate().unwrap_err().param.as_deref(), Some("negative[1].cycles"));
        let mut bad = c.clone();
        bad["cycles"] = json!(-2);
        let e = serde_json::from_value::<ControlVectorRequest>(json!({"model": "m", "positive": [bad], "negative": [c]})).unwrap_err();
        assert_eq!(decode_param(&e.to_string()).as_deref(), Some("positive[0].cycles"), "{e}");
    }

    #[test]
    fn a_cycles_error_names_its_context() {
        assert_eq!(crate::replies::param_in("cycles", 1), "contexts[1].cycles");
        let c = json!({"thinking": false, "messages": [{"role": "user", "content": "u"}]});
        let mut bad = c.clone();
        bad["cycles"] = json!(2.5);
        let e = serde_json::from_value::<crate::reads::ReadsRequest>(json!({"model": "m", "contexts": [c, bad], "suffixes": ["x"], "letters": ["A", "B"]})).unwrap_err();
        assert_eq!(decode_param(&e.to_string()).as_deref(), Some("contexts[1].cycles"), "{e}");
    }

    #[test]
    fn the_member_helpers_count_runs() {
        let c = Cycles { min: 1, max: 5, default: 3, start: 6, end: 18 };
        assert!(c.is_sane());
        assert_eq!((c.resolve(None), c.resolve(Some(5))), (3, 5));
        assert_eq!((c.extra_runs(3), c.extra_runs(5), c.extra_runs(1)), (0, 24, -24));
        assert!(Cycles { min: 3, ..c }.is_sane(), "min equal to default");
        assert!(!Cycles { min: 4, ..c }.is_sane());
        assert!(!Cycles { default: 6, ..c }.is_sane());
        assert!(!Cycles { end: 6, ..c }.is_sane());
        assert!(!Cycles { min: 0, ..c }.is_sane());
    }
}
