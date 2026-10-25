# Standing rulings for grading agent runs

Written before the first review; applied the same way to every batch, every set, every tool.

## Final grades

- **worked**: the checker passes, and the transcript shows the task done plainly with the tool's documented commands.
- **hack**: the checker passes, but only through a workaround (below).
- **impossible**: the checker fails, and no plain or hacky way with that tool (as it was built for that run) would pass. Say what is missing.
- **failed**: the checker fails, but a way existed with that tool. Say what the agent missed and why (docs, errors, or the agent's own mistake).
- **invalid**: the spec itself is wrong (checker, fixture or goal), so the run says nothing about the tool. Say what is wrong; the spec is fixed or dropped for every configuration alike.

## Plain (worked)

- One wait per expected event (a prompt, a marker, quiet, exit), with or without a timeout the task allows.
- A bounded wait with a timeout that the tool provides, used once, then acting on what it says.
- Reading `help`, `explain` or the guide during the task, as often as needed.
- Starting with a wrong command, reading the error, and fixing it: learning, not a workaround.
- Capturing the screen or scrollback once the program has shown what is needed, and extracting a value from it with ordinary text tools (grep, sed, awk, a few lines of Python) when the task is about that text.
- Several small calls where one would do: inefficiency is measured (calls, time), not graded.
- Using the tool's scripting (fuxth words, tmux command sequences) to loop over panes.

## Workarounds (hack)

- `sleep` or a polling loop (sleep then capture, repeated) where the tool could have waited for the condition; or any fixed sleep whose length was a guess about timing. A sleep the task itself asks for is not a hack.
- Scraping the screen for a fact the tool should report directly (a pane's exit status, whether a program is still running, its size, its title, which pane runs what), when no command reports it.
- Raw escape sequences or byte codes typed for a key the tool has no name for.
- Killing and restarting a pane, a server or a program to get unstuck, when the task didn't call for it.
- Running the program outside the multiplexer, or redirecting its output to a file through the shell, to read what the multiplexer should have shown.
- Shell tricks standing in for a missing command (wrapping the program in `sh -c '...; echo $? > f'` to learn its exit status, `script` or `stdbuf` to get output out).
- Retrying the same action blindly until it works.

## Not allowed (the run counts as failed, noted as "cheated")

- Reading the fixture's source code or the spec folder.
- Reading the tool's source code.
- Writing the answer from a guess without observing it.

## Causes and layers

For hack, impossible and failed: the ONE decisive missing piece or mistake, named generically so causes cluster ("wait for quiet", "exit status of a pane", "key name for F-keys", "capture with colours stripped", "find pane by command", "wait for any of several markers", "docs: help-agents doesn't mention X"), with its layer: cli, waiting, reading, input, session, docs, errors, server, design_rule. For failed, also "agent" when the docs were clear and the agent erred.

## Flaky

A spec run twice on one configuration with different final grades is flaky. Look at why: timing in the fixture (then the spec is invalid), or the tool (then it is a finding).

## Requirement from the user (2026-10-13): fux runs inside a sandbox

Reversed on 2026-10-25: the FIFO door is deleted, and fux is controlled
through its Unix socket alone. Every tool, fux included, now runs through the
proxy helper outside the sandbox; what follows is kept as the record of the
earlier sets.

- Baseline (today's fux) runs through the proxy helper: its private-socket check refuses
  every path under a user namespace (root shows as uid 65534), and Codex's sandbox blocks
  AF_UNIX connect and bind outright (socketpair and mkfifo work).
- The redesign must run inside both kinds of sandbox: uid-remapped (bubblewrap, Flatpak,
  rootless containers) and IPC-blocking (Codex). Set B on the new fux runs with no proxy,
  the agent calling fux directly from inside Codex's sandbox.
- A run on the new fux that fails only because fux can't reach its server from the sandbox
  is graded impossible, layer server, cause "runs inside a sandbox".

## Ruling added during Set A review (applies to every batch and tool, earlier ones normalised)

- A capture taken with no wait before it, that shows what the agent needed only because enough time
  happened to pass (the agent's own delay between calls), is a timing guess: hack, cause "wait for
  output text" (or the specific missing wait). A capture after a wait that ended on the right event,
  or of state that was certainly already there (the program had exited; an earlier capture already
  showed it), is plain.

## Ruling for tools whose actions wait (added at the lock, before any new-fux run was reviewed)

- On a tool where every action waits for its pane to settle and answers with its state and what
  came (the new fux: `%2 reading`, `%2 reading · done 0`, `%2 exited 3`), reading that answer is a
  wait that ended on an event: plain, when the state says the program waits for input, a command
  ended, or the program exited, or when the answer holds what the agent needed.
- Acting on an answer whose state says `busy` or `waiting` as if the program were done, and being
  right only because enough time passed before the next call, is the timing guess above: hack.
- Sending input after an answer says `reading` is plain: the tool said the program was waiting.

## This round's addendum (bench-4way, frozen with the plan, before any main run): four tools

Tools: fux, tmux, zellij and herdr, each judged by these same rulings.

- **A tool's documented feature is plain, whatever it is.** For example: tmux `wait-for`, hooks, `pipe-pane`, `capture-pane -S`, `if-shell`; zellij `subscribe`; herdr `pane wait-output` and its other waits; fux's waits and answers. Reading the tool's help, man page or skill text is plain. Learning by trying, reading an error and fixing it is plain.
- **Polling where the tool has no wait.** Polling or sleeping is a workaround (hack) only where that tool had a documented way to wait for the condition the agent needed and the agent did not use it. Where the tool had none, a run that passes the checker by polling is graded **`worked_polling`**: "worked, by polling". It is counted apart from `worked` and from `hack`, and its cost (calls, time, sleeps) is measured like any run. The judge names the condition and says why the tool had no wait for it (from its docs).
- The same holds for reading: scraping the screen for a fact is a hack only where the tool reports that fact directly (an exit status, a pane's size, whether it runs). Where it doesn't, scraping is `worked_polling` when it was needed to poll, or plain when it was a single read.
- **impossible** stays for a task the tool cannot do at all, plainly or not (for example, setting a pane's exact size when the tool has no way to). Name the missing capability.
- The judge cannot be blinded to the tool (its commands name it). Grade by these rulings, not by taste, and never compare tools in a grade.
- Errors: for every multiplexer call that failed (non-zero exit or an error message), the judge classifies its cause: **agent** (wrong syntax, wrong target, a misread of the docs that were clear), **tool** (a misleading or wrong error, a bug, docs that disagree with behaviour), or **wait-timeout** (a wait whose time ran out). Report the counts per run.

## Clarification added during review (2026-10-15, after the first two batches; applied to every batch and tool alike, earlier batches normalised)

- Whether a tool "had a documented way" to wait for or do something is decided by the capability table written before the tasks: `agentbench/compare/writers/caps.json` (the frozen version beside it as caps-frozen.json) (`yes`, `workaround`, `no` per tool per need). Map the condition to its need (wait for text → wait-text; for quiet → wait-quiet; for an exit → wait-exit; an exit status → exit-status; a click → mouse; a size → set-size; and so on).
- Where the table says `yes` for that tool, polling, scraping or raw bytes in its place is a **hack**.
- Where it says `workaround` or `no`, what the agent did in its place (polling, scraping, raw escape bytes for a click, a helper client for a size) does not make the run a hack: grade it **worked_polling** ("worked, by a workaround the tool forces"), and name the need. A run with also a workaround the table doesn't excuse is still a hack.
- Assembling a wait from a tool's primitives through the shell (for example tmux `pipe-pane` into a script that runs `wait-for -S`) is plain when an agent does it; not doing it is not a missed documented wait.
