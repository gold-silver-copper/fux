//! Bounded snapshots for destructive-operation preflight, separate from best-effort detection.
use super::{Pid, ProcessRef};
use std::{collections::BTreeMap, io, path::PathBuf, time::Instant};

pub fn process_tree_cwds(root: Pid, deadline: Instant) -> io::Result<Vec<PathBuf>> {
    scan(
        root,
        deadline,
        super::process_children,
        super::process_cwd,
        super::process_identity,
    )
}

fn scan(
    root: Pid,
    deadline: Instant,
    mut children: impl FnMut(Pid) -> io::Result<Vec<ProcessRef>>,
    mut cwd: impl FnMut(Pid) -> io::Result<PathBuf>,
    mut identity: impl FnMut(Pid) -> io::Result<ProcessRef>,
) -> io::Result<Vec<PathBuf>> {
    let mut pending = vec![identity(root)?];
    let mut tree = BTreeMap::new();
    let mut paths = Vec::new();
    while let Some(process) = pending.pop() {
        let pid = process.pid;
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "process traversal deadline",
            ));
        }
        if tree.contains_key(&pid) {
            return Err(io::Error::other("process ancestry changed or cycled"));
        }
        if tree.len() >= 256 {
            return Err(io::Error::other("process tree exceeds bound"));
        }
        if identity(pid)? != process {
            return Err(io::Error::other("process lifetime changed"));
        }
        let mut descendants = children(pid)?;
        descendants.sort_unstable();
        if descendants.len() > 256 || pending.len() + descendants.len() + tree.len() > 256 {
            return Err(io::Error::other("process tree exceeds bound"));
        }
        pending.extend(descendants.iter().copied());
        tree.insert(pid, (process, descendants));
        paths.push(cwd(pid)?);
        if identity(pid)? != process {
            return Err(io::Error::other(
                "process lifetime changed during cwd sample",
            ));
        }
    }
    // A second bounded pass detects forks/exits/reparenting during the first.
    // This is a snapshot check, not a freeze of future process activity.
    for (pid, (process, before)) in tree {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "process traversal deadline",
            ));
        }
        if identity(pid)? != process {
            return Err(io::Error::other(
                "process lifetime changed after cwd sample",
            ));
        }
        let mut after = children(pid)?;
        after.sort_unstable();
        if before != after || identity(pid)? != process {
            return Err(io::Error::other("process tree changed during inspection"));
        }
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn reference(pid: Pid) -> ProcessRef {
        ProcessRef { pid, birth: (1, 0) }
    }

    #[test]
    fn uncertain_and_oversized_process_trees_never_look_empty() -> io::Result<()> {
        let deadline = || Instant::now() + Duration::from_secs(1);
        let cwd = |_| Ok(PathBuf::from("/fixture"));
        let identity = |pid| Ok(reference(pid));
        assert_eq!(
            scan(
                1,
                deadline(),
                |pid| Ok(if pid == 1 { vec![reference(2)] } else { vec![] }),
                cwd,
                identity
            )?
            .len(),
            2
        );
        assert!(scan(1, deadline(), |_| Ok(vec![reference(1)]), cwd, identity).is_err());
        assert!(
            scan(
                1,
                deadline(),
                |_| Ok((2..260).map(reference).collect()),
                cwd,
                identity
            )
            .is_err()
        );
        assert!(
            scan(
                1,
                deadline(),
                |_| Err(io::Error::other("denied")),
                cwd,
                identity
            )
            .is_err()
        );
        assert!(scan(1, Instant::now(), |_| Ok(vec![]), cwd, identity).is_err());
        let mut calls = 0;
        assert!(
            scan(
                1,
                deadline(),
                |_| {
                    calls += 1;
                    Ok(if calls == 1 {
                        vec![]
                    } else {
                        vec![reference(2)]
                    })
                },
                cwd,
                identity
            )
            .is_err()
        );
        let mut births = 0;
        assert!(
            scan(
                1,
                deadline(),
                |_| Ok(vec![]),
                cwd,
                |pid| {
                    births += 1;
                    Ok(ProcessRef {
                        pid,
                        birth: (if births >= 4 { 2 } else { 1 }, 0),
                    })
                }
            )
            .is_err(),
            "same PID with a different birth after cwd must fail"
        );
        assert!(
            scan(
                1,
                deadline(),
                |pid| Ok(if pid == 1 { vec![reference(2)] } else { vec![] }),
                cwd,
                |pid| Ok(ProcessRef {
                    pid,
                    birth: (if pid == 2 { 2 } else { 1 }, 0)
                })
            )
            .is_err(),
            "child lifetime must match the identity captured during parent enumeration"
        );
        Ok(())
    }
}
