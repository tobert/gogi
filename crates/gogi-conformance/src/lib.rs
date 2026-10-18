//! A conformance suite for council v2 servers.
//!
//! The suite extends contexts only with the server's own responses, such as a generation's reply
//! with its call ids, and treats model-specific numbers as relations: it never assumes a
//! template, a tokenizer, or a token count. It uses unique text on each run, so a server that
//! already holds earlier runs' snapshots is checked the same way as a fresh one.

mod checks;
mod http;
mod report;

use gogi_proto::identity::ServerIdentity;
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
}

pub(crate) struct Suite {
    pub http: http::Http,
    pub r: Report,
    pub identity: Option<ServerIdentity>,
    pub nonce: String,
}

/// Runs every check against the server at `opts.url` and returns what happened. With `print`,
/// each check prints as it finishes.
pub fn run(mut opts: Opts, print: bool) -> Report {
    let nonce = {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        format!("{:x}{:x}", t & 0xffff_ffff_ffff, std::process::id())
    };
    let mut s = Suite { http: http::Http::new(&opts.url), r: Report::new(print), identity: None, nonce };
    if s.identity() {
        s.validation();
        s.builds();
        s.generations();
        s.boundaries();
        s.pins();
        s.specs();
        s.decisions();
        s.reads();
        s.ride();
        s.replay();
        match opts.restart.as_mut() {
            Some(restart) => s.restart(restart),
            None => s.r.skip("specs and pins survive a restart", "run with --allow-restart <cmd> to check"),
        }
    }
    s.r
}

pub(crate) fn num(v: &Value) -> i64 {
    v.as_i64().unwrap_or(-1)
}
