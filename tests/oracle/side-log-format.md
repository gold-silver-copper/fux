# Oracle side-log format (frozen)

Frozen before any change to fux. The oracle (design 9 of the provenance prompt) reads this format as written; fixture programs write it. Changing it means a new version of this file, recorded in the ledger as a deviation.

## Where

A fixture program writes its side log to the path in the environment variable `FUX_ORACLE_LOG`. If the variable is unset, the fixture writes no side log and otherwise behaves the same.

The file is JSON Lines: one JSON object per line, appended and flushed before the fixture continues. Every record has:

- `seq`: an integer, starting at 1, incremented by one per record across the whole process (all threads share one counter, under one lock).
- `ev`: the event kind (below).

All bytes are carried as `b64`: standard base64 of the exact bytes.

## Events

| `ev` | Fields | Meaning |
|---|---|---|
| `start` | `name`, `argv` | The fixture started. First record. |
| `mode` | `raw`, `echo`, `alt` (booleans) | The fixture changed its terminal: `raw` = it turned canonical (line) mode off; `echo` = the terminal's ECHO flag is on; `alt` = it is drawing on the alternate screen. Written after the change took effect, before anything else. Also written once right after `start` with the initial state. |
| `block` | `on`: `read`, `sleep`, `poll`, `work` | The fixture is about to block: `read` = a blocking read of its terminal; `sleep` = sleeping; `poll` = waiting in poll/select with a timeout; `work` = busy computing. Written immediately before blocking. |
| `read` | `b64`, `consumed` | Bytes the fixture read from its terminal. `consumed` = total input bytes read so far, including these. |
| `write` | `b64`, `role`, `answers`, `thread` | Bytes the fixture wrote to its terminal. Written under the same lock as the write itself, so record order is write order. `role`: `echo` (the fixture is echoing or redrawing what was typed: its own line editor) or `output` (anything else it prints, including prompts). `answers`: the value of `consumed` that this write reacts to, or `null` if it reacts to no input (startup, a timer, another thread's work). `thread`: a name for the writing thread (`main` if single-threaded). |
| `bell` | `answers`, `thread` | The fixture rang the bell (it also writes the BEL byte in a `write` record, role `output`). |
| `exit` | `status` | The fixture is about to exit with this status. Last record. |

## Rules the oracle relies on

1. **Every byte the fixture writes to its terminal appears in exactly one `write` record,** in order. A fixture never writes to its terminal outside the side log's lock.
2. **Bytes on the terminal that no `write` record covers are the kernel's echo** (line mode with ECHO on) or come from another process. Fixtures that start other processes say so in their own description; the plain fixtures don't.
3. **A write's `role` is the ground truth for echo versus output.** A line editor's redraw of the typed line (after a printable key, backspace, ctrl-u, ctrl-w, arrows, delete, a paste) is `echo`. Its reaction to the key that submits the line is `output`, except the newline it prints to end the typed line, which is `echo`.
4. **`answers` is the ground truth for which input a write followed.** The oracle maps input byte offsets to fux's input units by counting the bytes fux wrote.
5. **`block` with `on: read` is the ground truth for "reading".** Between a `block on read` and the next `read` record, the fixture is waiting for input.
6. **Determinism.** A fixture's behaviour is a function of its arguments, its input and a seed argument (`--seed N`, default 0). Timers use the seed for their delays.

## Example

```json
{"seq":1,"ev":"start","name":"echoer","argv":["--seed","3"]}
{"seq":2,"ev":"mode","raw":true,"echo":false,"alt":false}
{"seq":3,"ev":"write","b64":"PiA=","role":"output","answers":null,"thread":"main"}
{"seq":4,"ev":"block","on":"read"}
{"seq":5,"ev":"read","b64":"aA==","consumed":1}
{"seq":6,"ev":"write","b64":"aA==","role":"echo","answers":1,"thread":"main"}
{"seq":7,"ev":"block","on":"read"}
{"seq":8,"ev":"read","b64":"DQ==","consumed":2}
{"seq":9,"ev":"write","b64":"DQo=","role":"echo","answers":2,"thread":"main"}
{"seq":10,"ev":"write","b64":"WU9VIFNBSUQ6IGgNCg==","role":"output","answers":2,"thread":"main"}
{"seq":11,"ev":"exit","status":0}
```
