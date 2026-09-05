//! Optional observer child ownership. Observers never own a pane PTY or its command.
use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

pub struct Observer(Arc<Mutex<Option<Child>>>);
impl Observer {
    pub fn spawn(
        executable: &Path,
        socket: &Path,
        pane: u32,
        pid: u32,
        receive: impl Fn(super::Report) + Send + 'static,
    ) -> std::io::Result<(Self, JoinHandle<()>)> {
        let mut command = Command::new(executable);
        command
            .args(["observe", "--socket"])
            .arg(socket)
            .args(["--pane", &pane.to_string(), "--pid", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        // Pane markers and remote credentials are not observer configuration.
        for (key, _) in std::env::vars_os() {
            if key.to_str().is_some_and(|key| {
                key.starts_with("KOH_") || key.starts_with("FUX_") || key == "ZOR_PID"
            }) {
                command.env_remove(key);
            }
        }
        let observer = Self(Arc::new(Mutex::new(Some(command.spawn()?))));
        let stdout = observer
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
            .and_then(|child| child.stdout.take())
            .ok_or_else(|| std::io::Error::other("observer stdout unavailable"))?;
        let process = Arc::clone(&observer.0);
        let worker = std::thread::Builder::new()
            .name(format!("fux-observer-{pane}"))
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    let mut line = Vec::new();
                    match reader
                        .by_ref()
                        .take((super::MAX_REPORT_BYTES + 1) as u64)
                        .read_until(b'\n', &mut line)
                    {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    if line.pop() != Some(b'\n') || line.len() > super::MAX_REPORT_BYTES {
                        break;
                    }
                    let Ok(report) = super::parse(&line) else {
                        break;
                    };
                    receive(report);
                    // Bound hostile observer update frequency; pipe backpressure affects only it.
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                stop(&process);
                // A dead or malformed observer cannot leave an apparently live observation.
                if let Ok(report) = super::Report::new(
                    super::State::None,
                    None,
                    0,
                    super::Flags::default(),
                    false,
                    None,
                ) {
                    receive(report);
                }
            })?;
        Ok((observer, worker))
    }
}
impl Drop for Observer {
    fn drop(&mut self) {
        stop(&self.0);
    }
}

fn stop(process: &Mutex<Option<Child>>) {
    let Some(mut child) = process
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    else {
        return;
    };
    // The direct child has not been reaped, so its process-group ID cannot have been reused.
    if let Ok(pid) = i32::try_from(child.id()) {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    let _ = child.kill();
    let _ = child.wait();
}
