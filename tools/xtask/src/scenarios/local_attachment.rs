//! Two viewers preserve raw input and the original shell across detach/reattach.
use crate::support::{attachment, local::Root, process};
use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::{fs, path::Path, process::Command, time::Duration};

pub(super) fn run(binary: &Path) -> Result<()> {
    let root = Root::new("fl-rs-", &["/bin/sh".into()])?;
    let mut server = root.server(binary)?;
    let path = root.path().join("fux/default.attach.sock");
    let attach = || -> Result<std::os::unix::net::UnixStream> {
        let mut peer = attachment::connect(&path)?;
        attachment::send(
            &mut peer,
            &json!({"type":"hello","version":6,"rows":24,"columns":80}),
        )?;
        ensure!(
            attachment::receive(&mut peer)? == json!({"hello":{"version":6}}),
            "attachment hello mismatch"
        );
        ensure!(
            attachment::receive(&mut peer)?.get("state").is_some(),
            "missing initial state"
        );
        Ok(peer)
    };
    {
        let mut wrong = attachment::connect(&path)?;
        attachment::send(
            &mut wrong,
            &json!({"type":"hello","version":0,"rows":24,"columns":80}),
        )?;
        ensure!(
            attachment::receive(&mut wrong)?["error"]["message"]
                .as_str()
                .context("rejection message")?
                .contains("incompatible"),
            "wrong version accepted"
        );
    }
    let mut one = attach()?;
    let mut two = attach()?;
    fn marker(peer: &mut std::os::unix::net::UnixStream) -> Result<String> {
        for _ in 0..20 {
            let text = attachment::text(&attachment::receive(peer)?)?;
            for tail in text.split("LOCAL_OK_").skip(1) {
                let pid: String = tail.chars().take_while(char::is_ascii_digit).collect();
                if !pid.is_empty() {
                    return Ok(pid);
                }
            }
        }
        anyhow::bail!("missing shell output")
    }
    let input = json!({"type":"input","bytes":b"printf \"LOCAL_OK_%s\\n\" \"$$\"\n".to_vec()});
    attachment::send(&mut one, &input)?;
    let pid = marker(&mut one)?;
    ensure!(marker(&mut two)? == pid, "viewers disagree on shell PID");
    drop(one);
    drop(two);
    std::thread::sleep(Duration::from_millis(50));
    let mut three = attach()?;
    attachment::send(&mut three, &input)?;
    ensure!(marker(&mut three)? == pid, "reattach replaced shell PID");
    fn no_keys(path: &Path) -> Result<()> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                no_keys(&entry.path())?;
            } else {
                ensure!(
                    entry.path().extension() != Some(std::ffi::OsStr::new("key")),
                    "unexpected key file"
                );
            }
        }
        Ok(())
    }
    no_keys(root.path())?;
    let mut sockets = Command::new("lsof");
    sockets.args(["-nP", "-a", "-p", &server.child.id().to_string(), "-i"]);
    ensure!(
        process::output(sockets, Duration::from_secs(5), 65536)?
            .status
            .code()
            == Some(1),
        "owned server has network sockets"
    );
    drop(three);
    server.finish()?;
    ensure!(
        !server.diagnostic()?.contains("Passphrase"),
        "unexpected passphrase prompt"
    );
    println!(
        "PASS: two viewers, verbatim input, same shell PID after detach/reattach, no keys and no server network sockets"
    );
    Ok(())
}
