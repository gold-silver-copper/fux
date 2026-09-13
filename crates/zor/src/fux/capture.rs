//! Fux capture decoding and wire-to-observation conversion.
use crate::rules::view::{Captured, Progress};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{path::Path, time::Instant};
const MAX_TITLE_CHARS: usize = 256;

pub(crate) struct Cells {
    pub revision: u64,
    pub input_sequence: u64,
    pub unchanged: bool,
    pub truncated: bool,
    pub screen: Option<Captured>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WireCapture {
    revision: u64,
    input_sequence: u64,
    seq: u64,
    rows: u16,
    columns: u16,
    cursor: super::snapshot::Cursor,
    title: String,
    #[serde(deserialize_with = "Option::deserialize")]
    progress: Option<(u8, u8)>,
    unchanged: bool,
    truncated: bool,
    lines: Vec<Line>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Line {
    row: u16,
    wrapped: bool,
    cells: Vec<WireCell>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WireCell {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    kind: Option<CellKind>,
    #[serde(default)]
    style: Style,
    #[serde(default)]
    run: u16,
}
#[derive(Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
enum CellKind {
    Blank,
    Text,
    WideLeading,
    WideContinuation,
}
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Style {
    foreground: Color,
    background: Color,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
}
#[derive(Default, serde::Serialize, serde::Deserialize)]
enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

fn decode_cells(value: WireCapture) -> Result<Cells> {
    super::error::reply(|| {
        ensure!(
            (2..=512).contains(&value.rows) && (2..=512).contains(&value.columns),
            "invalid capture dimension"
        );
        ensure!(
            !(value.unchanged && value.truncated),
            "unchanged capture is truncated"
        );
        ensure!(
            value.lines.len() <= usize::from(value.rows),
            "too many capture rows"
        );
        ensure!(
            !value.unchanged || value.lines.is_empty(),
            "unchanged capture has rows"
        );
        // Sequence, cursor and wrap/style metadata remain typed and validated even when rules
        // do not interpret them. Their presence must not be confused with task policy.
        let _ = (value.seq, &value.cursor);
        let mut expanded = Vec::with_capacity(value.lines.len());
        for (index, line) in value.lines.iter().enumerate() {
            ensure!(
                usize::from(line.row) == index,
                "capture lines are out of order"
            );
            let _ = line.wrapped;
            expanded.push(expand_row(&line.cells, value.columns)?);
        }
        let screen = if value.unchanged || value.truncated {
            None
        } else {
            Some(screen(&value, expanded)?)
        };
        Ok(Cells {
            revision: value.revision,
            input_sequence: value.input_sequence,
            unchanged: value.unchanged,
            truncated: value.truncated,
            screen,
        })
    })
}

pub(crate) fn cells(
    socket: &Path,
    instance: Option<&str>,
    pane: u32,
    revision: Option<u64>,
    deadline: Instant,
) -> Result<Cells> {
    let reply = super::request_until(
        socket,
        serde_json::json!({"command":"capture","id":2,"instance":instance,
        "pane":pane,"format":"cells","max_bytes":131072,"if_revision":revision}),
        deadline,
    )?;
    let capture = decode_reply(reply)?;
    if capture.unchanged && revision != Some(capture.revision) {
        return Err(super::error::malformed(anyhow::anyhow!(
            "unexpected unchanged capture"
        )));
    }
    Ok(capture)
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TextCapture {
    seq: u64,
    input_sequence: u64,
    text: String,
    revision: u64,
    rows: u16,
    columns: u16,
    scrollback_offset: u32,
    title: String,
    #[serde(deserialize_with = "Option::deserialize")]
    progress: Option<(u8, u8)>,
    unchanged: bool,
    truncated: bool,
}

/// Sample the input counter from the same generic terminal capture used for display.
pub(crate) fn input_sequence(
    socket: &Path,
    instance: &str,
    pane: u32,
    deadline: Instant,
) -> Result<u64> {
    let reply = super::request_until(
        socket,
        serde_json::json!({"command":"capture","id":2,
        "instance":instance,"pane":pane,"format":"text","max_bytes":1}),
        deadline,
    )?;
    decode_text(reply)
}

fn decode_text(reply: Value) -> Result<u64> {
    super::error::reply(|| {
        match serde_json::from_value(reply).context("invalid text capture envelope")? {
            Reply::Completed {
                id,
                result: Payload::Capture(value),
            } => {
                ensure!(id == 2, "capture response ID mismatch");
                ensure!(
                    (2..=512).contains(&value.rows) && (2..=512).contains(&value.columns),
                    "invalid capture dimension"
                );
                ensure!(!value.unchanged, "unexpected unchanged text capture");
                Ok(value.input_sequence)
            }
            Reply::Completed { .. } => anyhow::bail!("expected text capture"),
            Reply::Failed { id, error } => {
                ensure!(id == 2, "capture failure ID mismatch");
                Err(error.into())
            }
        }
    })
}

#[derive(serde::Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
enum Reply {
    Completed { id: u64, result: Payload },
    Failed { id: u64, error: RemoteFailure },
}
#[derive(serde::Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
enum Payload {
    Cells(WireCapture),
    Capture(TextCapture),
}
use super::error::RemoteFailure;

fn decode_reply(reply: Value) -> Result<Cells> {
    super::error::reply(|| {
        match serde_json::from_value(reply).context("invalid capture envelope")? {
            Reply::Completed {
                id,
                result: Payload::Cells(value),
            } => {
                ensure!(id == 2, "capture response ID mismatch");
                decode_cells(value)
            }
            Reply::Completed { .. } => anyhow::bail!("expected cells capture"),
            Reply::Failed { id, error } => {
                ensure!(id == 2, "capture failure ID mismatch");
                Err(error.into())
            }
        }
    })
}
#[cfg(test)]
pub(crate) fn fixture_cells(value: Value) -> Result<Cells> {
    decode_cells(serde_json::from_value(value)?)
}

#[cfg(test)]
fn decode_screen(value: &Value) -> Result<Captured> {
    super::error::reply(|| {
        let value: WireCapture = serde_json::from_value(value.clone())?;
        decode_cells(value)?
            .screen
            .context("capture evidence incomplete")
    })
}

fn screen(value: &WireCapture, lines: Vec<String>) -> Result<Captured> {
    ensure!(
        value.lines.len() == usize::from(value.rows),
        "capture rows do not match lines"
    );
    let progress = value.progress.and_then(|(state, percent)| {
        (state <= 4 && percent <= 100).then_some(Progress { state, percent })
    });
    Captured::from_lines(
        value.rows,
        value.columns,
        lines,
        value.title.chars().take(MAX_TITLE_CHARS).collect(),
        progress,
    )
}

/// Expands one wire row to its text; the cells must cover exactly `columns`.
fn expand_row(cells: &[WireCell], columns: u16) -> anyhow::Result<String> {
    let mut text = String::new();
    let mut covered = 0_u64;
    for cell in cells {
        let kind = cell.kind;
        let run = u64::from(cell.run);
        let _ = &cell.style;
        match cell.text.as_deref() {
            Some(glyph) => {
                anyhow::ensure!(run <= 1, "text cell with a run");
                anyhow::ensure!(
                    !glyph.is_empty() && !glyph.chars().any(char::is_control),
                    "invalid text cell"
                );
                text.push_str(glyph);
                covered = covered.saturating_add(1);
            }
            None => {
                let count = run.max(1);
                if kind != Some(CellKind::WideContinuation) {
                    anyhow::ensure!(count <= u64::from(columns), "run exceeds the row");
                    text.extend(std::iter::repeat_n(' ', usize::try_from(count)?));
                }
                covered = covered.saturating_add(count);
            }
        }
    }
    anyhow::ensure!(
        covered == u64::from(columns),
        "capture line does not cover its columns"
    );
    text.truncate(text.trim_end().len());
    Ok(text)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::rules::view::ScreenView;
    use serde_json::json;

    fn fixture() -> Value {
        let reply: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/capture/reply_completed_cells.json"
        ))
        .expect("fixture parses");
        reply
            .pointer("/result/value")
            .cloned()
            .expect("fixture has a value")
    }

    #[test]
    fn incomplete_evidence_still_requires_well_formed_metadata_and_rows() {
        for field in [
            "revision",
            "input_sequence",
            "seq",
            "rows",
            "columns",
            "cursor",
            "title",
            "progress",
            "lines",
        ] {
            let mut value = fixture();
            value["truncated"] = json!(true);
            value.as_object_mut().expect("capture object").remove(field);
            assert!(fixture_cells(value).is_err(), "accepted missing {field}");
        }
        for (path, invalid) in [
            ("/rows", json!(513)),
            ("/cursor/row", json!("zero")),
            ("/lines/0/row", json!(1)),
            ("/lines/0/wrapped", json!(null)),
            ("/lines/0/cells/0/kind", json!("unknown")),
            ("/lines/0/cells/0/run", json!(-1)),
        ] {
            let mut value = fixture();
            if path.ends_with("kind") || path.ends_with("run") {
                let key = path.rsplit('/').next().expect("field");
                value["lines"][0]["cells"][0][key] = invalid;
            } else {
                *value.pointer_mut(path).expect("field") = invalid;
            }
            assert!(fixture_cells(value).is_err(), "accepted {path}");
        }
        let mut extra = fixture();
        extra["extra"] = json!(true);
        assert!(fixture_cells(extra).is_err());
        let mut unchanged = fixture();
        unchanged["unchanged"] = json!(true);
        assert!(
            fixture_cells(unchanged.clone()).is_err(),
            "unchanged reply carried rows"
        );
        unchanged["lines"] = json!([]);
        assert!(fixture_cells(unchanged).is_ok());
    }

    #[test]
    fn capture_envelopes_require_the_exact_operation_and_request() {
        let valid =
            json!({"status":"completed","id":2,"result":{"kind":"cells","value":fixture()}});
        assert!(decode_reply(valid.clone()).is_ok());
        for (path, value) in [
            ("/id", json!(1)),
            ("/status", json!("accepted")),
            ("/result/kind", json!("listing")),
        ] {
            let mut invalid = valid.clone();
            *invalid.pointer_mut(path).expect("field") = value;
            assert!(decode_reply(invalid).is_err());
        }
        assert!(
            decode_reply(
                json!({"status":"failed","id":2,"error":{"code":"not-found","message":"gone"}})
            )
            .is_err()
        );
    }

    #[test]
    fn text_capture_counter_requires_complete_evidence() {
        let mut reply: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/capture/reply_completed_capture.json"
        ))
        .expect("producer fixture");
        // The shared producer fixture uses request 3; this client sends request 2.
        assert!(decode_text(reply.clone()).is_err());
        reply["id"] = json!(2);
        assert_eq!(decode_text(reply.clone()).expect("counter"), 3);
        for field in ["input_sequence", "revision", "text", "progress"] {
            let mut invalid = reply.clone();
            invalid["result"]["value"]
                .as_object_mut()
                .expect("payload")
                .remove(field);
            assert!(decode_text(invalid).is_err(), "accepted missing {field}");
        }
        reply["result"]["value"]["unchanged"] = json!(true);
        assert!(decode_text(reply).is_err());
    }

    #[test]
    fn fixture_reply_expands_runs_wide_glyphs_and_styled_cells() {
        let view = decode_screen(&fixture()).expect("valid capture");
        assert_eq!(view.size(), (3, 8));
        assert_eq!(view.title(), "sh");
        assert_eq!(view.progress(), None);
        // `{"run":1}` is one blank, the wide pair shows its glyph once, styles never reach the
        // text, and the trailing run and blank row are trimmed.
        assert_eq!(view.lines().collect::<Vec<_>>(), ["$ 日", "hi"]);
        assert_eq!(view.text(), "$ 日\nhi\n");
    }

    #[test]
    fn progress_and_title_come_from_the_capture() {
        let mut value = fixture();
        value["progress"] = json!([1, 50]);
        value["title"] = json!("x".repeat(300));
        let view = decode_screen(&value).expect("valid capture");
        assert_eq!(
            view.progress(),
            Some(Progress {
                state: 1,
                percent: 50
            })
        );
        assert_eq!(view.title().chars().count(), 256);
        value["progress"] = json!([5, 50]);
        assert_eq!(decode_screen(&value).expect("valid").progress(), None);
    }

    #[test]
    fn incomplete_or_malformed_captures_are_not_classified() {
        let mut truncated = fixture();
        truncated["truncated"] = json!(true);
        assert!(decode_screen(&truncated).is_err());
        let mut wide = fixture();
        wide["columns"] = json!(513);
        assert!(decode_screen(&wide).is_err());
        let mut short = fixture();
        short["lines"] = json!([{"row":0,"wrapped":false,"cells":[{"run":8}]}]);
        assert!(decode_screen(&short).is_err());
        let mut uncovered = fixture();
        uncovered["lines"][2] = json!({"row":2,"wrapped":false,"cells":[{"run":7}]});
        assert!(decode_screen(&uncovered).is_err());
        let mut overrun = fixture();
        overrun["lines"][2] =
            json!({"row":2,"wrapped":false,"cells":[{"text":"a","run":2},{"run":6}]});
        assert!(decode_screen(&overrun).is_err());
        assert!(decode_screen(&json!({"text":"blocked"})).is_err());
    }
}
