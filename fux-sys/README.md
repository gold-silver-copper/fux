# fux-sys

The macOS system calls [fux](https://github.com/gold-silver-copper/fux) makes
that neither std nor rustix offers as safe functions, each behind one:

| Function | Call | Why not std or rustix |
| --- | --- | --- |
| `foreground_group` | `tcgetpgrp` | rustix 1.1 builds a `Pid` from the result unchecked on macOS, and 0 is undefined behaviour |
| `session` | `getsid` | the same, for kernel processes in session 0 |
| `peer_uid` | `getpeereid` | std's `UnixStream::peer_cred` is unstable; rustix's `socket_peercred` is Linux only |
| `cwd` | `proc_pidinfo(PROC_PIDVNODEPATHINFO)` | not in std or rustix |
| `processes` | `proc_listallpids` | not in std or rustix |

No function returns a value its caller has to check for validity: process
IDs and groups are positive, a failure is `None` or an error.

On every other platform the crate is empty. fux itself forbids `unsafe_code`;
this crate is where the unsafe code it needs lives, one call per `unsafe`
block, each with its reasoning.
