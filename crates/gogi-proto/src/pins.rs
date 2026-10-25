//! Snapshots and the pins that keep them.

use serde::{Deserialize, Deserializer, Serialize};

use crate::context::Tier;
use crate::error::Invalid;
use crate::ids::SnapshotId;
use crate::required_nullable;

/// The longest a pin lasts without a renewing `PUT`: 86400 seconds.
pub const PIN_TTL_MAX_S: u64 = 86_400;

/// An owner is 1 to 128 printable characters without a slash.
pub fn is_owner(s: &str) -> bool {
    !s.is_empty() && s.chars().count() <= 128 && !s.chars().any(char::is_control) && !s.contains('/')
}

pub fn validate_owner(owner: &str) -> Result<(), Invalid> {
    if is_owner(owner) {
        Ok(())
    } else {
        Err(Invalid::param("owner", "an owner is printable text of up to 128 characters, without a slash"))
    }
}

/// The body of `PUT /council/v2/pins/{owner}`: the owner's whole set, kept for `ttl_s` seconds.
/// Ids stay text: a malformed id is a `400`, and a pin on an id the server does not hold reports
/// `held: false`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinsPut {
    pub snapshots: Vec<String>,
    pub ttl_s: u64,
}

impl PinsPut {
    /// Checks a `ttl_s` of 1 to [`PIN_TTL_MAX_S`], and that each id is a whole content id.
    pub fn validate(&self) -> Result<(), Invalid> {
        if self.ttl_s == 0 || self.ttl_s > PIN_TTL_MAX_S {
            return Err(Invalid::param("ttl_s", format!("ttl_s is 1 to {}", PIN_TTL_MAX_S)));
        }
        for (i, id) in self.snapshots.iter().enumerate() {
            if let Some(problem) = crate::ids::content_id_problem(id) {
                return Err(Invalid::param("snapshots", format!("nothing is pinned: snapshots[{}] {}", i, problem)));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinHeld {
    pub id: SnapshotId,
    /// `false`: the server does not hold the snapshot, so the pin keeps nothing.
    pub held: bool,
}

/// The response to `PUT /council/v2/pins/{owner}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinsPutResult {
    pub owner: String,
    pub snapshots: Vec<PinHeld>,
    /// `null` for an empty set, which pins nothing.
    #[serde(deserialize_with = "required_nullable")]
    pub expires_in_s: Option<u64>,
}

/// The response to `GET /council/v2/pins/{owner}`. `snapshots` lists ids; an object with an
/// `id` is also accepted, as kaijutsu accepts it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PinSet {
    pub owner: String,
    #[serde(deserialize_with = "pinned_ids")]
    pub snapshots: Vec<String>,
    pub expires_in_s: u64,
}

fn pinned_ids<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Entry {
        Id(String),
        Object { id: String },
    }
    let ids: Vec<String> = Vec::<Entry>::deserialize(de)?.into_iter().map(|e| match e { Entry::Id(id) | Entry::Object { id } => id }).collect();
    match ids.iter().find_map(|id| crate::ids::content_id_problem(id)) {
        Some(p) => Err(serde::de::Error::custom(format!("not a snapshot id: {}", p))),
        None => Ok(ids),
    }
}

/// An owner that pins a snapshot, and the seconds its pins have left.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinOwner {
    pub owner: String,
    pub expires_in_s: u64,
}

/// The response to `GET /council/v2/snapshots/{id}`. `pins` names the owners that pin this
/// snapshot, not the owners whose pinned snapshots are built on it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotDetail {
    pub id: SnapshotId,
    /// The served model the snapshot belongs to.
    pub model: String,
    pub tokens: u64,
    pub tier: Tier,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    pub pins: Vec<PinOwner>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_owner_is_up_to_128_characters_not_bytes() {
        assert!(is_owner(&"鍵".repeat(128)) && !is_owner(&"鍵".repeat(129)));
        assert!(!is_owner("") && !is_owner("a/b") && !is_owner("a\nb"));
    }

    #[test]
    fn a_response_with_an_id_that_is_not_whole_does_not_decode() {
        assert!(serde_json::from_value::<PinSet>(json!({"owner": "k", "snapshots": ["snap:abc"], "expires_in_s": 1})).is_err());
        assert!(serde_json::from_value::<PinsPutResult>(json!({"owner": "k", "snapshots": [{"id": "blake3:ab", "held": true}], "expires_in_s": 1})).is_err());
        let r = |hash: &str| serde_json::from_value::<crate::decision::Read>(json!({"context": 0, "snapshot": null, "answers": {}, "rendered_hash": hash}));
        assert!(r(&format!("blake3:{}", "e".repeat(64))).is_ok());
        assert!(r(&"e".repeat(64)).is_err() && r("sha256:ab").is_err());
    }

    #[test]
    fn a_pin_on_an_id_that_is_not_a_whole_content_id_is_refused_by_name() {
        let whole = format!("blake3:{}", "a".repeat(64));
        PinsPut { snapshots: vec![whole.clone()], ttl_s: 60 }.validate().unwrap();
        let e = PinsPut { snapshots: vec![whole, "blake3:aaaa".into()], ttl_s: 60 }.validate().unwrap_err();
        assert!(e.param.as_deref() == Some("snapshots") && e.message.contains("snapshots[1] \"blake3:aaaa\""), "{e:?}");
    }

    #[test]
    fn ttl_is_one_second_to_a_day() {
        let put = |ttl_s| PinsPut { snapshots: vec![], ttl_s };
        for ok in [1, 3600, PIN_TTL_MAX_S] {
            put(ok).validate().unwrap();
        }
        for bad in [0, PIN_TTL_MAX_S + 1] {
            assert_eq!(put(bad).validate().unwrap_err().param.as_deref(), Some("ttl_s"));
        }
    }

    #[test]
    fn an_owner_is_printable_without_a_slash() {
        for ok in ["kernel-7f3a", "a", &"k".repeat(128), "名前"] {
            assert!(is_owner(ok), "{ok:?}");
        }
        for bad in ["", "a/b", "tab\there", &"k".repeat(129)] {
            assert!(!is_owner(bad), "{bad:?}");
            assert!(validate_owner(bad).is_err());
        }
    }

    #[test]
    fn a_pin_body_refuses_a_stray_member() {
        assert!(serde_json::from_value::<PinsPut>(json!({"snapshots": [], "ttl_s": 5, "extra": 1})).is_err());
    }

    #[test]
    fn a_put_response_decodes_with_held_flags_and_a_null_expiry() {
        let r: PinsPutResult = serde_json::from_value(json!({"owner": "k", "snapshots": [{"id": "blake3:1111111111111111111111111111111111111111111111111111111111111111", "held": false}], "expires_in_s": null})).unwrap();
        assert_eq!((r.snapshots[0].held, r.expires_in_s), (false, None));
    }

    #[test]
    fn a_get_response_accepts_ids_or_objects() {
        for snapshots in [json!(["blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]), json!([{"id": "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "held": true}])] {
            let s: PinSet = serde_json::from_value(json!({"owner": "k", "snapshots": snapshots, "expires_in_s": 9})).unwrap();
            assert_eq!(s.snapshots, ["blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]);
        }
    }
}
