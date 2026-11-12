use gogi_proto::identity::Capability;
use serde_json::{Value, json};

use crate::{Suite, num};

/// The tokens by which a head may fall short of the reply. A reply of this many tokens or fewer
/// cannot tell a held head from none.
const SLACK: i64 = 8;
/// How many prompts the suite tries to get a reply that the rule covers.
const ATTEMPTS: usize = 4;

const HEAD: &str = "a reply to an unmarked message holds a head";
const BY_PARENT: &str = "the next turn by `parent` feeds only its new message";
const WHOLE: &str = "the next turn sent whole feeds only its new message";

/// Whether a character is white space in the contract's sense: Unicode White_Space and U+001C to U+001F.
fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// A plain first turn: its context, the response, and its prompt and completion.
struct Turn {
    ctx: Value,
    reply: Value,
    prompt: i64,
    completion: i64,
}

impl Suite {
    /// A reply to a message with no `snap` holds a head, and the next turn feeds only its new
    /// message, by `parent` and sent whole. A reply that the rule does not cover is a skip.
    pub(crate) fn plain_turns(&mut self) {
        let all = [HEAD, BY_PARENT, WHOLE];
        if !self.has(Capability::Generate) {
            return self.skip_all(&all, "the server does not declare the generate capability");
        }
        let by_parent = match self.plain_turn("plain-parent") {
            Ok(Some(t)) => t,
            Ok(None) => return,
            Err(why) => return self.skip_all(&all, &why),
        };
        self.head_of(&by_parent);
        let Some(rid) = by_parent.reply["context"].as_str().map(String::from) else {
            self.r.fail(BY_PARENT, format!("the response names no record of its context, so the next turn cannot name a parent: {}", by_parent.reply));
            return;
        };
        let next = json!({"role": "user", "content": "Now say it once more, in other words."});
        let ctx = json!({"parent": rid, "messages": [next]});
        if let Some(second) = self.generate("the next turn by parent succeeds", &ctx, 4) {
            self.feeds_only_its_new_message(BY_PARENT, &by_parent, &second);
        }

        // The same turns, with the whole context sent as a client that keeps no record sends it.
        let whole = match self.plain_turn("plain-whole") {
            Ok(Some(t)) => t,
            Ok(None) => return,
            Err(why) => return self.r.skip(WHOLE, why),
        };
        let mut messages = whole.ctx["messages"].as_array().cloned().unwrap_or_default();
        messages.push(json!({"role": "assistant", "content": whole.reply["content"]}));
        messages.push(json!({"role": "user", "content": "Now say it once more, in other words."}));
        let mut ctx = whole.ctx.clone();
        ctx["messages"] = json!(messages);
        if let Some(second) = self.generate("the next turn sent whole succeeds", &ctx, 4) {
            self.feeds_only_its_new_message(WHOLE, &whole, &second);
        }
    }

    fn skip_all(&mut self, names: &[&str], why: &str) {
        for n in names {
            self.r.skip(n, why);
        }
    }

    /// Generates a plain reply to one unmarked user message, with no thinking and no tool, and
    /// returns it when the rule covers it. `Err` is the reason the last reply was not covered;
    /// `Ok(None)` means a request failed and the failure is recorded.
    fn plain_turn(&mut self, label: &str) -> Result<Option<Turn>, String> {
        let mut why = String::new();
        for attempt in 0..ATTEMPTS {
            let text = format!("Write two short plain sentences about the number {} ({} {} {}).", attempt + 3, label, self.nonce, attempt);
            let ctx = json!({"thinking": false, "messages": [{"role": "user", "content": text}]});
            let Some(g) = self.generate("a plain generation succeeds", &ctx, 160) else { return Ok(None) };
            let content = g["content"].as_str().unwrap_or("");
            let completion = num(&g["usage"]["completion"]);
            why = if g["finish"] != json!("end_turn") {
                format!("the reply ended with {}, not end_turn", g["finish"])
            } else if g["reasoning"].as_str().is_some_and(|r| !r.is_empty()) || num(&g["usage"]["reasoning"]) > 0 {
                "the reply has reasoning".to_string()
            } else if g["tool_calls"].as_array().is_some_and(|c| !c.is_empty()) {
                "the reply has a tool call".to_string()
            } else if content.is_empty() || content.trim_matches(is_space).len() != content.len() {
                "the reply is empty or has white space at its edge".to_string()
            } else if completion <= SLACK {
                format!("the reply has {} tokens, too few to tell a held head from none", completion)
            } else {
                return Ok(Some(Turn { ctx, prompt: num(&g["usage"]["prompt"]), completion, reply: g }));
            };
        }
        Err(format!("none of {} replies met the rule's conditions; the last: {}", ATTEMPTS, why))
    }

    fn head_of(&mut self, t: &Turn) {
        if t.reply["head"].is_string() {
            self.r.ok_with(HEAD, "");
        } else {
            self.r.fail(
                HEAD,
                format!(
                    "the server returned head {} for a reply that ends with end_turn, to a context with no snap on any message; the contract asks for a snapshot id there (the reply: {})",
                    t.reply["head"], t.reply["content"]
                ),
            );
        }
    }

    fn feeds_only_its_new_message(&mut self, name: &str, first: &Turn, second: &Value) {
        let kept = num(&second["usage"]["kept"]);
        let held = first.prompt + first.completion;
        if kept + SLACK >= held {
            self.r.ok_with(name, format!("kept {} of the {} tokens of the first prompt and reply", kept, held));
        } else {
            self.r.fail(
                name,
                format!(
                    "the server kept {} tokens and fed the first reply of {} tokens again; the contract asks it to keep at least {}, the {} tokens of the first prompt and the {} of its reply, less {} (first head {}, second usage {})",
                    kept, first.completion, held - SLACK, first.prompt, first.completion, SLACK, first.reply["head"], second["usage"]
                ),
            );
        }
    }
}
