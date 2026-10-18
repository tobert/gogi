# Where the implementations disagree

Found by reading mkviolet's `src/held.rs`, `src/council.rs`, `src/council_server.rs`, and
`src/council_v2.rs`, and kaijutsu's `kaijutsu-mk` crate on its `council-v2` branch, against the
drafts in this directory. `gogi-proto` follows mkviolet where the two differ and says so
here. Each item is an observation from the code unless it says "inferred".

| # | Where | mkviolet | kaijutsu | gogi-proto |
|---|---|---|---|---|
| 1 | A `tool` message names a call | Its `tool_call_id` must match a call of the assistant message before its run of tool messages. | Checks only that it is non-empty. | mkviolet's rule, which the draft states. |
| 2 | A call id | Printable text of 1 to 256 bytes. | Non-empty. | mkviolet's. |
| 3 | A context with no messages | `400`. | Accepted by `Context::validate`. | `400`. |
| 4 | Tools | Not checked beyond `queue` naming one. | Each needs a name; `parameters` must be an object. | Not checked (`tools` stay JSON values). |
| 5 | A pin owner's length | At most 128 bytes. | At most 128 characters. | Bytes. The draft says characters. |
| 6 | A `reads` top token | Reports `{token, id, logprob}`, as the draft says. | `TopToken` has no `id`, so a decoded read loses it. | Has `id`, optional on decode. |
| 7 | A linear pool | Sums the weighted probabilities. | Also divides by their total, which is 1 up to rounding. | Kaijutsu's; the two agree within 1e-9 on 400 random cases. |
| 8 | A loglinear pool | Adds `w * ln(p)`; a probability that underflows to 0 drops its option. | Adds `w * (logprob - mass)`, which stays finite. | Kaijutsu's. Same results except at underflow. |
| 9 | A non-finite logprob | Accepted. | Refused. | Refused. |
| 10 | A decision with no `contexts` in v2 | Its one read names `context: 0` (inferred from `numbered: v2` in `respond`). | The verifier expects `context: null`, as v1 and the draft's "Unchanged from v1" say. | `null`. `gogi-conformance` fails mkviolet here if the inference holds. |
| 11 | `context_full`'s `context_tokens` | The longest slot, one more than `limits.context_tokens` (inferred from `identity_v2` using `longest - 1`). | n/a | The suite allows a difference of 1 and names it in the check. |
| 12 | A snapshot's `tokens` | Left out when the server cannot count them (`if let Some(n)`). | Required. | Required in `SnapshotState`. |
| 13 | A pin set's `GET` | `snapshots` is a list of ids. | Accepts ids or objects with an `id`. | Accepts both. The draft now says ids. |
| 14 | An unknown `role` | `400` with a message naming the role. | Decode error. | Decode error; still a `400` on a server built on this crate, but the text differs from mkviolet's. |
| 15 | The `template` a decision, a held spec, and `identity` report | A decision and a held spec reported the letters template alone; `identity` added the chat template. | n/a | A decision reports its served model's `template`, which `gogi-conformance` checks; mkviolet now reports it. A held spec reports none in v2, because each served model renders it with its own template; `HeldSpec` still accepts mkviolet's. |
| 16 | `GET /council/v2/identity` | One model's identity, limits, and capabilities at the top level. | Reads the one-model shape. | `models`, each with its status, limits, and capabilities, beside the server's `limits` and `capabilities`. The one-model shape does not decode, so `gogi-conformance` stops at identity until mkviolet changes. |
| 17 | `model` on a build, raw read, or generation | Decoded with `gogi-proto`'s types and not checked (inferred: `src/council_v2.rs` never reads `req.model`). | Not sent. | A name that is not a served model's `model` or alias is a `400` naming `model`. |
| 18 | `model` in a build's response and in `GET /snapshots/{id}` | Not sent. A generation's `done` and a raw read's response send it. | n/a | Required in all four. |

mkviolet runs `gogi-proto::math::verify` on each decision before it replies; that function is kaijutsu's, ported, and
checked against mkviolet's algorithm in `crates/gogi-proto/tests/relations.rs`.
