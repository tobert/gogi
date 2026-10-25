//! `GET /council/v2/identity`: the served models, who computed a number, what the server and
//! each served model limit, and what they can do.

use serde::{Deserialize, Serialize};

use crate::error::Invalid;

/// What a server can do beyond the base contract. A client ignores one it does not know.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    DryRun,
    Describe,
    LeaveOneOut,
    /// `POST /council/v2/reads`.
    Reads,
    /// `POST /council/v2/generations`.
    Generate,
    /// Decisions accept `think` with `tokens` above 0.
    Think,
    /// `/council/v2/pins`.
    Pins,
    /// `/council/v2/objects` and `/council/v2/keeps`.
    Objects,
    /// Snapshots report `tier`, and pinned snapshots move between tiers.
    Tiers,
    /// Generations accept `decisions` that ride with them.
    Ride,
    /// Generations accept an open last message, `continue: true`.
    Continuation,
    /// Reserved for a later top-level `infill` member of a generation. No server declares it yet.
    Infill,
    /// Requests are scheduled by `class` and `priority`. Every client is trusted with
    /// `priority`.
    Classes,
    #[serde(other)]
    Unknown,
}

/// Whether a served model takes requests now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelStatus {
    Ready,
    Loading,
    /// Finishes the requests it has and takes no new ones.
    Draining,
    Unloaded,
    #[serde(other)]
    Unknown,
}

/// How a request is scheduled. It never changes a number.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    #[default]
    Interactive,
    Batch,
}

/// A served model's limits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelLimits {
    pub context_tokens: u64,
    pub decision_context_tokens: u64,
    pub snapshot_bytes: u64,
    pub contexts_per_decision: u64,
    pub default_timeout_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_tokens: Option<u64>,
}

/// The server's own limits, which every served model shares.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerLimits {
    /// The longest case, in bytes.
    pub case_bytes: u64,
    pub choice_options: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub questions_per_spec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_bytes: Option<u64>,
    /// The largest object, in bytes, with `objects`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_bytes: Option<u64>,
    /// The most object bytes all keeps together hold, with `objects`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_bytes: Option<u64>,
}

/// The media types a served model reads in a message or a case, and writes in a reply. A type is
/// `type/subtype`, or `type/*` for every subtype. Text is always read and written, so a served
/// model without `media` reads and writes text only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Media {
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub outputs: Vec<String>,
}

/// Whether `pattern`, `type/subtype` or `type/*`, covers the media type `mime`.
pub fn mime_matches(pattern: &str, mime: &str) -> bool {
    match pattern.strip_suffix("/*") {
        Some(kind) => mime.split_once('/').is_some_and(|(k, _)| k == kind),
        None => pattern == mime,
    }
}

/// One served model: its identity, limits, capabilities, and status. It reports the same identity
/// in every status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServedModel {
    pub model: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub status: ModelStatus,
    pub weight_hash: String,
    pub tokenizer_hash: String,
    pub template: String,
    pub engine: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_stop: Option<String>,
    pub limits: ModelLimits,
    pub capabilities: Vec<Capability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<Media>,
}

impl ServedModel {
    /// Whether the served model reads objects of media type `mime`.
    pub fn reads(&self, mime: &str) -> bool {
        self.media.as_ref().is_some_and(|m| m.inputs.iter().any(|p| mime_matches(p, mime)))
    }

    /// The media types it reads, for an error message.
    pub fn inputs(&self) -> Vec<String> {
        self.media.as_ref().map(|m| m.inputs.clone()).unwrap_or_default()
    }

    pub fn has(&self, c: Capability) -> bool {
        self.capabilities.contains(&c)
    }

    /// Whether a request's `model` names this served model, by its `model` or an alias.
    pub fn is_named(&self, name: &str) -> bool {
        self.model == name || self.aliases.iter().any(|a| a == name)
    }

    /// The identity fields a number depends on, in a decision's `identity` shape.
    pub fn identity(&self) -> [(&'static str, &str); 5] {
        [("model", &self.model), ("weight_hash", &self.weight_hash), ("tokenizer_hash", &self.tokenizer_hash), ("template", &self.template), ("engine", &self.engine)]
    }
}

/// The response to `GET /council/v2/identity`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerIdentity {
    /// Every model the server serves, loaded or not.
    pub models: Vec<ServedModel>,
    pub limits: ServerLimits,
    pub capabilities: Vec<Capability>,
    /// The server's own route prefixes, such as `["/mk/v1"]`.
    #[serde(default)]
    pub extensions: Vec<String>,
}

impl ServerIdentity {
    /// Whether the server has a capability of its own, such as `pins`.
    pub fn has(&self, c: Capability) -> bool {
        self.capabilities.contains(&c)
    }

    /// The served model a request's `model` names, by its `model` or an alias. The caller checks
    /// the model's status.
    pub fn route(&self, model: &str) -> Result<&ServedModel, Invalid> {
        self.models
            .iter()
            .find(|m| m.is_named(model))
            .ok_or_else(|| Invalid::param("model", format!("model {} is not served here; identity lists the served models", model)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_media_pattern_covers_its_type_only() {
        assert!(mime_matches("image/*", "image/png") && mime_matches("audio/wav", "audio/wav"));
        assert!(!mime_matches("image/*", "audio/wav") && !mime_matches("image/*", "imagex/png") && !mime_matches("audio/wav", "audio/flac"));
    }

    /// mkviolet's identity, as `GET /council/v2/identity` returned it on 2026-10-18.
    const MKVIOLET: &str = r#"{"models":[{"model":"gemma-4-26b-a4b","weight_hash":"0x1EBEA246FC63EB7F","tokenizer_hash":"sha256:4d777ef5bdc1aa36","engine":"mkviolet:a93ed27853c9835c+numbers:e0cf8f63c78acb7a","device":"2x Intel(R) Arc(TM) Pro B70 Graphics","template":"mkv-letters-1:d71541e05da41620+gemma4-chat:344a60f6d8cc3423","aliases":[],"status":"ready","limits":{"context_tokens":65535,"decision_context_tokens":3072,"snapshot_bytes":450560,"contexts_per_decision":8,"default_timeout_ms":30000},"capabilities":["reads","generate","ride","dry_run","leave_one_out"]}],"limits":{"case_bytes":65536,"choice_options":26,"questions_per_spec":16,"pin_bytes":17179869184},"capabilities":["pins"],"extensions":["/mk/v1"]}"#;

    fn two_models() -> ServerIdentity {
        let mut id: ServerIdentity = serde_json::from_str(MKVIOLET).unwrap();
        let mut night = id.models[0].clone();
        night.model = "qwen-3.8-mutant-636".into();
        night.aliases = vec!["mutant".into()];
        night.status = ModelStatus::Unloaded;
        id.models.push(night);
        id
    }

    #[test]
    fn mkviolets_identity_decodes_and_round_trips() {
        let id: ServerIdentity = serde_json::from_str(MKVIOLET).unwrap();
        let m = &id.models[0];
        assert!(m.has(Capability::Reads) && id.has(Capability::Pins) && !m.has(Capability::Pins) && !m.has(Capability::Think));
        assert_eq!((m.status, m.limits.decision_context_tokens), (ModelStatus::Ready, 3072));
        let a: serde_json::Value = serde_json::from_str(MKVIOLET).unwrap();
        assert_eq!(serde_json::to_value(&id).unwrap(), a);
    }

    #[test]
    fn a_request_routes_by_name_or_alias() {
        let two = two_models();
        assert_eq!(two.route("gemma-4-26b-a4b").unwrap().model, "gemma-4-26b-a4b");
        assert_eq!(two.route("mutant").unwrap().model, "qwen-3.8-mutant-636");
        assert_eq!(two.route("qwen-3.8-mutant-636").unwrap().status, ModelStatus::Unloaded);
        for bad in ["gpt-2", ""] {
            assert_eq!(two.route(bad).unwrap_err().param.as_deref(), Some("model"), "{bad:?}");
        }
    }

    #[test]
    fn continuation_and_infill_are_capabilities() {
        let c: Vec<Capability> = serde_json::from_str(r#"["continuation","infill"]"#).unwrap();
        assert_eq!(c, [Capability::Continuation, Capability::Infill]);
        assert_eq!(serde_json::to_string(&c).unwrap(), r#"["continuation","infill"]"#);
    }

    #[test]
    fn an_unknown_capability_or_status_is_not_an_error() {
        let c: Vec<Capability> = serde_json::from_str(r#"["reads","telepathy"]"#).unwrap();
        assert_eq!(c, [Capability::Reads, Capability::Unknown]);
        assert_eq!(serde_json::from_str::<ModelStatus>(r#""warming""#).unwrap(), ModelStatus::Unknown);
    }

    #[test]
    fn a_class_is_interactive_or_batch() {
        assert_eq!(Class::default(), Class::Interactive);
        assert_eq!(serde_json::from_str::<Class>(r#""batch""#).unwrap(), Class::Batch);
        assert!(serde_json::from_str::<Class>(r#""filler""#).is_err());
    }
}
