//! Python bindings for gogi-proto and gogi-fake. Functions take and return JSON text;
//! `gogi/__init__.py` turns it into Python values.

use gogi_proto::decision::{DecisionRequest, DecisionResponse, PoolMethod};
use gogi_proto::math::{self, Row, WeightSpec};
use gogi_proto::spec::{SpecRules, parse_spec};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use serde_json::{Value, json};

fn value_err(e: impl std::fmt::Display) -> PyErr {
    PyValueError::new_err(e.to_string())
}

fn json_of(text: &str) -> PyResult<Value> {
    serde_json::from_str(text).map_err(value_err)
}

#[pymodule]
mod _gogi {
    use super::*;

    /// Checks a council v2 context (`{thinking, tools, queue, messages}`) the way mkviolet does.
    /// Raises `ValueError` naming the first rule it breaks.
    #[pyfunction]
    fn check_context(context_json: &str) -> PyResult<()> {
        let c: gogi_proto::Context = serde_json::from_str(context_json).map_err(value_err)?;
        c.check().map_err(value_err)
    }

    /// Checks `messages` appended after `before`, such as a generation's reply after its context.
    #[pyfunction]
    fn validate_messages(before_json: &str, messages_json: &str) -> PyResult<()> {
        let before: Vec<gogi_proto::Message> = serde_json::from_str(before_json).map_err(value_err)?;
        let messages: Vec<gogi_proto::Message> = serde_json::from_str(messages_json).map_err(value_err)?;
        gogi_proto::message::validate(&before, &messages).map_err(value_err)
    }

    /// The canonical JSON text of a spec.
    #[pyfunction]
    fn canonical_json(json_text: &str) -> PyResult<String> {
        let mut out = String::new();
        gogi_proto::canonical_json(&json_of(json_text)?, &mut out).map_err(value_err)?;
        Ok(out)
    }

    /// The `sha256:...` id of a spec. Raises `ValueError` for a number that is not an integer
    /// below 2^53.
    #[pyfunction]
    fn spec_id(spec_json: &str) -> PyResult<String> {
        gogi_proto::spec_id_of(&json_of(spec_json)?).map_err(value_err)
    }

    /// Parses a spec by mkviolet's rules and returns its id. `describe` allows `text` questions.
    #[pyfunction]
    #[pyo3(signature = (spec_json, describe=false))]
    fn parse_spec_id(spec_json: &str, describe: bool) -> PyResult<String> {
        let rules = SpecRules { describe, text_tokens: if describe { 256 } else { 0 }, ..SpecRules::default() };
        let s = parse_spec(&json_of(spec_json)?, &rules).map_err(value_err)?;
        Ok(s.id())
    }

    /// One read's numbers from raw logprobs, as JSON: `{mass, probabilities, confidence}`.
    #[pyfunction]
    fn read_json(logprobs: Vec<f64>) -> PyResult<String> {
        let r = Row::from_logprobs(&logprobs).map_err(value_err)?;
        Ok(json!({"mass": r.mass, "probabilities": r.probs, "confidence": math::confidence(r.mass, &r.probs)}).to_string())
    }

    /// Pools reads (one list of logprobs per read), as JSON:
    /// `{probabilities, weights, agree, spread, confidence, leave_one_out}`.
    #[pyfunction]
    #[pyo3(signature = (reads, method="linear", weights="uniform", values=None))]
    fn pool_json(reads: Vec<Vec<f64>>, method: &str, weights: &str, values: Option<Vec<f64>>) -> PyResult<String> {
        let method = match method {
            "linear" => PoolMethod::Linear,
            "loglinear" => PoolMethod::Loglinear,
            other => return Err(value_err(format!("unknown pool method {}", other))),
        };
        let spec = match (weights, values) {
            ("uniform", None) => WeightSpec::Uniform,
            ("mass", None) => WeightSpec::Mass,
            ("given", Some(v)) => WeightSpec::Given(v),
            ("given", None) => return Err(value_err("weights given needs values")),
            ("uniform" | "mass", Some(_)) => return Err(value_err("values go with weights given")),
            (other, _) => return Err(value_err(format!("unknown pool weights {}", other))),
        };
        let rows: Result<Vec<Row>, _> = reads.iter().map(|l| Row::from_logprobs(l)).collect();
        let p = math::pool(&rows.map_err(value_err)?, method, &spec).map_err(value_err)?;
        Ok(json!({"probabilities": p.probs, "weights": p.weights, "agree": p.agree, "spread": p.spread,
                  "confidence": p.confidence(), "leave_one_out": p.leave_one_out}).to_string())
    }

    /// Recomputes a decision response's derived numbers from its reads and raises `ValueError`
    /// naming the first field that disagrees by more than 1e-9.
    #[pyfunction]
    fn verify_decision(request_json: &str, response_json: &str) -> PyResult<()> {
        let req: DecisionRequest = serde_json::from_str(request_json).map_err(value_err)?;
        let resp: DecisionResponse = serde_json::from_str(response_json).map_err(value_err)?;
        math::verify(&resp, &req.expect()).map_err(value_err)
    }

    /// gogi-fake's server, serving on its own thread until `close` or until it is dropped.
    #[pyclass(name = "_FakeServer")]
    struct FakeServer {
        server: Option<gogi_fake::Server>,
    }

    impl FakeServer {
        fn running(&self) -> PyResult<&gogi_fake::Server> {
            self.server.as_ref().ok_or_else(|| value_err("the fake server is closed"))
        }
    }

    #[pymethods]
    impl FakeServer {
        #[new]
        #[pyo3(signature = (addr="127.0.0.1:0", state_dir=None, context_tokens=None))]
        fn new(addr: &str, state_dir: Option<std::path::PathBuf>, context_tokens: Option<usize>) -> PyResult<Self> {
            let d = gogi_fake::Config::default();
            let cfg = gogi_fake::Config { addr: addr.into(), state_dir, context_tokens: context_tokens.unwrap_or(d.context_tokens), ..d };
            Ok(FakeServer { server: Some(gogi_fake::Server::start(cfg).map_err(value_err)?) })
        }

        /// The base URL, such as `http://127.0.0.1:41234`.
        fn url(&self) -> PyResult<String> {
            Ok(self.running()?.url())
        }

        /// Advances the clock by `secs` seconds, so pins expire without waiting.
        fn advance_clock(&self, secs: u64) -> PyResult<()> {
            self.running()?.advance_clock(secs);
            Ok(())
        }

        /// Stops the server and starts it again on the same address. Everything in memory is lost;
        /// specs, pins, and the snapshots pins keep come back from the state directory, when
        /// there is one.
        fn restart(&mut self) -> PyResult<()> {
            self.server.as_mut().ok_or_else(|| value_err("the fake server is closed"))?.restart().map_err(value_err)
        }

        fn close(&mut self) {
            self.server = None;
        }
    }
}
