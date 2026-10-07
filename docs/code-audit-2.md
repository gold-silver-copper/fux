# Code audit, second pass: fux, fux-vt and fuxix

A second reading of the three crates as a whole, at `4f3975b` (after the first audit's PR, #108), and what came of it. The first audit is [`code-audit.md`](code-audit.md). It changed no behaviour, and it left bugs as follow-ups. This pass reads the code again, finds what the first pass missed, and fixes the bugs.

**How it was read.** Every source file was read line by line, split by area among six passes. Each pass was given what a reader and a user need, and not the first audit, so that it came to the code fresh. Each was asked for bugs as well as for what costs a reader:

- a bug is behaviour that contradicts the code's own docs, its tests, a reference it cites, or what a real program needs;
- each bug had to come with a reproduction, and most were run against the public API, in a scratch crate outside the repo;
- each finding acted on here was checked again before it was changed: a bug by a test that fails on `main` first.

**The rules.** A bug is fixed by the smallest change, one per commit, proven by a test that failed before it. Nothing else changes behaviour. Where the right behaviour is a choice, not a bug, it is left for the user, with both readings. Nothing gets slower or larger. Instructions are measured by the working tree's counts before and after each change, built in the same place (see the first audit's H1).

## Beside the first audit

**What it missed.** Above all, bugs. It looked for what costs a reader, and found two bugs on the way (its K1 and X27). This pass, looking for bugs, found about thirty more, in every area.

**What its own changes made worse:**
- A `use` it added in the middle of `keys/decode.rs` (KQ12).
- `PaintClock::now()`, a setter that reads like a getter (XQ2).
- Two structs named `Terminal`: the server's, which it added, and `outer`'s (XQ2).
- A comment it wrote on `Terminal`, "held only while its client is attached", which a failed write makes untrue (XQ4).
- A line it lengthened past the width in `grid.rs`, with a citation a reader cannot follow ("finding 017 of the Bevy version") (SQ15).

All are fixed here.

**What it left, decided again:**
- Its follow-ups (section 9 of the first audit) are each decided below: most fixed as bugs, the rest left as choices, with reasons.
- Its "leave" decisions stand, unless a finding below says otherwise.

## What the code does well

The first audit's list holds. These readers added:

- **The fast paths' oracles go further than the first audit said.** Printable runs, UTF-8 runs, the escape and CSI loop and OSC slicing are each held to the byte-at-a-time path, under random options and random chunking. The two reflow layouts are held to each other, margin scrolling to row scrolling, compact cells to the public `Cell` rows.
- **The keys decoder's promise is tested.** "How input is split never changes what it decodes to" is held by splitting at every byte and into every piece size, with a regression case from the fuzzer.
- **Small modules that are complete:** `words.rs` (its grammar documented, quoting round-tripped), `json.rs` (checked against the RFC's vectors), `mouse.rs`.
- **Bounded everything:** the input queue by byte cost, OSC payloads, DECRQSS requests, the keyboard stacks.
- **Ids taken all or nothing**, tested.

## Findings

Ids are by area: K keys, M session, commands and config, R render, overlay and copy, X server, client, protocol and fuxix, P parser and screen, S storage. **B** marks a bug and **Q** a quality finding. "Choice" marks behaviour that may be deliberate: it is left, with both readings.

### Bugs

| Id | Where | What happens | What should | Decision |
| --- | --- | --- | --- | --- |
| KB1 | `keys/decode.rs` `deadline` | While an over-long answer is dropped, the wait is `ESCAPE_DELAY`; the rest of the answer, arriving later, is typed (first audit K1) | `REPLY_DELAY` while dropping, as the module's doc says | fix |
| KB4 | `keys/decode.rs` `csi` | A `CSI ?` answer past 64 parameter bytes: 67 bytes dropped, the rest typed | Dropped to its final byte, as an over-long OSC or DCS is | fix |
| KB5 | `keys/decode.rs` `csi` | An ESC inside a CSI is taken for its final byte: `ESC [ ESC [ A` gives `A`, Up lost | ESC ends the sequence (ECMA-48): Alt-[ then Up | fix |
| KB6 | `keys/decode.rs` `decode` | `ESC O` and a byte no final key names swallow both: `ESC O x` gives nothing | Alt-O, then the byte | fix |
| KB2 | `keys/decode.rs` `expire` | A DCS answer begun while expected, whose window passes before its end, is typed as Alt-P and keys | Once begun as an answer, read as one | fix |
| KB9 | `keys/decode.rs` `csi` | `CSI u` with code 0 or 1 decodes to a control character, which no key name can express (first audit K20) | No key | fix |
| KB7 | `src/input.rs` `mouse` | A press forwarded before an overlay opens keeps `mouse_held` set; its release is never sent | The program never sees a button left down, as its doc says | verify, fix if so |
| KB3 | `keys/decode.rs` `mark` | A wait that ends and another that begins in the same read keep the first's start | Choice: a test pins it | leave: choice |
| KB8 | `keys/decode.rs` | `ESC ESC [ A` is Escape, then Up (rxvt's Alt-Up) | Choice | leave: choice |
| MB1 | `command.rs` `parse` | `kill-server`, `list-keys`, `unbind-all`, `reload` and `list-buffers` take any extra words: `fux kill-server now` runs | "unexpected argument", as every other command says (the parser's own rule) | fix |
| MB2 | `session.rs` `check_workspace_name` | A workspace may be named `@logs` or `+1`, which targets never read as names: `rename -t x +1` then `kill-workspace -t +1` kills the first workspace | Names a target cannot read back refused | fix |
| MB3 | `command.rs` `parse` | The kind word is overridden by a `-t` of another kind: `rename-prompt tab -t %1` titles "rename tab" and renames the pane | A usage error | fix |
| MB7 | `command.rs` `parse` | `swap-pane %M -L` drops `-L` | A usage error, as other commands refuse what they would ignore | fix |
| MB6 | `layout.rs` `resize` | A resize stores weight 1 for children too small to show, for good | Their weights kept | verify, fix if so |
| MB4 | `command.rs`; `config.rs` | `capture-pane foo` says "unknown flag"; `-S +3` and `set history-lines +5` taken where `number()` refuses `+` (first audit M11) | Choice of words; left | leave: changes error words users may match |
| MB5, MB8, MB9, MB10 | `config.rs`, `session.rs` | `clipboard write-only`; two words for a missing buffer; `set buffers` lower takes effect at the next copy; `reload` of a deleted file resets to defaults | Choices | leave: choice |
| RB1 | `copy.rs` `offset`, `cursor_in_view`, `selected` | After the pane grows, copy mode's cursor and selection are drawn on other rows than the ones `y` copies | The view's top clamped once, for all of them | fix |
| RB2 | `copy.rs` `key`; `render.rs` | On a client larger than the pane, copy mode moves and pages by the client's rows, and its cursor can sit below the rows shown | The rows shown | fix |
| RB3 | `overlay.rs` `panel_room`; `render.rs` `surface` | On a screen too short for its lines, a list shows only "▼ n more" and its help: no entry, not the selected one | The selected entry stays | fix |
| RB4 | `copy.rs` `enter` | Entered with the program's cursor on a wide glyph's second half: the highlight and what `y` copies differ | Snapped to the glyph's start, as every move snaps | fix |
| RB7 | `overlay.rs` prompt | The text "at most 4096 bytes" can reach 4099 | At most 4096, as a paste into it is | fix |
| RB5 | `render.rs` | Repeat mode hides the pane's cursor; a comment says the layout stays in view | Choice | leave: the comment is corrected |
| RB6 | `render.rs` `text` | Zero-width characters are left out of fux's own text (tab names) | A limitation | leave |
| XB1 | `client.rs` `terminal_taken`, `attached` | Relaying, frames read with `Frame::Terminal` wait in the decoder until the socket next has bytes; an `Exit` buffered at the end is lost (first audit X27) | Decoded before polling, and before returning | fix |
| XB2 | `client.rs` `terminal_taken`; `fuxix` `recv_with_fd` | If the descriptor is lost in passing, the server never sends `Frame::Terminal`, and the client waits for good in raw mode, throwing its paints away | A paint before `Terminal` taken as "not taken" | fix |
| XB4 | `server.rs` `give_back_tty` | Giving the terminal back drops what it did not take, the title's restore with it | What it did not take sent to the client to write, as it writes paints | fix |
| XB5 | `server.rs` `flush_tty` | A failed write to the taken terminal drops it silently; keys are then read by no one (first audit X18) | The client detached, as a failed read does | fix |
| XB6 | `server.rs` `stop` | Stopping sends `Exit` but leaves the client attached: paints go on being made for it | Detached, as `flush_outbox` does | fix |
| XB9 | `client.rs` `attach` | stdin is checked and sent, stdout sized and written: `fux attach > file` leaves the terminal on the alternate screen | Choice: stdout redirected is not supported | leave: choice |
| XB10 | `server.rs` | A resize read from the terminal, then the client's `Resize` for the same size: two whole repaints (first audit X16) | One | fix |
| XB3 | `client.rs` | Detached by a signal, the client restores the terminal while the server may still write a paint to it | A bounded wait for the server | leave: a short window, and a wait changes how detaching by a signal behaves; a follow-up |
| XB7, XB8 | `server.rs`, `main.rs` | Stopping waits a second per stuck pane; `--socket` without a value takes the default | Choices | leave: choice |
| PB1 | `screen.rs` CUU, CUD, CNL, CPL | From outside the scroll region, the cursor goes to the screen's edge (`\e[2;3r\e[5;1H\e[9AX`, 5x5: X on line 1) | The margin it comes to, as xterm and Ghostty do (line 2; checked with `replay`) | fix |
| PB2 | `parser.rs` `run` | `process_until_frame` does not stop after an XTRESTORE that begins a frame, and later stops at an `h` inside an OSC string | Right after the sequence that set the mode, as its doc says | fix |
| PB3 | `screen.rs` `escape` | Every string ended by ST reports `ESC \` to `Sink::unhandled` | ST is handled | fix |
| PB4 | `parser.rs` `encoded_version` | `1.2` encodes as 102 | 10200, as its doc says | fix |
| SB1 | `grid.rs` `resized` | A resize without reflow drops the pending wrap, at the same width too | Kept, as reflow keeps it (DEC STD 070 D.6.1) | fix |
| SB2 | `grid.rs` `resized` | A resize without reflow keeps the scroll region, which may clamp to one row | Reset, as reflow resets it and as its doc says xterm does | verify with xterm, fix if so |
| SB3 | `screen.rs` `wrap` | A wide glyph that does not fit leaves the last column's old character, which copy and reflow read as text | Blanked, as Ghostty blanks it | verify with xterm, fix if so |
| SB6 | `grid.rs` `reset_links` | Leaves `Meta::linked` set on rows whose arrays it drops | Cleared (latent: unreachable today) | fix |
| SB4, SB5 | `reflow.rs`, `grid.rs` | A coloured erased tail counts as text in reflow; a resize without reflow clears soft wraps | Choices (one tested) | leave: choice |
| SB7 | `compact.rs` | Appending in place can pass the short text's bound by 12 bytes | A doc that says so | fix the doc |

### Quality

Ranked by what they cost a reader. Those already in the first audit say so and keep its decision unless noted.

| Id | Where | What | Decision |
| --- | --- | --- | --- |
| XQ1 | `docs/rewrite-design.md`; `server.rs:1` | Still stale, though it promises to be corrected where it stands: titles and bells "not passed on"; `bind` "letters only"; "no threads … the only blocking call"; the launcher "resets the signal mask"; the server "one thread" | act |
| XQ2 | `server.rs`, `outer.rs` | Two structs named `Terminal`; three `typed`; `PaintClock::now()` a setter (two of these the first audit's own) | act |
| XQ4 | `server.rs` `Terminal` | Its doc's rule is untrue after a failed write (fixed with XB5) | act |
| PQ4, SQ15 | `lib.rs` `Row::version`; `history.rs:34`; `cell.rs:108`; `grid.rs:1607, 999` | `version` "with nothing else" (links and resizes change it); "a block made is half full"; attributes "copied onto erased cells"; a citation and a line the first audit wrote; a redundant test | act |
| PQ5, PQ11, PQ12, PQ13, PQ14, PQ15, PQ17, PQ6 | `README.md:53`; `screen.rs` `ascii`, `changed_since`; `style.rs`; `parser.rs` `Options::events`; `link.rs` `Hyperlink::key`; `lib.rs` `Window`; `palette.rs` `Answer` | Docs that say more or less than the code | act |
| KQ1, KQ3, KQ8 | `src/input.rs`, `server.rs:503`, `decode.rs` `timeout`; `keys.rs` `folded`; `decode.rs:5-6` | "Escape" for every decoder wait; `folded` overclaims; "each key has one xterm encoding" | act |
| SQ1, SQ5, SQ6, SQ12, SQ14 | `grid.rs`, `compact.rs` | `Meta::linked` and `Grid::linked` documented wrongly; `Text::len` counts half of what `is_empty` sees; `empty` and `clear` swapped in sense; `made(.., unmade)`; `Meta::width` always the width for screen rows, unsaid | act: docs, and `Text::len` renamed |
| MQ7, MQ8 | `pane.rs` `output`; `layout.rs` `distribute`; `session.rs` `Workspace` | `output` "returns whether a reply was dropped" (only the first time); `mins` doc; a workspace always has a tab, unsaid, and a branch dead because of it | act |
| XQ5, XQ6, XQ9, XQ10, XQ12 | `protocol.rs`; `fuxix/src/socket.rs`, `lib.rs`, `process.rs`; `server.rs:897` | `buffered()` doc; variant docs on one variant each; a SAFETY comment that asserts a bound it does not check; "256 frames" where 257 pass; fuxix's users and `Signal` docs; `//` where rustdoc needs `///` | act |
| RQ8, RQ7 | `render.rs` `bar`; `overlay.rs`; `view.rs:89`; `README.md:72` | The bar's order, the overlay module's scope, one doc for two fields, repeat mode's "any other key" | act |
| PQ7 | `link.rs` `free_unused` | A parameter every caller passes empty | act |
| RQ3, RQ4, RQ5 | `render.rs`, `overlay.rs`, `session.rs` | "Prefix and keys" built four times; "is this binding in the layer" four ways; list navigation twice | act |
| KQ2 | `src/input.rs`, `overlay.rs` | The root-binding test written twice | act |
| MQ1 | `command.rs`, `config.rs`, `session.rs`, `lib.rs` | A command's name in six places, with no test tying `USAGE` to the parser | act: a test |
| MQ4 | `session.rs` | `layout::split`'s result unchecked where a pane already exists | act: written down |
| PQ19 | `screen.rs` `rectangle_checksum` | `clamp` panics on min > max; safe only by an unstated invariant | act |
| XQ11, RQ1, MQ3, MQ2, KQ5, PQ2, PQ3, SQ2, SQ3, SQ4, SQ7-SQ10, SQ13, SQ16, SQ17 | | Long functions and structs; duplication on hot paths; restructures | leave, as the first audit did (its decisions and measurements hold), but SQ4's dispatch: act if free |
| RQ10, KQ4 | `render.rs` `Frame::of`; `decode.rs` `csi` | Allocations per compose and per CSI key | leave: a measured PR of their own |
| MQ13 | `tests/` | Polling loops eight times; fixed sleeps as premises | act for the loops if free; leave the sleeps (as the first audit) |

## What was not read closely

- `fux-vt/src/unicode/tables.rs`, generated.
- The packages beside the crates, but where a finding led there.
- The integration tests test by test.

## Changes made

Filled in as each finding is acted on.
