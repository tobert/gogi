# Where the implementations disagree

Found by reading mkviolet (`src/held.rs`, `src/council.rs`, `src/council_server.rs`,
`src/council_v2.rs`) and kaijutsu's `kaijutsu-mk` on the `council-v2` branch against the
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
| 13 | A pin set's `GET` | `snapshots` is a list of ids. | Accepts ids or objects with an `id`. | Accepts both. The draft gives no shape. |
| 14 | An unknown `role` | `400` with a message naming the role. | Decode error. | Decode error; still a `400` on a server built on this crate, but the text differs from mkviolet's. |

mkviolet has no verifier of the relations; `gogi-proto::math::verify` is kaijutsu's, ported, and
checked against mkviolet's algorithm in `crates/gogi-proto/tests/relations.rs`.
