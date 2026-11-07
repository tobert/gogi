//! The toy model's residual, and how control vectors change its logits.
//!
//! The toy has [`LAYERS`] layers and a residual of [`WIDTH`]. Its residual after layer `l` at a
//! model state is a deterministic function of that state and `l`, with each component from -1
//! to 1. A control list changes the logits, never the model state: for each layer, the items apply
//! to that layer's residual in list order, and each component's change `d` adds `d·w` to each
//! logit, with `w` a fixed weight between -1 and 1 for the layer, the component, and the token.
//! A layer whose residual does not change adds nothing, so `add` and `ablate` at strength 0 give
//! the plain logits bit for bit.

use gogi_proto::control::{ControlOp, ControlVector};

use crate::{State, VOCAB, mix};

/// The layers of the toy model.
pub const LAYERS: u32 = 4;

/// The width of the toy model's residual.
pub const WIDTH: usize = 8;

/// A number between -1 and 1 from `z`.
fn signed(z: u64) -> f64 {
    (z >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
}

/// The residual after each layer at model state `state`, in layer order.
pub fn residuals(state: State) -> Vec<[f64; WIDTH]> {
    (0..LAYERS)
        .map(|l| {
            let mut h = [0.0; WIDTH];
            for (j, x) in h.iter_mut().enumerate() {
                *x = signed(mix(state.0 ^ mix(0xC0_0000 + u64::from(l) * WIDTH as u64 + j as u64)));
            }
            h
        })
        .collect()
}

/// The weight of component `j` of layer `l`'s residual in the logit of token `v`.
fn weight(l: u32, j: usize, v: usize) -> f64 {
    signed(mix(mix(0xC1_0000 + u64::from(l) * WIDTH as u64 + j as u64) ^ mix(0xC2_0000 + v as u64)))
}

/// The logits at model state `state` under a control list: each item's op, strength, and vector,
/// in order. Each vector fits the toy: its width is [`WIDTH`] and its layers are below [`LAYERS`].
pub fn logits(state: State, control: &[(ControlOp, f64, ControlVector)]) -> [f64; VOCAB] {
    let mut out = state.logits();
    for (l, h) in residuals(state).iter().enumerate() {
        let mut changed = *h;
        for (op, strength, vector) in control {
            if let Some(v) = vector.layers.get(&(l as u32)) {
                op.apply(&mut changed, v, *strength);
            }
        }
        for j in 0..WIDTH {
            let d = changed[j] - h[j];
            if d != 0.0 {
                for (v, x) in out.iter_mut().enumerate() {
                    *x += d * weight(l as u32, j, v);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vector(layers: &[u32]) -> ControlVector {
        ControlVector { model: "m".into(), weight_hash: "w".into(), layers: layers.iter().map(|l| (*l, (0..WIDTH).map(|j| j as f32 - 3.5).collect())).collect() }
    }

    #[test]
    fn the_residual_is_the_states_and_lies_in_minus_1_to_1() {
        let s = State::after(0, &[1, 2, 3]);
        let r = residuals(s);
        assert_eq!(r.len(), LAYERS as usize);
        assert!(r.iter().flatten().all(|x| (-1.0..=1.0).contains(x)));
        assert_eq!(r, residuals(State::after(0, &[1, 2, 3])));
        assert_ne!(r, residuals(State::after(1, &[1, 2, 3])), "two seeds are two models");
        assert_ne!(r[0], r[1], "each layer has its own residual");
    }

    #[test]
    fn add_and_ablate_at_0_give_the_plain_logits_and_other_items_do_not() {
        let s = State::after(0, &[7, 8]);
        let v = vector(&[0, 2]);
        assert_eq!(logits(s, &[]), s.logits());
        assert_eq!(logits(s, &[(ControlOp::Add, 0.0, v.clone()), (ControlOp::Ablate, 0.0, v.clone())]), s.logits());
        for (op, a) in [(ControlOp::Clamp, 0.0), (ControlOp::Add, 0.5), (ControlOp::Ablate, 1.0)] {
            assert_ne!(logits(s, &[(op, a, v.clone())]), s.logits(), "{op:?} at {a}");
        }
        assert_ne!(logits(s, &[(ControlOp::Add, 1.0, vector(&[0]))]), logits(s, &[(ControlOp::Add, 1.0, vector(&[1]))]), "each layer has its own weights");
    }
}
