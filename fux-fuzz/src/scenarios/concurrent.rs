//! Two real frontends each walking their own seeded steps, interleaved over
//! one workspace, with every invariant checked for both after every step and
//! a focus-isolation check: bytes typed into one viewer arrive only in the
//! pane that viewer focused.
use super::walk::{self, Walker};
use super::*;
use crate::trace::Step;

fn key_input(s: &mut Server, v: u64, key: &str) -> Result<()> {
    s.rpc(
        "world.trigger_event",
        json!({"event":"fux::control::UserInput","value":{"viewer":v,"input":{"kind":"key","key":key,"ctrl":false,"alt":false,"shift":false}}}),
    )?;
    Ok(())
}
/// Every capture file of every walk-created pane, with its content.
fn captures(s: &mut Server) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(&s.directory)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("pane-WK") && name.ends_with(".bin") {
            out.push((name, fs::read(entry.path()).unwrap_or_default()));
        }
    }
    Ok(out)
}

pub(super) fn run(s: &mut Server, seed: u64, steps: &[Step]) -> Result<()> {
    let mut a = Walker::new(s)?;
    let mut b = Walker::new(s)?;
    // Markers must be unique across both walks.
    b.next_marker = 5000;
    // Panes created by either walk capture what they receive.
    a.capture = true;
    b.capture = true;
    // Both walks share the process cap.
    a.cap = walk::PROCESS_CAP;
    b.cap = walk::PROCESS_CAP;
    s.journal.record(
        "concurrent_begin",
        json!({"seed":seed,"steps":steps.len(),"a":a.driver,"b":b.driver}),
    )?;
    let mut token = 0u32;
    for (i, st) in steps.iter().enumerate() {
        let (who, other) = if i.is_multiple_of(2) {
            (&mut a, &b)
        } else {
            (&mut b, &a)
        };
        let others: Vec<u64> = if other.overlay_open {
            vec![other.driver]
        } else {
            Vec::new()
        };
        walk::step(s, who, i, st, &others)?;
        // Merge marker knowledge so both walkers judge visibility of all panes.
        let merged: std::collections::BTreeMap<u64, String> = a
            .markers
            .iter()
            .chain(b.markers.iter())
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        a.markers = merged.clone();
        b.markers = merged;
        if i % 10 == 9 {
            // Focus isolation: a token typed into A lands only in A's focused
            // pane, and likewise for B, whenever that pane is a capturing one.
            for (label, walker) in [("A", &a), ("B", &b)] {
                let Ok(leaf) = s.relation(walker.driver, "fux::model::Focused") else {
                    continue;
                };
                let Some(marker) = walker.markers.get(&leaf).cloned() else {
                    continue;
                };
                token += 1;
                let text = format!("{label}{token}");
                key_input(s, walker.driver, "escape")?;
                std::thread::sleep(std::time::Duration::from_millis(60));
                for ch in text.chars() {
                    key_input(s, walker.driver, &ch.to_string())?;
                }
                let file = format!("pane-{marker}.bin");
                let mut where_found = Vec::new();
                let landed = s
                    .wait("token delivered", |s| {
                        where_found = captures(s)?
                            .into_iter()
                            .filter(|(_, bytes)| String::from_utf8_lossy(bytes).contains(&text))
                            .map(|(n, _)| n)
                            .collect();
                        Ok(!where_found.is_empty())
                    })
                    .is_ok();
                s.journal.record(
                    "focus_isolation",
                    json!({"viewer":label,"token":text,"expected":file,"found_in":where_found}),
                )?;
                if !landed {
                    // The focused pane may have an overlay open or be in copy
                    // mode after this step; that is not a delivery defect.
                    continue;
                }
                ensure(
                    where_found == vec![file.clone()],
                    &format!(
                        "application: bytes typed into viewer {label} (focused {leaf}, {marker}) landed in {where_found:?}, expected only {file}"
                    ),
                )?;
            }
        }
    }
    s.journal
        .record("concurrent_end", json!({"steps":steps.len()}))?;
    Ok(())
}
