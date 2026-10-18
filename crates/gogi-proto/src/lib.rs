//! The council API contract as code, with no I/O.
//!
//! `docs/council-api.md` is the contract; this crate is what a client or a server needs to
//! speak it:
//! - wire types for every council API request and response (`context`, `pins`, `reads`,
//!   `generation`, `decision`, `identity`, `error`);
//! - the rules a request must meet, checked the way mkviolet checks them
//!   (`message::validate`, `Context::check`, `spec::parse_spec`);
//! - canonical JSON and `spec_id` (`canon`);
//! - the pooling math in `f64` and the check that a decision's numbers obey it
//!   (`math::pool`, `math::verify`).

pub mod canon;
pub mod context;
pub mod decision;
pub mod error;
pub mod generation;
pub mod identity;
pub mod ids;
pub mod math;
pub mod message;
pub mod pins;
pub mod reads;
pub mod spec;

pub use canon::{canonical_json, sha256_hex, spec_id_of};
pub use context::{Context, ContextBuild, ContextBuilt, SnapshotInfo, Tier, Tool, Usage};
pub use error::{ErrorBody, ErrorDetail, ErrorType, Invalid};
pub use ids::{SnapshotId, SpecId};
pub use message::{Message, Role, ToolCall};

/// Accepts `null` or a value but, unlike a plain `Option`, requires the member.
pub(crate) fn required_nullable<'de, D, T>(de: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    <Option<T> as serde::Deserialize>::deserialize(de)
}

pub(crate) fn is_false(b: &bool) -> bool {
    !*b
}
