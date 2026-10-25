//! Objects, the bytes a request names by content id, and the keeps that hold them.
//!
//! An object is bytes and a media type. Its id is the content id of its bytes. A server may evict
//! an object that no keep holds, as it evicts a snapshot that no pin keeps.

use serde::{Deserialize, Serialize};

use crate::canon::content_id;
use crate::error::Invalid;
use crate::ids::content_id_problem;
use crate::required_nullable;

/// The longest a keep lasts without a renewing `PUT`: 30 days, 2,592,000 seconds. A client cannot
/// always make an object again, so a keep lasts longer than a pin.
pub const KEEP_TTL_MAX_S: u64 = 2_592_000;

/// Why `s` is not a media type, or `None` when it is one. A media type is `type/subtype` in
/// lowercase, with no parameters: each part is 1 to 127 letters, digits, and `!#$&-^_.+`, and
/// starts with a letter or a digit.
pub fn mime_problem(s: &str) -> Option<String> {
    let rule = "a media type is `type/subtype` in lowercase with no parameters, such as `audio/wav`";
    if s.contains(';') {
        return Some(format!("{:?} has parameters: {}", s, rule));
    }
    let Some((kind, sub)) = s.split_once('/') else {
        return Some(format!("{:?} has no `/`: {}", s, rule));
    };
    for part in [kind, sub] {
        let ok = (1..=127).contains(&part.len())
            && part.bytes().next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            && part.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"!#$&-^_.+".contains(&b));
        if !ok {
            let hint = if part.bytes().any(|b| b.is_ascii_uppercase()) { "write it in lowercase" } else { rule };
            return Some(format!("{:?} has the part {:?}, which is not a media type part: {}", s, part, hint));
        }
    }
    None
}

/// An object a request names: the content id of its bytes and how to read them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectRef {
    pub id: String,
    pub mime: String,
}

impl ObjectRef {
    /// Checks a whole content id and a media type. `at` names the member, such as
    /// `messages[2].content[1]`.
    pub fn check(&self, at: &str) -> Result<(), Invalid> {
        if let Some(p) = content_id_problem(&self.id) {
            return Err(Invalid::param(at, format!("nothing ran: {}.id {}", at, p)));
        }
        if let Some(p) = mime_problem(&self.mime) {
            return Err(Invalid::param(at, format!("nothing ran: {}.mime {}", at, p)));
        }
        Ok(())
    }
}

/// An object a request names: the member that names it, such as `messages[2].content[1]` or
/// `case_media[0]`, the object, and whether an assistant message holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Named {
    pub at: String,
    pub object: ObjectRef,
    /// An assistant message may also hold an object of a type the served model writes, such as an
    /// output sent back.
    pub assistant: bool,
}

/// Each object that messages name.
pub fn objects_of(messages: &[crate::message::Message]) -> Vec<Named> {
    let mut out = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        if let Some(crate::message::Content::Parts(parts)) = &m.content {
            for (j, p) in parts.iter().enumerate() {
                if let crate::message::Part::Object { id, mime } = p {
                    let object = ObjectRef { id: id.clone(), mime: mime.clone() };
                    out.push(Named { at: format!("messages[{}].content[{}]", i, j), object, assistant: m.role == crate::message::Role::Assistant });
                }
            }
        }
    }
    out
}

/// Checks that served model `model` reads the media type of each object, or writes it when an
/// assistant message holds it: otherwise a `400` that names the member, the media type, and the
/// types the served model reads.
pub fn check_reads(model: &crate::identity::ServedModel, objects: &[Named]) -> Result<(), Invalid> {
    for Named { at, object: o, assistant } in objects {
        if !model.reads(&o.mime) && !(*assistant && model.writes(&o.mime)) {
            let inputs = model.inputs();
            let reads = if inputs.is_empty() { "text only".to_string() } else { format!("text and {}", inputs.join(", ")) };
            return Err(Invalid::param(at.clone(), format!("nothing ran: {} is {} of media type {}, and {} reads {}", at, o.id, o.mime, model.model, reads)));
        }
    }
    Ok(())
}

/// Checks a `PUT /council/v2/objects/{id}`: a whole id in the path, a media type in
/// `content-type`, and bytes whose content id is the id. Nothing is held when a check fails.
pub fn check_put(id: &str, content_type: Option<&str>, bytes: &[u8]) -> Result<(), Invalid> {
    if let Some(p) = content_id_problem(id) {
        return Err(Invalid::param("id", format!("nothing is held: {}", p)));
    }
    let Some(mime) = content_type else {
        return Err(Invalid::param("content-type", "nothing is held: the request has no content-type header; send the object's media type, such as `audio/wav`"));
    };
    if let Some(p) = mime_problem(mime) {
        return Err(Invalid::param("content-type", format!("nothing is held: content-type {}", p)));
    }
    let actual = content_id(bytes);
    if actual != id {
        return Err(Invalid::param("id", format!("nothing is held: the {} bytes sent hash to {}, not {}; PUT them at {}, or send the bytes {} names", bytes.len(), actual, id, actual, id)));
    }
    Ok(())
}

/// The response to `PUT /council/v2/objects/{id}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectPut {
    pub id: String,
    /// The media type of the object's first put. A later put with another type changes nothing.
    pub mime: String,
    pub bytes: u64,
}

/// What `GET /council/v2/objects/{id}` returns for a `Range` header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByteRange {
    /// No range, or one this server does not serve, such as several ranges: the whole object.
    Whole,
    /// Bytes `start` to `end`, both included: a `206`.
    Part { start: u64, end: u64 },
    /// A range that starts past the end: a `416`.
    Unsatisfiable,
}

/// The range a `Range` header asks of an object of `len` bytes: `bytes=a-b`, `bytes=a-`, or
/// `bytes=-n`.
pub fn byte_range(header: Option<&str>, len: u64) -> ByteRange {
    let Some(spec) = header.and_then(|h| h.trim().strip_prefix("bytes=")) else { return ByteRange::Whole };
    if spec.contains(',') {
        return ByteRange::Whole;
    }
    let Some((a, b)) = spec.split_once('-') else { return ByteRange::Whole };
    let (a, b) = (a.trim(), b.trim());
    match (a.parse::<u64>().ok(), b.parse::<u64>().ok()) {
        (None, Some(n)) if a.is_empty() => {
            if n == 0 || len == 0 {
                ByteRange::Unsatisfiable
            } else {
                ByteRange::Part { start: len.saturating_sub(n), end: len - 1 }
            }
        }
        (Some(start), end) if b.is_empty() || end.is_some() => {
            // An end before the start makes the field invalid, so it is ignored (RFC 9110 14.1.1).
            if end.is_some_and(|e| e < start) {
                return ByteRange::Whole;
            }
            if start >= len {
                return ByteRange::Unsatisfiable;
            }
            match end {
                Some(e) => ByteRange::Part { start, end: e.min(len - 1) },
                None => ByteRange::Part { start, end: len - 1 },
            }
        }
        _ => ByteRange::Whole,
    }
}

/// The body of `PUT /council/v2/keeps/{owner}`: the owner's whole set, kept for `ttl_s` seconds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeepsPut {
    pub objects: Vec<String>,
    pub ttl_s: u64,
}

impl KeepsPut {
    /// Checks a `ttl_s` of 1 to [`KEEP_TTL_MAX_S`], and that each id is a whole content id.
    pub fn validate(&self) -> Result<(), Invalid> {
        if self.ttl_s == 0 || self.ttl_s > KEEP_TTL_MAX_S {
            return Err(Invalid::param("ttl_s", format!("nothing is kept: ttl_s is {}; a keep lasts 1 to {} seconds (30 days)", self.ttl_s, KEEP_TTL_MAX_S)));
        }
        for (i, id) in self.objects.iter().enumerate() {
            if let Some(p) = content_id_problem(id) {
                return Err(Invalid::param("objects", format!("nothing is kept: objects[{}] {}", i, p)));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeepHeld {
    pub id: String,
    /// `false`: the server does not hold the object, so the keep holds nothing for it.
    pub held: bool,
}

/// The response to `PUT /council/v2/keeps/{owner}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeepsPutResult {
    pub owner: String,
    pub objects: Vec<KeepHeld>,
    /// `null` for a set that holds nothing.
    #[serde(deserialize_with = "required_nullable")]
    pub expires_in_s: Option<u64>,
}

/// The response to `GET /council/v2/keeps/{owner}`: the held objects only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeepSet {
    pub owner: String,
    pub objects: Vec<String>,
    pub expires_in_s: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_media_type_is_lowercase_type_and_subtype() {
        for good in ["audio/wav", "image/png", "audio/midi", "application/vnd.nagaya.snapshot+safetensors", "text/vnd.abc", "x-y/z.9"] {
            assert_eq!(mime_problem(good), None, "{good}");
        }
        for (bad, says) in [("audio", "no `/`"), ("Audio/WAV", "lowercase"), ("text/plain; charset=utf-8", "has parameters"), ("audio/", "not a media type part"), ("/wav", "not a media type part"), ("audio/w v", "not a media type part"), ("a/b/c", "not a media type part")] {
            let p = mime_problem(bad).unwrap_or_default();
            assert!(p.contains(says) && p.contains(&format!("{:?}", bad)), "{bad}: {p}");
        }
    }

    #[test]
    fn a_put_holds_only_bytes_whose_id_is_the_path() {
        let bytes = b"RIFF....WAVE";
        let id = content_id(bytes);
        check_put(&id, Some("audio/wav"), bytes).unwrap();
        let e = check_put(&id, Some("audio/wav"), b"other").unwrap_err();
        assert!(e.param.as_deref() == Some("id") && e.message.contains(&id) && e.message.contains(&content_id(b"other")), "{e:?}");
        assert_eq!(check_put(&id, None, bytes).unwrap_err().param.as_deref(), Some("content-type"));
        assert_eq!(check_put(&id, Some("audio/WAV"), bytes).unwrap_err().param.as_deref(), Some("content-type"));
        let short = &id[..20];
        let e = check_put(short, Some("audio/wav"), bytes).unwrap_err();
        assert!(e.param.as_deref() == Some("id") && e.message.contains(short), "{e:?}");
    }

    #[test]
    fn an_object_ref_names_the_member_at_fault() {
        let ok = ObjectRef { id: content_id(b"x"), mime: "image/png".into() };
        ok.check("case_media[0]").unwrap();
        let e = ObjectRef { id: "blake3:abc".into(), ..ok.clone() }.check("messages[1].content[0]").unwrap_err();
        assert!(e.param.as_deref() == Some("messages[1].content[0]") && e.message.contains("messages[1].content[0].id \"blake3:abc\""), "{e:?}");
        let e = ObjectRef { mime: "png".into(), ..ok }.check("case_media[2]").unwrap_err();
        assert!(e.message.contains("case_media[2].mime \"png\""), "{e:?}");
        assert!(serde_json::from_str::<ObjectRef>(r#"{"id": "x", "mime": "y", "url": "z"}"#).is_err());
    }

    #[test]
    fn a_range_is_one_span_or_the_whole_object() {
        use ByteRange::*;
        assert_eq!(byte_range(None, 10), Whole);
        assert_eq!(byte_range(Some("bytes=2-5"), 10), Part { start: 2, end: 5 });
        assert_eq!(byte_range(Some("bytes=2-"), 10), Part { start: 2, end: 9 });
        assert_eq!(byte_range(Some("bytes=-3"), 10), Part { start: 7, end: 9 });
        assert_eq!(byte_range(Some("bytes=-30"), 10), Part { start: 0, end: 9 });
        assert_eq!(byte_range(Some("bytes=8-100"), 10), Part { start: 8, end: 9 });
        assert_eq!(byte_range(Some("bytes=10-"), 10), Unsatisfiable);
        assert_eq!(byte_range(Some("bytes=-0"), 10), Unsatisfiable);
        assert_eq!(byte_range(Some("bytes=0-1,4-5"), 10), Whole);
        assert_eq!(byte_range(Some("bytes=5-2"), 10), Whole);
        assert_eq!(byte_range(Some("bytes=20-5"), 10), Whole, "an invalid span is ignored, not a 416");
        assert_eq!(byte_range(Some("items=0-1"), 10), Whole);
        assert_eq!(byte_range(Some("bytes=x-1"), 10), Whole);
    }

    #[test]
    fn a_keep_lasts_up_to_30_days_and_names_whole_ids() {
        let id = content_id(b"x");
        KeepsPut { objects: vec![id.clone()], ttl_s: KEEP_TTL_MAX_S }.validate().unwrap();
        assert_eq!(KeepsPut { objects: vec![id.clone()], ttl_s: 0 }.validate().unwrap_err().param.as_deref(), Some("ttl_s"));
        assert_eq!(KeepsPut { objects: vec![id.clone()], ttl_s: KEEP_TTL_MAX_S + 1 }.validate().unwrap_err().param.as_deref(), Some("ttl_s"));
        let e = KeepsPut { objects: vec![id, "blake3:12".into()], ttl_s: 60 }.validate().unwrap_err();
        assert!(e.param.as_deref() == Some("objects") && e.message.contains("objects[1] \"blake3:12\""), "{e:?}");
    }
}
