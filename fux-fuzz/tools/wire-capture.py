#!/usr/bin/env python3
"""Capture everything about a fux binary that a no-behaviour-change refactor
must leave identical, as one JSON document.

Usage: wire-capture.py /path/to/fux OUT.json

Run it against two binaries and diff the outputs:

    python3 fux-fuzz/tools/wire-capture.py /tmp/main-fux /tmp/main.json
    python3 fux-fuzz/tools/wire-capture.py target/debug/fux /tmp/branch.json
    diff /tmp/main.json /tmp/branch.json && echo identical

What it records:

- `rpc.discover` and `registry.schema`, so every method name and every
  reflected type path and shape;
- on a one-tab, one-pane workspace: the painted frame, the prefix command
  column, the help overlay and the pane, tab and workspace menus, each as the
  painted frame and the reflected `Prefix`/`Overlay` components;
- for every action, pressed through a binding on a fresh server, once with
  one tab and one pane and once with that pane closed: the notice it leaves (for an unavailable action, the
  reason `actions::unavailable` gave), the overlay it opens, whether the viewer
  is still attached, and the counts of workspaces, tabs and pane views after.

Every server runs in its own temporary directory with a private socket and a
pane program that prints a fixed line, so two runs of the same binary produce
the same document; the script checks that before trusting a diff.
Exit 0 on success, 2 if a server cannot be started or answered.
"""
import concurrent.futures
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time

ROWS, COLS = 60, 120
KEYS = [chr(c) for c in range(ord("a"), ord("z") + 1)] + \
       [chr(c) for c in range(ord("A"), ord("Z") + 1)] + \
       [str(d) for d in range(10)]
PANE = ["/bin/sh", "-c", "printf 'PANE-READY\\n'; exec cat"]


def rpc(sock, method, params=None, timeout=6.0):
    body = {"jsonrpc": "2.0", "id": 1, "method": method}
    if params is not None:
        body["params"] = params
    payload = json.dumps(body).encode()
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    try:
        s.connect(sock)
        s.sendall(b"POST / HTTP/1.1\r\nHost: fux\r\nContent-Type: application/json\r\n"
                  b"Content-Length: %d\r\nConnection: close\r\n\r\n" % len(payload) + payload)
        raw = b""
        while True:
            chunk = s.recv(65536)
            if not chunk:
                break
            raw += chunk
    finally:
        s.close()
    head, _, b = raw.partition(b"\r\n\r\n")
    if b"transfer-encoding: chunked" in head.lower():
        out = b""
        while b:
            size, _, rest = b.partition(b"\r\n")
            n = int(size, 16)
            if n == 0:
                break
            out += rest[:n]
            b = rest[n + 2:]
        b = out
    return json.loads(b.decode())


class Server:
    def __init__(self, fux, bindings):
        self.dir = tempfile.mkdtemp(prefix="fux-wire.")
        self.sock = os.path.join(self.dir, "s", "fux.sock")
        with open(os.path.join(self.dir, "fux.json"), "w") as f:
            json.dump({"shell": PANE, "clipboard": "disabled", "bindings": bindings}, f)
        env = {"PATH": "/usr/bin:/bin", "HOME": self.dir, "TERM": "xterm-256color"}
        self.proc = subprocess.Popen(
            [fux, "server", "--socket", self.sock, "--config", os.path.join(self.dir, "fux.json")],
            cwd=self.dir, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            start_new_session=True)
        for _ in range(300):
            try:
                rpc(self.sock, "rpc.discover", timeout=1.0)
                break
            except Exception:
                time.sleep(0.05)
        else:
            self.stop()
            raise SystemExit("setup: server never answered")
        self.viewer = self.call("fux.attach", {"rows": ROWS, "cols": COLS})["viewer"]
        for _ in range(200):
            if "PANE-READY" in json.dumps(self.frame()):
                break
            time.sleep(0.05)
        else:
            self.stop()
            raise SystemExit("setup: the pane never printed its line")

    def call(self, method, params=None):
        reply = rpc(self.sock, method, params)
        if "error" in reply:
            return {"error": reply["error"]}
        return reply["result"]

    def frame(self):
        return self.call("fux.frame", {"viewer": self.viewer})

    def control(self, command):
        return self.call("world.trigger_event", {"event": "fux::control::Control",
                                                 "value": {"viewer": self.viewer, "command": command}})

    def key(self, key):
        return self.call("world.trigger_event", {
            "event": "fux::control::UserInput",
            "value": {"viewer": self.viewer,
                      "input": {"kind": "key", "key": key, "ctrl": False, "alt": False, "shift": False}}})

    def prefix(self):
        return self.call("world.trigger_event", {
            "event": "fux::control::UserInput",
            "value": {"viewer": self.viewer,
                      "input": {"kind": "key", "key": "b", "ctrl": True, "alt": False, "shift": False}}})

    def viewer_state(self):
        return self.call("world.get_components", {"entity": self.viewer, "components": [
            "fux::model::Viewer", "fux::interaction::Prefix", "fux::interaction::Overlay"]})

    def count(self, component):
        result = self.call("world.query", {"data": {"components": [component]}})
        return len(result) if isinstance(result, list) else result

    def settle(self):
        # Two round trips through the ECS, then a short wait for deferred work.
        time.sleep(0.25)
        self.call("rpc.discover")
        time.sleep(0.1)

    def stop(self):
        try:
            os.killpg(self.proc.pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass
        self.proc.wait()
        shutil.rmtree(self.dir, ignore_errors=True)


def bindings_for(actions):
    bound = [{"key": KEYS[i], "action": a} for i, a in enumerate(actions)]
    bound.append({"key": KEYS[len(actions)], "action": "not_an_action"})
    return bound


def base(fux, actions):
    s = Server(fux, bindings_for(actions))
    try:
        out = {}
        out["rpc.discover"] = s.call("rpc.discover")
        out["registry.schema"] = s.call("registry.schema")
        s.settle()
        out["frame"] = s.frame()
        s.prefix(); s.settle()
        out["prefix_column"] = {"frame": s.frame(), "viewer": s.viewer_state()}
        s.key("escape"); s.settle()
        s.control({"kind": "help"}); s.settle()
        out["help"] = {"frame": s.frame(), "viewer": s.viewer_state()}
        s.key("escape"); s.settle()
        tree = s.call("world.query", {"data": {"components": ["fux::model::PaneView"]}})
        pane = tree[0]["entity"]
        tabs = s.call("world.query", {"data": {"components": ["fux::model::Tab"]}})
        tab = tabs[0]["entity"]
        ws = s.call("world.query", {"data": {"components": ["fux::model::Workspace"]}})
        workspace = ws[0]["entity"]
        for kind, entity in (("pane", pane), ("tab", tab), ("workspace", workspace)):
            s.control({"kind": "menu", "subject": {kind: entity}}); s.settle()
            out["menu_" + kind] = {"frame": s.frame(), "viewer": s.viewer_state()}
            s.key("escape"); s.settle()
        return out
    finally:
        s.stop()


def one_action(fux, actions, index, empty_tab):
    s = Server(fux, bindings_for(actions))
    try:
        s.settle()
        if empty_tab:
            pane = s.call("world.query", {"data": {"components": ["fux::model::PaneView"]}})[0]["entity"]
            s.control({"kind": "close", "subject": {"pane": pane}})
            s.settle()
        s.prefix()
        s.key(KEYS[index])
        s.settle()
        time.sleep(0.2)
        state = s.viewer_state()
        attached = "error" not in state and "fux::model::Viewer" in state.get("components", {})
        return {
            "action": actions[index],
            "attached": attached,
            "viewer": state,
            "workspaces": s.count("fux::model::Workspace"),
            "tabs": s.count("fux::model::Tab"),
            "pane_views": s.count("fux::model::PaneView"),
        }
    finally:
        s.stop()


def action_ids(fux):
    s = Server(fux, [])
    try:
        schema = s.call("registry.schema", {"with_crates": ["fux"]})
        for path, shape in (schema.items() if isinstance(schema, dict) else []):
            if path == "fux::actions::Action":
                return [v["shortPath"] if isinstance(v, dict) and "shortPath" in v else v
                        for v in shape.get("oneOf", [])]
        raise SystemExit("setup: registry.schema has no fux::actions::Action")
    finally:
        s.stop()


def main():
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    fux, out_path = os.path.abspath(sys.argv[1]), sys.argv[2]
    variants = action_ids(fux)
    # Configuration names are the snake_case wire form of the Rust variant.
    actions = [''.join('_' + c.lower() if c.isupper() else c for c in v).lstrip('_') for v in variants]
    doc = {"actions": actions, "base": base(fux, actions)}
    with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:
        doc["per_action"] = list(pool.map(lambda i: one_action(fux, actions, i, False), range(len(actions))))
        doc["per_action_empty_tab"] = list(pool.map(lambda i: one_action(fux, actions, i, True), range(len(actions))))
    with open(out_path, "w") as f:
        json.dump(doc, f, indent=1, sort_keys=True)
        f.write("\n")


if __name__ == "__main__":
    main()
