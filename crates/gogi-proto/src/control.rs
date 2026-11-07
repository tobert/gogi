//! Control vectors: a vector for each layer that changes a served model's residual while it runs,
//! the `control` list of a context that applies them, and `POST /council/v2/control-vectors`,
//! which makes one from contexts.
//!
//! A control vector is an object of media type [`MEDIA_TYPE`]: a safetensors file with one F32
//! tensor of shape `[width]` for each layer it changes, named `layers.<l>`, and `model` and
//! `weight_hash` in its `__metadata__`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::context::{Context, Usage};
use crate::decision::DecisionIdentity;
use crate::error::Invalid;
use crate::identity::{Capability, Class, ServedModel};
use crate::ids::{ContentId, content_id_problem};
use crate::required_nullable;

/// The media type of a control vector object.
pub const MEDIA_TYPE: &str = "application/x-safetensors";

/// What a control item does to a residual row `h` with the layer's vector `v`, its unit vector
/// `v̂`, and the strength `α`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlOp {
    /// `h + α·v`.
    Add,
    /// `h − α·(h·v̂)·v̂`: with `α` 1, the part of `h` along `v` goes.
    Ablate,
    /// `h + (α − h·v̂)·v̂`: the part of `h` along `v` becomes `α`, whatever it was.
    Clamp,
}

impl ControlOp {
    /// Applies the op to the residual row `h` with the layer's vector `v` and strength `a`, in
    /// float64. A vector of all zeros has no direction, so `ablate` and `clamp` leave `h` as it is.
    pub fn apply(self, h: &mut [f64], v: &[f32], a: f64) {
        assert_eq!(h.len(), v.len(), "a control vector's width is the residual's width; check the vector first");
        if self == ControlOp::Add {
            for (x, y) in h.iter_mut().zip(v) {
                *x += a * f64::from(*y);
            }
            return;
        }
        let norm = v.iter().map(|y| f64::from(*y) * f64::from(*y)).sum::<f64>().sqrt();
        if norm == 0.0 {
            return;
        }
        let unit: Vec<f64> = v.iter().map(|y| f64::from(*y) / norm).collect();
        let along: f64 = h.iter().zip(&unit).map(|(x, u)| x * u).sum();
        let by = match self {
            ControlOp::Ablate => -a * along,
            _ => a - along,
        };
        for (x, u) in h.iter_mut().zip(&unit) {
            *x += by * u;
        }
    }

    /// Whether the op changes nothing at strength `a`: `add` and `ablate` at 0. `clamp` at 0 sets
    /// the part along the vector to 0, so it always changes the residual.
    pub fn changes_nothing(self, a: f64) -> bool {
        a == 0.0 && self != ControlOp::Clamp
    }
}

/// One item of a context's `control`: a control vector by its content id, what it does, and how
/// strongly.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Control {
    pub vector: ContentId,
    pub op: ControlOp,
    /// A finite number. Its scale is the vector's: `add` at 1 adds the vector as it is.
    pub strength: f64,
}

impl Control {
    /// Checks a finite strength. `at` names the item, such as `control[1]`.
    pub fn check(&self, at: &str) -> Result<(), Invalid> {
        if !self.strength.is_finite() {
            return Err(Invalid::param(format!("{}.strength", at), format!("nothing ran: {}.strength is {}; send a finite number", at, self.strength)));
        }
        Ok(())
    }
}

/// Decodes a context's `control`: a list of `{vector, op, strength}`. An error names the member
/// at fault, such as `control[1].strength`, as `invalid field` does for `thinking`.
pub(crate) fn control_list<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Vec<Control>, D::Error> {
    let v = Value::deserialize(de)?;
    decode_list(&v).map_err(|(param, message)| serde::de::Error::custom(format!("invalid field `{}`: {}", param, message)))
}

fn decode_list(v: &Value) -> Result<Vec<Control>, (String, String)> {
    let Value::Array(items) = v else {
        return Err(("control".into(), format!("control is a list of {{vector, op, strength}}, not {}", v)));
    };
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let at = format!("control[{}]", i);
        let Value::Object(m) = item else {
            return Err((at.clone(), format!("{} is an object {{vector, op, strength}}, not {}", at, item)));
        };
        if let Some(k) = m.keys().find(|k| !["vector", "op", "strength"].contains(&k.as_str())) {
            return Err((format!("{}.{}", at, k), format!("{} has the unknown member {}; an item has vector, op, and strength only", at, k)));
        }
        let vector = match m.get("vector") {
            Some(Value::String(s)) => match content_id_problem(s) {
                None => ContentId::parse(s.clone()).expect("a content id parses"),
                Some(p) => return Err((format!("{}.vector", at), format!("{}.vector {}", at, p))),
            },
            other => return Err((format!("{}.vector", at), format!("{}.vector is the content id of a control vector, not {}", at, other.unwrap_or(&Value::Null)))),
        };
        let op = match m.get("op").and_then(Value::as_str) {
            Some("add") => ControlOp::Add,
            Some("ablate") => ControlOp::Ablate,
            Some("clamp") => ControlOp::Clamp,
            _ => return Err((format!("{}.op", at), format!("{}.op is add, ablate, or clamp, not {}", at, m.get("op").unwrap_or(&Value::Null)))),
        };
        let strength = match m.get("strength").and_then(Value::as_f64).filter(|a| a.is_finite()) {
            Some(a) => a,
            None => return Err((format!("{}.strength", at), format!("{}.strength is a finite number, not {}", at, m.get("strength").unwrap_or(&Value::Null)))),
        };
        out.push(Control { vector, op, strength });
    }
    Ok(out)
}

/// Decodes a request's list of contexts under `side`, such as `contexts`. An error that names a
/// member of a context's `control` names it under its context, such as
/// `contexts[0].control[1].strength`.
pub(crate) fn contexts_under<'de, D: serde::Deserializer<'de>>(de: D, side: &str) -> Result<Vec<Context>, D::Error> {
    let items = Vec::<Value>::deserialize(de)?;
    let mut out = Vec::with_capacity(items.len());
    for (k, v) in items.into_iter().enumerate() {
        match serde_json::from_value::<Context>(v) {
            Ok(c) => out.push(c),
            Err(e) => {
                let text = e.to_string().replacen("invalid field `control", &format!("invalid field `{}[{}].control", side, k), 1);
                return Err(serde::de::Error::custom(format!("{}[{}]: {}", side, k, text)));
            }
        }
    }
    Ok(out)
}

/// A control vector: the served model it was made for, its weights, and a vector for each layer
/// it changes, all of one width.
#[derive(Clone, Debug, PartialEq)]
pub struct ControlVector {
    /// The served model's `model` it was made for. The server checks `weight_hash`, not this.
    pub model: String,
    /// The `weight_hash` of the served model it was made for.
    pub weight_hash: String,
    /// The 0-based layer after which each vector applies, to the vector.
    pub layers: BTreeMap<u32, Vec<f32>>,
}

impl ControlVector {
    /// The width of its vectors: the residual's width.
    pub fn width(&self) -> usize {
        self.layers.values().next().map_or(0, Vec::len)
    }

    /// Reads a control vector from the bytes of its object. An error says what is wrong.
    pub fn parse(bytes: &[u8]) -> Result<ControlVector, String> {
        let Some(n) = bytes.get(..8).map(|b| u64::from_le_bytes(b.try_into().expect("8 bytes"))) else {
            return Err(format!("the object is {} bytes; a safetensors file starts with 8 bytes that give the header's length", bytes.len()));
        };
        let end = usize::try_from(n).ok().and_then(|n| n.checked_add(8)).filter(|e| *e <= bytes.len());
        let Some(end) = end else { return Err(format!("the header is {} bytes, past the end of the {} bytes of the object", n, bytes.len())) };
        let header: Map<String, Value> = serde_json::from_slice(&bytes[8..end]).map_err(|e| format!("the header is not a JSON object: {}", e))?;
        let data = &bytes[end..];
        let meta = header.get("__metadata__").and_then(Value::as_object).ok_or("the header has no __metadata__ object; it holds model and weight_hash")?;
        let text = |k: &str| meta.get(k).and_then(Value::as_str).map(String::from).ok_or_else(|| format!("__metadata__ has no {} string", k));
        let (model, weight_hash) = (text("model")?, text("weight_hash")?);
        let mut layers = BTreeMap::new();
        let mut spans = Vec::new();
        let mut width = None;
        for (name, t) in header.iter().filter(|(k, _)| *k != "__metadata__") {
            let l = name.strip_prefix("layers.").filter(|l| *l == "0" || (!l.starts_with('0') && l.bytes().all(|b| b.is_ascii_digit()))).and_then(|l| l.parse::<u32>().ok());
            let Some(l) = l else { return Err(format!("the tensor {:?} is not named layers.<l>, with <l> a layer number", name)) };
            if t["dtype"] != json!("F32") {
                return Err(format!("{} has dtype {}; a control vector is F32", name, t["dtype"]));
            }
            let w = match t["shape"].as_array().map(Vec::as_slice) {
                Some([w]) => w.as_u64().filter(|w| *w > 0).ok_or_else(|| format!("{} has the shape {}; it is [width], with width above 0", name, t["shape"]))? as usize,
                _ => return Err(format!("{} has the shape {}; it is [width]", name, t["shape"])),
            };
            if *width.get_or_insert(w) != w {
                return Err(format!("{} has the width {}, and another layer has the width {}; every layer has one width", name, w, width.unwrap_or(0)));
            }
            let span = match t["data_offsets"].as_array().map(Vec::as_slice) {
                Some([a, b]) => (a.as_u64(), b.as_u64()),
                _ => (None, None),
            };
            let (Some(a), Some(b)) = span else { return Err(format!("{} has the data_offsets {}; they are [start, end]", name, t["data_offsets"])) };
            if b < a || b - a != 4 * w as u64 {
                return Err(format!("{} has the data_offsets [{}, {}], which do not hold {} float32 values", name, a, b, w));
            }
            spans.push((a, b, l));
        }
        if spans.is_empty() {
            return Err("the file holds no tensor; a control vector holds layers.<l> for one layer or more".into());
        }
        spans.sort();
        let mut at = 0;
        for (a, b, l) in spans {
            if a != at {
                return Err(format!("the data of layers.{} starts at {}, not {}; the tensors cover the data with no gap or overlap", l, a, at));
            }
            at = b;
            let values: Vec<f32> = data.get(a as usize..b as usize).ok_or("the data ends before the tensors do")?.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
            if let Some(j) = values.iter().position(|x| !x.is_finite()) {
                return Err(format!("layers.{} has {} at {}; every value is finite", l, values[j], j));
            }
            layers.insert(l, values);
        }
        if at != data.len() as u64 {
            return Err(format!("the data is {} bytes, and the tensors hold {}", data.len(), at));
        }
        Ok(ControlVector { model, weight_hash, layers })
    }

    /// The bytes of its object: the header, padded with spaces to a multiple of 8 bytes, then
    /// each layer's float32 values, little-endian, in layer order. The same vector gives the same
    /// bytes, so the same object id.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut header = Map::new();
        header.insert("__metadata__".into(), json!({"model": self.model, "weight_hash": self.weight_hash}));
        let mut at = 0;
        for (l, v) in &self.layers {
            header.insert(format!("layers.{}", l), json!({"dtype": "F32", "shape": [v.len()], "data_offsets": [at, at + 4 * v.len()]}));
            at += 4 * v.len();
        }
        let mut text = Value::Object(header).to_string();
        while !text.len().is_multiple_of(8) {
            text.push(' ');
        }
        let mut out = (text.len() as u64).to_le_bytes().to_vec();
        out.extend_from_slice(text.as_bytes());
        for v in self.layers.values() {
            out.extend(v.iter().flat_map(|x| x.to_le_bytes()));
        }
        out
    }

    /// Checks that it fits a served model with weights `weight_hash`, `layers` layers, and a
    /// residual of `width`. An error says which does not fit.
    pub fn check_fits(&self, weight_hash: &str, layers: u32, width: usize) -> Result<(), String> {
        if self.weight_hash != weight_hash {
            return Err(format!("the vector was made for weight_hash {}, and the served model has weight_hash {}; make a vector for these weights", self.weight_hash, weight_hash));
        }
        if self.width() != width {
            return Err(format!("the vector has width {}, and the served model's residual has width {}", self.width(), width));
        }
        if let Some(l) = self.layers.keys().find(|l| **l >= layers) {
            return Err(format!("the vector has layers.{}, and the served model has {} layers", l, layers));
        }
        Ok(())
    }
}

/// The body of `POST /council/v2/control-vectors`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlVectorRequest {
    /// The served model: its `model` or an alias.
    pub model: String,
    /// Contexts that show the behavior the vector adds. At least one.
    #[serde(deserialize_with = "positive_list")]
    pub positive: Vec<Context>,
    /// Contexts that show the behavior the vector takes away. At least one.
    #[serde(deserialize_with = "negative_list")]
    pub negative: Vec<Context>,
    /// Renders and counts the contexts and makes no vector.
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub dry_run: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<Class>,
    /// Higher runs first within its class. Absent: 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
}

/// Decodes a request's `contexts`.
pub(crate) fn contexts<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Vec<Context>, D::Error> {
    contexts_under(de, "contexts")
}

fn positive_list<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Vec<Context>, D::Error> {
    contexts_under(de, "positive")
}

fn negative_list<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Vec<Context>, D::Error> {
    contexts_under(de, "negative")
}

impl ControlVectorRequest {
    /// Each context with the member that names it, such as `positive[0]`, positive first.
    pub fn contexts(&self) -> Vec<(String, &Context)> {
        let pos = self.positive.iter().enumerate().map(|(i, c)| (format!("positive[{}]", i), c));
        pos.chain(self.negative.iter().enumerate().map(|(i, c)| (format!("negative[{}]", i), c))).collect()
    }

    /// Checks the rules a client can check without the server: at least one context on each
    /// side, each valid and closed, and none with a `control` of its own.
    pub fn validate(&self) -> Result<(), Invalid> {
        for (side, list) in [("positive", &self.positive), ("negative", &self.negative)] {
            if list.is_empty() {
                return Err(Invalid::param(side, format!("nothing ran: {} holds no context; send one context or more on each side", side)));
            }
            for (i, c) in list.iter().enumerate() {
                c.check_in(side, i, "a control vector reads")?;
                if !c.control.is_empty() {
                    let at = format!("{}[{}].control", side, i);
                    return Err(Invalid::param(at.clone(), format!("nothing ran: {} is not empty; a control vector reads each context with the plain model, so send no control", at)));
                }
            }
        }
        Ok(())
    }

    /// Checks the request against the served model it names: `control_vectors`, `thinking` in
    /// each context, and `limits.control_vector_contexts` on each side.
    pub fn check(&self, model: &ServedModel) -> Result<(), Invalid> {
        if !model.has(Capability::ControlVectors) {
            return Err(Invalid::param("model", format!("nothing ran: model {} does not declare control_vectors; send control-vector requests to a served model whose capabilities list control_vectors", model.model)));
        }
        for (side, list) in [("positive", &self.positive), ("negative", &self.negative)] {
            if let Some(most) = model.limits.control_vector_contexts
                && list.len() as u64 > most
            {
                return Err(Invalid::param(side, format!("nothing ran: {} holds {} contexts; model {} takes at most {} on each side (control_vector_contexts)", side, list.len(), model.model, most)));
            }
            for (i, c) in list.iter().enumerate() {
                if let Err(e) = c.check_thinking(model) {
                    return Err(Invalid::param(format!("{}[{}].thinking", side, i), e.message));
                }
            }
        }
        Ok(())
    }
}

/// How many contexts made a control vector, on each side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlVectorContexts {
    pub positive: u64,
    pub negative: u64,
}

/// The response to `POST /council/v2/control-vectors`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ControlVectorResponse {
    /// The served model's `model`, never an alias.
    pub model: String,
    /// The object the server put, or `null` for a dry run.
    #[serde(deserialize_with = "required_nullable")]
    pub vector: Option<ContentId>,
    /// The number of layers the vector has a tensor for.
    pub layers: u32,
    /// The residual's width: the length of each layer's vector.
    pub width: u32,
    pub contexts: ControlVectorContexts,
    /// The identity fields the vector depends on.
    pub identity: DecisionIdentity,
    pub usage: Usage,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Message;

    fn id(c: char) -> String {
        format!("blake3:{}", c.to_string().repeat(64))
    }

    fn vector() -> ControlVector {
        ControlVector { model: "m".into(), weight_hash: "0x01".into(), layers: [(0, vec![1.0, 0.0]), (2, vec![0.5, -0.25])].into_iter().collect() }
    }

    #[test]
    fn add_ablate_and_clamp_follow_their_formulas() {
        let v = [3.0f32, 4.0];
        let mut h = [1.0, 2.0];
        ControlOp::Add.apply(&mut h, &v, 2.0);
        assert_eq!(h, [7.0, 10.0]);
        // h·v̂ = (1·3 + 2·4) / 5 = 2.2, v̂ = (0.6, 0.8).
        let mut h = [1.0, 2.0];
        ControlOp::Ablate.apply(&mut h, &v, 1.0);
        assert!((h[0] - (1.0 - 2.2 * 0.6)).abs() < 1e-12 && (h[1] - (2.0 - 2.2 * 0.8)).abs() < 1e-12, "{h:?}");
        assert!((h[0] * 0.6 + h[1] * 0.8).abs() < 1e-12, "ablate at 1 leaves nothing along v: {h:?}");
        let mut h = [1.0, 2.0];
        ControlOp::Clamp.apply(&mut h, &v, 5.0);
        assert!((h[0] * 0.6 + h[1] * 0.8 - 5.0).abs() < 1e-12, "clamp sets the part along v to the strength: {h:?}");
        assert!((h[0] * 0.8 - h[1] * 0.6 - (1.0 * 0.8 - 2.0 * 0.6)).abs() < 1e-12, "clamp keeps the part across v: {h:?}");
    }

    #[test]
    fn add_and_ablate_at_0_change_nothing_and_clamp_at_0_does() {
        let v = [3.0f32, -4.0];
        for op in [ControlOp::Add, ControlOp::Ablate] {
            let mut h = [1.5, -2.25];
            op.apply(&mut h, &v, 0.0);
            assert_eq!(h, [1.5, -2.25], "{op:?}");
            assert!(op.changes_nothing(0.0) && !op.changes_nothing(0.5), "{op:?}");
        }
        let mut h = [1.5, -2.25];
        ControlOp::Clamp.apply(&mut h, &v, 0.0);
        assert_ne!(h, [1.5, -2.25]);
        assert!((h[0] * 0.6 - h[1] * 0.8).abs() < 1e-12, "clamp at 0 is a whole ablation: {h:?}");
        assert!(!ControlOp::Clamp.changes_nothing(0.0));
    }

    #[test]
    fn a_vector_of_zeros_has_no_direction() {
        for op in [ControlOp::Ablate, ControlOp::Clamp] {
            let mut h = [1.0, 2.0];
            op.apply(&mut h, &[0.0, 0.0], 3.0);
            assert_eq!(h, [1.0, 2.0], "{op:?}");
        }
    }

    fn context(control: Value) -> Result<Context, String> {
        serde_json::from_value::<Context>(json!({"thinking": false, "control": control, "messages": [{"role": "user", "content": "u"}]})).map_err(|e| e.to_string())
    }

    #[test]
    fn a_control_list_decodes_and_is_left_out_when_empty() {
        let c = context(json!([{"vector": id('a'), "op": "add", "strength": 4}, {"vector": id('b'), "op": "clamp", "strength": -0.5}])).unwrap();
        assert_eq!(c.control.len(), 2);
        assert_eq!((c.control[0].op, c.control[0].strength, c.control[1].op), (ControlOp::Add, 4.0, ControlOp::Clamp));
        assert_eq!(serde_json::to_value(&c).unwrap()["control"][1], json!({"vector": id('b'), "op": "clamp", "strength": -0.5}));
        let plain = context(json!([])).unwrap();
        assert!(plain.control.is_empty() && serde_json::to_value(&plain).unwrap().get("control").is_none());
        let absent: Context = serde_json::from_value(json!({"thinking": false, "messages": [{"role": "user", "content": "u"}]})).unwrap();
        assert_eq!(absent, plain);
    }

    #[test]
    fn a_bad_control_item_names_its_member() {
        let ok = json!({"vector": id('a'), "op": "add", "strength": 1});
        for (item, param) in [
            (json!({"vector": id('a'), "op": "boost", "strength": 1}), "control[1].op"),
            (json!({"vector": id('a'), "op": 3, "strength": 1}), "control[1].op"),
            (json!({"vector": id('a'), "op": "add", "strength": "1"}), "control[1].strength"),
            (json!({"vector": id('a'), "op": "add", "strength": null}), "control[1].strength"),
            (json!({"vector": id('a'), "op": "add"}), "control[1].strength"),
            (json!({"vector": "blake3:abc", "op": "add", "strength": 1}), "control[1].vector"),
            (json!({"op": "add", "strength": 1}), "control[1].vector"),
            (json!({"vector": id('a'), "op": "add", "strength": 1, "layer": 3}), "control[1].layer"),
            (json!("blake3:x"), "control[1]"),
        ] {
            let e = context(json!([ok.clone(), item.clone()])).unwrap_err();
            assert_eq!(crate::error::decode_param(&e).as_deref(), Some(param), "{item}: {e}");
        }
        for list in [json!(null), json!({"vector": id('a')})] {
            let e = context(list.clone()).unwrap_err();
            assert_eq!(crate::error::decode_param(&e).as_deref(), Some("control"), "{list}: {e}");
        }
        let e = context(json!([{"vector": "blake3:abc", "op": "add", "strength": 1}])).unwrap_err();
        assert!(e.contains("\"blake3:abc\"") && e.contains("3 hex digits"), "the message quotes the id: {e}");
    }

    #[test]
    fn a_context_built_in_code_with_a_strength_that_is_not_finite_is_refused() {
        let mut c = Context::new(false, vec![Message::user("u")]);
        c.control = vec![Control { vector: ContentId::parse(id('a')).unwrap(), op: ControlOp::Add, strength: 1.0 }, Control { vector: ContentId::parse(id('a')).unwrap(), op: ControlOp::Add, strength: f64::NAN }];
        assert_eq!(c.check().unwrap_err().param.as_deref(), Some("control[1].strength"));
        assert_eq!(c.check_read(2, "a decision reads").unwrap_err().param.as_deref(), Some("contexts[2].control[1].strength"));
    }

    #[test]
    fn a_decision_or_raw_read_names_the_member_under_its_context() {
        let bad = json!({"thinking": false, "control": [{"vector": id('a'), "op": "boost", "strength": 1}], "messages": [{"role": "user", "content": "u"}]});
        let good = json!({"thinking": false, "messages": [{"role": "user", "content": "u"}]});
        let e = serde_json::from_value::<crate::decision::DecisionRequest>(json!({"model": "m", "spec_id": id('c'), "case": "x", "contexts": [good, bad]})).unwrap_err();
        assert_eq!(crate::error::decode_param(&e.to_string()).as_deref(), Some("contexts[1].control[0].op"), "{e}");
        let e = serde_json::from_value::<crate::reads::ReadsRequest>(json!({"model": "m", "contexts": [bad], "suffixes": ["A"], "letters": ["A", "B"]})).unwrap_err();
        assert_eq!(crate::error::decode_param(&e.to_string()).as_deref(), Some("contexts[0].control[0].op"), "{e}");
        let e = serde_json::from_value::<crate::decision::DecisionRequest>(json!({"model": "m", "spec_id": id('c'), "case": "x", "contexts": [{"messages": []}]})).unwrap_err();
        assert_eq!(crate::error::decode_param(&e.to_string()).as_deref(), Some("thinking"), "other members are named as before: {e}");
    }

    fn served(capabilities: Vec<Capability>) -> ServedModel {
        serde_json::from_value(json!({"model": "m", "status": "ready", "weight_hash": "0x01", "tokenizer_hash": "t", "template": "p", "engine": "e",
            "limits": {"context_tokens": 8, "decision_context_tokens": 8, "snapshot_bytes": 1, "contexts_per_decision": 1, "default_timeout_ms": 1, "control_vector_contexts": 2},
            "capabilities": capabilities}))
        .unwrap()
    }

    #[test]
    fn control_needs_the_control_vectors_capability() {
        let plain = Context::new(false, vec![Message::user("u")]);
        let mut steered = plain.clone();
        steered.control = vec![Control { vector: ContentId::parse(id('a')).unwrap(), op: ControlOp::Add, strength: 0.0 }];
        let without = served(vec![Capability::Generate]);
        let e = steered.check_control(&without).unwrap_err();
        assert_eq!((e.param.as_deref(), e.message.as_str()), (Some("control"), "nothing ran: m does not declare control_vectors; send no control"));
        plain.check_control(&without).unwrap();
        let with = served(vec![Capability::Generate, Capability::ControlVectors]);
        steered.check_control(&with).unwrap();
        assert_eq!(serde_json::to_value(Capability::ControlVectors).unwrap(), json!("control_vectors"));
        assert_eq!(with.limits.control_vector_contexts, Some(2));
    }

    #[test]
    fn a_control_vector_round_trips_through_its_bytes() {
        let v = vector();
        let bytes = v.to_bytes();
        let n = u64::from_le_bytes(bytes[..8].try_into().unwrap()) as usize;
        assert_eq!(n % 8, 0, "the header is padded to a multiple of 8 bytes");
        let header: Value = serde_json::from_slice(&bytes[8..8 + n]).unwrap();
        assert_eq!(header["__metadata__"], json!({"model": "m", "weight_hash": "0x01"}));
        assert_eq!(header["layers.2"], json!({"dtype": "F32", "shape": [2], "data_offsets": [8, 16]}));
        assert_eq!(bytes.len(), 8 + n + 16);
        assert_eq!(ControlVector::parse(&bytes).unwrap(), v);
        assert_eq!(v.width(), 2);
        assert_eq!(bytes, vector().to_bytes(), "the same vector gives the same bytes");
    }

    /// The bytes of a safetensors file with this header and data.
    fn file(header: Value, data: &[u8]) -> Vec<u8> {
        let h = header.to_string();
        let mut out = (h.len() as u64).to_le_bytes().to_vec();
        out.extend_from_slice(h.as_bytes());
        out.extend_from_slice(data);
        out
    }

    #[test]
    fn a_file_that_is_not_a_control_vector_is_refused_with_its_reason() {
        let meta = json!({"model": "m", "weight_hash": "w"});
        let t = |shape: Value, offsets: Value| json!({"dtype": "F32", "shape": shape, "data_offsets": offsets});
        let one = 1.0f32.to_le_bytes();
        let two: Vec<u8> = [1.0f32, 2.0].iter().flat_map(|x| x.to_le_bytes()).collect();
        ControlVector::parse(&file(json!({"__metadata__": meta, "layers.0": t(json!([1]), json!([0, 4]))}), &one)).unwrap();
        for (bytes, says) in [
            (b"short".to_vec(), "8 bytes"),
            (file(json!({"__metadata__": meta}), &[]), "no tensor"),
            (file(json!({"layers.0": t(json!([1]), json!([0, 4]))}), &one), "__metadata__"),
            (file(json!({"__metadata__": {"model": "m"}, "layers.0": t(json!([1]), json!([0, 4]))}), &one), "weight_hash"),
            (file(json!({"__metadata__": meta, "layer.0": t(json!([1]), json!([0, 4]))}), &one), "layers.<l>"),
            (file(json!({"__metadata__": meta, "layers.01": t(json!([1]), json!([0, 4]))}), &one), "layers.<l>"),
            (file(json!({"__metadata__": meta, "layers.0": {"dtype": "F16", "shape": [2], "data_offsets": [0, 4]}}), &one), "F32"),
            (file(json!({"__metadata__": meta, "layers.0": t(json!([1, 1]), json!([0, 4]))}), &one), "shape"),
            (file(json!({"__metadata__": meta, "layers.0": t(json!([1]), json!([0, 4])), "layers.1": t(json!([2]), json!([4, 12]))}), &[one.to_vec(), two.clone()].concat()), "width"),
            (file(json!({"__metadata__": meta, "layers.0": t(json!([2]), json!([0, 4]))}), &one), "data_offsets"),
            (file(json!({"__metadata__": meta, "layers.0": t(json!([1]), json!([0, 4]))}), &two), "data"),
            (file(json!({"__metadata__": meta, "layers.0": t(json!([1]), json!([0, 4]))}), &f32::NAN.to_le_bytes()), "finite"),
        ] {
            let e = ControlVector::parse(&bytes).unwrap_err();
            assert!(e.contains(says), "{says}: {e}");
        }
    }

    #[test]
    fn a_vector_fits_only_the_weights_layers_and_width_it_was_made_for() {
        let v = vector();
        v.check_fits("0x01", 3, 2).unwrap();
        let e = v.check_fits("0x02", 3, 2).unwrap_err();
        assert!(e.contains("weight_hash 0x01") && e.contains("0x02"), "{e}");
        let e = v.check_fits("0x01", 3, 4).unwrap_err();
        assert!(e.contains("width 2") && e.contains("4"), "{e}");
        let e = v.check_fits("0x01", 2, 2).unwrap_err();
        assert!(e.contains("layers.2") && e.contains("2 layers"), "{e}");
    }

    fn request(v: Value) -> Result<ControlVectorRequest, String> {
        serde_json::from_value(v).map_err(|e| e.to_string())
    }

    fn plain() -> Value {
        json!({"thinking": false, "messages": [{"role": "user", "content": "u"}]})
    }

    #[test]
    fn a_control_vector_request_has_two_sides_of_closed_plain_contexts() {
        let r = request(json!({"model": "m", "positive": [plain()], "negative": [plain(), plain()]})).unwrap();
        r.validate().unwrap();
        assert_eq!(r.contexts().iter().map(|(at, _)| at.as_str()).collect::<Vec<_>>(), ["positive[0]", "negative[0]", "negative[1]"]);
        r.check(&served(vec![Capability::ControlVectors])).unwrap();
        let steered = json!({"thinking": false, "control": [{"vector": id('a'), "op": "add", "strength": 1}], "messages": [{"role": "user", "content": "u"}]});
        let open = json!({"thinking": false, "messages": [{"role": "user", "content": "u"}, {"role": "assistant", "content": "a", "continue": true}]});
        for (body, param) in [
            (json!({"model": "m", "positive": [], "negative": [plain()]}), "positive"),
            (json!({"model": "m", "positive": [plain()], "negative": []}), "negative"),
            (json!({"model": "m", "positive": [plain()], "negative": [plain(), steered]}), "negative[1].control"),
            (json!({"model": "m", "positive": [open], "negative": [plain()]}), "continue"),
            (json!({"model": "m", "positive": [{"thinking": false, "messages": []}], "negative": [plain()]}), "positive"),
        ] {
            let e = request(body.clone()).unwrap().validate().unwrap_err();
            assert_eq!(e.param.as_deref(), Some(param), "{body}: {e:?}");
        }
        for (body, param) in [
            (json!({"model": "m", "positive": [plain()], "negative": [plain()], "layers": [3]}), "layers"),
            (json!({"model": "m", "negative": [plain()]}), "positive"),
            (json!({"positive": [plain()], "negative": [plain()]}), "model"),
            (json!({"model": "m", "positive": [plain()], "negative": [{"thinking": false, "control": [{"vector": "x", "op": "add", "strength": 1}], "messages": []}]}), "negative[0].control[0].vector"),
        ] {
            let e = request(body.clone()).unwrap_err();
            assert_eq!(crate::error::decode_param(&e).as_deref(), Some(param), "{body}: {e}");
        }
    }

    #[test]
    fn a_control_vector_request_checks_the_served_model() {
        let r = request(json!({"model": "m", "positive": [plain()], "negative": [plain()]})).unwrap();
        assert_eq!(r.check(&served(vec![Capability::Generate])).unwrap_err().param.as_deref(), Some("model"));
        let mut thinks = plain();
        thinks["thinking"] = json!(true);
        let r = request(json!({"model": "m", "positive": [plain()], "negative": [plain(), thinks]})).unwrap();
        assert_eq!(r.check(&served(vec![Capability::ControlVectors])).unwrap_err().param.as_deref(), Some("negative[1].thinking"));
        r.check(&served(vec![Capability::ControlVectors, Capability::Thinking])).unwrap();
        let r = request(json!({"model": "m", "positive": [plain(), plain(), plain()], "negative": [plain()]})).unwrap();
        let e = r.check(&served(vec![Capability::ControlVectors])).unwrap_err();
        assert!(e.param.as_deref() == Some("positive") && e.message.contains("control_vector_contexts"), "{e:?}");
    }

    #[test]
    fn a_response_and_a_dry_run_round_trip() {
        let v = json!({"model": "m", "vector": id('d'), "layers": 4, "width": 8, "contexts": {"positive": 2, "negative": 1},
            "identity": {"model": "m", "weight_hash": "w", "tokenizer_hash": "t", "template": "p", "engine": "e"},
            "usage": {"prompt": 30, "kept": 0, "fed": 30, "completion": 0, "reasoning": 0}});
        let r: ControlVectorResponse = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(serde_json::to_value(&r).unwrap(), v);
        let mut dry = v.clone();
        dry["vector"] = Value::Null;
        assert_eq!(serde_json::from_value::<ControlVectorResponse>(dry).unwrap().vector, None);
        let mut absent = v;
        absent.as_object_mut().unwrap().remove("vector");
        assert!(serde_json::from_value::<ControlVectorResponse>(absent).is_err(), "vector is required, null for a dry run");
    }
}
