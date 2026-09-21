use super::*;

const NODE: &str = "bevy_ui::ui_node::Node";
const SPLIT: &str = "fux::model::Split";
const WORKSPACE: &str = "fux::model::Workspace";

fn notice(s: &mut Server, viewer: u64) -> Result<Value> {
    let rows = s.query(VIEWER)?;
    let row = rows
        .iter()
        .find(|row| id(row).ok() == Some(viewer))
        .ok_or("fidelity viewer disappeared")?;
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
fn ok_notice(done: &Value, label: &str) -> Result<()> {
    ensure(
        done.get("error") != Some(&json!(true)),
        &format!("application: {label} failed: {done}"),
    )
}

/// A saved scene with entity ids masked and its entity blocks sorted, so two
/// saves of the same layout compare equal regardless of allocation order.
pub(super) fn canonical(ron: &str) -> Vec<String> {
    let masked: String = {
        let mut out = String::new();
        let mut digits = String::new();
        for c in ron.chars() {
            if c.is_ascii_digit() {
                digits.push(c);
            } else {
                if digits.len() >= 9 {
                    out.push_str("<id>");
                } else {
                    out.push_str(&digits);
                }
                digits.clear();
                out.push(c);
            }
        }
        if digits.len() >= 9 {
            out.push_str("<id>");
        } else {
            out.push_str(&digits);
        }
        out
    };
    // The file's own closing `},` and `)` would otherwise land on whichever
    // entity happens to be serialized last.
    let masked = match masked.rfind("\n  },\n)") {
        Some(i) => masked.get(..i).unwrap_or(&masked).to_owned(),
        None => masked,
    };
    let mut blocks: Vec<String> = masked
        .split("\n    <id>: (\n")
        .skip(1)
        .map(|b| b.trim_end_matches(['\n', ' ', ')', ',']).to_owned())
        .collect();
    blocks.sort();
    blocks
}
fn node_of(s: &mut Server, entity: u64) -> Result<Value> {
    s.query(NODE)?
        .iter()
        .find(|r| id(r).ok() == Some(entity))
        .and_then(|r| r.pointer(&format!("/components/{NODE}")).cloned())
        .ok_or_else(|| format!("{entity} has no Node").into())
}
/// The bottom bar always carries one `│` as its zone divider; only content
/// rows can hold a split separator.
fn content_has_separator(frame: &str) -> bool {
    frame.lines().take(23).any(|l| l.contains('│'))
}
fn first_content_line(frame: &str) -> String {
    frame
        .lines()
        .take(23)
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .to_owned()
}
fn gap_columns(frame: &str) -> Vec<usize> {
    // Columns that are blank on every content row: a wide gap paints nothing.
    let rows: Vec<&str> = frame.lines().take(23).collect();
    let width = rows.iter().map(|r| r.chars().count()).max().unwrap_or(0);
    (0..width)
        .filter(|x| {
            rows.iter()
                .all(|r| r.chars().nth(*x).is_none_or(|c| c == ' '))
        })
        .collect()
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let f = s.attach(24, 80)?;
    let v = s.frontend(f)?.viewer;
    s.wait("first shell output", |s| {
        Ok(s.frame(v, 24, 80)?.contains("DEFAULT-SHELL"))
    })?;
    let workspace = s.relation(v, "fux::model::Viewing")?;
    s.control(v, json!({"kind":"split","axis":"horizontal","program":"stty raw -echo; printf '\\033[2J\\033[HFID-B'; exec cat > /dev/null"}))?;
    s.wait("two panes", |s| Ok(s.frame(v, 24, 80)?.contains("FID-B")))?;
    let pids = running(s)?;

    // Give the split container a non-default Node: a five-cell column gap and
    // asymmetric padding. The README promises these survive save and load
    // verbatim, and that only one-cell gaps get separator glyphs.
    let container = s
        .query(SPLIT)?
        .iter()
        .map(id)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .find(|e| *e != workspace)
        .ok_or("no split container")?;
    let mut node = node_of(s, container)?;
    let object = node.as_object_mut().ok_or("Node is not an object")?;
    object.insert("column_gap".into(), json!({"Px":5.0}));
    let mut padding = object.get("padding").cloned().unwrap_or(json!({}));
    if let Some(p) = padding.as_object_mut() {
        p.insert("left".into(), json!({"Px":2.0}));
        p.insert("top".into(), json!({"Px":1.0}));
    }
    object.insert("padding".into(), padding);
    s.rpc(
        "world.insert_components",
        json!({"entity":container,"components":{NODE:node}}),
    )?;
    let mut frame = String::new();
    s.wait("wide gap painted blank", |s| {
        frame = s.frame(v, 24, 80)?;
        // Five blank columns in a row somewhere between the two panes, and no
        // separator glyph anywhere.
        let blanks = gap_columns(&frame);
        Ok(!content_has_separator(&frame)
            && blanks.windows(5).any(|w| w.iter().zip(w.iter().skip(1)).all(|(a, b)| a + 1 == *b)))
    })
    .map_err(|e| {
        format!("application: a five-cell column gap should paint blank with no separator: {e}; frame {:?}", frame.lines().next())
    })?;
    ensure(
        first_content_line(&frame).starts_with("  "),
        &format!(
            "application: two cells of left padding should indent the first pane: {:?}",
            frame.lines().next()
        ),
    )?;

    // Save, load, save again: the two files must be structurally identical
    // and the custom Node must be present verbatim in both.
    s.control(
        v,
        json!({"kind":"save_layout","workspace":workspace,"path":"first.scn.ron"}),
    )?;
    ok_notice(&settled(s, v, "first save")?, "first save")?;
    let first = fs::read_to_string(s.directory.join("first.scn.ron"))?;
    for needle in ["column_gap: Px(5.0)", "left: Px(2.0)", "top: Px(1.0)"] {
        ensure(
            first.contains(needle),
            &format!("application: saved layout lost a custom Node property {needle:?}"),
        )?;
    }
    s.control(
        v,
        json!({"kind":"load_layout","workspace":workspace,"path":"first.scn.ron","mapping":[]}),
    )?;
    ok_notice(&settled(s, v, "load")?, "load")?;
    let workspace = s.relation(v, "fux::model::Viewing")?;
    s.wait("loaded layout painted", |s| {
        let frame = s.frame(v, 24, 80)?;
        Ok(frame.contains("FID-B") && frame.contains("DEFAULT-SHELL"))
    })?;
    ensure(
        pids.iter().all(|p| alive(*p)),
        "application: loading a layout terminated a process",
    )?;
    s.control(
        v,
        json!({"kind":"save_layout","workspace":workspace,"path":"second.scn.ron"}),
    )?;
    ok_notice(&settled(s, v, "second save")?, "second save")?;
    let second = fs::read_to_string(s.directory.join("second.scn.ron"))?;
    let (a, b) = (canonical(&first), canonical(&second));
    if a != b {
        let mut diffs = Vec::new();
        for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
            if x != y {
                let line = x
                    .lines()
                    .zip(y.lines())
                    .find(|(l, r)| l != r)
                    .map(|(l, r)| format!("{l:?} vs {r:?}"))
                    .unwrap_or_else(|| "lengths differ".into());
                diffs.push(json!({"block":i,"first_line_that_differs":line}));
            }
        }
        s.journal.record(
            "round_trip_diff",
            json!({"blocks_first":a.len(),"blocks_second":b.len(),"diffs":diffs}),
        )?;
        return Err(format!(
            "application: save, load, save changed the layout: {} vs {} blocks, first differences {}",
            a.len(),
            b.len(),
            serde_json::to_string(&diffs)?
        )
        .into());
    }
    s.journal
        .record("round_trip", json!({"blocks":a.len(),"identical":true}))?;

    // After the round trip the gap is still wide and blank on screen, and
    // the loaded container carries the custom values in the live world.
    let loaded = s
        .query(SPLIT)?
        .iter()
        .map(id)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .find(|e| *e != workspace)
        .ok_or("no split container after load")?;
    let live = node_of(s, loaded)?;
    ensure(
        live.get("column_gap") == Some(&json!({"Px":5.0})),
        &format!(
            "application: loaded container lost its column gap: {:?}",
            live.get("column_gap")
        ),
    )?;
    let frame = s.frame(v, 24, 80)?;
    ensure(
        !content_has_separator(&frame),
        "application: a wide gap gained a separator glyph after load",
    )?;

    // A one-cell gap gets exactly the shared separator back.
    let mut node = node_of(s, loaded)?;
    node.as_object_mut()
        .ok_or("Node")?
        .insert("column_gap".into(), json!({"Px":1.0}));
    s.rpc(
        "world.insert_components",
        json!({"entity":loaded,"components":{NODE:node}}),
    )?;
    s.wait("one-cell gap paints a separator", |s| {
        Ok(content_has_separator(&s.frame(v, 24, 80)?))
    })
    .map_err(|e| format!("application: restoring a one-cell gap did not paint a separator: {e}"))?;
    let _ = s.query(WORKSPACE)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::canonical;
    #[test]
    fn canonical_masks_ids_and_orders_blocks() {
        let a = "(\n  entities: {\n    4294967200: (\n      x: (4294967201),\n    ),\n    4294967201: (\n      y: 1,\n    ),\n  },\n)";
        let b = "(\n  entities: {\n    4294967300: (\n      y: 1,\n    ),\n    4294967299: (\n      x: (4294967300),\n    ),\n  },\n)";
        assert_eq!(canonical(a), canonical(b));
        let c = "(\n  entities: {\n    4294967300: (\n      y: 2,\n    ),\n  },\n)";
        assert_ne!(canonical(a), canonical(c));
    }
}
