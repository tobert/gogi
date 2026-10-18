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
| 4 | Tools | Not checked beyond `queue` naming one; reads the OpenAI shape `{type, function: {name, parameters}}`. | Each needs a name; `parameters` must be an object. Its queue tools are built in the OpenAI shape (kaijutsu's `council/seat.rs`). | `{name, description?, input_schema}`, names distinct and not empty, any other member refused. The OpenAI shape does not decode. |
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
| 17 | `model` on a build, raw read, or generation | Decoded with `gogi-proto`'s types and not checked (inferred: `src/council_v2.rs` never reads `req.model`). | Sent on every request. | Required on all four requests; a missing `model`, or a name that is not a served model's `model` or alias, is a `400` naming `model`. |
| 18 | `model` in a build's response and in `GET /snapshots/{id}` | Not sent. A generation's `done` and a raw read's response send it. | n/a | Required in all four. |

mkviolet runs `gogi-proto::math::verify` on each decision before it replies; that function is kaijutsu's, ported, and
checked against mkviolet's algorithm in `crates/gogi-proto/tests/relations.rs`.

## Ahead of both

Rows 19 to 28 are contract changes that mkviolet and kaijutsu have not taken yet: both pin an
earlier gogi rev. They are not choices between the two. Until each side moves to the gogi rev that
has them, `gogi-conformance` fails mkviolet on them and kaijutsu's requests do not decode on a
server built on this crate.

| # | Where | mkviolet | kaijutsu | gogi-proto |
|---|---|---|---|---|
| 19 | A decision's case | `state`; inline `questions` accepted (`src/council_server.rs`); the gateway fields `session_id`, `user`, `trace`, `provider` accepted. | Sends `state`. | `case`; `spec_id` required; no inline questions or gateway fields. |
| 20 | The case limit | `limits.state_bytes`, and `param: state_bytes` on its `413`. | n/a | `case_bytes`. |
| 21 | A call | `{id, name, arguments}`. | The same. | `{id, name, input}`; a reply's unparsed call is unchanged. |
| 22 | A failed tool result | No `is_error`; the message does not decode. | Not sent. | `is_error: true` on a `tool` message; the template renders it. |
| 23 | `usage` | A build's `tokens`, `kept`, `fed` at the top level; a decision's `input_tokens`, `output_tokens`, `fed_tokens`; none on a raw read; a generation's without `reasoning`. | Reads those shapes. | One `{prompt, kept, fed, completion, reasoning}` on all four; `math::verify` checks `kept + fed = prompt`. |
| 24 | A generation without `max_tokens` | Uses 4,096. | Always sends it. | A `400` naming `max_tokens`. |
| 25 | `finish` | `stop` or `length`; `length` also when the reply reaches the slot's end. | Decodes the earlier `stop`, `length`, and `cancelled`; its own provider events already use `end_turn` and `tool_use`. | `end_turn`, `tool_use`, `max_tokens`, or `context_full`. |
| 26 | A `token` event's `phase` | `think`, `answer`, or `tool`, with marker pieces such as `</think>` and `<tool_call>`. | n/a | `reasoning`, `content`, or `call`; `piece` is display text. |
| 27 | Request ids | No `request-id` header; error bodies and the `error` event have no `request_id`. | Logs none. | Every response has the header, and every error body has `request_id`. |
| 28 | `done`'s other members | Sends `seed` and `ms`. | n/a | Reads `seed`; ignores `ms`, which the contract does not name. |
