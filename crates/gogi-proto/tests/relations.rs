//! Property-style checks of the pooling relations over seeded random reads, a differential
//! check against mkviolet's own algorithm, and `verify` over responses built from the math.
// The reference pooling follows mkviolet's algorithm line for line, its index loops and its
// `!(x > 0.0)` included, so a difference from mkviolet is easy to see.
#![allow(clippy::needless_range_loop, clippy::neg_cmp_op_on_partial_ord)]

use gogi_proto::decision::{DecisionResponse, Expect, Pool, PoolMethod, PoolWeights};
use gogi_proto::math::{self, Row, WeightSpec, argmax, verify};
use serde_json::{Value, json};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Reads with `k` options and logprobs between -12 and -0.05.
fn random_reads(r: &mut Rng, n: usize, k: usize) -> Vec<Vec<f64>> {
    (0..n).map(|_| (0..k).map(|_| -0.05 - 12.0 * r.unit()).collect()).collect()
}

fn rows(lps: &[Vec<f64>]) -> Vec<Row> {
    lps.iter().map(|l| Row::from_logprobs(l).unwrap()).collect()
}

fn near(a: f64, b: f64, tol: f64) {
    assert!((a - b).abs() <= tol, "{a} vs {b}");
}

const CASES: usize = 400;

#[test]
fn a_reads_numbers_obey_the_exact_relations() {
    let mut r = Rng(1);
    for _ in 0..CASES {
        let k = 2 + r.below(8);
        let lp = random_reads(&mut r, 1, k).remove(0);
        let row = Row::from_logprobs(&lp).unwrap();
        near(row.probs.iter().sum::<f64>(), 1.0, 1e-12);
        for o in 0..k {
            near(row.probs[o], (lp[o] - row.mass).exp(), 1e-12);
        }
        near(row.mass, lp.iter().map(|x| x.exp()).sum::<f64>().ln(), 1e-12);
        near(math::confidence(row.mass, &row.probs), row.mass.exp() * row.probs[argmax(&row.probs)], 1e-15);
    }
}

#[test]
fn a_linear_pool_is_the_weighted_mean_and_a_loglinear_pool_the_normalized_product() {
    let mut r = Rng(2);
    for _ in 0..CASES {
        let (n, k) = (1 + r.below(5), 2 + r.below(5));
        let rs = rows(&random_reads(&mut r, n, k));
        let given: Vec<f64> = (0..n).map(|_| 0.1 + r.unit()).collect();
        let total: f64 = given.iter().sum();
        let lin = math::pool(&rs, PoolMethod::Linear, &WeightSpec::Given(given.clone())).unwrap();
        let log = math::pool(&rs, PoolMethod::Loglinear, &WeightSpec::Given(given.clone())).unwrap();
        let prod: Vec<f64> = (0..k).map(|o| rs.iter().zip(&given).map(|(row, w)| row.probs[o].powf(w / total)).product()).collect();
        let z: f64 = prod.iter().sum();
        for o in 0..k {
            near(lin.probs[o], rs.iter().zip(&given).map(|(row, w)| w / total * row.probs[o]).sum(), 1e-12);
            near(log.probs[o], prod[o] / z, 1e-12);
        }
        near(lin.probs.iter().sum::<f64>(), 1.0, 1e-12);
        near(log.probs.iter().sum::<f64>(), 1.0, 1e-12);
    }
}

#[test]
fn pooling_ignores_read_order_and_the_scale_of_given_weights() {
    let mut r = Rng(3);
    for _ in 0..CASES {
        let (n, k) = (2 + r.below(4), 2 + r.below(4));
        let lps = random_reads(&mut r, n, k);
        let w: Vec<f64> = (0..n).map(|_| 0.1 + r.unit()).collect();
        for method in [PoolMethod::Linear, PoolMethod::Loglinear] {
            let a = math::pool(&rows(&lps), method, &WeightSpec::Given(w.clone())).unwrap();
            let (mut lps2, mut w2) = (lps.clone(), w.clone());
            lps2.reverse();
            w2.reverse();
            let b = math::pool(&rows(&lps2), method, &WeightSpec::Given(w2)).unwrap();
            let c = math::pool(&rows(&lps), method, &WeightSpec::Given(w.iter().map(|x| x * 7.5).collect())).unwrap();
            for o in 0..k {
                near(a.probs[o], b.probs[o], 1e-12);
                near(a.probs[o], c.probs[o], 1e-12);
            }
            near(a.spread, b.spread, 1e-15);
            assert_eq!(a.agree, b.agree);
        }
    }
}

#[test]
fn leave_one_out_is_the_pool_of_the_other_reads() {
    let mut r = Rng(4);
    for _ in 0..CASES {
        let (n, k) = (2 + r.below(4), 2 + r.below(4));
        let rs = rows(&random_reads(&mut r, n, k));
        let w: Vec<f64> = (0..n).map(|_| r.unit()).collect();
        for method in [PoolMethod::Linear, PoolMethod::Loglinear] {
            let p = math::pool(&rs, method, &WeightSpec::Given(w.clone())).unwrap();
            let loo = p.leave_one_out.unwrap();
            for i in 0..n {
                let rest: Vec<Row> = rs.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, x)| x.clone()).collect();
                let rest_w: Vec<f64> = w.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, x)| *x).collect();
                match math::pool(&rest, method, &WeightSpec::Given(rest_w.clone())) {
                    Ok(q) => (0..k).for_each(|o| near(loo[i].as_ref().unwrap()[o], q.probs[o], 1e-12)),
                    Err(_) => assert!(loo[i].is_none(), "no weight remains without read {i}"),
                }
            }
        }
    }
}

#[test]
fn agree_and_spread_follow_their_definitions() {
    let mut r = Rng(5);
    for _ in 0..CASES {
        let (n, k) = (1 + r.below(5), 2 + r.below(5));
        let rs = rows(&random_reads(&mut r, n, k));
        let p = math::pool(&rs, PoolMethod::Linear, &WeightSpec::Uniform).unwrap();
        let tops: Vec<usize> = rs.iter().map(|x| argmax(&x.probs)).collect();
        assert_eq!(p.agree, tops.iter().all(|t| *t == tops[0]));
        let mut spread: f64 = 0.0;
        for o in 0..k {
            let col: Vec<f64> = rs.iter().map(|x| x.probs[o]).collect();
            spread = spread.max(col.iter().cloned().fold(f64::MIN, f64::max) - col.iter().cloned().fold(f64::MAX, f64::min));
        }
        near(p.spread, spread, 1e-15);
        if n == 1 {
            assert!(p.agree && p.spread == 0.0 && p.leave_one_out.is_none());
        }
    }
}

#[test]
fn mass_weights_are_exp_mass_and_pooled_confidence_uses_their_mean() {
    let mut r = Rng(6);
    for _ in 0..CASES {
        let (n, k) = (1 + r.below(4), 2 + r.below(4));
        let rs = rows(&random_reads(&mut r, n, k));
        let p = math::pool(&rs, PoolMethod::Loglinear, &WeightSpec::Mass).unwrap();
        let raw: Vec<f64> = rs.iter().map(|x| x.mass.exp()).collect();
        let total: f64 = raw.iter().sum();
        for i in 0..n {
            near(p.weights[i], raw[i] / total, 1e-12);
        }
        near(p.confidence(), p.probs[argmax(&p.probs)] * raw.iter().map(|x| x * x).sum::<f64>() / total, 1e-12);
    }
}

#[test]
fn a_loglinear_pool_lets_one_read_veto() {
    let rs = rows(&[vec![-0.1, -2.5, -9.0], vec![-9.0, -2.5, -0.1]]);
    let log = math::pool(&rs, PoolMethod::Loglinear, &WeightSpec::Uniform).unwrap();
    assert_eq!(argmax(&log.probs), 1);
}

// ---- mkviolet's algorithm, copied from its src/council.rs: the second implementation
// the math must agree with.

mod mkviolet {
    #[derive(Clone, Copy)]
    pub enum Method {
        Linear,
        LogLinear,
    }

    fn argmax(values: &[f64]) -> usize {
        let mut best = 0usize;
        for (i, &v) in values.iter().enumerate() {
            if v > values[best] {
                best = i;
            }
        }
        best
    }

    pub struct Read {
        pub probabilities: Vec<f64>,
        pub mass: f64,
        pub top: usize,
    }

    impl Read {
        pub fn from_logprobs(logprobs: &[f64]) -> Self {
            let max_lp = logprobs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let sum_exp: f64 = logprobs.iter().map(|&lp| (lp - max_lp).exp()).sum();
            let mass = max_lp + sum_exp.ln();
            let probabilities: Vec<f64> = logprobs.iter().map(|&lp| (lp - mass).exp()).collect();
            let top = argmax(&probabilities);
            Read { probabilities, mass, top }
        }
    }

    pub struct Out {
        pub probabilities: Vec<f64>,
        pub confidence: f64,
        pub agree: bool,
        pub spread: f64,
        pub weights: Vec<f64>,
        pub loo: Vec<Option<Vec<f64>>>,
    }

    pub fn evaluate(reads: &[Read], method: Method, raw_weights: Vec<f64>) -> Out {
        let n_opts = reads[0].probabilities.len();
        let w_sum: f64 = raw_weights.iter().sum();
        let weights: Vec<f64> = raw_weights.iter().map(|&w| w / w_sum).collect();
        let probabilities = pool_subset(reads, &raw_weights, None, method, n_opts).unwrap();
        let best = argmax(&probabilities);
        let wm: f64 = reads.iter().zip(&weights).map(|(r, &w)| w * r.mass.exp()).sum();
        let confidence = probabilities[best] * wm;
        let agree = reads.iter().all(|r| r.top == reads[0].top);
        let mut spread = 0.0f64;
        for o in 0..n_opts {
            let min_o = reads.iter().map(|r| r.probabilities[o]).fold(f64::INFINITY, f64::min);
            let max_o = reads.iter().map(|r| r.probabilities[o]).fold(f64::NEG_INFINITY, f64::max);
            spread = spread.max(max_o - min_o);
        }
        let loo = if reads.len() >= 2 { (0..reads.len()).map(|s| pool_subset(reads, &raw_weights, Some(s), method, n_opts)).collect() } else { vec![] };
        Out { probabilities, confidence, agree, spread, weights, loo }
    }

    fn pool_subset(reads: &[Read], weights: &[f64], skip: Option<usize>, method: Method, n_opts: usize) -> Option<Vec<f64>> {
        let active: f64 = weights.iter().enumerate().filter(|(i, _)| Some(*i) != skip).map(|(_, &w)| w).sum();
        if !(active > 0.0) {
            return None;
        }
        let mut out = vec![0.0f64; n_opts];
        match method {
            Method::Linear => {
                for (i, r) in reads.iter().enumerate() {
                    if Some(i) == skip {
                        continue;
                    }
                    let w = weights[i] / active;
                    for o in 0..n_opts {
                        out[o] += w * r.probabilities[o];
                    }
                }
            }
            Method::LogLinear => {
                let mut ls = vec![0.0f64; n_opts];
                for (i, r) in reads.iter().enumerate() {
                    if Some(i) == skip || weights[i] == 0.0 {
                        continue;
                    }
                    let w = weights[i] / active;
                    for o in 0..n_opts {
                        ls[o] += w * r.probabilities[o].ln();
                    }
                }
                let max_ls = ls.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                if max_ls == f64::NEG_INFINITY {
                    return None;
                }
                let sum_exp: f64 = ls.iter().map(|&x| (x - max_ls).exp()).sum();
                for o in 0..n_opts {
                    out[o] = (ls[o] - max_ls).exp() / sum_exp;
                }
            }
        }
        Some(out)
    }
}

#[test]
fn gogi_agrees_with_mkviolets_algorithm_within_1e_9() {
    let mut r = Rng(7);
    for _ in 0..CASES {
        let (n, k) = (1 + r.below(5), 2 + r.below(6));
        let lps = random_reads(&mut r, n, k);
        let mine = rows(&lps);
        let theirs: Vec<mkviolet::Read> = lps.iter().map(|l| mkviolet::Read::from_logprobs(l)).collect();
        let kind = r.below(3);
        let given: Vec<f64> = (0..n).map(|_| if r.below(4) == 0 { 0.0 } else { 0.1 + r.unit() }).collect();
        let (spec, raw) = match kind {
            0 => (WeightSpec::Uniform, vec![1.0; n]),
            1 => (WeightSpec::Mass, theirs.iter().map(|x| x.mass.exp()).collect()),
            _ if given.iter().sum::<f64>() > 0.0 => (WeightSpec::Given(given.clone()), given),
            _ => (WeightSpec::Uniform, vec![1.0; n]),
        };
        for (m, t) in [(PoolMethod::Linear, mkviolet::Method::Linear), (PoolMethod::Loglinear, mkviolet::Method::LogLinear)] {
            let a = math::pool(&mine, m, &spec).unwrap();
            let b = mkviolet::evaluate(&theirs, t, raw.clone());
            for o in 0..k {
                near(a.probs[o], b.probabilities[o], 1e-9);
            }
            near(a.confidence(), b.confidence, 1e-9);
            near(a.spread, b.spread, 1e-9);
            assert_eq!(a.agree, b.agree);
            for i in 0..n {
                near(a.weights[i], b.weights[i], 1e-9);
            }
            match (&a.leave_one_out, b.loo.is_empty()) {
                (None, true) => {}
                (Some(x), false) => {
                    for (xi, yi) in x.iter().zip(&b.loo) {
                        match (xi, yi) {
                            (None, None) => {}
                            (Some(p), Some(q)) => (0..k).for_each(|o| near(p[o], q[o], 1e-9)),
                            other => panic!("leave-one-out null disagreement: {other:?}"),
                        }
                    }
                }
                _ => panic!("leave_one_out presence differs"),
            }
        }
    }
}

// ---- verify over responses built from the math

fn map_of(keys: &[String], v: &[f64]) -> Value {
    Value::Object(keys.iter().zip(v).map(|(k, x)| (k.clone(), json!(x))).collect())
}

/// A decision response for one choice question, every derived number computed by `math`.
fn response(lps: &[Vec<f64>], method: PoolMethod, weights: PoolWeights, given: Option<Vec<f64>>) -> (Value, Expect) {
    let k = lps[0].len();
    let keys: Vec<String> = (0..k).map(|i| format!("opt{i}")).collect();
    let rs = rows(lps);
    let spec = match weights {
        PoolWeights::Uniform => WeightSpec::Uniform,
        PoolWeights::Mass => WeightSpec::Mass,
        PoolWeights::Given => WeightSpec::Given(given.clone().unwrap()),
    };
    let p = math::pool(&rs, method, &spec).unwrap();
    let ids: Vec<String> = (0..rs.len()).map(|i| i.to_string()).collect();
    let reads: Vec<Value> = rs
        .iter()
        .enumerate()
        .map(|(i, row)| {
            json!({"context": i, "snapshot": null, "rendered_hash": "x", "answers": {"q": {"type": "choice", "choice": keys[argmax(&row.probs)],
                "probabilities": map_of(&keys, &row.probs), "confidence": math::confidence(row.mass, &row.probs),
                "logprobs": map_of(&keys, &lps[i]), "mass": row.mass}}})
        })
        .collect();
    let mut pooled = json!({"type": "choice", "choice": keys[argmax(&p.probs)], "probabilities": map_of(&keys, &p.probs),
        "confidence": p.confidence(), "agree": p.agree, "spread": p.spread});
    if let Some(loo) = &p.leave_one_out {
        pooled["leave_one_out"] = Value::Object(ids.iter().zip(loo).map(|(id, l)| (id.clone(), l.as_ref().map_or(Value::Null, |v| map_of(&keys, v)))).collect());
    }
    let echo = json!({"method": method, "weights": weights, "normalized": {"q": p.weights}});
    let v = json!({"model": "m", "answers": {"q": pooled}, "reads": reads, "pool": echo,
        "identity": {"model": "m", "weight_hash": "w", "tokenizer_hash": "t", "template": "t", "engine": "e"},
        "usage": {"prompt": 30, "kept": 20, "fed": 10, "completion": 0, "reasoning": 0}});
    (v, Expect { pool: Pool { method: Some(method), weights: Some(weights), values: given }, contexts: ids.len() })
}

#[test]
fn verify_accepts_what_the_math_produces_and_rejects_a_changed_number() {
    let mut r = Rng(8);
    for case in 0..200 {
        let (n, k) = (1 + r.below(4), 2 + r.below(4));
        let lps = random_reads(&mut r, n, k);
        let (method, weights) = [(PoolMethod::Linear, PoolWeights::Uniform), (PoolMethod::Loglinear, PoolWeights::Mass), (PoolMethod::Linear, PoolWeights::Mass), (PoolMethod::Loglinear, PoolWeights::Given)][case % 4];
        let given = (weights == PoolWeights::Given).then(|| (0..n).map(|_| 0.2 + r.unit()).collect::<Vec<_>>());
        let (v, expect) = response(&lps, method, weights, given);
        let resp: DecisionResponse = serde_json::from_value(v.clone()).unwrap();
        verify(&resp, &expect).unwrap_or_else(|m| panic!("case {case}: {m}"));
        let mut bad_usage = resp.clone();
        bad_usage.usage.fed += 1;
        assert!(verify(&bad_usage, &expect).is_err(), "case {case}: usage.fed changed");
        let top = argmax(&rows(&lps[..1])[0].probs);
        let top_lp = format!("/reads/0/answers/q/logprobs/opt{top}");
        for ptr in [top_lp.as_str(), "/answers/q/probabilities/opt0", "/answers/q/confidence", "/answers/q/spread", "/reads/0/answers/q/mass", "/pool/normalized/q/0"] {
            let mut bad = v.clone();
            let x = bad.pointer(ptr).unwrap().as_f64().unwrap();
            *bad.pointer_mut(ptr).unwrap() = json!(x + 1e-6);
            let resp: DecisionResponse = serde_json::from_value(bad).unwrap();
            // A raw logprob is the server's and is not itself checked; the numbers derived from it must then disagree.
            assert!(verify(&resp, &expect).is_err(), "case {case}: a change at {ptr} went unnoticed");
        }
    }
}
