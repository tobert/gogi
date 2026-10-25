//! A fake council API server over a toy deterministic model.
//!
//! It exists so a client has something to talk to in tests and so the conformance suite can be
//! tested against a server whose behavior is known. It keeps the contract's semantics: snapshots
//! at the boundaries the rendering keeps, builds from the longest held snapshot, pins with an
//! owner and a `ttl_s`, specs held by content, decisions, raw reads, and generations, with replay
//! bit for bit, for one or more served models whose status a test sets. It is not a framework:
//! nothing plugs into it.
//!
//! `docs/gogi-fake.md` documents the toy template. The toy model is the `gogi-toy` crate.

mod api;
mod http;
mod state;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

pub use gogi_proto::identity::ModelStatus;
pub use state::{BYTES_PER_TOKEN, Config, FakeModel};

/// A running fake server.
pub struct Server {
    store: Arc<Mutex<state::Store>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    addr: std::net::SocketAddr,
    cfg: Config,
}

impl Server {
    pub fn start(cfg: Config) -> Result<Server, String> {
        let store = Arc::new(Mutex::new(state::Store::new(cfg.clone())?));
        Self::serve(cfg, store)
    }

    fn serve(cfg: Config, store: Arc<Mutex<state::Store>>) -> Result<Server, String> {
        let listener = std::net::TcpListener::bind(&cfg.addr).map_err(|e| format!("{}: {}", cfg.addr, e))?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (store, stop) = (store.clone(), stop.clone());
            std::thread::spawn(move || http::serve(listener, store, stop))
        };
        Ok(Server { store, stop, thread: Some(thread), addr, cfg })
    }

    /// The base URL, such as `http://127.0.0.1:41234`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Advances the clock by `secs` seconds, so pins expire without waiting.
    pub fn advance_clock(&self, secs: u64) {
        self.store.lock().unwrap().skew += secs;
    }

    /// Sets a served model's status, as a server does when it loads or unloads one. A restart
    /// returns each served model to its status in the config.
    pub fn set_status(&self, model: &str, status: ModelStatus) -> Result<(), String> {
        let mut store = self.store.lock().unwrap();
        let Some(m) = store.cfg.models.iter_mut().find(|m| m.name == model) else {
            return Err(format!("the status is unchanged: the fake serves no model {}", model));
        };
        m.status = status;
        Ok(())
    }

    /// Stops the server and starts a new one on the same address. Everything in memory is lost;
    /// specs, pins, the snapshots pins keep, keeps, and the objects keeps hold come back from the
    /// state directory, when there is one.
    pub fn restart(&mut self) -> Result<(), String> {
        self.shutdown();
        let cfg = Config { addr: self.addr.to_string(), ..self.cfg.clone() };
        let store = Arc::new(Mutex::new(state::Store::new(cfg.clone())?));
        *self = Self::serve(cfg, store)?;
        Ok(())
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown();
    }
}
