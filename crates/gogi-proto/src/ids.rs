//! Content ids, `blake3:` and 64 lowercase hex digits, for snapshots, specs, and objects, and the
//! headers that name a request.

use serde::{Deserialize, Serialize};

fn is_lower_hex(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The scheme of every content id.
pub const SCHEME: &str = "blake3";

/// The hex digits of a content id: the whole 256-bit hash.
pub const HEX_DIGITS: usize = 64;

/// Why `s` is not a content id, or `None` when it is one. A content id is `blake3:` and 64
/// lowercase hex digits: the whole hash, never a short form or a prefix.
pub fn content_id_problem(s: &str) -> Option<String> {
    let rule = "a content id is `blake3:` and 64 lowercase hex digits";
    let Some((scheme, hex)) = s.split_once(':') else {
        return Some(format!("{:?} has no scheme: {}", s, rule));
    };
    if scheme != SCHEME {
        return Some(format!("{:?} has the scheme `{}`: {}", s, scheme, rule));
    }
    if let Some((i, c)) = hex.char_indices().find(|(_, c)| !c.is_ascii_hexdigit()) {
        return Some(format!("{:?} has {:?} at hex digit {}, which is not a hex digit: {}", s, c, i + 1, rule));
    }
    if hex.len() != HEX_DIGITS {
        return Some(format!("{:?} has {} hex digits, not {}: send the whole id, because no server accepts a short form or a prefix", s, hex.len(), HEX_DIGITS));
    }
    if !is_lower_hex(hex) {
        return Some(format!("{:?} has uppercase hex digits: {}", s, rule));
    }
    None
}

/// The response header that carries the id a server gives each request.
pub const REQUEST_ID_HEADER: &str = "request-id";

/// The W3C Trace Context request header a client sends to join the server's spans to its trace.
/// It travels only as a header, never in a body.
pub const TRACEPARENT_HEADER: &str = "traceparent";

/// A request id: printable ASCII without spaces, 1 to 128 bytes. A client treats it as opaque.
pub fn is_request_id(s: &str) -> bool {
    (1..=128).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_graphic())
}

macro_rules! prefixed_id {
    ($(#[$doc:meta])* $name:ident, $what:literal) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Checks the id's form.
            pub fn parse(s: impl Into<String>) -> Result<Self, String> {
                Self::try_from(s.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = String;
            fn try_from(s: String) -> Result<Self, String> {
                match content_id_problem(&s) {
                    None => Ok(Self(s)),
                    Some(problem) => Err(format!(concat!("not a ", $what, ": {}"), problem)),
                }
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> String {
                id.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

prefixed_id!(
    /// The address of one model state. A client treats it as opaque and never derives one.
    SnapshotId,
    "snapshot id"
);
prefixed_id!(
    /// The BLAKE3 hash of a spec's canonical JSON.
    SpecId,
    "spec id"
);
prefixed_id!(
    /// The content id of other bytes, such as the text a decision read was conditioned on.
    ContentId,
    "content id"
);

#[cfg(test)]
mod tests {
    use super::*;

    fn is_content_id(s: &str) -> bool {
        content_id_problem(s).is_none()
    }

    fn id(hex: &str) -> String {
        format!("blake3:{}", hex)
    }

    #[test]
    fn a_content_id_is_blake3_and_64_lowercase_hex_digits() {
        for good in [id(&"0".repeat(64)), id(&"ab".repeat(32)), id(&"0123456789abcdef".repeat(4))] {
            assert!(is_content_id(&good), "{good}");
            assert_eq!(SnapshotId::parse(good.clone()).unwrap().as_str(), good);
            assert_eq!(SpecId::parse(good.clone()).unwrap().as_str(), good);
        }
        for bad in [
            String::new(),
            "blake3:".into(),
            id(&"a".repeat(63)),
            id(&"a".repeat(65)),
            id(&"a".repeat(12)),
            id(&"AB".repeat(32)),
            id(&format!("{}g", "a".repeat(63))),
            format!("snap:{}", "a".repeat(64)),
            format!("sha256:{}", "a".repeat(64)),
            format!("Blake3:{}", "a".repeat(64)),
            "a".repeat(64),
            format!("blake3:{}/x", "a".repeat(64)),
        ] {
            assert!(!is_content_id(&bad), "{bad:?}");
            assert!(SnapshotId::parse(bad.clone()).is_err(), "{bad:?}");
            assert!(SpecId::parse(bad.clone()).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_problem_names_the_id_and_what_is_wrong() {
        let short = id("7cc44eb19abc");
        let p = content_id_problem(&short).unwrap();
        assert!(p.contains(&format!("{:?}", short)) && p.contains("12 hex digits") && p.contains("short form"), "{p}");
        let p = content_id_problem(&format!("sha256:{}", "a".repeat(64))).unwrap();
        assert!(p.contains("the scheme `sha256`"), "{p}");
        let p = content_id_problem(&"a".repeat(64)).unwrap();
        assert!(p.contains("has no scheme"), "{p}");
        let p = content_id_problem(&id(&"A".repeat(64))).unwrap();
        assert!(p.contains("uppercase"), "{p}");
        let p = content_id_problem(&id(&format!("{}z", "a".repeat(9)))).unwrap();
        assert!(p.contains("'z' at hex digit 10"), "{p}");
    }

    #[test]
    fn a_decode_error_says_which_id_it_was() {
        let e = serde_json::from_str::<SpecId>("\"sha256:zz\"").unwrap_err().to_string();
        assert!(e.starts_with("not a spec id: "), "{e}");
        let e = SnapshotId::parse(id("ab")).unwrap_err();
        assert!(e.starts_with("not a snapshot id: "), "{e}");
    }
}
