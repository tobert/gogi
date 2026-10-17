//! RFC 8785 canonical JSON and spec ids.
//!
//! A spec holds integers only: a number must be an integer below 2^53 in magnitude, because
//! writing any other number canonically needs ES6 number formatting.

use serde_json::Value;
use sha2::{Digest, Sha256};

const MAX_EXACT: i64 = 1 << 53;

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{:02x}", b)).collect()
}

/// Appends the canonical JSON of `value`: object members sorted by the UTF-16 code units of
/// their names, no whitespace, strings escaped minimally.
pub fn canonical_json(value: &Value, out: &mut String) -> Result<(), String> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            let s = match (n.as_i64(), n.as_u64()) {
                (Some(i), _) if -MAX_EXACT < i && i < MAX_EXACT => i.to_string(),
                (None, Some(u)) if u < MAX_EXACT as u64 => u.to_string(),
                _ => return Err(format!("{} is not an integer below 2^53: a spec holds integers only", n)),
            };
            out.push_str(&s);
        }
        Value::String(s) => canonical_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical_json(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut members: Vec<(Vec<u16>, &String, &Value)> = map.iter().map(|(k, v)| (k.encode_utf16().collect(), k, v)).collect();
            members.sort_by(|a, b| a.0.cmp(&b.0));
            out.push('{');
            for (i, (_, k, v)) in members.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical_string(k, out);
                out.push(':');
                canonical_json(v, out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn canonical_string(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The id of a spec: `sha256:` and the hex sha256 of its canonical JSON.
pub fn spec_id_of(spec: &Value) -> Result<String, String> {
    let mut text = String::new();
    canonical_json(spec, &mut text)?;
    Ok(format!("sha256:{}", sha256_hex(text.as_bytes())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const FIXTURE: &str = include_str!("../tests/fixtures/spec.json");
    /// The id kaijutsu's client holds for the same file (`DOC_SPEC_ID`).
    const FIXTURE_ID: &str = "sha256:5114ff063b887c710333afd4fe35238c55946ac2a63fb76d6c9004b56a1503ea";

    fn canon(v: &Value) -> String {
        let mut s = String::new();
        canonical_json(v, &mut s).unwrap();
        s
    }

    #[test]
    fn spec_id_matches_kaijutsu_fixture() {
        let v: Value = serde_json::from_str(FIXTURE).unwrap();
        assert_eq!(spec_id_of(&v).unwrap(), FIXTURE_ID);
    }

    #[test]
    fn the_id_does_not_depend_on_member_order_or_spacing() {
        let a: Value = serde_json::from_str(r#"{"a": 1, "b": [true, null, "x"]}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"b":[true,null,"x"],"a":1}"#).unwrap();
        assert_eq!(spec_id_of(&a).unwrap(), spec_id_of(&b).unwrap());
        assert_ne!(spec_id_of(&a).unwrap(), spec_id_of(&json!({"a": 2, "b": [true, null, "x"]})).unwrap());
    }

    #[test]
    fn members_sort_by_utf16_code_units() {
        let v: Value = serde_json::from_str("{\"\u{ffff}\":2,\"\u{1f600}\":1,\"b\":0,\"A\":0,\"\":0}").unwrap();
        assert_eq!(canon(&v), "{\"\":0,\"A\":0,\"b\":0,\"\u{1f600}\":1,\"\u{ffff}\":2}");
    }

    #[test]
    fn a_number_that_is_not_a_small_integer_is_refused() {
        for text in ["1.5", "1.0", "1e3", "9007199254740992", "-9007199254740992", "18446744073709551615"] {
            let v: Value = serde_json::from_str(text).unwrap();
            assert!(spec_id_of(&v).is_err(), "{text}");
        }
        for text in ["0", "-1", "9007199254740991", "-9007199254740991"] {
            let v: Value = serde_json::from_str(text).unwrap();
            assert_eq!(canon(&v), text);
        }
    }

    #[test]
    fn strings_escape_minimally() {
        assert_eq!(canon(&json!("a\"b\\c\n\t\u{1}\u{7f}é")), "\"a\\\"b\\\\c\\n\\t\\u0001\u{7f}é\"");
    }
}
