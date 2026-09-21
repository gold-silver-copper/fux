use super::scene_fidelity::canonical;
use super::*;

const NODE: &str = "bevy_ui::ui_node::Node";
const TAB: &str = "fux::model::Tab";
const WORKSPACE: &str = "fux::model::Workspace";
const PANE_VIEW: &str = "fux::model::PaneView";

fn notice(s: &mut Server, viewer: u64) -> Result<Value> {
    let rows = s.query(VIEWER)?;
    let row = rows
        .iter()
        .find(|row| id(row).ok() == Some(viewer))
        .ok_or("tabless viewer disappeared")?;
    Ok(component(row, VIEWER)?
        .get("notice")
        .cloned()
        .unwrap_or(Value::Null))
}
fn settled(s: &mut Server, viewer: u64, label: &str) -> Result<Value> {
    let mut last = Value::Null;
    s.wait(label, |s| {
        last = notice(s, viewer)?;
        let text = last.get("text").and_then(Value::as_str).unwrap_or_default();
        Ok(!text.is_empty() && !text.ends_with("..."))
    })?;
    Ok(last)
}

/// One `    <id>: (` entity block of a saved scene, with its id.
struct Block {
    id: u64,
    text: String,
}
fn blocks(ron: &str) -> Result<(String, Vec<Block>, String)> {
    let start = ron.find("  entities: {\n").ok_or("no entities")? + "  entities: {\n".len();
    let end = ron.rfind("\n  },\n").ok_or("no entities end")?;
    let head = ron.get(..start).ok_or("head")?.to_owned();
    let tail = ron.get(end..).ok_or("tail")?.to_owned();
    let body = ron.get(start..end).ok_or("body")?;
    let mut out = Vec::new();
    for piece in body.split("\n    ),").filter(|p| !p.trim().is_empty()) {
        let piece = piece.trim_start_matches('\n');
        let (id_text, rest) = piece.split_once(": (\n").ok_or("block id")?;
        out.push(Block {
            id: id_text.trim().parse()?,
            text: rest.to_owned(),
        });
    }
    Ok((head, out, tail))
}
fn assemble(head: &str, blocks: &[Block], tail: &str) -> String {
    let mut out = head.to_owned();
    for b in blocks {
        out.push_str(&format!("    {}: (\n{}\n    ),\n", b.id, b.text));
    }
    out.push_str(tail.trim_start_matches('\n'));
    out
}
fn has(block: &Block, component: &str) -> bool {
    block.text.contains(&format!("\"{component}\""))
}

/// Turns a saved tabbed scene into the PR #20 shape: the tab is removed, the
/// split hangs directly off the workspace, and the workspace root carries a
/// distinctive Node so the documented move into the wrapper tab is checkable.
fn make_tabless(ron: &str) -> Result<(String, u64, u64)> {
    let (head, mut list, tail) = blocks(ron)?;
    let root = list
        .iter()
        .find(|b| has(b, WORKSPACE))
        .ok_or("no workspace block")?
        .id;
    let tab = list.iter().find(|b| has(b, TAB)).ok_or("no tab block")?.id;
    let split = list
        .iter()
        .find(|b| has(b, "fux::model::Split") && b.text.contains(&format!("ChildOf\": ({tab})")))
        .ok_or("no split under the tab")?
        .id;
    list.retain(|b| b.id != tab);
    for b in &mut list {
        if b.id == split {
            b.text = b.text.replace(
                &format!("ChildOf\": ({tab})"),
                &format!("ChildOf\": ({root})"),
            );
        }
        if b.id == root {
            b.text = b.text.replace(&format!("{tab},"), &format!("{split},"));
            // A distinctive root Node: three cells of left padding.
            b.text = b.text.replacen(
                "padding: (\n            left: Px(0.0),",
                "padding: (\n            left: Px(3.0),",
                1,
            );
        }
    }
    ensure(
        list.iter()
            .any(|b| b.id == root && b.text.contains("left: Px(3.0)")),
        "could not mark the root Node",
    )?;
    Ok((assemble(&head, &list, &tail), root, split))
}
fn node_padding_left(s: &mut Server, entity: u64) -> Result<Value> {
    Ok(s.query(NODE)?
        .iter()
        .find(|r| id(r).ok() == Some(entity))
        .and_then(|r| {
            r.pointer(&format!("/components/{NODE}/padding/left"))
                .cloned()
        })
        .unwrap_or(Value::Null))
}
fn parent_of(s: &mut Server, entity: u64) -> Result<Option<u64>> {
    Ok(s.query("bevy_ecs::hierarchy::ChildOf")?
        .iter()
        .find(|r| id(r).ok() == Some(entity))
        .and_then(|r| {
            r.pointer("/components/bevy_ecs::hierarchy::ChildOf")?
                .as_u64()
        }))
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let f = s.attach(24, 80)?;
    let v = s.frontend(f)?.viewer;
    s.wait("first shell output", |s| {
        Ok(s.frame(v, 24, 80)?.contains("DEFAULT-SHELL"))
    })?;
    let workspace = s.relation(v, "fux::model::Viewing")?;
    s.control(v, json!({"kind":"split","axis":"horizontal","program":"stty raw -echo; printf '\\033[2J\\033[HTABLESS-B'; exec cat > /dev/null"}))?;
    s.wait("two panes", |s| {
        Ok(s.frame(v, 24, 80)?.contains("TABLESS-B"))
    })?;
    let pids = running(s)?;
    s.control(
        v,
        json!({"kind":"save_layout","workspace":workspace,"path":"tabbed.scn.ron"}),
    )?;
    let saved = settled(s, v, "save")?;
    ensure(
        saved.get("error") != Some(&json!(true)),
        &format!("save failed: {saved}"),
    )?;
    let tabbed = fs::read_to_string(s.directory.join("tabbed.scn.ron"))?;
    let (tabless, _, _) = make_tabless(&tabbed)?;
    ensure(
        !tabless.contains(&format!("\"{TAB}\"")),
        "tabless scene still contains a Tab",
    )?;
    fs::write(s.directory.join("tabless.scn.ron"), &tabless)?;
    s.journal
        .record("tabless_scene", json!({"bytes":tabless.len()}))?;

    // Loading wraps the legacy layout into one `main` tab: the root's Node
    // moves into that tab exactly once, the wrapper root is neutral, the
    // child entities and process references survive, and both processes
    // stay alive because loading launches nothing.
    s.control(
        v,
        json!({"kind":"load_layout","workspace":workspace,"path":"tabless.scn.ron","mapping":[]}),
    )?;
    let loaded = settled(s, v, "tabless load")?;
    ensure(
        loaded.get("error") != Some(&json!(true)),
        &format!("application: loading a tabless scene failed: {loaded}"),
    )?;
    let workspace = s.relation(v, "fux::model::Viewing")?;
    let tabs: Vec<u64> = s.query(TAB)?.iter().map(id).collect::<Result<_>>()?;
    ensure(
        tabs.len() == 1,
        &format!(
            "application: a tabless scene should be wrapped into one tab, found {}",
            tabs.len()
        ),
    )?;
    let tab = *tabs.first().ok_or("tab")?;
    ensure(
        parent_of(s, tab)? == Some(workspace),
        "application: the wrapper tab is not a direct workspace child",
    )?;
    let tab_pad = node_padding_left(s, tab)?;
    let root_pad = node_padding_left(s, workspace)?;
    s.journal.record(
        "wrapped_nodes",
        json!({"tab_padding_left":tab_pad,"root_padding_left":root_pad}),
    )?;
    ensure(
        tab_pad == json!({"Px":3.0}),
        &format!(
            "application: the root's Node should move into the wrapper tab (left padding 3), tab has {tab_pad:?}"
        ),
    )?;
    ensure(
        root_pad != json!({"Px":3.0}),
        &format!(
            "application: the root's Node should move, not copy: workspace still has {root_pad:?}"
        ),
    )?;
    let views: Vec<u64> = s.query(PANE_VIEW)?.iter().map(id).collect::<Result<_>>()?;
    ensure(
        views.len() == 2,
        &format!("application: wrapping lost pane views: {}", views.len()),
    )?;
    for view in &views {
        let mut cursor = Some(*view);
        let mut under_tab = false;
        for _ in 0..8 {
            let Some(c) = cursor else { break };
            if c == tab {
                under_tab = true;
                break;
            }
            cursor = parent_of(s, c)?;
        }
        ensure(
            under_tab,
            &format!("application: pane view {view} is not beneath the wrapper tab"),
        )?;
    }
    s.wait("wrapped layout painted with both processes", |s| {
        let frame = s.frame(v, 24, 80)?;
        Ok(frame.contains("TABLESS-B")
            && frame.contains("DEFAULT-SHELL")
            && frame.starts_with("   "))
    })
    .map_err(|e| {
        format!("application: wrapped layout did not paint both panes with the moved padding: {e}")
    })?;
    ensure(
        pids.iter().all(|p| alive(*p)),
        "application: loading a tabless scene terminated a process",
    )?;
    ensure(
        states(s)?.len() == pids.len(),
        "application: loading a tabless scene launched a process",
    )?;

    // Saving the wrapped layout and loading it again must not change it
    // further: a second wrap would nest the tab or duplicate the padding.
    s.control(
        v,
        json!({"kind":"save_layout","workspace":workspace,"path":"wrapped-1.scn.ron"}),
    )?;
    let saved = settled(s, v, "wrapped save")?;
    ensure(
        saved.get("error") != Some(&json!(true)),
        &format!("application: saving the wrapped layout failed: {saved}"),
    )?;
    s.control(
        v,
        json!({"kind":"load_layout","workspace":workspace,"path":"wrapped-1.scn.ron","mapping":[]}),
    )?;
    let reloaded = settled(s, v, "wrapped reload")?;
    ensure(
        reloaded.get("error") != Some(&json!(true)),
        &format!("application: reloading the wrapped layout failed: {reloaded}"),
    )?;
    let workspace = s.relation(v, "fux::model::Viewing")?;
    s.control(
        v,
        json!({"kind":"save_layout","workspace":workspace,"path":"wrapped-2.scn.ron"}),
    )?;
    let saved = settled(s, v, "wrapped second save")?;
    ensure(
        saved.get("error") != Some(&json!(true)),
        &format!("application: second save of the wrapped layout failed: {saved}"),
    )?;
    let one = fs::read_to_string(s.directory.join("wrapped-1.scn.ron"))?;
    let two = fs::read_to_string(s.directory.join("wrapped-2.scn.ron"))?;
    ensure(
        canonical(&one) == canonical(&two),
        "application: a wrapped layout changed again on its second round trip",
    )?;
    ensure(
        s.query(TAB)?.len() == 1,
        "application: the second round trip added a tab",
    )?;
    ensure(
        pids.iter().all(|p| alive(*p)),
        "application: the second round trip terminated a process",
    )?;
    Ok(())
}
