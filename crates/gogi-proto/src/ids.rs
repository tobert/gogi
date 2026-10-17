//! Snapshot and spec ids: a prefix and 64 lowercase hex digits.

use serde::{Deserialize, Serialize};

fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

macro_rules! prefixed_id {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Checks the form `<prefix><64 lowercase hex digits>`.
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
                match s.strip_prefix($prefix) {
                    Some(rest) if is_lower_hex(rest, 64) => Ok(Self(s)),
                    _ => Err(format!(concat!("not a ", stringify!($name), ": {:?}"), s)),
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
    /// `snap:` and 64 hex digits: the opaque address of one snapshot.
    SnapshotId,
    "snap:"
);
prefixed_id!(
    /// `sha256:` and 64 hex digits: the hash of a spec's canonical JSON.
    SpecId,
    "sha256:"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_need_the_prefix_and_64_lowercase_hex_digits() {
        let good = format!("snap:{}", "ab".repeat(32));
        assert_eq!(SnapshotId::parse(good.clone()).unwrap().as_str(), good);
        for bad in ["", "snap:", &format!("snap:{}", "AB".repeat(32)), &format!("snap:{}", "a".repeat(63)), &format!("sha256:{}", "a".repeat(64))] {
            assert!(SnapshotId::parse(bad).is_err(), "{bad:?}");
        }
        assert!(SpecId::parse(format!("sha256:{}", "0".repeat(64))).is_ok());
        assert!(serde_json::from_str::<SpecId>("\"sha256:zz\"").is_err());
    }
}
