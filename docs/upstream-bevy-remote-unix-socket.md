# Proposal: `bevy_remote` over a Unix domain socket

Status: draft for the user to decide on. Not posted anywhere.

## Problem

`bevy_remote`'s `RemoteHttpPlugin` serves BRP only on TCP (`127.0.0.1:15702`
by default). On a desktop machine that means:

- every local process of every user can reach the app;
- so can any web page, because a browser can send a "simple" cross-origin
  POST to a loopback port. fux measured this: a page could drive the app
  (fux hunt 5 finding 002).

Applications that want BRP but not that exposure have to write their own
transport. fux does: about 800 lines of hyper serving plus the socket rules
below (`src/transport.rs`), which duplicate what `RemoteHttpPlugin` already
does for TCP.

## Proposal

Let `RemoteHttpPlugin` listen on a Unix domain socket as an alternative to a
TCP address:

```rust
app.add_plugins(RemoteHttpPlugin::default().with_unix_socket("/run/user/1000/app/brp.sock"));
```

Serving, batching, watching (server-sent events) and every existing method
stay exactly as they are; only the listener changes. Clients reach it with
`curl --unix-socket PATH http://localhost/ -d '…'`.

## What fux learned that the plugin should do

- **Permissions before listening.** Create the socket with mode 0600, in a
  directory the plugin creates with 0700 (or refuses when an existing one is
  looser or owned by someone else). No connection may be accepted before the
  mode is set.
- **Check the peer.** On accept, compare the connecting process's UID
  (`SO_PEERCRED` on Linux, `getpeereid` on the BSDs and macOS) with the
  server's own, and close the connection if they differ.
- **One owner.** Use a lock file beside the socket, so a second instance
  refuses to start instead of stealing the path.
- **Stale sockets.** Replace a leftover socket only after a connect to it
  fails, never unconditionally.
- **Cleanup by identity, not by inode number.** At shutdown, remove the socket
  only if it is still the one this server bound. A freed inode number is
  reused at once on ext4, so comparing `(dev, ino)` can remove a replacement
  (fux finding 014). Hold an `O_PATH` descriptor to the bound socket and
  compare against that.
- **Bounds.** Keep the existing request-body, batch and reply limits. Drain the
  whole accept backlog under descriptor pressure (fux finding 012), and retry
  on `EINTR` (fux finding 010).

## What fux would delete

If the plugin offered this, fux would remove its own transport (hyper serving,
`smol-hyper`, `http-body-util`, the accept loop and its pressure handling) and
keep only its path-selection rules. It would also drop `ureq` if the plugin's
examples showed a small blocking client for Unix sockets.

## Related

`bevy_remote` → `bevy_dev_tools` → `bevy_audio` makes every BRP user link
ALSA on Linux (fux finding 011). It is a separate issue; its text is in fux's
PR #51 description.
