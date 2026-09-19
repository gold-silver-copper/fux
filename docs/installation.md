# Source installation, updates and recovery

This workspace is an **unpublished alpha**. The manifests currently identify fux and zor as
`1.0.0-alpha.1`; that is a source version, not evidence of a published crate, release tag,
installer or downloadable binary. Build from the reviewed checkout you have. There is no
curl installer or automatic remote update procedure here. Active acceptance is fux/zor only;
koh and future iroh-ssh work are outside this procedure.

[verification.md](verification.md) is the authority for commands actually exercised and
platform/provider results. The recipe below is source-derived; it does not claim every
platform has been tested. Read [security.md](security.md) and [ownership.md](ownership.md)
before running plugins or recovering existing tasks.

## Prerequisites and workspace build

- Rust/Cargo compatible with the workspace's `rust-version = "1.95"` and edition 2024.
- A Unix environment with PTYs, process groups/signals, loopback networking and ordinary
  private-file permissions. Current code uses Unix APIs; this is not a Windows support claim.
- A working native linker/toolchain for that host and access to the dependencies in
  `Cargo.lock` (or an already populated Cargo cache). `--locked` refuses lockfile drift; it
  does not mean offline.
- A terminal for interactive viewers. Git is needed for worktree operations. Provider
  executables, authentication and plugin-specific build tools are separate prerequisites;
  installing fux/zor does not install or verify them.

Run from the repository root (the directory containing the virtual-workspace `Cargo.toml`):

```sh
cargo build --locked --release --workspace
```

This builds the fux/zor workspace; `tools/xtask` has its own workspace and is excluded from
that command. The optional fux `bell` feature is not enabled by this default recipe.

## Install to an isolated local root

Use a dedicated root, not a system prefix or an existing production install. Cargo's root
contains `bin/` plus its install metadata; XDG runtime/config/state isolation is a separate
choice shown below. Choose another absolute root if this one already has valuable state.

```sh
export FUX_ALPHA_ROOT="$HOME/.local/fux-alpha"
umask 077
mkdir -p "$FUX_ALPHA_ROOT"
cargo install --locked --path crates/fux --root "$FUX_ALPHA_ROOT" --force
cargo install --locked --path crates/zor --root "$FUX_ALPHA_ROOT" --force
export PATH="$FUX_ALPHA_ROOT/bin:$PATH"
fux --version
zor --version
```

`cargo install --path` builds the local packages (release profile by default); `--force`
replaces only that root's registered binaries. Installing both from the same checkout avoids
intentionally mixing versions. It is not an atomic two-binary update and does not restart
already-running servers. Version text alone cannot distinguish different commits with the
same alpha version; record the source revision with any build/evidence you retain.

For a disposable or isolated local alpha deployment, set the following in **every terminal
that will run its servers or clients**:

```sh
export FUX_ALPHA_ROOT="$HOME/.local/fux-alpha"
export PATH="$FUX_ALPHA_ROOT/bin:$PATH"
export XDG_RUNTIME_DIR="$FUX_ALPHA_ROOT/runtime"
export XDG_CONFIG_HOME="$FUX_ALPHA_ROOT/config"
export XDG_STATE_HOME="$FUX_ALPHA_ROOT/state"
umask 077
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME" "$XDG_STATE_HOME"
export FUX_BRP="$XDG_RUNTIME_DIR/fux/default.brp.json"
```

These variables must be absolute paths. The example runtime directory is intentionally
local to this isolated root; unlike an OS-managed runtime directory it is not automatically
removed at logout/reboot. Do not mistake a leftover descriptor for a live server. The
explicit `FUX_BRP` makes zor use this fux rather than an inherited override from another
session. Ordinary CLI server selection still uses the corresponding runtime directory and
`--server` name; do not rely on a plugin's `ZOR_BRP` environment to redirect the ordinary CLI.

With no overrides, fux uses `$XDG_RUNTIME_DIR/fux`, or `$TMPDIR/fux-<uid>` (falling back to
`/tmp` when necessary). Config/state use `$XDG_CONFIG_HOME/fux` and
`$XDG_STATE_HOME/fux`, falling back to `$HOME/.config/fux` and `$HOME/.local/state/fux`.
zor uses the sibling `zor` directories (including `zor-<uid>` in the runtime fallback).

Runtime and state application directories are created private `0700` and owner-checked.
zor also requires its configuration directory to be private because it contains the machine
catalog; the catalog and action-intent files are written `0600`. fux's general config
directory is not itself forced private by the application, though its saved-layout directory
is private. The `umask` in this recipe keeps new example directories private. If an existing
zor directory is too permissive, inspect its owner/type and correct that specific directory
to `0700`; keep credential-bearing files at `0600`. Do not use a broad recursive permission
change or loosen the check to get past an error.

## Start deliberately, then discover the running contract

In one terminal, keep fux in the foreground:

```sh
fux serve --name default --restore ask
```

In another terminal with the same environment:

```sh
zor serve --name default
```

In a third terminal, inspect the running instances and generated schemas before scripting
mutations:

```sh
fux fux/server.info
fux rpc.discover
fux fux/schema
zor zor/server.info
zor rpc.discover
zor zor/schema
zor status
```

The CLI reads the private descriptor and supplies the token/instance envelope. Do not copy
bearer tokens into shell history or paste descriptors into bug reports. The generated method
schemas describe typed payloads; [protocol.md](protocol.md) describes their lifecycle and
error semantics. `--help` is the source of CLI flags:

```sh
fux --help
zor --help
fux bindings
```

To attach to the existing default workspace without invoking auto-start:

```sh
fux attach --brp "$XDG_RUNTIME_DIR/fux/default.brp.json" --workspace default
```

Once both servers are ready, a simple command can be run under zor:

```sh
zor run -- /bin/sh -c 'printf "alpha task\n"'
```

`zor run` creates an ephemeral workspace and uses retained final evidence for the result;
this is not a provider-integration test. `zor dashboard` opens the task dashboard;
`zor attach TASK` attaches to a specific live task attempt. Closing either view is not a task
stop. Bare `fux` is a convenience that can start a missing server and create its workspace;
its automatically started server uses the default restore mode (`auto`). Avoid that shortcut
when you need a controlled recovery decision.

## Restore is an explicit execution decision

fux's `serve --restore` accepts:

- `ask`: rebuild a saved layout but leave each saved pane pending. Inspect
  `fux fux/session.status`, then choose `fux/session.restore` or `fux/session.skip` per pane.
- `auto`: rebuild the layout and launch all saved pane commands. This is the default, not a
  resume of the old operating-system processes.
- `none`: ignore the saved session and bootstrap the default workspace. Later saves may
  replace the snapshot; copy valuable state before using this as a recovery experiment.

For `ask`, substitute a pane ID returned in `session.status`; each command decides **one**
pane (these are examples with ID 42, not commands to run blindly):

```sh
fux fux/session.status
fux fux/session.restore '{"pane":42}'
# Or, instead of restoring that pane:
fux fux/session.skip '{"pane":42}'
```

Inspect argv/cwd and the new instance before approving execution. Restored history is
historical text, not proof the old task is still running. Invalid session files are renamed
with a `.rejected-<ms>` suffix and a default workspace is bootstrapped; retain the rejected
file privately for diagnosis rather than editing away evidence.

zor restores its journal automatically and reconciles uncertain records. It does not replay
in-flight launches/prompts. Enabled plugin intent may start fresh plugin startup/hook
processes, and controller-owned sidecars/checks/plugin children do not share the survival
guarantee of fux task PTYs. Inspect task/check/plugin/machine status after a controller
restart; an unknown or uncertain result is not a retry instruction. Native resume has
provider-specific eligibility and an explicit operation/fux-instance contract; discover it
rather than treating it as universal process restoration.

## Controlled source update

1. Select/review the replacement checkout and its [changelog](../CHANGELOG.md) and
   [verification record](verification.md). Preserve the old source revision/binaries so an
   alpha-version string is not your only provenance. Do not assume an unreviewed fetch,
   tag, package publication or schema migration exists.
2. Inspect active tasks, checks, plugin runs and machine intents. Plan a quiescent maintenance
   boundary. Detaching viewers does **not** stop tasks; stopping zor does **not** stop fux task
   PTYs, but does stop controller-owned children. Stop the foreground zor server with its
   terminal interrupt when losing those children is acceptable.
3. If updating only a client or staging binaries, leave the running fux process alone. A fux
   restart loses live PTYs; complete or deliberately stop tasks first. Save with
   `fux fux/session.save` before an intentional fux shutdown, then interrupt its foreground
   server. Do not signal an unverified PID taken from a stale descriptor.
4. With writers stopped, make a private backup of config/state, including zor's catalog,
   adjacent intent log, journal/archive, plugin state and fux session files. A live file copy
   is not a coordinated snapshot across apps. These backups may contain credentials, command
   output and sensitive argv. Runtime descriptors are ephemeral credentials, **not** recovery
   state: never restore them over new descriptors.
5. Build/install both packages from the replacement checkout using the commands above,
   preferably into a second isolated root first. Keep both versions available until the new
   pair is checked. Changing PATH does not upgrade a running server, and sequential installs
   are not an atomic deployment.
6. Start the new fux deliberately with `--restore ask`, inspect/restore/skip panes, then start
   zor. Rediscover schemas and inspect the reconciled task state before permitting new
   writes. A fresh server incarnation invalidates old descriptors/grants, attachment
   identities and catalog endpoint snapshots; update bindings explicitly from current
   descriptors. Do not reconnect a stale mutation to a new incarnation or replay an intent.
7. For rollback, stop new writers and use the saved binary/source pair with a compatible
   private state backup. There is no promised forward/backward alpha schema compatibility.
   Copying old state over a running server or simply downgrading a binary cannot undo
   external side effects and may destroy the only evidence of them.

A crash may leave descriptors or temporary files behind. First establish which server, if
any, is alive and which incarnation it owns; never delete a live descriptor or start two
servers under the same name/runtime directory. Inspect the exact leftover paths and logs,
preserve evidence, and remove only confirmed-stale artifacts. Do not repair an uncertain
operation by deleting its intent/receipt or inventing a new operation key.

## Uninstall and support evidence

Stop only the services/processes you own, after deciding the task-loss boundary. Remove the
binaries with Cargo against the same installation root:

```sh
cargo uninstall --root "$FUX_ALPHA_ROOT" fux zor
```

This removes Cargo-installed packages, not the separately retained XDG state/config/runtime
subdirectories. Keep those until you have intentionally handled tasks, worktrees, plugin
state and machine credentials; do not delete worktrees as an uninstall shortcut. Remove the
PATH/XDG overrides from your shell configuration when leaving this isolated deployment.

For a report, include source revision, command, host/toolchain and the relevant redacted
error/result. Check [verification.md](verification.md) for the exercised install/runtime
matrix. Do not call an unrun build, provider, platform or benchmark verified merely because
these commands are documented.
