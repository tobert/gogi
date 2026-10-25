//! Continuations: what a server checks of an open message, as pure functions. The server
//! renders with its own template; these functions say what to render and what to compare.

use crate::error::Invalid;
use crate::message::Message;

/// One open field's seam check: the open message cut just before the field, and the open
/// message through the field. A server renders the context with each in place of the open
/// message; the tokens of `before` must begin the tokens of `with`.
#[derive(Clone, Debug, PartialEq)]
pub struct Seam {
    /// `reasoning` or `content`.
    pub field: &'static str,
    /// The open message up to the start of the field. Its text may be empty, so it is only
    /// rendered, never validated.
    pub before: Message,
    /// The open message through the end of the field.
    pub with: Message,
}

/// The seams of an open message: one for each open field that is not empty, `reasoning` first.
/// The `reasoning` seam has no `content` member, so the thinking stays open in both renderings.
pub fn seams(open: &Message) -> Vec<Seam> {
    let mut out = Vec::new();
    let reasoning = open.reasoning.as_deref().unwrap_or("");
    if !reasoning.is_empty() {
        let thinking = |r: &str| Message { reasoning: Some(r.into()), content: None, ..open.clone() };
        out.push(Seam { field: "reasoning", before: thinking(""), with: thinking(reasoning) });
    }
    if !open.text().unwrap_or("").is_empty() {
        out.push(Seam { field: "content", before: Message { content: Some("".into()), ..open.clone() }, with: open.clone() });
    }
    out
}

/// Checks that an open field tokenizes on its own: the tokens of the rendering before the field,
/// `before`, begin the tokens of the rendering with it, `with`. Otherwise the error names
/// `continue`, the field, the text of the last token before the field, and the text of the first
/// token that differs, which `text` gives as characters for a run of tokens.
pub fn check_seam(field: &str, before: &[u32], with: &[u32], text: impl Fn(&[u32]) -> String) -> Result<(), Invalid> {
    if with.starts_with(before) {
        return Ok(());
    }
    let i = before.iter().zip(with).take_while(|(a, b)| a == b).count();
    let end = text(&before[before.len().saturating_sub(1)..]);
    let merged = text(&with[i..with.len().min(i + 1)]);
    Err(Invalid::param(
        "continue",
        format!("nothing ran: the open {} does not tokenize on its own: the text before it ends with {:?}, which merges with its start into {:?}; start the {} with a character that does not merge", field, end, merged, field),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(tokens: &[u32]) -> String {
        tokens.iter().map(|&t| match t {
            300 => "\n\n".to_string(),
            301 => "B\n".to_string(),
            _ => char::from_u32(t).unwrap().to_string(),
        }).collect()
    }

    #[test]
    fn a_field_whose_start_merges_with_the_text_before_it_is_refused() {
        // Token 300 is "\n\n": a tokenizer that merges a newline after the template's last newline.
        check_seam("content", &[65, 10], &[65, 10, 10, 66], text).unwrap();
        let e = check_seam("content", &[65, 10], &[65, 300, 66], text).unwrap_err();
        assert_eq!(e.param.as_deref(), Some("continue"));
        assert_eq!(e.message, r#"nothing ran: the open content does not tokenize on its own: the text before it ends with "\n", which merges with its start into "\n\n"; start the content with a character that does not merge"#);
        assert!(check_seam("reasoning", &[65, 10], &[65], text).unwrap_err().message.contains("open reasoning"));
        // Token 301 is "B\n": the merge reaches back past the last token, and the error names only
        // that last token.
        let e = check_seam("content", &[65, 66, 10], &[65, 301, 10, 67], text).unwrap_err();
        assert!(e.message.contains(r#"ends with "\n", which merges with its start into "B\n""#), "{}", e.message);
    }

    #[test]
    fn each_open_field_that_is_not_empty_has_a_seam() {
        let both = Message { reasoning: Some("plan".into()), ..Message::assistant("```toml\n").opened() };
        let s = seams(&both);
        assert_eq!(s.iter().map(|x| x.field).collect::<Vec<_>>(), ["reasoning", "content"]);
        assert_eq!((s[0].before.reasoning.as_deref(), s[0].before.text()), (Some(""), None));
        assert_eq!((s[0].with.reasoning.as_deref(), s[0].with.text()), (Some("plan"), None));
        assert_eq!((s[1].before.reasoning.as_deref(), s[1].before.text()), (Some("plan"), Some("")));
        assert_eq!(s[1].with, both);
        assert!(s.iter().all(|x| x.before.open && x.with.open));
        let closed_thinking = Message { reasoning: Some("plan".into()), ..Message::assistant("").opened() };
        assert_eq!(seams(&closed_thinking).iter().map(|x| x.field).collect::<Vec<_>>(), ["reasoning"]);
        assert_eq!(seams(&Message::assistant("x").opened()).iter().map(|x| x.field).collect::<Vec<_>>(), ["content"]);
    }
}
