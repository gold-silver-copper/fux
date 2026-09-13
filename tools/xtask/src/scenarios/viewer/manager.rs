use super::*;

/// Real manager replies delayed independently of the attachment connection.
pub(super) fn run(binary: &Path) -> Result<()> {
    use crate::support::manager_delay::{Pending, Proxy};
    fn gated(h: &mut Harness, proxy: &Proxy, label: &str) -> Result<Pending> {
        let mut pending = None;
        h.wait(
            |_| {
                pending = proxy.take();
                Ok(pending.is_some())
            },
            label,
            3,
        )?;
        pending.context("manager gate")
    }
    fn reorder(h: &mut Harness, proxy: &Proxy) -> Result<Pending> {
        proxy.arm("reorder")?;
        h.send(0, b"\x01f")?;
        h.wait(
            |h| Ok(h.text(0).contains("Place workspace before")),
            "reorder chooser",
            4,
        )?;
        h.send(0, b"\r")?;
        let gate = gated(h, proxy, "manager mutation withheld")?;
        ensure!(gate.request["request"] == "reorder", "wrong gate");
        Ok(gate)
    }
    let root = Root::new(
        "fmgrdelay-",
        &[
            "/bin/sh".into(),
            "-c".into(),
            "i=1; while [ $i -le 80 ]; do printf 'HIST%03d\\n' $i; i=$((i+1)); done; exec cat"
                .into(),
        ],
    )?;
    let mut h = Harness {
        root,
        binary: binary.into(),
        viewers: Vec::new(),
        finished: false,
    };
    h.add(&[])?;
    h.wait(
        |h| Ok(h.text(0).contains("HIST080")),
        "manager fixture ready",
        8,
    )?;
    h.cli(&["workspace", "new", "other"])?;
    h.send(0, b"\x01\\")?;
    h.state(|s| panes(&s[0]) == 2, "manager fixture two panes", 8)?;
    h.send(0, b"\x01h")?;
    h.wait(
        |h| Ok(h.bar(0).trim_end().ends_with('1')),
        "manager fixture focus first",
        4,
    )?;
    let proxy = Proxy::start(&h.root.path().join("fux/manager.sock"))?;

    proxy.arm("catalog")?;
    h.send(0, b"\x01s")?;
    let lookup = gated(&mut h, &proxy, "workspace lookup withheld")?;
    h.send(0, b"j\x1b[<64;3;3M")?;
    h.pump(0.1)?;
    h.send(0, b"\x1b")?;
    h.wait(
        |h| Ok(!h.text(0).contains("Choose workspace")),
        "cancel pending workspace lookup",
        4,
    )?;
    h.wait(
        |h| Ok(h.text(0).contains("History pane 1")),
        "lookup wheel history survives dialog cancellation",
        4,
    )?;
    h.send(0, b"\x1b")?;
    h.wait(
        |h| Ok(!h.text(0).contains("History pane")),
        "lookup history dismissal",
        4,
    )?;
    h.send(0, b"\x01s")?;
    h.wait(
        |h| Ok(h.text(0).contains("Choose workspace") && !h.text(0).contains("Loading workspaces")),
        "replacement workspace chooser",
        4,
    )?;
    lookup.finish(Some(
        serde_json::json!({"reply":"failed","message":"OLD_LOOKUP_FAILURE"}),
    ))?;
    h.hold(
        |h| h.text(0).contains("Choose workspace") && !h.text(0).contains("OLD_LOOKUP_FAILURE"),
        0.2,
    )?;
    h.send(0, b"\x1b")?;
    h.wait(
        |h| Ok(!h.text(0).contains("Choose workspace")),
        "replacement chooser dismisses",
        4,
    )?;

    proxy.arm("catalog")?;
    h.send(0, b"\x01i")?;
    let destination = gated(&mut h, &proxy, "destination lookup withheld")?;
    h.wait(
        |h| Ok(h.text(0).contains("Loading destinations")),
        "pending destination chooser",
        4,
    )?;
    h.send(0, b"j\x1b[<64;3;3M\x1b[<0;3;3M\x1b[<0;3;3m")?;
    h.wait(
        |h| Ok(!h.text(0).contains("Move pane") && h.text(0).contains("History pane 1")),
        "destination outside click dismisses with history retained",
        4,
    )?;
    h.send(0, b"\x1b")?;
    h.wait(
        |h| Ok(!h.text(0).contains("History pane")),
        "destination lookup history dismissal",
        4,
    )?;
    h.send(0, b"\x01i")?;
    h.wait(
        |h| {
            Ok(h.text(0).contains("Move pane 1 to workspace")
                && !h.text(0).contains("Loading destinations"))
        },
        "replacement destination chooser",
        4,
    )?;
    destination.finish(None)?;
    h.hold(|h| h.text(0).contains("Move pane 1 to workspace"), 0.2)?;
    h.send(0, b"\x1b")?;
    h.wait(
        |h| Ok(!h.text(0).contains("Move pane")),
        "replacement destination dismisses without stale gesture",
        4,
    )?;

    let mutation = reorder(&mut h, &proxy)?;
    h.send(0, b"\x1b[<64;3;3M")?;
    h.wait(
        |h| Ok(h.text(0).contains("History pane 1") && h.text(0).contains("offset 3")),
        "history during delayed manager mutation",
        4,
    )?;
    h.send(0, b"\x1b")?;
    h.wait(
        |h| Ok(!h.text(0).contains("History pane")),
        "Escape during delayed manager mutation",
        4,
    )?;
    h.send(0, b"\x01,discarded")?;
    h.wait(
        |h| Ok(h.text(0).contains("Waiting for previous")),
        "dependent rename during manager mutation",
        4,
    )?;
    h.send(0, b"\x1b")?;
    h.wait(
        |h| Ok(!h.text(0).contains("Waiting for previous")),
        "cancel manager dependent rename",
        4,
    )?;
    h.send(0, b"HELD_MANAGER_INPUT\r")?;
    h.hold(|h| !h.text(0).contains("HELD_MANAGER_INPUT"), 0.15)?;
    mutation.finish(None)?;
    h.wait(
        |h| Ok(h.text(0).matches("HELD_MANAGER_INPUT").count() == 2),
        "manager reply releases original pane input",
        4,
    )?;
    ensure!(
        !h.rows(0).iter().any(|row| row
            .chars()
            .skip(40)
            .collect::<String>()
            .contains("HELD_MANAGER_INPUT")),
        "input reached other pane"
    );
    ensure!(
        h.tabs("default")?[0]["name"] == "main",
        "canceled rename ran after manager reply"
    );

    let mutation = reorder(&mut h, &proxy)?;
    h.send(0, b"NEVER_RETARGET\r")?;
    h.hold(|h| !h.text(0).contains("NEVER_RETARGET"), 0.15)?;
    h.cli(&["default", "kill", "1"])?;
    h.state(
        |s| panes(&s[0]) == 1,
        "original target removed while manager pending",
        4,
    )?;
    mutation.finish(None)?;
    h.wait(
        |h| Ok(h.text(0).contains("target changed")),
        "manager queued input target loss notice",
        4,
    )?;
    h.hold(|h| !h.text(0).contains("NEVER_RETARGET"), 0.2)?;
    h.send(0, b"AFTER_TARGET_CHANGE\r")?;
    h.wait(
        |h| Ok(h.text(0).contains("AFTER_TARGET_CHANGE")),
        "normal input after manager target loss",
        4,
    )?;
    let mutation = reorder(&mut h, &proxy)?;
    h.send(0, b"\x01")?;
    h.wait(
        |h| Ok(h.text(0).contains("split side by side")),
        "command popup during manager mutation",
        4,
    )?;
    h.send(0, b"\x1b")?;
    h.wait(
        |h| Ok(!h.text(0).contains("split side by side")),
        "command popup dismisses before manager reply",
        4,
    )?;
    mutation.finish(Some(
        serde_json::json!({"reply":"failed","message":"MANAGER_EXPECTED_FAILURE"}),
    ))?;
    h.wait(
        |h| {
            Ok(h.text(0).contains("MANAGER_EXPECTED_FAILURE")
                && !h.text(0).contains("split side by side"))
        },
        "manager failure is a notice without commands",
        4,
    )?;
    h.send(0, b"AFTER_MANAGER_FAILURE\r")?;
    h.wait(
        |h| Ok(h.text(0).matches("AFTER_MANAGER_FAILURE").count() == 2),
        "input works after manager failure",
        4,
    )?;
    drop(proxy);
    h.finish()?;
    println!(
        "PASS delayed manager lookup/mutation, cancel/re-entry, local history, dependent commands and original input target"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unfinished_osc_is_bounded_across_terminal_reads() -> Result<()> {
        let mut bound = EscapeBound::default();
        bound.feed(b"\x1b")?;
        bound.feed(b"]52;c;")?;
        for _ in 0..31 {
            bound.feed(&vec![b'x'; 65536])?;
        }
        assert!(bound.feed(&vec![b'x'; 65536]).is_err());
        let mut bounded = EscapeBound::default();
        for _ in 0..40 {
            bounded.feed(b"\x1b]52;c;")?;
            bounded.feed(&vec![b'x'; 65536])?;
            bounded.feed(b"\x1b")?;
            bounded.feed(b"\\")?;
        }
        Ok(())
    }
}
