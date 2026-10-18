//! `GET /council/v2/identity`: who computed a number, what the server limits, and what it can do.

use serde::{Deserialize, Serialize};

/// What a server can do beyond the base contract. A client ignores one it does not know.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Park,
    Warm,
    DryRun,
    Persist,
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
    /// Snapshots report `tier`, and pinned snapshots move between tiers.
    Tiers,
    /// Generations accept `decisions` that ride with them.
    Ride,
    #[serde(other)]
    Unknown,
}

/// A server's limits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub context_tokens: u64,
    pub state_bytes: u64,
    pub contexts_per_decision: u64,
    pub choice_options: u64,
    pub default_timeout_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub questions_per_spec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_context_tokens: Option<u64>,
}

/// The response to `GET /council/v2/identity`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerIdentity {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
    pub weight_hash: String,
    pub tokenizer_hash: String,
    pub template: String,
    pub engine: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_stop: Option<String>,
    pub limits: Limits,
    pub capabilities: Vec<Capability>,
    /// The server's own route prefixes, such as `["/mk/v1"]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<String>>,
}

impl ServerIdentity {
    pub fn has(&self, c: Capability) -> bool {
        self.capabilities.contains(&c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// mkviolet's identity, as `GET /council/v2/identity` returned it.
    const MKVIOLET: &str = r#"{"model":"google/gemma-4-26B-A4B-it","aliases":[],"weight_hash":"0x1EBEA246FC63EB7F","tokenizer_hash":"sha256:4d777ef5bdc1aa36","engine":"mkviolet:a93ed27853c9835c+numbers:15c10b5ac08a8126","device":"2x Intel(R) Arc(TM) Pro B70 Graphics","template":"mkv-letters-1:a57f304ef64f619e+gemma4-chat:ae53464bf3be2580","limits":{"context_tokens":65535,"state_bytes":65536,"contexts_per_decision":8,"questions_per_spec":16,"choice_options":26,"default_timeout_ms":30000,"pin_bytes":17179869184,"snapshot_bytes":450560,"decision_context_tokens":3072},"capabilities":["reads","generate","pins","dry_run","leave_one_out"],"extensions":["/mk/v1"]}"#;

    #[test]
    fn mkviolets_identity_decodes_and_round_trips() {
        let id: ServerIdentity = serde_json::from_str(MKVIOLET).unwrap();
        assert!(id.has(Capability::Reads) && id.has(Capability::Pins) && !id.has(Capability::Think));
        assert_eq!(id.limits.decision_context_tokens, Some(3072));
        let a: serde_json::Value = serde_json::from_str(MKVIOLET).unwrap();
        assert_eq!(serde_json::to_value(&id).unwrap(), a);
    }

    #[test]
    fn an_unknown_capability_is_not_an_error() {
        let c: Vec<Capability> = serde_json::from_str(r#"["reads","telepathy"]"#).unwrap();
        assert_eq!(c, [Capability::Reads, Capability::Unknown]);
    }
}
