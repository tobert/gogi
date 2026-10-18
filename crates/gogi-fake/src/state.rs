//! What the fake holds: its served models, snapshots, pins, and specs.
//!
//! Specs and pins, with the snapshots they keep, are written to `state_dir` and survive a
//! restart. Other snapshots live in memory only.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use gogi_proto::canon::sha256_hex;
use gogi_proto::identity::ModelStatus;
use serde_json::{Value, json};

pub const IDENTITY_KEY: &str = "gogi-fake-1";
/// What one token of a snapshot costs, as `limits.snapshot_bytes` reports.
pub const BYTES_PER_TOKEN: u64 = 64;

/// A served model of the fake. Its `seed` makes its numbers its own.
#[derive(Clone, Debug)]
pub struct FakeModel {
    pub name: String,
    pub aliases: Vec<String>,
    pub seed: u64,
    pub status: ModelStatus,
}

impl FakeModel {
    pub fn ready(name: &str, seed: u64) -> FakeModel {
        FakeModel { name: name.into(), aliases: vec![], seed, status: ModelStatus::Ready }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub addr: String,
    /// The served models, in the order identity lists them.
    pub models: Vec<FakeModel>,
    pub state_dir: Option<PathBuf>,
    /// The most tokens a prompt may have, counting a generation's prompt and reply together.
    pub context_tokens: usize,
    /// The longest context a decision reads.
    pub decision_context_tokens: usize,
    pub pin_bytes: u64,
    /// Past this many held snapshots, the oldest unpinned snapshots are evicted first.
    pub max_snapshots: usize,
}

impl Default for Config {
    fn default() -> Self {
        let toy = FakeModel { aliases: vec!["toy".into()], ..FakeModel::ready("gogi-toy-1", 0) };
        Config { addr: "127.0.0.1:0".into(), models: vec![toy], state_dir: None, context_tokens: 4096, decision_context_tokens: 3072, pin_bytes: 1 << 20, max_snapshots: 10_000 }
    }
}

pub struct Snap {
    /// The served model the snapshot belongs to.
    pub model: String,
    pub tokens: Arc<Vec<u32>>,
    pub parent: Option<String>,
    born: u64,
}

pub struct Pins {
    pub snapshots: Vec<String>,
    /// Unix seconds.
    pub expires: u64,
}

pub struct Store {
    pub cfg: Config,
    pub snaps: HashMap<String, Snap>,
    pub pins: BTreeMap<String, Pins>,
    pub specs: indexmap::IndexMap<String, Value>,
    /// Seconds added to the clock, so a test can make a pin expire without waiting.
    pub skew: u64,
    next_born: u64,
}

/// The id of served model `model`'s snapshot after `tokens`, with boundaries at the token counts
/// `counts`.
pub fn snapshot_id(model: &FakeModel, tokens: &[u32], counts: &[usize]) -> String {
    let mut bytes = Vec::with_capacity(tokens.len() * 4 + 64);
    bytes.extend_from_slice(b"gogi-fake-held-v1\0");
    bytes.extend_from_slice(IDENTITY_KEY.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(model.name.as_bytes());
    bytes.extend_from_slice(&model.seed.to_le_bytes());
    bytes.push(0);
    for t in tokens {
        bytes.extend_from_slice(&t.to_le_bytes());
    }
    bytes.push(0);
    for c in counts {
        bytes.extend_from_slice(&(*c as u64).to_le_bytes());
    }
    format!("snap:{}", sha256_hex(&bytes))
}

impl Store {
    pub fn new(cfg: Config) -> Result<Store, String> {
        let mut s = Store { cfg, snaps: HashMap::new(), pins: BTreeMap::new(), specs: Default::default(), skew: 0, next_born: 0 };
        s.load()?;
        Ok(s)
    }

    pub fn now(&self) -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) + self.skew
    }

    pub fn expires_in(&self, expires: u64) -> u64 {
        expires.saturating_sub(self.now())
    }

    /// Drops pins whose `ttl_s` has run out.
    pub fn expire_pins(&mut self) {
        let now = self.now();
        let before = self.pins.len();
        self.pins.retain(|_, p| p.expires > now);
        if self.pins.len() != before {
            self.save();
        }
    }

    pub fn has(&self, id: &str) -> bool {
        self.snaps.contains_key(id)
    }

    /// Holds a snapshot of served model `model` built on `parent`.
    pub fn hold(&mut self, id: String, model: &str, tokens: &[u32], parent: Option<String>) {
        if self.snaps.contains_key(&id) {
            return;
        }
        self.next_born += 1;
        self.snaps.insert(id, Snap { model: model.to_string(), tokens: Arc::new(tokens.to_vec()), parent, born: self.next_born });
        self.evict();
    }

    /// The tokens a snapshot adds to its parent's.
    pub fn own_tokens(&self, id: &str) -> usize {
        let s = &self.snaps[id];
        s.tokens.len() - s.parent.as_ref().and_then(|p| self.snaps.get(p)).map_or(0, |p| p.tokens.len())
    }

    /// Each pinned snapshot and the snapshots it is built on.
    pub fn kept(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for p in self.pins.values() {
            for id in &p.snapshots {
                let mut at = Some(id.clone());
                while let Some(i) = at {
                    if !out.insert(i.clone()) {
                        break;
                    }
                    at = self.snaps.get(&i).and_then(|s| s.parent.clone());
                }
            }
        }
        out
    }

    fn evict(&mut self) {
        if self.snaps.len() <= self.cfg.max_snapshots {
            return;
        }
        let kept = self.kept();
        let mut order: Vec<(u64, String)> = self.snaps.iter().filter(|(id, _)| !kept.contains(*id)).map(|(id, s)| (s.born, id.clone())).collect();
        order.sort();
        for (_, id) in order {
            if self.snaps.len() <= self.cfg.max_snapshots {
                break;
            }
            self.snaps.remove(&id);
        }
    }

    /// The bytes the snapshots kept by `ids` and their parents hold together.
    pub fn bytes_of_closure(&self, ids: &[String]) -> u64 {
        let mut seen = BTreeSet::new();
        for id in ids {
            let mut at = Some(id.clone());
            while let Some(i) = at {
                if !seen.insert(i.clone()) {
                    break;
                }
                at = self.snaps.get(&i).and_then(|s| s.parent.clone());
            }
        }
        seen.iter().filter(|i| self.snaps.contains_key(*i)).map(|i| self.own_tokens(i) as u64 * BYTES_PER_TOKEN).sum()
    }

    // ---- persistence

    pub fn save(&self) {
        let Some(dir) = &self.cfg.state_dir else { return };
        let kept = self.kept();
        let snaps: serde_json::Map<String, Value> =
            kept.iter().filter_map(|id| self.snaps.get(id).map(|s| (id.clone(), json!({"model": s.model, "tokens": *s.tokens, "parent": s.parent})))).collect();
        let pins: serde_json::Map<String, Value> = self.pins.iter().map(|(o, p)| (o.clone(), json!({"snapshots": p.snapshots, "expires": p.expires}))).collect();
        let v = json!({"specs": self.specs, "pins": pins, "snapshots": snaps});
        let _ = std::fs::create_dir_all(dir);
        let tmp = dir.join("state.json.tmp");
        if std::fs::write(&tmp, serde_json::to_vec(&v).unwrap()).and_then(|_| std::fs::rename(&tmp, dir.join("state.json"))).is_err() {
            eprintln!("gogi-fake: could not write {}", dir.display());
        }
    }

    fn load(&mut self) -> Result<(), String> {
        let Some(dir) = self.cfg.state_dir.clone() else { return Ok(()) };
        let Ok(bytes) = std::fs::read(dir.join("state.json")) else { return Ok(()) };
        let v: Value = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {}", dir.join("state.json").display(), e))?;
        for (id, spec) in v["specs"].as_object().into_iter().flatten() {
            self.specs.insert(id.clone(), spec.clone());
        }
        // Parents first: a parent has fewer tokens than the snapshots built on it.
        let mut snaps: Vec<(&String, &Value)> = v["snapshots"].as_object().into_iter().flatten().collect();
        snaps.sort_by_key(|(_, s)| s["tokens"].as_array().map_or(0, |t| t.len()));
        for (id, s) in snaps {
            let tokens: Vec<u32> = s["tokens"].as_array().into_iter().flatten().filter_map(|t| t.as_u64().map(|t| t as u32)).collect();
            self.hold(id.clone(), s["model"].as_str().unwrap_or(""), &tokens, s["parent"].as_str().map(String::from));
        }
        let now = self.now();
        for (o, p) in v["pins"].as_object().into_iter().flatten() {
            let expires = p["expires"].as_u64().unwrap_or(0);
            if expires > now {
                let snapshots = p["snapshots"].as_array().into_iter().flatten().filter_map(|s| s.as_str().map(String::from)).collect();
                self.pins.insert(o.clone(), Pins { snapshots, expires });
            }
        }
        Ok(())
    }
}
