//! The supervisor run as a program: restarts with backoff, forwards SIGTERM,
//! kills what outlives its grace, and exits once all have ended.
use fuxix::process::{Pid, Signal, kill};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn restarts_forwards_term_and_kills_after_grace() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!("supervise-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let file = dir.join("programs");
    std::fs::write(
        &file,
        "# three programs\n\
         quick sh -c 'exit 3'\n\
         polite sh -c 'trap \"exit 0\" TERM; while :; do sleep 0.05; done'\n\
         stubborn sh -c 'trap \"\" TERM; while :; do sleep 0.05; done'\n",
    )?;
    let mut supervisor = Command::new(env!("CARGO_BIN_EXE_fuxix-supervise"))
        .args(["--backoff", "50", "--backoff-max", "400", "--grace", "300"])
        .arg(&file)
        .stderr(Stdio::piped())
        .spawn()?;
    std::thread::sleep(Duration::from_millis(800));
    let pid = Pid::of(&supervisor).ok_or("no pid")?;
    kill(pid, Signal::Term)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = supervisor.try_wait()? {
            break status;
        }
        if Instant::now() > deadline {
            supervisor.kill()?;
            return Err("the supervisor didn't exit".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut log = String::new();
    supervisor.stderr.take().ok_or("no stderr")?.read_to_string(&mut log)?;
    std::fs::remove_dir_all(&dir)?;
    assert!(status.success(), "{status}\n{log}");
    let has = |text: &str| log.lines().any(|line| line.contains(text));
    for text in [
        "quick: started pid",
        "exited with status 3",
        "quick: restarting in 0.050s",
        "quick: restarting in 0.100s",
        "quick: restarting in 0.400s",
        "supervisor: got TERM; forwarding to 2 programs",
        "supervisor: grace over; killing 1 programs",
        "killed by SIGKILL (9)",
        "supervisor: every program has ended; exiting",
    ] {
        assert!(has(text), "no {text:?} in:\n{log}");
    }
    let polite = log.lines().find(|l| l.contains("polite: pid")).unwrap_or_default();
    assert!(polite.contains("exited with status 0"), "{log}");
    let after_term = log.split("got TERM").nth(1).unwrap_or_default();
    assert!(!after_term.contains("started pid"), "nothing restarts once stopping:\n{log}");
    Ok(())
}
