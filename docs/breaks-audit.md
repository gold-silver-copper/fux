# The BREAKS audit

`bevy-final:fux-fuzz/BREAKS.md` (`git show bevy-final:fux-fuzz/BREAKS.md`)
records 31 findings against the Bevy version of fux, from hunts 5 to 8. This
is each one checked against the rewrite: whether it can still happen, and if
it can, the code that prevents it and the test that fails when that code is
reverted. Each revert was made, the tests run, and the file restored from a
copy. A revert that only fails where this machine cannot run it (dash, ext4,
macOS) was pushed to a throwaway branch and run by CI.

## Findings that apply

| Finding | The fix now | The test that fails with it reverted | Revert run |
| --- | --- | --- | --- |
| 006: descriptor pressure wedges the accept loop | A spare descriptor (`server.rs`, `Server::spare`, `accept`): a connection that arrives when none is left is accepted, told why, and closed | `a_server_out_of_descriptors_refuses_promptly_and_recovers` (`tests/pressure.rs`) | here |
| 007: a request of any size is read whole | Every protocol frame is at most `MAX_FRAME` (1 MiB), refused from its length before its body arrives (`protocol.rs`, `Decoder::frame`) | `oversized_frames_are_refused_on_both_sides` (`protocol.rs`) | here |
| 008: the attach client buffers an unended line | The same cap on the client's side, which reads with the same `protocol::Decoder`: what it holds for a frame not yet whole stays under `4 + MAX_FRAME` | `oversized_frames_are_refused_on_both_sides` | here |
| 010: a signal ends the attachment | The attach client goes on when a signal interrupts its `poll` (`client.rs`, `attach`); the command client retries an interrupted read | `resizing_while_typing_keeps_the_client_attached_and_every_byte` (`tests/attach.rs`), for the `poll` | here |
| 012: under descriptor pressure a Linux client waits 6.5 s | `accept` runs until the backlog is empty, refusing every connection there is no room for, in one tick (`server.rs`, `accept`) | `a_server_out_of_descriptors_refuses_promptly_and_recovers`, now holding 150 connections so that about a hundred wait in the backlog: with the fix the slowest client took about 2 ms in each of five runs; with one refusal a 50 ms tick, as the Bevy version did, about 4.7 s | here |
| 013: `terminate` leaves dash's background jobs alive | `Leader::hang_up` signals every process in the pane's session, not only its group | `a_closed_panes_background_jobs_end_with_it_under_dash`, `terminate_ends_the_foreground_and_leaves_background_jobs` (`tests/processes.rs`) | CI, Ubuntu (no `/bin/dash` here) |
| 014: socket cleanup trusts a reused inode number | The socket's inode stays allocated while fux holds it (`fuxix::file::pin`, `socket.rs`, `Pinned`) | `cleanup_leaves_a_socket_that_replaced_ours_where_inodes_are_reused` (`socket.rs`): "fux's socket was inode 8912904, the replacement's 8912904" | CI, Ubuntu (ext4; this machine's `/home` is btrfs, its `/tmp` tmpfs) |
| 016: a file is read without bound | The config file is read up to 1 MiB, and a larger one refused (`config.rs`, `read_bounded`) | `a_config_file_over_a_mebibyte_is_refused`, new | here |
| 017: a resized pane loses its bottom line | A shrink keeps the cursor's row on screen (`fux-vt` `grid.rs`, `reflowed`) | `a_split_pane_keeps_its_last_line_and_regains_its_rows` (`tests/layout.rs`) | here |
| 020: a stopped pane process reads as exited on macOS | `fuxix::process::ended` decides by the report's code: a stop is not an end | `input_waits_for_a_program_that_is_not_reading` failed first, with a broken pipe: the stopped program's pane was closed. `a_stopped_child_has_not_ended_and_a_killed_one_has` (`fuxix`) and `a_stopped_pane_stays_until_its_program_ends` cover it too | CI, macOS (Linux never reported a stop here) |
| 021: input past sixteen pieces is lost | A pane's input queue is bounded by bytes, not pieces (`pane.rs`, `InputQueue`) | `input_waits_for_a_program_that_is_not_reading` (`tests/pressure.rs`) | here |

### What is not proven

- **010, the command client.** Its read loop retries `Interrupted`
  (`client.rs`, `read_frame`). Removing that retry failed no test, and the
  test meant for it was not examined.

## Findings that do not apply

| Finding | Why not |
| --- | --- |
| 001, 009: a `Viewer` component aborts the server | No ECS, no components |
| 002: a web page can drive the server | No HTTP: a Unix socket in a 0700 directory, and only the server's own uid is served (`socket::peer_uid`) |
| 003, 004, 005: BRP requests despawn or mutate entities | No BRP |
| 011: the build needs ALSA headers | No Bevy: no audio crate is in the dependency tree |
| 015: removing the `Settings` resource stops painting | No resources |
| 018: a blocking scene read stalls the server | No scene files; the one file read, the config, is bounded (016) |
| 019: scene file threads are unbounded | One thread |
| 022: a panicking request answers with a channel error | No request channels; a command's error is its usage message |
| 023: closing a view despawns what it names | Views are a map from client to state, not entities |
| 024: raw hierarchy edits orphan layout and processes | The layout is a tree only the session changes |
| 025: a workspace's order can be missing or shared | Workspaces are a vector: the order is the vector's |
| 026: a viewed process loses its state or gets an impossible size | A pane's size is the smallest rectangle showing it, at least 1×1 (`size_panes`) |
| 027: a viewer created over BRP is never repaired | No BRP; `settle` repairs every view |
| 028: despawning fux's observers silences it | No observers |
| 029: an unprojectable workspace drops commands silently | No projection; a command with a bad target fails, saying why |
| 030: the placeholder entity passes the guard | No entities |
| 031: a tab spawned in one request nests in another tab | Tabs are made by one function, in one place |
