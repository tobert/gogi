//! A conformance suite for council API servers.
//!
//! The suite extends contexts only with the server's own responses, such as a generation's reply
//! with its call ids, and treats model-specific numbers as relations: it never assumes a
//! template, a tokenizer, or a token count. It uses unique text on each run, so a server that
//! already holds earlier runs' snapshots is checked the same way as a fresh one.
//!
//! It checks one served model, named by `Opts::model` or else the first `ready` one in identity,
//! and names it in every request that reads a context. It checks routing against the other served
//! models identity lists without assuming which of them are loaded.

mod checks;
mod http;
mod report;

use gogi_proto::identity::{ServedModel, ServerIdentity};
use serde_json::Value;

pub use report::{Check, Report, Status};

/// How to restart the server, for the checks that need it.
pub enum Restart {
    /// A shell command, run with `sh -c`.
    Command(String),
    /// A function that restarts the server in-process, for tests.
    Hook(Box<dyn FnMut() -> Result<(), String>>),
}

pub struct Opts {
    pub url: String,
    pub restart: Option<Restart>,
    /// The served model to check, by its `model` or an alias. `None`: the first `ready` one.
    pub model: Option<String>,
}

pub(crate) struct Suite {
    pub http: http::Http,
    pub r: Report,
    pub identity: Option<ServerIdentity>,
    /// The served model the checks read.
    pub model: Option<ServedModel>,
    pub nonce: String,
    /// The `request-id` of each response, in order.
    pub request_ids: Vec<String>,
    /// Responses whose `request-id` header is missing or malformed, or whose error body's
    /// `request_id` differs from it.
    pub request_id_faults: Vec<String>,
}

/// Runs every check against the server at `opts.url` and returns what happened. With `print`,
/// each check prints as it finishes.
pub fn run(mut opts: Opts, print: bool) -> Report {
    let nonce = {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        format!("{:x}{:x}", t & 0xffff_ffff_ffff, std::process::id())
    };
    let mut s = Suite { http: http::Http::new(&opts.url), r: Report::new(print), identity: None, model: None, nonce, request_ids: Vec::new(), request_id_faults: Vec::new() };
    if s.identity(opts.model.as_deref()) {
        s.validation();
        s.models();
        s.builds();
        s.generations();
        s.continuations();
        s.boundaries();
        s.pins();
        s.specs();
        s.ids();
        s.objects();
        s.media();
        s.decisions();
        s.reads();
        s.ride();
        s.replay();
        s.tracing();
        match opts.restart.as_mut() {
            Some(restart) => s.restart(restart),
            None => s.r.skip("specs and pins survive a restart", "run with --allow-restart <cmd> to check"),
        }
        s.request_ids();
    }
    s.r
}

pub(crate) fn num(v: &Value) -> i64 {
    v.as_i64().unwrap_or(-1)
}
