//! The cassette runner's own tests: what it refuses, bounds and reports,
//! on made-up observations and a fux that never answers.
#[expect(
    dead_code,
    reason = "the replaying entry point is tests/cassettes.rs's"
)]
mod runner {
    include!("cassettes/runner.rs");

    #[cfg(test)]
    mod tests {
        use super::*;

        const R1: &str = "1-1111111111111111";
        const R2: &str = "2-2222222222222222";

        fn ls(cursor: u64, panes: &[&str]) -> Result<Snapshot, String> {
            let panes: Vec<String> = (panes.iter())
                .map(|v| {
                    format!(r#"{{"id":"%1","version":"{v}","exited":0,"signal":null,"pid":null}}"#)
                })
                .collect();
            Snapshot::read(&format!(
                r#"{{"run":"{R1}","cursor":"{cursor}:{R1}","panes":[{}]}}"#,
                panes.join(",")
            ))
        }

        fn call(argv: &[&str], status: i64, err: &str) -> Call {
            Call {
                argv: argv.iter().map(|a| (*a).to_owned()).collect(),
                stdin: String::new(),
                status,
                out: String::new(),
                err: err.to_owned(),
                out_cut: false,
                err_cut: false,
            }
        }

        fn replay(
            snapshots: Vec<Snapshot>,
            calls: Vec<(usize, Call)>,
            events: &[&str],
        ) -> Result<Replay, String> {
            Ok(Replay {
                steps: Vec::new(),
                snapshots,
                calls,
                events: events
                    .iter()
                    .map(|e| Json::read(e, ITEMS))
                    .collect::<Result<_, _>>()?,
                problems: Vec::new(),
                took: Duration::ZERO,
                dir: String::new(),
            })
        }

        fn monitored(replay: &Replay) -> (Vec<String>, BTreeSet<Promise>) {
            let mut problems = Vec::new();
            let exercised = monitor(replay, &mut problems, &mut Vec::new());
            (problems, exercised)
        }

        /// A fux, made of `script`, that writes its pid to `pid` first; and
        /// its directory, with a place in it to replay from.
        fn fake(name: &str, script: &str) -> Result<(PathBuf, Place, PathBuf), String> {
            let dir = fuxix::file::scratch()
                .map_err(e)?
                .join(format!("cassette-{name}-{}", std::process::id()));
            let own = dir.join(".cassette");
            std::fs::create_dir_all(&own).map_err(e)?;
            let fux = dir.join("fux");
            let pid = dir.join("pid");
            let text = format!("#!/bin/sh\necho $$ >'{}'\n{script}\n", pid.display());
            std::fs::write(&fux, text).map_err(e)?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fux, std::fs::Permissions::from_mode(0o700)).map_err(e)?;
            let place = Place {
                socket: own.join("fux.sock"),
                dir: dir.clone(),
                own,
            };
            Ok((fux, place, pid))
        }

        /// Whether the process whose pid `file` holds is still there.
        fn left(file: &Path) -> Result<bool, String> {
            let pid: i32 = std::fs::read_to_string(file)
                .map_err(e)?
                .trim()
                .parse()
                .map_err(e)?;
            Ok(fuxix::process::Pid::from_raw(pid).is_some_and(fuxix::process::exists))
        }

        /// A fux that prints past the bound and then hangs is killed at the
        /// bound, not at the deadline, with nothing of it left running.
        #[test]
        fn a_request_that_prints_past_the_bound_is_killed_at_it() -> Result<(), String> {
            let (fux, place, pid) = fake("noisy", "head -c 2000000 /dev/zero\nexec sleep 600")?;
            let mut ctx = Ctx::new()?;
            ctx.fux = fux;
            let started = Instant::now();
            let asked = place.fux(&ctx, &["ls"], after(Duration::from_secs(30)));
            let took = started.elapsed();
            let still = left(&pid)?;
            let _ = std::fs::remove_dir_all(&place.dir);
            assert!(asked.is_err_and(|why| why.contains("printed past")));
            assert!(took < Duration::from_secs(5), "{took:?}");
            assert!(!still, "it is still there");
            Ok(())
        }

        /// A replay's journal is read within the replay's own time: a
        /// server whose `next-events` never answers holds it no longer.
        #[test]
        fn the_journal_is_read_within_the_replays_time() -> Result<(), String> {
            let real = Ctx::new()?.fux;
            let wrap = format!(
                "case \" $* \" in *\" next-events \"*) exec sleep 600;; esac\nexec '{}' \"$@\"",
                real.display()
            );
            let (fux, _, pid) = fake("journal", &wrap)?;
            let mut ctx = Ctx::new()?;
            ctx.fux = fux;
            let cassette = parse(
                "quick".to_owned(),
                "# fux cassette 1\n# guards: x\n$ true\n",
            )?;
            let started = Instant::now();
            let replayed = super::replay(&cassette, &ctx, false, after(Duration::from_secs(3)));
            let took = started.elapsed();
            let still = left(&pid)?;
            let _ = std::fs::remove_dir_all(pid.parent().ok_or("its directory")?);
            let problems = replayed?.problems;
            for kept in problems
                .iter()
                .filter_map(|p| p.strip_prefix("its files: "))
            {
                let _ = std::fs::remove_dir_all(kept);
            }
            assert!(
                problems.iter().any(|p| p.contains("no answer in time")),
                "{problems:?}"
            );
            // 3 s, and the stop; never a further 10 s for the journal.
            assert!(took < Duration::from_secs(6), "{took:?}");
            assert!(!still, "the hung request is still there");
            Ok(())
        }

        /// A fux whose request never answers (one that hangs, as a server that
        /// greets and then never answers `next-events` leaves it) is killed at
        /// the deadline, with nothing of it left running.
        #[test]
        fn a_request_that_never_answers_ends_at_its_deadline() -> Result<(), String> {
            let (fux, place, pid) = fake("hang", "exec sleep 600")?;
            let mut ctx = Ctx::new()?;
            ctx.fux = fux;
            let started = Instant::now();
            let read = journal(&ctx, &place, R1, after(Duration::from_secs(1)));
            let took = started.elapsed();
            let left = left(&pid)?;
            let _ = std::fs::remove_dir_all(&place.dir);
            assert!(read.is_err_and(|why| why.contains("no answer in time")));
            assert!(took < Duration::from_secs(5), "{took:?}");
            assert!(!left, "it is still there");
            Ok(())
        }

        /// Past the suite's time, no cassette starts: each is a failure.
        #[test]
        fn no_replay_starts_past_the_suites_time() -> Result<(), String> {
            let cassette = parse("hold".to_owned(), "# fux cassette 1\n# guards: x\n$ true\n")?;
            let options = Options {
                update: false,
                keep: false,
                dir: PathBuf::new(),
                names: Vec::new(),
                threads: 1,
            };
            let reports = replay_all(&[cassette], &Ctx::new()?, &options, Instant::now());
            assert!(
                matches!(reports.as_slice(), [r] if r.problems.iter().any(|p| p.starts_with("not replayed")))
            );
            Ok(())
        }

        /// Only a call of one `expect`ed send or one wait, and nothing else,
        /// establishes that operation; text that only names one does not.
        #[test]
        fn only_a_call_of_one_operation_establishes_it() -> Result<(), String> {
            let v = format!("%1:0.0.0.0.0:{R1}");
            let expect = call(
                &[
                    "str",
                    &v,
                    "expect",
                    "str",
                    "y",
                    "text",
                    "str",
                    "Enter",
                    "key",
                    "send-keys",
                ],
                1,
                "",
            );
            let wait = call(&["str", &v, "changed", "1", "timeout", "wait"], 0, "");
            assert!(matches!(expect.operation(), Some(Operation::Expect(_))));
            assert!(matches!(wait.operation(), Some(Operation::Wait(_))));
            let printed = call(&["str", "expect", "print", "str", &v, "print"], 0, "");
            let caught = call(
                &[
                    "str", "timeout", "try", "str", &v, "changed", "1", "timeout", "wait", "catch",
                    "drop", "end",
                ],
                0,
                "",
            );
            assert_eq!((printed.operation(), caught.operation()), (None, None));
            // Refused before the operation: past a wait's longest limit, no
            // key of that name, no keys at all.
            let refused = [
                call(
                    &["str", &v, "changed", "4294967296", "timeout", "wait"],
                    1,
                    "fux: range: timeout",
                ),
                call(
                    &["str", &v, "expect", "str", "Nope", "key", "send-keys"],
                    1,
                    "fux: not_a_key: key",
                ),
                call(
                    &["str", &v, "expect", "send-keys"],
                    1,
                    "fux: usage: send-keys",
                ),
            ];
            assert!(refused.iter().all(|c| c.operation().is_none()));
            // The pane had moved past v before either ran.
            let moved = || -> Result<Vec<Snapshot>, String> {
                Ok(vec![
                    ls(1, &[&format!("%1:0.3.0.0.0:{R1}")])?,
                    ls(1, &[&format!("%1:0.3.0.0.0:{R1}")])?,
                ])
            };
            let mut calls = vec![(1, printed), (1, caught)];
            calls.extend(refused.into_iter().map(|c| (1, c)));
            let (problems, exercised) = monitored(&replay(moved()?, calls, &[])?);
            assert!(problems.is_empty(), "{problems:?}");
            assert!(
                !exercised.contains(&Promise::StaleExpect)
                    && !exercised.contains(&Promise::WaitAcrossChange)
            );
            let timed_out = call(
                &["str", &v, "changed", "1", "timeout", "wait"],
                124,
                "fux: timeout: wait",
            );
            let (problems, exercised) = monitored(&replay(moved()?, vec![(1, timed_out)], &[])?);
            assert!(exercised.contains(&Promise::WaitAcrossChange));
            assert!(
                problems.iter().any(|p| p.starts_with("wait-across-change")),
                "{problems:?}"
            );
            Ok(())
        }

        /// A word that bears a version but is none is refused before anything
        /// is normalized, never skipped.
        #[test]
        fn a_malformed_version_is_refused() -> Result<(), String> {
            let bad = format!("%1:0.x.0.0.0:{R1} screen");
            assert!(versions(&bad, false).is_err());
            assert_eq!(
                versions(&format!("%1:0.1.0.0.0:{R1} screen"), false)?
                    .0
                    .len(),
                1
            );
            // A whole word at the shim's 4,000 characters is still checked,
            // cut or not; only a cut one no delimiter ends is not, and it is
            // answered apart, as unchecked.
            let whole = format!("%1:0.x.0.0.0:{R1}\n");
            let padded = format!(
                "{}{whole}",
                std::iter::repeat_n(' ', 4000_usize.saturating_sub(whole.len()))
                    .collect::<String>()
            );
            assert_eq!(padded.chars().count(), 4000);
            assert!(versions(&padded, false).is_err() && versions(&padded, true).is_err());
            let part = format!("%1:0.1.0.0.0:{R1}\n%2:0.1.0.0.0:1-11");
            let (seen, unchecked) = versions(&part, true)?;
            assert_eq!(
                (seen.len(), unchecked.as_deref()),
                (1, Some("%2:0.1.0.0.0:1-11"))
            );
            let mut cut = call(&["str", "x", "changed", "wait"], 0, "");
            (cut.out, cut.out_cut) = (part, true);
            let mut left = Vec::new();
            let snapshots = vec![ls(0, &[])?, ls(0, &[])?];
            monitor(
                &replay(snapshots, vec![(1, cut)], &[])?,
                &mut Vec::new(),
                &mut left,
            );
            assert!(
                left.iter().any(|l| l.contains("may be part of a version")),
                "{left:?}"
            );
            let mut answered = call(&["str", "x", "changed", "wait"], 0, "");
            answered.out = bad;
            let snapshots = vec![ls(0, &[])?, ls(0, &[])?];
            let (problems, _) = monitored(&replay(snapshots, vec![(1, answered)], &[])?);
            assert!(
                problems.iter().any(|p| p.contains("a malformed version")),
                "{problems:?}"
            );
            Ok(())
        }

        /// An id is named within its own server run, however the runs
        /// interleave.
        #[test]
        fn an_id_is_named_within_its_own_run() {
            let mut names = Names::new("/nowhere");
            let lines: Vec<String> = [R1, R2, R2, R1, R2]
                .iter()
                .map(|run| names.line(&format!("%1:0.0.0.0.0:{run}")))
                .collect();
            assert_eq!(
                lines,
                [
                    "%a:0.0.0.0.0:R1",
                    "%b:0.0.0.0.0:R2",
                    "%b:0.0.0.0.0:R2",
                    "%a:0.0.0.0.0:R1",
                    "%b:0.0.0.0.0:R2"
                ]
            );
            // A bare id is of the last run printed.
            assert_eq!(names.line("%1"), "%b");
        }

        /// An ended program's SCREEN may grow while its terminal is read on
        /// (D-90); once it is drained (two observations with no output
        /// between them), by one revision for each resize or history change,
        /// and no more.
        #[test]
        fn a_resize_excuses_one_revision_of_an_ended_screen() -> Result<(), String> {
            let events = [
                format!(r#"{{"event":"created","workspace":"+1","next":"1:{R1}"}}"#),
                format!(r#"{{"event":"created","tab":"@1","workspace":"+1","next":"2:{R1}"}}"#),
                format!(
                    r#"{{"event":"created","pane":"%1","tab":"@1","workspace":"+1","next":"3:{R1}"}}"#
                ),
                format!(
                    r#"{{"event":"exited","pane":"%1","tab":"@1","workspace":"+1","exited":0,"next":"4:{R1}"}}"#
                ),
                format!(
                    r#"{{"event":"resized","pane":"%1","tab":"@1","workspace":"+1","next":"5:{R1}"}}"#
                ),
            ];
            let events: Vec<&str> = events.iter().map(String::as_str).collect();
            let at = |screen: u64| format!("%1:0.{screen}.0.0.1:{R1}");
            let still = |n| ls(n, &[&at(10)]);
            for (drained, screen, broken) in
                [(true, 11, false), (true, 110, true), (false, 110, false)]
            {
                let mut snapshots = vec![still(4)?];
                if drained {
                    snapshots.push(still(4)?);
                }
                snapshots.push(ls(5, &[&at(screen)])?);
                let (problems, exercised) = monitored(&replay(snapshots, Vec::new(), &events)?);
                assert!(exercised.contains(&Promise::EndedFrozen));
                assert_eq!(
                    problems.iter().any(|p| p.starts_with("ended-frozen")),
                    broken,
                    "{problems:?}"
                );
            }
            Ok(())
        }

        /// Each population bound refuses the one past it before keeping it.
        #[test]
        fn each_bound_refuses_one_over() -> Result<(), String> {
            let array = |n: usize| format!("[{}]", vec!["0"; n].join(","));
            assert!(Json::read(&array(ITEMS), ITEMS).is_ok());
            assert!(Json::read(&array(ITEMS.saturating_add(1)), ITEMS).is_err());
            let pane = format!("%1:0.0.0.0.0:{R1}");
            assert!(ls(0, &vec![pane.as_str(); ITEMS.saturating_add(1)]).is_err());
            let line = r#"{"argv":["ls"],"stdin":"","status":0,"out":"","err":""}"#;
            let mut calls = Vec::new();
            keep_calls(&mut calls, 1, &vec![line; CALLS].join("\n"))?;
            assert!(keep_calls(&mut calls, 2, line).is_err());
            assert_eq!(calls.len(), CALLS);
            let mut events = vec![Json::Null; EVENTS.saturating_sub(1)];
            keep_events(&mut events, &[Json::Null])?;
            assert!(keep_events(&mut events, &[Json::Null]).is_err());
            assert_eq!(events.len(), EVENTS);
            Ok(())
        }
    }
}
