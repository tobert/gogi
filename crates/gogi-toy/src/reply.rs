//! The reply parser: reads a reply's tokens, one at a time, after the generation prompt
//! `<asst> [<think>]` or after an open message. It reads only the sampled tokens, so text in an
//! open message, such as `<endthink>` written as text, never changes a phase.
//!
//! Each token gets a phase and a piece of text. At the end the parser gives the reply. A call
//! `<call> name(INPUT_JSON) <endcall>` parses to its name and input. A call that does not parse
//! is kept as written, with an error. The parser makes no call ids.

use gogi_proto::generation::Phase;
use serde_json::{Map, Value};

use crate::{CALL, END, ENDCALL, ENDTHINK, VOCAB, piece};

/// What one token adds to the reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// The part of the reply the token belongs to. A call's markers belong to `Call`.
    pub phase: Phase,
    /// The text the token adds to its part. A marker in reasoning or content adds none, and a
    /// byte that does not yet complete a character adds none.
    pub piece: String,
}

/// A call in a reply.
#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    Parsed {
        name: String,
        /// A JSON object, in the member order the model wrote.
        input: Map<String, Value>,
    },
    /// A call that is not `name(INPUT_JSON)`: its text as written, between `<call>` and
    /// `<endcall>`, and why it does not parse.
    Malformed { text: String, error: String },
}

/// The reply a parser has read.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub content: String,
    pub reasoning: String,
    pub calls: Vec<Call>,
    /// The reply wrote `<end>`.
    pub ended: bool,
}

/// Reads a reply token by token.
pub struct ReplyParser {
    thinking: bool,
    in_call: bool,
    ended: bool,
    reasoning: Vec<u8>,
    content: Vec<u8>,
    /// How many bytes of `reasoning` and of `content` earlier pieces showed.
    shown: (usize, usize),
    /// The bytes of the open call, between `<call>` and `<endcall>`.
    call: Vec<u8>,
    calls: Vec<Call>,
}

impl ReplyParser {
    /// `thinking` is true when the reply starts in its reasoning: the generation prompt ended with
    /// `<think>`, or an open message without `content` did (`crate::starts_in_reasoning`).
    pub fn new(thinking: bool) -> Self {
        ReplyParser { thinking, in_call: false, ended: false, reasoning: vec![], content: vec![], shown: (0, 0), call: vec![], calls: vec![] }
    }

    /// Reads the next token of the reply. A token after `<end>`, or an id that is not a token,
    /// is a mistake of the caller and panics.
    pub fn push(&mut self, token: u32) -> Step {
        assert!(!self.ended, "token {} came after <end>: the reply is over", token);
        assert!((token as usize) < VOCAB, "{} is not a token: ids run 0 to {}", token, VOCAB - 1);
        let opens = token == CALL && !self.in_call;
        let phase = match token {
            CALL if !self.in_call => {
                self.in_call = true;
                self.call.clear();
                Phase::Call
            }
            ENDCALL => {
                if self.in_call {
                    self.in_call = false;
                    let text = String::from_utf8_lossy(&self.call).into_owned();
                    self.calls.push(parse_call(text));
                }
                Phase::Call
            }
            _ if self.in_call => Phase::Call,
            _ if self.thinking => Phase::Reasoning,
            _ => Phase::Content,
        };
        if token == END {
            self.ended = true;
        } else if token == ENDTHINK && phase == Phase::Reasoning {
            self.thinking = false;
        }
        let piece = match (phase, token) {
            (Phase::Reasoning, t) if t < 256 => {
                self.reasoning.push(t as u8);
                next_piece(&self.reasoning, &mut self.shown.0)
            }
            (Phase::Content, t) if t < 256 => {
                self.content.push(t as u8);
                next_piece(&self.content, &mut self.shown.1)
            }
            (Phase::Call, t) => {
                if self.in_call && !opens {
                    self.call.extend(piece_bytes(t));
                }
                piece(t)
            }
            _ => String::new(),
        };
        Step { phase, piece }
    }

    /// The phase the next token would have: `call` inside a call, `reasoning` before the reply's
    /// `<endthink>`, else `content`. A reply cut by `max_tokens` reports it as `done.phase`.
    pub fn phase(&self) -> Phase {
        if self.in_call {
            Phase::Call
        } else if self.thinking {
            Phase::Reasoning
        } else {
            Phase::Content
        }
    }

    /// The reply read so far. A call still open is malformed: it has no `<endcall>`.
    pub fn finish(mut self) -> Reply {
        if self.in_call {
            self.calls.push(unterminated(&self.call));
        }
        Reply {
            content: String::from_utf8_lossy(&self.content).into_owned(),
            reasoning: String::from_utf8_lossy(&self.reasoning).into_owned(),
            calls: self.calls,
            ended: self.ended,
        }
    }
}

/// The bytes a token adds to a call's text: its byte, or the text of its marker.
fn piece_bytes(token: u32) -> Vec<u8> {
    if token < 256 { vec![token as u8] } else { piece(token).into_bytes() }
}

fn unterminated(call: &[u8]) -> Call {
    Call::Malformed { text: String::from_utf8_lossy(call).into_owned(), error: "the call has no <endcall> before the reply stopped".into() }
}

/// Parses `name(INPUT_JSON)`.
fn parse_call(text: String) -> Call {
    let malformed = |error: String| Call::Malformed { text: text.clone(), error };
    let Some((name, rest)) = text.split_once('(') else {
        return malformed("the call has no '(': a call is name(INPUT_JSON)".into());
    };
    if name.is_empty() {
        return malformed("the call has no name before '(': a call is name(INPUT_JSON)".into());
    }
    let Some(json) = rest.strip_suffix(')') else {
        return malformed("the call does not end with ')': a call is name(INPUT_JSON)".into());
    };
    match serde_json::from_str::<Value>(json) {
        Ok(Value::Object(input)) => Call::Parsed { name: name.to_string(), input },
        Ok(_) => malformed("the call's input is JSON but not an object: a call's input is a JSON object".into()),
        Err(e) => malformed(format!("the call's input is not JSON: {}", e)),
    }
}

/// The text `buf` adds past the `shown` bytes of its decoding. An incomplete last character waits
/// for a later piece.
pub(crate) fn next_piece(buf: &[u8], shown: &mut usize) -> String {
    let upto = match std::str::from_utf8(buf) {
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        _ => buf.len(),
    };
    let text = String::from_utf8_lossy(&buf[..upto]);
    let piece = text.get(*shown..).unwrap_or("").to_string();
    *shown = text.len();
    piece
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ASST, BOS, THINK, render_tokens, tokenize_suffix};
    use gogi_proto::message::{Message, ToolCall};

    /// Feeds `text` as a suffix (marker text becomes marker ids), then more raw tokens.
    fn run(thinking: bool, tokens: &[u32]) -> (Vec<Step>, Reply) {
        let mut p = ReplyParser::new(thinking);
        let steps = tokens.iter().map(|&t| p.push(t)).collect();
        (steps, p.finish())
    }

    fn run_text(thinking: bool, text: &str) -> (Vec<Step>, Reply) {
        run(thinking, &tokenize_suffix(text))
    }

    fn phases(steps: &[Step]) -> Vec<Phase> {
        steps.iter().map(|s| s.phase).collect()
    }

    fn joined(steps: &[Step]) -> String {
        steps.iter().map(|s| s.piece.as_str()).collect()
    }

    #[test]
    fn a_thinking_reply_splits_reasoning_from_content() {
        use Phase::{Content as C, Reasoning as R};
        let (steps, r) = run_text(true, "hmm<endthink>hi<end>");
        assert_eq!(phases(&steps), [R, R, R, R, C, C, C]);
        assert_eq!(steps[3].piece, "", "a marker adds no text");
        assert_eq!(joined(&steps), "hmmhi", "pieces add up to the parts, each once");
        assert_eq!(steps[6].piece, "");
        assert_eq!((r.reasoning.as_str(), r.content.as_str(), r.ended), ("hmm", "hi", true));
        assert!(r.calls.is_empty());
    }

    #[test]
    fn a_reply_without_thinking_is_all_content() {
        let (steps, r) = run_text(false, "hi<end>");
        assert!(phases(&steps).iter().all(|p| *p == Phase::Content));
        assert_eq!((r.reasoning.as_str(), r.content.as_str()), ("", "hi"));
    }

    #[test]
    fn a_reply_that_never_writes_end_has_not_ended() {
        let (_, r) = run_text(true, "hm<endthink>hi");
        assert!(!r.ended);
        let (_, r) = run_text(true, "hm");
        assert!(!r.ended);
        assert_eq!(r.reasoning, "hm");
    }

    #[test]
    fn an_endthink_without_thinking_adds_nothing() {
        let (steps, r) = run_text(false, "a<endthink>b<end>");
        assert_eq!(steps[1].phase, Phase::Content);
        assert_eq!(steps[1].piece, "");
        assert_eq!(r.content, "ab");
        assert_eq!(r.reasoning, "");
    }

    #[test]
    fn a_call_parses_to_its_name_and_input_in_member_order() {
        use Phase::{Call as K, Content as C};
        let (steps, r) = run_text(false, r#"ok<call>sh({"b":1,"a":[2]})<endcall><end>"#);
        assert_eq!(&phases(&steps)[..3], [C, C, K]);
        assert!(phases(&steps)[2..steps.len() - 1].iter().all(|p| *p == K));
        assert_eq!(steps[2].piece, "<call>");
        assert_eq!(steps[steps.len() - 2].piece, "<endcall>");
        assert_eq!(steps[steps.len() - 1].phase, C);
        assert_eq!(r.content, "ok", "the call text is not content");
        assert!(r.ended);
        let [Call::Parsed { name, input }] = &r.calls[..] else { panic!("one parsed call: {:?}", r.calls) };
        assert_eq!(name, "sh");
        assert_eq!(input.keys().collect::<Vec<_>>(), ["b", "a"]);
        assert_eq!(input["a"], serde_json::json!([2]));
    }

    #[test]
    fn calls_keep_their_order_and_content_may_follow_a_call() {
        let (_, r) = run_text(false, "a<call>x({})<endcall>b<call>y({\"k\":null})<endcall>c<end>");
        let names: Vec<_> = r.calls.iter().map(|c| match c { Call::Parsed { name, .. } => name.as_str(), _ => "?" }).collect();
        assert_eq!(names, ["x", "y"]);
        assert_eq!(r.content, "abc");
    }

    #[test]
    fn a_call_that_does_not_parse_is_kept_as_written_with_an_error() {
        let written = ["sh", "sh({", "sh({}", "sh([1])", "sh(7)", "({})", "sh({} )x", "sh({\"a\":1,})", "", "sh({})x"];
        let mut errors = Vec::new();
        for w in written {
            let (_, r) = run_text(false, &format!("<call>{}<endcall><end>", w));
            let [Call::Malformed { text, error }] = &r.calls[..] else { panic!("{:?} should be malformed: {:?}", w, r.calls) };
            assert_eq!(text, w);
            assert!(!error.is_empty(), "{:?}", w);
            errors.push(error.clone());
        }
        errors.sort();
        errors.dedup();
        assert!(errors.len() >= 4, "errors tell the causes apart: {:?}", errors);
    }

    #[test]
    fn a_call_without_endcall_is_malformed_and_the_reply_has_not_ended() {
        let (_, r) = run_text(false, "<call>sh({})");
        assert!(!r.ended);
        let [Call::Malformed { text, error }] = &r.calls[..] else { panic!("{:?}", r.calls) };
        assert_eq!(text, "sh({})");
        assert!(error.contains("endcall"), "{}", error);
        let (_, r) = run_text(false, "<call>sh({})<end>");
        assert!(r.ended);
        assert!(matches!(&r.calls[..], [Call::Malformed { .. }]));
    }

    #[test]
    fn a_character_split_over_tokens_shows_at_its_last_byte() {
        // "é" is 0xC3 0xA9.
        for thinking in [false, true] {
            let toks: Vec<u32> = if thinking { vec![0xC3, 0xA9, ENDTHINK, 0xC3, 0xA9] } else { vec![0xC3, 0xA9] };
            let (steps, r) = run(thinking, &toks);
            assert_eq!(steps[0].piece, "");
            assert_eq!(steps[1].piece, "é");
            assert_eq!(joined(&steps), if thinking { "éé" } else { "é" });
            assert_eq!(format!("{}{}", r.reasoning, r.content), joined(&steps));
        }
    }

    #[test]
    fn a_call_shows_each_token_as_the_toy_piece_and_decodes_its_bytes_whole() {
        let mut toks = vec![CALL];
        toks.extend(r#"sh({"a":"é"})"#.bytes().map(u32::from));
        toks.push(ENDCALL);
        let (steps, r) = run(false, &toks);
        assert_eq!(steps[0].piece, "<call>");
        let at = toks.iter().position(|&t| t == 0xC3).unwrap();
        assert_eq!(steps[at].piece, "<0xc3>");
        assert_eq!(steps[1].piece, "s");
        let [Call::Parsed { input, .. }] = &r.calls[..] else { panic!("{:?}", r.calls) };
        assert_eq!(input["a"], "é");
    }

    #[test]
    fn the_name_ends_at_the_first_open_parenthesis() {
        let (_, r) = run_text(false, r#"<call>say({"t":"f(x)"})<endcall>"#);
        let [Call::Parsed { name, input }] = &r.calls[..] else { panic!("{:?}", r.calls) };
        assert_eq!((name.as_str(), input["t"].as_str()), ("say", Some("f(x)")));
    }

    #[test]
    fn a_marker_inside_a_call_is_kept_in_its_text() {
        let (_, r) = run_text(false, "<call>a<call>b<endcall><end>");
        let [Call::Malformed { text, .. }] = &r.calls[..] else { panic!("{:?}", r.calls) };
        assert_eq!(text, "a<call>b");
        assert!(r.ended);
    }

    #[test]
    fn a_marker_in_content_adds_no_text() {
        let (steps, r) = run(false, &[b'a'.into(), crate::USER, b'b'.into(), END]);
        assert_eq!(steps[1].piece, "");
        assert_eq!(r.content, "ab");
    }

    #[test]
    fn phase_names_the_part_the_next_token_belongs_to() {
        let mut p = ReplyParser::new(true);
        assert_eq!(p.phase(), Phase::Reasoning);
        p.push(u32::from(b'a'));
        assert_eq!(p.phase(), Phase::Reasoning);
        p.push(ENDTHINK);
        assert_eq!(p.phase(), Phase::Content, "a reply cut just after <endthink> resumes in its content");
        p.push(CALL);
        assert_eq!(p.phase(), Phase::Call);
        p.push(ENDCALL);
        assert_eq!(p.phase(), Phase::Content);
        assert_eq!(ReplyParser::new(false).phase(), Phase::Content);
    }

    #[test]
    #[should_panic(expected = "after <end>")]
    fn a_token_after_end_is_refused() {
        let mut p = ReplyParser::new(false);
        p.push(END);
        p.push(u32::from(b'a'));
    }

    #[test]
    #[should_panic(expected = "not a token")]
    fn a_token_id_past_the_vocabulary_is_refused() {
        ReplyParser::new(false).push(VOCAB as u32);
    }

    #[test]
    fn the_template_renders_a_message_that_the_parser_reads_back() {
        let mut input = Map::new();
        input.insert("z".into(), Value::from(1));
        input.insert("a".into(), serde_json::json!({"y": [true, null]}));
        let m = Message {
            reasoning: Some("think é".into()),
            tool_calls: vec![ToolCall { id: "id1".into(), name: "sh".into(), input: input.clone() }],
            ..Message::assistant("say")
        };
        let t = render_tokens(&[], true, &[m], false);
        assert_eq!(t[..3], [BOS, ASST, THINK]);
        let (_, r) = run(true, &t[3..]);
        assert_eq!(r, Reply { content: "say".into(), reasoning: "think é".into(), calls: vec![Call::Parsed { name: "sh".into(), input }], ended: true });
        let Call::Parsed { input, .. } = &r.calls[0] else { unreachable!() };
        assert_eq!(input.keys().collect::<Vec<_>>(), ["z", "a"]);
    }
}
