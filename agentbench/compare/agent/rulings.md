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
