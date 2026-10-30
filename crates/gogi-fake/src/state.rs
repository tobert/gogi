//! What the fake holds: its served models, snapshots, pins, and specs.
//!
//! Specs and pins, with the snapshots they keep, are written to `state_dir` and survive a
//! restart. Other snapshots live in memory only.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use gogi_proto::canon::content_id;
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
    /// It only embeds, with the toy embedding model (`gogi_toy::embed`): it declares `embed` and
    /// no `generate`, and a build, a decision, a raw read, or a generation that names it is a
    /// `400` naming `model`.
    pub embeds: bool,
}

impl FakeModel {
    pub fn ready(name: &str, seed: u64) -> FakeModel {
        FakeModel { name: name.into(), aliases: vec![], seed, status: ModelStatus::Ready, embeds: false }
    }

    /// A ready served model that only embeds.
    pub fn embedding(name: &str, seed: u64) -> FakeModel {
        FakeModel { embeds: true, ..FakeModel::ready(name, seed) }
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
    /// Hold a cut reply's head sealed: a build that starts from it reads a rounded model state, as
    /// a server that rounds model state at a boundary does.
    pub seal_cut_heads: bool,
    /// The largest object, in bytes.
    pub object_bytes: u64,
    /// The most object bytes all keeps together hold.
    pub keep_bytes: u64,
    /// The media types every served model reads, as identity's `media.inputs` lists them. Empty:
    /// the served models read text only.
    pub media_inputs: Vec<String>,
    /// The media types every served model writes, as identity's `media.outputs` lists them.
    pub media_outputs: Vec<String>,
    /// The longest input a served model that embeds reads, with its task prompt: its
    /// `limits.context_tokens`.
    pub embed_tokens: usize,
}

impl Default for Config {
    fn default() -> Self {
        let toy = FakeModel { aliases: vec!["toy".into()], ..FakeModel::ready("gogi-toy-1", 0) };
        Config { addr: "127.0.0.1:0".into(), models: vec![toy], embed_tokens: 512, state_dir: None, context_tokens: 4096, decision_context_tokens: 3072, pin_bytes: 1 << 20, max_snapshots: 10_000, seal_cut_heads: false, object_bytes: 1 << 20, keep_bytes: 8 << 20, media_inputs: vec!["image/*".into(), "audio/*".into()], media_outputs: vec!["application/x-gogi-toy".into()] }
    }
}

pub struct Snap {
    /// The served model the snapshot belongs to.
    pub model: String,
    pub tokens: Arc<Vec<u32>>,
    pub parent: Option<String>,
    /// A build that starts from it reads its model state rounded.
    pub sealed: bool,
    born: u64,
}

pub struct Pins {
    pub snapshots: Vec<String>,
    /// Unix seconds.
    pub expires: u64,
}

pub struct Keeps {
    pub objects: Vec<String>,
    /// Unix seconds.
    pub expires: u64,
}

pub struct Store {
    pub cfg: Config,
    pub snaps: HashMap<String, Snap>,
    pub pins: BTreeMap<String, Pins>,
    pub specs: indexmap::IndexMap<String, Value>,
    /// Object id to its media type and bytes. The fake never evicts an object.
    pub objects: HashMap<String, (String, Vec<u8>)>,
    pub keeps: BTreeMap<String, Keeps>,
    /// A pending id to the content id of the part it named, once sealed.
    pub pending: HashMap<String, String>,
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
    content_id(&bytes)
}

impl Store {
    pub fn new(cfg: Config) -> Result<Store, String> {
        let mut s = Store { cfg, snaps: HashMap::new(), pins: BTreeMap::new(), specs: Default::default(), objects: HashMap::new(), keeps: BTreeMap::new(), pending: HashMap::new(), skew: 0, next_born: 0 };
        s.load()?;
        Ok(s)
    }

    /// The most body bytes the server reads: `object_bytes`, or 16 MiB for JSON when that is more.
    pub fn body_limit(&self) -> u64 {
        self.cfg.object_bytes.max(16 << 20)
    }

    pub fn now(&self) -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) + self.skew
    }

    pub fn expires_in(&self, expires: u64) -> u64 {
        expires.saturating_sub(self.now())
    }

    /// Drops pins and keeps whose `ttl_s` has run out.
    pub fn expire_pins(&mut self) {
        let now = self.now();
        let before = self.pins.len() + self.keeps.len();
        self.pins.retain(|_, p| p.expires > now);
        self.keeps.retain(|_, k| k.expires > now);
        if self.pins.len() + self.keeps.len() != before {
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
        self.snaps.insert(id, Snap { model: model.to_string(), tokens: Arc::new(tokens.to_vec()), parent, sealed: false, born: self.next_born });
        self.evict();
    }

    /// Seals a held snapshot, so a build that starts from it reads its model state rounded.
    pub fn seal(&mut self, id: &str) {
        if let Some(s) = self.snaps.get_mut(id) {
            s.sealed = true;
        }
    }

    /// Whether a held snapshot is sealed.
    pub fn sealed(&self, id: &str) -> bool {
        self.snaps.get(id).is_some_and(|s| s.sealed)
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
            kept.iter().filter_map(|id| self.snaps.get(id).map(|s| (id.clone(), json!({"model": s.model, "tokens": *s.tokens, "parent": s.parent, "sealed": s.sealed})))).collect();
        let pins: serde_json::Map<String, Value> = self.pins.iter().map(|(o, p)| (o.clone(), json!({"snapshots": p.snapshots, "expires": p.expires}))).collect();
        let keeps: serde_json::Map<String, Value> = self.keeps.iter().map(|(o, k)| (o.clone(), json!({"objects": k.objects, "expires": k.expires}))).collect();
        let kept: BTreeSet<&String> = self.keeps.values().flat_map(|k| k.objects.iter()).collect();
        let objects: serde_json::Map<String, Value> = kept.iter().filter_map(|id| self.objects.get(*id).map(|(mime, _)| ((*id).clone(), json!(mime)))).collect();
        let v = json!({"specs": self.specs, "pins": pins, "snapshots": snaps, "keeps": keeps, "objects": objects});
        let objects_dir = dir.join("objects");
        let _ = std::fs::create_dir_all(&objects_dir);
        for id in &kept {
            if let Some((_, bytes)) = self.objects.get(*id) {
                let file = objects_dir.join(id.trim_start_matches("blake3:"));
                let whole = std::fs::read(&file).is_ok_and(|b| content_id(&b) == **id);
                let tmp = objects_dir.join(format!("{}.tmp", id.trim_start_matches("blake3:")));
                if !whole && std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, &file)).is_err() {
                    eprintln!("gogi-fake: could not write {}", file.display());
                }
            }
        }
        // Files of objects no keep holds go, so the directory holds the kept objects only.
        for e in std::fs::read_dir(&objects_dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !kept.iter().any(|id| id.trim_start_matches("blake3:") == name) {
                let _ = std::fs::remove_file(e.path());
            }
        }
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
        // Pins first, so that holding the snapshots below never evicts a pinned one.
        let now = self.now();
        for (o, p) in v["pins"].as_object().into_iter().flatten() {
            let expires = p["expires"].as_u64().unwrap_or(0);
            if expires > now {
                let snapshots = p["snapshots"].as_array().into_iter().flatten().filter_map(|s| s.as_str().map(String::from)).collect();
                self.pins.insert(o.clone(), Pins { snapshots, expires });
            }
        }
        // Parents first: a parent has fewer tokens than the snapshots built on it.
        let mut snaps: Vec<(&String, &Value)> = v["snapshots"].as_object().into_iter().flatten().collect();
        snaps.sort_by_key(|(_, s)| s["tokens"].as_array().map_or(0, |t| t.len()));
        for (id, s) in snaps {
            let tokens: Vec<u32> = s["tokens"].as_array().into_iter().flatten().filter_map(|t| t.as_u64().map(|t| t as u32)).collect();
            self.hold(id.clone(), s["model"].as_str().unwrap_or(""), &tokens, s["parent"].as_str().map(String::from));
            if s["sealed"] == json!(true) {
                self.seal(id);
            }
        }
        // A missing or damaged object file drops that object: a client puts it again, as after an
        // eviction.
        for (id, mime) in v["objects"].as_object().into_iter().flatten() {
            let file = dir.join("objects").join(id.trim_start_matches("blake3:"));
            match std::fs::read(&file) {
                Ok(bytes) if content_id(&bytes) == *id => {
                    self.objects.insert(id.clone(), (mime.as_str().unwrap_or("").to_string(), bytes));
                }
                Ok(_) => eprintln!("gogi-fake: {} does not hash to {}; the object is dropped", file.display(), id),
                Err(e) => eprintln!("gogi-fake: {}: {}; the object is dropped", file.display(), e),
            }
        }
        for (o, k) in v["keeps"].as_object().into_iter().flatten() {
            let expires = k["expires"].as_u64().unwrap_or(0);
            if expires > now {
                let objects: Vec<String> = k["objects"].as_array().into_iter().flatten().filter_map(|s| s.as_str().map(String::from)).filter(|id| self.objects.contains_key(id)).collect();
                if !objects.is_empty() {
                    self.keeps.insert(o.clone(), Keeps { objects, expires });
                }
            }
        }
        Ok(())
    }
}
