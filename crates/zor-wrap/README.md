# zor-wrap

`zor-wrap` is the standalone PTY wrapper: it runs one command in a pseudoterminal, forwards
every byte, resize, signal and exit status unchanged, and publishes the agent state it observes
as OSC 7877 into the passthrough stream. It needs no fux; a terminal, tmux, or koh sees the
state in the window title, and an optional event stream carries the same observations as JSON
lines. Detection reuses zor's rule sets, screen emulation and hysteresis over the wrapped bytes.

## Usage

```text
zor-wrap [options] [--] <command> [args…]    # default: $SHELL -l
zor-wrap --events <path> …                   # unix socket or fifo event lines
zor-wrap --events - …                        # event lines on fd 3
zor-wrap --title never|prefix|replace …      # default: prefix
zor-wrap --no-osc …                          # title updates only
zor-wrap --rules <dir> …                     # later rule sets replace earlier ids
zor-wrap --agent <id> …                      # force one rule set
zor-wrap --debug …                           # diagnostics on stderr
```

Everything else passes through untouched. Child output reaches stdout byte-for-byte before the
wrapper appends its own OSCs; stdin, window size (including pixels), signals, and exit status
propagate to the child. The wrapper does not set `TERM`, answer terminal queries, or implement
keyboard protocols. A wrapper started inside another wrapper (`ZOR_PID` is set) executes the
command transparently instead of adding a second PTY and emulator layer. `SIGUSR1` writes the
exact observed detection window to `TMPDIR` with owner-only permissions.

The wire format and event contract are documented in zor's
[OBSERVATION-CONTRACT.md](../zor/OBSERVATION-CONTRACT.md); the rule sets are zor's bundled
rules (`zor agents`, `zor check`).

## License

MIT
