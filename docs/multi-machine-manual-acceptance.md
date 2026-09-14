# Two-host manual acceptance

Run this with one controller and two remote hosts, A and B, using separate fresh Bash
shells for the test. Keep those shells open for cleanup. This checklist is a procedure,
not a claim that physical-host, WAN, relay, NAT or mobile acceptance has been performed.
Current automated evidence uses Local plus two isolated stacks over real koh loopback
connections; see [the implementation ledger](multi-machine-supervision-implementation.md).

Use matching fux/zor builds from this workspace on all three hosts. The controller works with
the clean published koh companion pin: it probes `koh gateway connect --help` and, when that
koh lacks `--status-file`, reports a failed connection as one generic transport failure rather
than distinguishing unauthorized, expired, ended and offline. To get those distinct states,
build the optional development koh status extension (its patch and published base are retained
in `verification/multi-machine/checkpoint-27/` and `checkpoint-12/`); the pinned companion does
not yet include it, and it must not be advanced to a local-only commit. Use routable numeric
IPv4 addresses for the direct-network steps below. The hosts must permit UDP traffic to their
gateway ports.

Build fux/zor from this workspace on each host's native platform:

```bash
cargo +stable build --locked -p fux -p zor
```

To reconstruct the development koh build in a new checkout, run from this workspace:

```bash
ACCEPT_KOH_SOURCE=$(mktemp -d /tmp/koh-manual-source-XXXXXX)
git clone --no-hardlinks references/koh "$ACCEPT_KOH_SOURCE/repo"
git -C "$ACCEPT_KOH_SOURCE/repo" checkout --detach "$(cat docs/verification/multi-machine/checkpoint-12/koh-base.txt)"
git -C "$ACCEPT_KOH_SOURCE/repo" apply "$PWD/docs/verification/multi-machine/checkpoint-12/koh-development.patch"
cargo +stable build --locked --manifest-path "$ACCEPT_KOH_SOURCE/repo/Cargo.toml" \
  --no-default-features --features cli,gateway --target-dir "$ACCEPT_KOH_SOURCE/build"
printf 'koh binary: %s/build/debug/koh\n' "$ACCEPT_KOH_SOURCE"
```

## 1. Prepare each shell

Run this on the controller, A and B, substituting absolute executable paths:

```bash
export FUX_BIN=/absolute/path/to/fux
export ZOR_BIN=/absolute/path/to/zor
export KOH_BIN=/absolute/path/to/development/koh
export ACCEPT_ROOT=$(mktemp -d /tmp/zor-manual-XXXXXX)
export XDG_RUNTIME_DIR="$ACCEPT_ROOT"
export XDG_CONFIG_HOME="$ACCEPT_ROOT/config"
export XDG_STATE_HOME="$ACCEPT_ROOT/state"
mkdir -m 700 "$XDG_CONFIG_HOME" "$XDG_STATE_HOME"
printf 'Retain this test directory: %s\n' "$ACCEPT_ROOT"
read -r -s -p 'Disposable key passphrase: ' KOH_KEY_PASSPHRASE
printf '\n'
export KOH_KEY_PASSPHRASE
export KOH_KEY_NEW_PASSPHRASE="$KOH_KEY_PASSPHRASE"

await_socket() {
  for acceptance_try in $(seq 1 100); do
    test -S "$1" && return 0
    sleep 0.1
  done
  printf 'Socket did not appear: %s\n' "$1" >&2
  return 1
}
await_file() {
  for acceptance_try in $(seq 1 100); do
    test -s "$1" && return 0
    sleep 0.1
  done
  printf 'File did not appear: %s\n' "$1" >&2
  return 1
}
"$FUX_BIN" serve >"$ACCEPT_ROOT/fux.log" 2>&1 &
"$ZOR_BIN" serve >"$ACCEPT_ROOT/zor.log" 2>&1 &
await_socket "$ACCEPT_ROOT/fux/default.sock"
await_socket "$ACCEPT_ROOT/zor/control.sock"
```

Expected: both sockets appear, and neither service exits. Inspect the retained logs if a
startup check fails. These isolated directories avoid reusing existing user services.

## 2. Create the controller identity

On the controller:

```bash
"$KOH_BIN" id --key-file "$ACCEPT_ROOT/client.key" | tee "$ACCEPT_ROOT/client-id.txt"
```

Copy the printed public endpoint ID to both remote shells in the next step. Keep the client
key on the controller; profiles reference that existing key rather than embedding its contents.

## 3. Expose distinct remote control and attachment services

On A, enter label `A`; on B, enter label `B`. Use each host's own routable IPv4 address:

```bash
read -r -p 'Host label (A or B): ' ACCEPT_LABEL
read -r -p 'This host routable IPv4 address: ' ACCEPT_IP
read -r -p 'Controller public endpoint ID: ' CONTROLLER_ID
"$FUX_BIN" workspace new agent
await_socket "$ACCEPT_ROOT/fux/agent.attach.sock"
"$FUX_BIN" agent list > "$ACCEPT_ROOT/listing.json"
ACCEPT_INSTANCE=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["result"]["value"]["instance"])' "$ACCEPT_ROOT/listing.json")
"$ZOR_BIN" task start same --title "$ACCEPT_LABEL same-name task" \
  --instance "$ACCEPT_INSTANCE" --workspace agent --cwd "$ACCEPT_ROOT" -- /bin/sh
"$KOH_BIN" gateway serve --local --socket "$ACCEPT_ROOT/zor/control.sock" \
  --key-file "$ACCEPT_ROOT/control.key" --allow "$CONTROLLER_ID" \
  > "$ACCEPT_ROOT/control-ad.json" 2> "$ACCEPT_ROOT/control-gateway.log" &
"$KOH_BIN" gateway serve --local --socket "$ACCEPT_ROOT/fux/agent.attach.sock" \
  --key-file "$ACCEPT_ROOT/attachment.key" --allow "$CONTROLLER_ID" \
  > "$ACCEPT_ROOT/attachment-ad.json" 2> "$ACCEPT_ROOT/attachment-gateway.log" &
await_file "$ACCEPT_ROOT/control-ad.json"
await_file "$ACCEPT_ROOT/attachment-ad.json"
python3 - "$ACCEPT_ROOT" "$ACCEPT_IP" <<'PY'
import json, pathlib, sys
for kind in ('control', 'attachment'):
    ad = json.loads((pathlib.Path(sys.argv[1]) / (kind + '-ad.json')).read_text())
    port = ad['direct_addr'].rsplit(':', 1)[1]
    print(kind, 'endpoint=' + ad['endpoint_id'], 'direct=' + sys.argv[2] + ':' + port)
PY
```

Expected: two different service endpoint IDs per host and two managed tasks named `same`,
one on each host in workspace `agent`. `--local` disables relay use for this procedure.
The advertised loopback address is intentionally replaced with the host's routable address
while retaining its port. This address supplies location; the endpoint ID supplies identity.

## 4. Save the machines on the controller

Enter the endpoint IDs and `IP:PORT` values printed by each remote:

```bash
for ACCEPT_NAME in remote-a remote-b; do
  printf 'Configure %s\n' "$ACCEPT_NAME"
  read -r -p 'Control endpoint: ' ACCEPT_CONTROL
  read -r -p 'Control IP:PORT: ' ACCEPT_CONTROL_ADDR
  read -r -p 'Attachment endpoint: ' ACCEPT_ATTACHMENT
  read -r -p 'Attachment IP:PORT: ' ACCEPT_ATTACHMENT_ADDR
  "$ZOR_BIN" machine add "$ACCEPT_NAME" --endpoint "$ACCEPT_CONTROL" \
    --key-file "$ACCEPT_ROOT/client.key" --direct "$ACCEPT_CONTROL_ADDR"
  "$ZOR_BIN" machine bind "$ACCEPT_NAME" --workspace agent --endpoint "$ACCEPT_ATTACHMENT" \
    --key-file "$ACCEPT_ROOT/client.key" --direct "$ACCEPT_ATTACHMENT_ADDR"
done
"$ZOR_BIN" machine list
"$ZOR_BIN" --koh-binary "$KOH_BIN" dashboard --all-machines --once \
  > "$ACCEPT_ROOT/aggregate.json"
python3 - "$ACCEPT_ROOT/aggregate.json" <<'PY'
import json, sys
view = json.load(open(sys.argv[1]))
assert len(view['machines']) == 3
assert all(machine['fresh'] for machine in view['machines']), view
for machine in view['machines']:
    print(machine['id'], machine['name'], machine['view']['service_instance'])
PY
"$ZOR_BIN" --machine remote-a --koh-binary "$KOH_BIN" task inspect same
"$ZOR_BIN" --machine remote-b --koh-binary "$KOH_BIN" task inspect same
```

Expected: Local, remote-a and remote-b are fresh. The remote task titles and machine/service
identities differ. An unknown selector must fail rather than inspecting a Local task:

```bash
"$ZOR_BIN" --machine no-such-host --koh-binary "$KOH_BIN" task inspect same
```

## 5. Navigate, attach and return

On the controller:

```bash
stty -g > "$ACCEPT_ROOT/terminal-before.txt"
"$ZOR_BIN" --koh-binary "$KOH_BIN" --fux-binary "$FUX_BIN" dashboard --all-machines
```

1. Press Tab to remote-a, then j/k to its `same` task. Enter must show A's task and attempt.
2. Escape must close inspection without changing selection. Press a to attach.
3. In that pane, run `printf 'A_ONLY\n'; printf A >> acceptance-effects`.
4. Detach with Ctrl-A, then d. The original dashboard selection must return.
5. Visit remote-b and attach to its `same` task. Run `cat acceptance-effects`. It must report
   a missing file. Then run `printf 'B_ONLY\n'; printf B >> acceptance-effects` and detach.
6. Return to A, attach, and run `cat acceptance-effects`. It must print exactly `A`.
7. Detach and press ?. Help must be readable; Escape returns to the same task.
8. Resize the controller terminal to 80×24 and then 40×16. Repeat inspection and detach.
   Controls and action errors must remain readable; long inspection values must be reachable
   by j/k scrolling. Row labels can truncate, but inspection must retain their full values.
9. Press q to quit. Check terminal attributes below; on macOS, the driver's PENDIN transition
   bit can differ even when the application has restored all modes it controls.

```bash
stty -g > "$ACCEPT_ROOT/terminal-after.txt"
diff -u "$ACCEPT_ROOT/terminal-before.txt" "$ACCEPT_ROOT/terminal-after.txt"
"$ZOR_BIN" --machine remote-a --koh-binary "$KOH_BIN" task inspect same
"$ZOR_BIN" --machine remote-b --koh-binary "$KOH_BIN" task inspect same
```

Expected: both tasks and their original panes still exist. Verify `acceptance-effects` on each
remote shell with `cat "$ACCEPT_ROOT/acceptance-effects"`: A contains `A`, B contains `B`.

## 6. Profile reload, independent failure and task routing

In the controller shell, rename A, reopen its scope, then use a second controller shell with
the same exported test paths to edit profiles while the dashboard is open:

```bash
"$ZOR_BIN" machine rename remote-a renamed-a
"$ZOR_BIN" --machine renamed-a --koh-binary "$KOH_BIN" --fux-binary "$FUX_BIN" dashboard
```

In the second shell, run `"$ZOR_BIN" machine control remote-b --clear`. Press R in the
dashboard. B must become unavailable while A stays usable. No remote service should stop.
Restore B with `machine control remote-b --endpoint ... --key-file ... --direct ...`, using
its original values, and press R again. The restored view must be fresh before actions work.

For an independent attachment failure, remove and recreate B's profile with only its original
control binding. Press R, inspect B's task, then press a. Inspection must succeed while Attach
refuses the missing workspace binding. Reapply B's attachment binding from step 4 and reload.
This tests routing configuration. To test actual authorization, start an additional attachment
gateway on B with a separate key and an `--allow` ID other than the controller, bind B's
workspace to that endpoint, and verify Attach refuses it while its control endpoint still works.
Restore the authorized binding afterward.

Quit the dashboard and cancel A's coordination from the controller:

```bash
"$ZOR_BIN" --machine renamed-a --koh-binary "$KOH_BIN" task cancel same
"$ZOR_BIN" --machine renamed-a --koh-binary "$KOH_BIN" task inspect same
"$ZOR_BIN" --machine remote-b --koh-binary "$KOH_BIN" task inspect same
```

Expected: only A's coordination is cancelled; A's shell survives and B is unchanged. Use a
new disposable task for further mutation checks. A lost reply is an unknown outcome: inspect
the original task/attempt before an explicit retry. Connectivity alone never authorizes replay.

## 7. Recovery evidence and remaining physical-network checks

The deterministic loopback fault scenario closes actual QUIC connections three times and
refuses redials beyond the production 30-second retention window. It verifies shell effects,
input counters, expiry messaging, explicit fresh attachment and surviving remote owners.
Run it from the workspace with the matching test-only koh fixture executable:

```bash
export KOH_FAULT_SERVER_BIN=/absolute/path/to/koh-test-executable
python3 docs/verification/multi-machine/checkpoint-6/dashboard-handoff.py \
  --initial-machine first --transport-faults \
  --artifacts-dir /absolute/path/to/new-fault-artifacts
```

The executable is produced by `cargo +stable test --no-default-features --features cli,gateway
--lib --no-run` in the patched koh development checkout; use the printed `src/lib.rs` executable.
It is required explicitly and is absent from production builds. Keep FUX_BIN/ZOR_BIN/KOH_BIN
exported. This scenario creates its own disposable stacks, independent of the manual hosts.

For physical-network acceptance, record disconnect/reconnect timing, visible state, original
pane identity and effect-file contents before and after a controlled network interruption on
the disposable hosts. koh's idle detection is separate from its 30-second detached retention;
do not assume a 31-second packet outage is guaranteed to expire an otherwise retained link.
Controller, remote zor and remote fux restarts, lost mutation replies and supported application
resume remain separate acceptance items in the implementation ledger. Do not mark them passed
based on this checklist or the transport-only fixture.

## 8. Clean up

Quit all dashboards first. On A and B, stop the managed test pane before shutting down the
disposable services:

```bash
"$ZOR_BIN" task stop same
```

In each original fresh test shell, terminate its remaining test jobs, wait for them, then
remove only its recorded temporary directory:

```bash
for acceptance_pid in $(jobs -pr); do kill "$acceptance_pid"; done
wait
case "$ACCEPT_ROOT" in
  /tmp/zor-manual-*) rm -rf -- "$ACCEPT_ROOT" ;;
  *) printf 'Unexpected test directory; inspect before removing\n' >&2 ;;
esac
unset KOH_KEY_PASSPHRASE KOH_KEY_NEW_PASSPHRASE
```

Expected: no test jobs remain in those shells. Existing user services and directories were
never part of this test. Retain any failed-step logs and exact binary revisions before cleanup.
