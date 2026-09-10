# Owned worktree creation

Zor owns worktree intent, git execution and recovery. Fux receives only a working directory and
command when a pane is launched. The worktree CLI uses the same private bounded journal as tasks;
the service exposes equivalent actions through its task worker.

```sh
zor worktree create parser --repo /absolute/repository --branch agent/parser --base HEAD
zor worktree list
zor worktree inspect parser
zor worktree reconcile parser
```

Create accepts a new literal branch name and resolves the requested base to a commit before
mutation. Existing branches conflict; checkout shorthand such as `@{-1}` is rejected. The main
checkout is unchanged. Start a managed task in a ready worktree using its ID:

```sh
zor task start parser-task --title Parser --instance FUX_INSTANCE \
  --workspace default --worktree parser -- /path/to/agent
```

Specify exactly one of `--cwd` and `--worktree`. The launch stores the worktree ID; the task's
attempt links to its managed session and that session links to the launch. Zor checks the ready
worktree's current ownership and registration under its journal lock before recording intent and
again before sending a prepared launch. Fux receives only cwd and argv. Missing, unready, replaced
or mismatched worktrees reject new launches. Dirty files and worker commits are allowed. Native
git/filesystem changes by other programs are not serialized by zor's journal lock.

Identical attached/closed launch retries return stored history even if the checkout later becomes
unavailable; they do not launch again. Changing between a worktree ID and its literal cwd is a
different intent and requires a new task ID. Supplying a path with `--cwd` does not infer ownership
or association. Worktree creation and task start remain separate recoverable operations.

Every worktree has a stable caller ID and a randomly named parent directly inside the selected
zor state directory. The parent is mode 0700 and the checkout is its `tree` child. Arbitrary target
paths are not accepted. Records pin canonical repository/common-directory identities and the
parent's filesystem identity. First readiness also pins the checkout directory's device/inode;
replacement directories are rejected even if they copy the original `.git` pointer. Up to 128
worktree records share the existing 4 MiB journal bound.

Creation states:

- `allocating`: the reserved path is durable before mkdir. If a crash leaves an empty private
  directory at that exact reserved path, explicit reconcile can pin its identity and advance to
  prepared. Nonempty directories or changed ownership are preserved and rejected.
- `prepared`: directory identity is committed; no git add has been submitted. Repeating create
  may proceed from this state. Reconcile alone does not submit creation.
- `creating`: committed before `git worktree add`. Repeating create or reconcile only inspects
  repository registration; it never runs add again after this phase may have been reached.
- `ready`: creation was confirmed. Reconcile checks the same path/branch/repository and exposes
  problems while retaining ownership. Worker commits may advance HEAD after readiness.
- `uncertain`: creation failed or evidence is incomplete. Partial directories/branches are kept
  for reconciliation, without automatic deletion, branch reset, or replacement creation.

Initial readiness requires unique matching registration, an unlocked/non-prunable worktree,
the pinned commit, matching common directory, and a clean tracked checkout against HEAD. A
registered but incomplete checkout is not ready. Once ready, dirty files and new commits are
normal worker activity; this command does not remove them. Inspect and identical ready-create
retries return stored records, including any last reconciliation problem.

Git runs through argv without a zor shell or PTY, with noninteractive input, hooks disabled and
system/global Git configuration excluded. Repository-local checkout filters still follow Git's
semantics. Each stdout/stderr stream is capped at 256 KiB; subprocess phases have deadlines,
with a ten-second worktree-add budget. Deadline/output failures terminate the command's private
process group and reap its child. Native spawn/filesystem/wait calls are not hard real-time bounds.
Killing zor itself may let git finish or encounter closed output pipes and roll back; either
outcome is reconciled. Started service jobs may outlive a disconnected client's reply budget.

## Removing an owned checkout

```sh
zor worktree remove parser
zor worktree remove parser --force
```

Removal requires a ready owned checkout with matching repository, registration, parent and
checkout identities. Normal removal refuses tracked modifications, untracked files and ignored
files. `--force` allows git to discard those files; it never overrides identity or active-use
checks and does not stop processes. Non-closed managed launches (including prepared/uncertain
launches and literal cwd launches inside the checkout) block removal even after task cancellation.
Stop and reconcile them first. Other retained sessions are checked against fux's pinned target,
reported creation cwd and current OS cwd for the root and its descendants. Zor reads cwd through
macOS process metadata or Linux `/proc`, requires resolvable absolute paths, and revalidates the
fux target after sampling. The ancestry walk permits at most 256 processes, checks each parent's
child list again after collecting cwd, retains OS start identities from child discovery through
both passes and around cwd reads, and shares the existing three-second session-inspection
deadline. Linux also bounds threads per process and each child-list read. Missing, denied,
changing, oversized or ambiguous evidence refuses removal; native OS calls retain their usual
latency limitations. These are local
process checks; remote PID interpretation is not supported. A matching retained managed close
supplies historical exit evidence for an adopted view of the same target.

Removal also discovers unadopted panes through fux's existing manager and workspace listings.
The census includes the default local fux runtime and runtimes referenced by retained sessions
without confirmed managed closure. Each discovered pane receives the same launch-cwd,
current process-tree cwd and identity checks; adoption is not a prerequisite for protection.
Discovery permits at most 64 workspaces per runtime, 32 tabs per workspace, and 256 panes
across the census, under the same three-second inspection deadline. Oversized, malformed,
unavailable or changing evidence refuses removal. A missing runtime permits standalone
worktree use; an unavailable manager with retained workspace socket names does not establish
that all panes are gone. Directory inspection is bounded to 256 entries.

This is a local fux census, not an OS-wide census: unknown nondefault runtimes, unrelated
processes and descendants already reparented away from a sampled root are not covered.
The current-cwd sample catches roots and discovered descendants that changed directories
after launch; it cannot prevent later pane creation, forks, reparenting, chdir or external file
mutations after preflight. No removal check grants authority to stop an unadopted process.

The synced `removing` phase retains the selected force policy before invoking `git worktree
remove`. After submission might have occurred, retries and reconcile never run removal again.
Only absence of both the checkout path (including symlinks) and its git registration confirms
`removed`. An incomplete/failed removal remains uncertain in `removing` with a problem; manual
resolution is currently required before reconciliation can finish. This also covers caller death
before git was actually invoked. A changed force policy cannot silently replace retained intent.

The branch, private parent, sibling files and journal history are retained. Removed IDs cannot
recreate a checkout; identical create/remove retries return history. Existing task associations
remain inspectable, while new managed launches into a path with removal intent are rejected.
Automatic retry of ambiguous partial removal is intentionally unavailable; explicit external
completion followed by `worktree reconcile ID` is the supported recovery path. Automatic
branch/parent cleanup is outside the completion path. Removal grants no authority over the main checkout or other worktrees.
