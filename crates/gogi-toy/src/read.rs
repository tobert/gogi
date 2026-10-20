//! The toy council read, as pure functions: the tokens a decision or a raw read feeds, and
//! the letter tokens it reads. No model and no I/O. A caller folds the tokens through its own
//! model.
//!
//! A decision over a context with no queue reads each asked question after the whole text:
//!
//! ```text
//! context tokens (or <bos>)  <user> SPEC "\n" LABEL ": " CASE <end> <asst>  then, per question,  ID " " INSTRUCTIONS ":"
//! ```
//!
//! A decision over a queue context renders the queue tool's result and then the queue tool's call,
//! asked questions in order of id, each followed by the most likely letter of the one before:
//!
//! ```text
//! context tokens + <tool> CASE <end> <asst>  "call:QUEUE{answers:{"  then, per question,  ID ":\"" ... LETTER "\","
//! ```

use serde_json::Value;

use crate::{ASST, BOS, END, USER, push_text, render, tokenize_suffix};
use gogi_proto::canon::sha256_hex;
use gogi_proto::context::Context;
use gogi_proto::message::Message;

/// The token of the letter for option `option`: `A` for 0, `B` for 1, and so on.
pub fn option_letter(option: usize) -> u32 {
    u32::from(b'A') + option as u32
}

/// The tokens of the letters a raw read reads. A letter must be one token, and no two may be the same.
pub fn letter_tokens(letters: &[String]) -> Result<Vec<u32>, String> {
    let mut ids = Vec::new();
    for l in letters {
        let t = tokenize_suffix(l);
        if t.len() != 1 || ids.contains(&t[0]) {
            return Err(format!("letter {:?} is not one token of its own", l));
        }
        ids.push(t[0]);
    }
    Ok(ids)
}

/// The text of a case: a string as it is, any other value as its JSON.
pub fn case_text(case: &Value) -> String {
    match case {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The SHA-256 of tokens as 32-bit little-endian words, in lowercase hexadecimal.
pub fn rendered_sha256(tokens: &[u32]) -> String {
    sha256_hex(tokens.iter().flat_map(|t| t.to_le_bytes()).collect::<Vec<u8>>().as_slice())
}

/// The tokens a decision over no context starts from.
pub fn empty_context() -> Vec<u32> {
    vec![BOS]
}

/// The tokens of a plain read of `base`: the spec, the case, and the start of the answer.
pub fn plain_text(base: &[u32], instructions: &str, label: &str, case: &str) -> Vec<u32> {
    let mut t = base.to_vec();
    t.push(USER);
    push_text(&mut t, &format!("{}\n{}: {}", instructions, label, case));
    t.extend([END, ASST]);
    t
}

/// The tokens a plain read feeds for one question: the plain text and the question.
pub fn plain_question(text: &[u32], id: &str, instructions: &str) -> Vec<u32> {
    let mut t = text.to_vec();
    push_text(&mut t, &format!("{} {}:", id, instructions));
    t
}

/// The tokens a queue read starts from: the context with a tool message that gives the case as
/// the result of the queue call `call_id`, the generation prompt, and the start of the queue call.
pub fn queue_start(c: &Context, call_id: &str, case: &str) -> Vec<u32> {
    let mut messages = c.messages.clone();
    messages.push(Message::tool(call_id, case.to_string()));
    let mut t = render(&c.tools, c.thinks(), &messages, true).tokens;
    push_text(&mut t, &format!("call:{}{{answers:{{", c.queue.as_deref().unwrap_or("")));
    t
}

/// Appends the start of a queue answer for question `id`.
pub fn queue_question(tokens: &mut Vec<u32>, id: &str) {
    push_text(tokens, &format!("{}:\"", id));
}

/// Appends a chosen letter to a queue answer, and the end of the answer.
pub fn queue_letter(tokens: &mut Vec<u32>, option: usize) {
    push_text(tokens, &format!("{}\",", char::from(b'A' + option as u8)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_are_one_token_each_and_distinct() {
        let l = |v: &[&str]| letter_tokens(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(l(&["A", "B"]).unwrap(), [65, 66]);
        assert!(l(&["A", "A"]).is_err());
        assert!(l(&["AB"]).is_err());
        assert_eq!(l(&["<end>"]).unwrap(), [END]);
    }

    #[test]
    fn a_plain_read_has_the_documented_shape() {
        let text = plain_text(&empty_context(), "ins", "case", "x");
        assert_eq!(text[0], BOS);
        assert_eq!(text[1], USER);
        assert_eq!(text[text.len() - 2..], [END, ASST]);
        let q = plain_question(&text, "q1", "go");
        assert_eq!(q[text.len()..], *b"q1 go:".map(u32::from).as_slice());
    }

    #[test]
    fn a_queue_answer_appends_id_then_letter() {
        let mut t = Vec::new();
        queue_question(&mut t, "q");
        queue_letter(&mut t, 1);
        assert_eq!(t, "q:\"B\",".bytes().map(u32::from).collect::<Vec<_>>());
    }
}
