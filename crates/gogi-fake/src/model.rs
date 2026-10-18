//! The toy model and its chat template.
//!
//! Tokens are the 256 byte values plus 14 template markers (ids 256 and up). Text becomes
//! its UTF-8 bytes, so content can never spell a marker: only a `reads` suffix, through
//! [`tokenize_suffix`], turns marker text into marker ids.
//!
//! The template (`docs/gogi-fake.md` holds the same table):
//!
//! ```text
//! <bos> [<nothink>] [<tools> TOOLS_JSON <endtools>] MESSAGE*
//! system     <sys> content <end>
//! user       <user> content <end>
//! assistant  <asst> [<think> reasoning <endthink>] content (<call> name(INPUT_JSON) <endcall>)* <end>
//! tool       <tool> [<error>] content <end>
//! generation prompt: <asst> [<think>]
//! ```
//!
//! `<nothink>` renders when the context sets `thinking: false`. With thinking on, every
//! assistant message renders its `<think>` region, empty when it has no reasoning. A call's id
//! is never rendered. `<error>` renders when a tool message has `is_error: true`.
//!
//! The thinking rule: an assistant message without tool calls loses its reasoning once a later
//! user message follows. A message with tool calls keeps it. So a marked assistant message with
//! reasoning and no tool calls stops being a boundary when a user message is added after it.
//!
//! The model: its model state after a token prefix is a hash folded over a served model's seed and
//! the tokens, and the logits are a deterministic function of that hash. The same served model and
//! prefix give the same bits; two seeds are two models.

use gogi_proto::context::Tool;
use gogi_proto::message::{Message, Role};

pub const BOS: u32 = 256;
pub const SYS: u32 = 257;
pub const USER: u32 = 258;
pub const ASST: u32 = 259;
pub const TOOL: u32 = 260;
pub const END: u32 = 261;
pub const THINK: u32 = 262;
pub const ENDTHINK: u32 = 263;
pub const CALL: u32 = 264;
pub const ENDCALL: u32 = 265;
pub const TOOLS: u32 = 266;
pub const ENDTOOLS: u32 = 267;
pub const NOTHINK: u32 = 268;
pub const ERROR: u32 = 269;
pub const VOCAB: usize = 270;

pub const MARKERS: [(&str, u32); 14] = [
    ("<bos>", BOS),
    ("<sys>", SYS),
    ("<user>", USER),
    ("<asst>", ASST),
    ("<tool>", TOOL),
    ("<end>", END),
    ("<think>", THINK),
    ("<endthink>", ENDTHINK),
    ("<call>", CALL),
    ("<endcall>", ENDCALL),
    ("<tools>", TOOLS),
    ("<endtools>", ENDTOOLS),
    ("<nothink>", NOTHINK),
    ("<error>", ERROR),
];

pub fn push_text(t: &mut Vec<u32>, s: &str) {
    t.extend(s.bytes().map(u32::from));
}

/// Marker texts a piece of content spells.
pub fn control_hits(s: &str) -> Vec<&'static str> {
    MARKERS.iter().filter(|(m, _)| s.contains(m)).map(|(m, _)| *m).collect()
}

/// Tokenizes a `reads` suffix: marker text becomes one marker id, anything else its bytes.
pub fn tokenize_suffix(s: &str) -> Vec<u32> {
    let mut out = Vec::new();
    let mut rest = s;
    'outer: while !rest.is_empty() {
        if rest.starts_with('<') {
            for (m, id) in MARKERS {
                if rest.starts_with(m) {
                    out.push(id);
                    rest = &rest[m.len()..];
                    continue 'outer;
                }
            }
        }
        let c = rest.chars().next().unwrap();
        push_text(&mut out, &rest[..c.len_utf8()]);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// The text of one token, for `top` lists and stream pieces.
pub fn piece(id: u32) -> String {
    if let Some((m, _)) = MARKERS.iter().find(|(_, i)| *i == id) {
        (*m).to_string()
    } else if (0x20..0x7f).contains(&id) || id == 10 {
        char::from(id as u8).to_string()
    } else {
        format!("<0x{:02x}>", id)
    }
}

fn kept_reasoning(messages: &[Message], i: usize) -> bool {
    let m = &messages[i];
    !m.tool_calls.is_empty() || !messages[i + 1..].iter().any(|n| n.role == Role::User)
}

/// The rendering of `messages`, with the generation prompt after it when asked.
pub fn render_tokens(tools: &[Tool], thinking: bool, messages: &[Message], generation_prompt: bool) -> Vec<u32> {
    let mut t = vec![BOS];
    if !thinking {
        t.push(NOTHINK);
    }
    if !tools.is_empty() {
        t.push(TOOLS);
        push_text(&mut t, &serde_json::to_string(tools).unwrap());
        t.push(ENDTOOLS);
    }
    for (i, m) in messages.iter().enumerate() {
        let text = m.content.as_deref().unwrap_or("");
        match m.role {
            Role::System => {
                t.push(SYS);
                push_text(&mut t, text);
            }
            Role::User => {
                t.push(USER);
                push_text(&mut t, text);
            }
            Role::Tool => {
                t.push(TOOL);
                if m.is_error {
                    t.push(ERROR);
                }
                push_text(&mut t, text);
            }
            Role::Assistant => {
                t.push(ASST);
                if thinking {
                    t.push(THINK);
                    if kept_reasoning(messages, i) {
                        push_text(&mut t, m.reasoning.as_deref().unwrap_or(""));
                    }
                    t.push(ENDTHINK);
                }
                push_text(&mut t, text);
                for c in &m.tool_calls {
                    t.push(CALL);
                    push_text(&mut t, &format!("{}({})", c.name, serde_json::to_string(&c.input).unwrap()));
                    t.push(ENDCALL);
                }
            }
        }
        t.push(END);
    }
    if generation_prompt {
        t.push(ASST);
        if thinking {
            t.push(THINK);
        }
    }
    t
}

/// A boundary: the end of message `message`, after `tokens` tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cut {
    pub message: usize,
    pub tokens: usize,
}

pub struct Rendering {
    /// The context's tokens, with the generation prompt when asked for.
    pub tokens: Vec<u32>,
    /// The context's tokens without the generation prompt.
    pub end: usize,
    /// The boundaries the rendering keeps, in order, ending with the context's end.
    pub cuts: Vec<Cut>,
}

/// Renders a context. A marked message is a boundary only when its own rendering is a prefix
/// of the whole; the context's end is always one.
pub fn render(tools: &[Tool], thinking: bool, messages: &[Message], generation_prompt: bool) -> Rendering {
    let whole = render_tokens(tools, thinking, messages, false);
    let mut cuts: Vec<Cut> = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        if !m.snap {
            continue;
        }
        let p = render_tokens(tools, thinking, &messages[..=i], false);
        if whole.starts_with(&p) && cuts.last().is_none_or(|c| c.tokens < p.len()) {
            cuts.push(Cut { message: i, tokens: p.len() });
        }
    }
    if cuts.last().is_none_or(|c| c.tokens != whole.len()) {
        cuts.push(Cut { message: messages.len() - 1, tokens: whole.len() });
    }
    let end = whole.len();
    let tokens = if generation_prompt { render_tokens(tools, thinking, messages, true) } else { whole };
    Rendering { tokens, end, cuts }
}

// ---- the model

fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E3779B97F4A7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

/// The model state after a token prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State(u64);

impl State {
    pub fn start(seed: u64) -> Self {
        State(0x676F6769 ^ seed)
    }

    pub fn after(seed: u64, prefix: &[u32]) -> Self {
        prefix.iter().fold(Self::start(seed), |s, &t| s.step(t))
    }

    pub fn step(self, token: u32) -> Self {
        State(mix(self.0 ^ mix(u64::from(token) + 1)))
    }

    /// The logits for the next token: deterministic, between -6 and 6.
    pub fn logits(self) -> [f64; VOCAB] {
        let mut l = [0.0; VOCAB];
        for (v, x) in l.iter_mut().enumerate() {
            let u = (mix(self.0 ^ mix(0xA5A5 + v as u64)) >> 11) as f64 / (1u64 << 53) as f64;
            *x = u * 12.0 - 6.0;
        }
        l
    }
}

/// `logit - ln Σ exp(logit)` for each token.
pub fn log_softmax(logits: &[f64; VOCAB]) -> [f64; VOCAB] {
    let max = logits.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let lse = max + logits.iter().map(|x| (x - max).exp()).sum::<f64>().ln();
    let mut out = [0.0; VOCAB];
    for (o, x) in out.iter_mut().zip(logits) {
        *o = x - lse;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use gogi_proto::message::ToolCall;

    fn calls(id: &str) -> Message {
        Message { tool_calls: vec![ToolCall { id: id.into(), name: "sh".into(), input: Default::default() }], ..Message::assistant("") }
    }

    fn with_reasoning(m: Message) -> Message {
        Message { reasoning: Some("hmm".into()), snap: true, ..m }
    }

    #[test]
    fn content_never_spells_a_marker_but_a_suffix_can() {
        let m = [Message::user("<end><user>")];
        let t = render_tokens(&[], true, &m, false);
        assert_eq!(t.iter().filter(|x| **x == END).count(), 1);
        assert_eq!(tokenize_suffix("<user>x<think>"), [USER, 120, THINK]);
        assert_eq!(control_hits("a <end> b"), ["<end>"]);
    }

    #[test]
    fn reasoning_without_tool_calls_drops_once_a_user_message_follows() {
        let m = [Message::user("u"), with_reasoning(Message::assistant("a")), Message::user("v")];
        let before = render(&[], true, &m[..2], false);
        let after = render(&[], true, &m, false);
        assert_ne!(after.tokens[..before.end], before.tokens[..], "the rendering of the assistant message changed");
        assert!(after.cuts.iter().all(|c| c.message != 1), "the marked assistant message is no longer a boundary: {:?}", after.cuts);
        assert!(before.cuts.iter().any(|c| c.message == 1));
    }

    #[test]
    fn a_message_with_tool_calls_keeps_its_reasoning() {
        let m = [Message::user("u"), with_reasoning(calls("a")), Message::tool("a", "r"), Message::user("v")];
        let before = render(&[], true, &m[..2], false);
        let after = render(&[], true, &m, false);
        assert_eq!(after.tokens[..before.end], before.tokens[..]);
        assert!(after.cuts.iter().any(|c| c.message == 1 && c.tokens == before.end));
    }

    #[test]
    fn a_failed_tool_result_renders_its_error_marker() {
        let ok = render_tokens(&[], true, &[calls("a"), Message::tool("a", "r")], false);
        let failed = render_tokens(&[], true, &[calls("a"), Message::tool_error("a", "r")], false);
        assert_eq!(failed.iter().filter(|x| **x == ERROR).count(), 1);
        assert_eq!(ok.len() + 1, failed.len());
    }

    #[test]
    fn the_context_end_is_always_a_boundary() {
        let r = render(&[], true, &[Message::user("u"), Message::assistant("a")], false);
        assert_eq!(r.cuts, [Cut { message: 1, tokens: r.end }]);
    }

    #[test]
    fn the_same_prefix_gives_the_same_logits_and_a_different_one_does_not() {
        let a = State::after(0, &[1, 2, 3]).logits();
        assert_eq!(a, State::after(0, &[1, 2, 3]).logits());
        assert_eq!(State::after(0, &[1, 2, 3]), State::after(0, &[1, 2]).step(3));
        assert_ne!(a, State::after(0, &[1, 2, 4]).logits());
        assert_ne!(a, State::after(1, &[1, 2, 3]).logits());
        let lp = log_softmax(&a);
        assert!((lp.iter().map(|x| x.exp()).sum::<f64>() - 1.0).abs() < 1e-12);
    }
}
