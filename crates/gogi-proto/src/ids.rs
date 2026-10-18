//! Snapshot ids, `<scheme>:<hex>`, spec ids, `sha256:` and 64 lowercase hex digits, and the
//! headers that name a request.

use serde::{Deserialize, Serialize};

fn is_lower_hex(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A scheme of 1 to 16 lowercase letters and digits, a colon, then 32 to 128 lowercase hex digits.
fn is_snapshot_id(s: &str) -> bool {
    s.split_once(':').is_some_and(|(scheme, hex)| {
        (1..=16).contains(&scheme.len())
            && scheme.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            && (32..=128).contains(&hex.len())
            && is_lower_hex(hex)
    })
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

fn is_spec_id(s: &str) -> bool {
    s.strip_prefix("sha256:").is_some_and(|hex| hex.len() == 64 && is_lower_hex(hex))
}

macro_rules! prefixed_id {
    ($(#[$doc:meta])* $name:ident, $check:ident) => {
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
                if $check(&s) { Ok(Self(s)) } else { Err(format!(concat!("not a ", stringify!($name), ": {:?}"), s)) }
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
    /// The opaque address of one model state, such as `snap:` or `blake3:` and 64 hex digits. A
    /// client never derives one.
    SnapshotId,
    is_snapshot_id
);
prefixed_id!(
    /// `sha256:` and 64 hex digits: the hash of a spec's canonical JSON.
    SpecId,
    is_spec_id
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_id_is_a_scheme_and_lowercase_hex() {
        for good in [format!("snap:{}", "ab".repeat(32)), format!("blake3:{}", "0".repeat(64)), format!("sha256:{}", "a".repeat(32)), format!("k2:{}", "f".repeat(128))] {
            assert_eq!(SnapshotId::parse(good.clone()).unwrap().as_str(), good);
        }
        for bad in [
            String::new(),
            "snap:".into(),
            format!("snap:{}", "AB".repeat(32)),
            format!("snap:{}", "a".repeat(31)),
            format!("snap:{}", "a".repeat(129)),
            format!("Snap:{}", "a".repeat(64)),
            format!(":{}", "a".repeat(64)),
            format!("{}:{}", "s".repeat(17), "a".repeat(64)),
            format!("snap:{}/x", "a".repeat(64)),
        ] {
            assert!(SnapshotId::parse(bad.clone()).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_spec_id_is_sha256_and_64_lowercase_hex_digits() {
        assert!(SpecId::parse(format!("snap:{}", "0".repeat(64))).is_err());
        assert!(SpecId::parse(format!("sha256:{}", "0".repeat(63))).is_err());
        assert!(SpecId::parse(format!("sha256:{}", "0".repeat(64))).is_ok());
        assert!(serde_json::from_str::<SpecId>("\"sha256:zz\"").is_err());
    }
}
