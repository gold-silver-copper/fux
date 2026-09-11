use serde_json::Value;
use std::borrow::Cow;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Progress {
    pub state: u8,
    pub percent: u8,
}

pub trait ScreenView {
    fn lines(&self) -> impl Iterator<Item = Cow<'_, str>>;
    fn text(&self) -> &str;
    fn title(&self) -> &str;
    fn progress(&self) -> Option<Progress>;
    fn size(&self) -> (u16, u16);
}

const MAX_TITLE_CHARS: usize = 256;

/// One complete `format:"cells"` capture from fux, expanded into the rows the rules read.
///
/// fux has already emulated the pane; this only expands its run-length wire cells (`{"text":..}`
/// is one text cell, `{}` one blank, `{"run":N}` N equal blanks, `wide-continuation` the second
/// half of a wide glyph) into right-trimmed rows, dropping trailing blank rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Captured {
    rows: u16,
    columns: u16,
    lines: Vec<String>,
    text: String,
    title: String,
    progress: Option<Progress>,
}

impl Captured {
    /// Interpret one coherent, complete cells capture, never metadata from an earlier listing.
    pub fn from_capture(value: &Value) -> anyhow::Result<Self> {
        let dimension = |name| -> anyhow::Result<u16> {
            let n = value
                .get(name)
                .and_then(Value::as_u64)
                .ok_or_else(|| anyhow::anyhow!("capture is missing {name}"))?;
            anyhow::ensure!((2..=512).contains(&n), "invalid capture dimension");
            Ok(u16::try_from(n)?)
        };
        anyhow::ensure!(
            value.get("truncated").and_then(Value::as_bool) == Some(false),
            "cannot classify a truncated capture"
        );
        let rows = dimension("rows")?;
        let columns = dimension("columns")?;
        let wire = value
            .get("lines")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("capture is missing lines"))?;
        anyhow::ensure!(
            wire.len() == usize::from(rows),
            "capture rows do not match lines"
        );
        let mut lines = vec![String::new(); usize::from(rows)];
        for (index, line) in wire.iter().enumerate() {
            let row = line
                .get("row")
                .and_then(Value::as_u64)
                .ok_or_else(|| anyhow::anyhow!("capture line is missing row"))?;
            anyhow::ensure!(
                usize::try_from(row)? == index,
                "capture lines are out of order"
            );
            let cells = line
                .get("cells")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow::anyhow!("capture line is missing cells"))?;
            let text = expand_row(cells, columns)?;
            if let Some(slot) = lines.get_mut(index) {
                *slot = text;
            }
        }
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        let mut text = lines.join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        let title = value
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .chars()
            .take(MAX_TITLE_CHARS)
            .collect();
        let progress = value
            .get("progress")
            .and_then(Value::as_array)
            .and_then(|values| {
                let state = u8::try_from(values.first()?.as_u64()?).ok()?;
                let percent = u8::try_from(values.get(1)?.as_u64()?).ok()?;
                (state <= 4 && percent <= 100).then_some(Progress { state, percent })
            });
        Ok(Self {
            rows,
            columns,
            lines,
            text,
            title,
            progress,
        })
    }
}

/// Expands one wire row to its text; the cells must cover exactly `columns`.
fn expand_row(cells: &[Value], columns: u16) -> anyhow::Result<String> {
    let mut text = String::new();
    let mut covered = 0_u64;
    for cell in cells {
        let kind = cell.get("kind").and_then(Value::as_str);
        let run = cell.get("run").and_then(Value::as_u64).unwrap_or(0);
        match cell.get("text").and_then(Value::as_str) {
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
                if kind != Some("wide-continuation") {
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

impl ScreenView for Captured {
    fn lines(&self) -> impl Iterator<Item = Cow<'_, str>> {
        self.lines.iter().map(|line| Cow::Borrowed(line.as_str()))
    }
    fn text(&self) -> &str {
        &self.text
    }
    fn title(&self) -> &str {
        &self.title
    }
    fn progress(&self) -> Option<Progress> {
        self.progress
    }
    fn size(&self) -> (u16, u16) {
        (self.rows, self.columns)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
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
    fn fixture_reply_expands_runs_wide_glyphs_and_styled_cells() {
        let view = Captured::from_capture(&fixture()).expect("valid capture");
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
        let view = Captured::from_capture(&value).expect("valid capture");
        assert_eq!(
            view.progress(),
            Some(Progress {
                state: 1,
                percent: 50
            })
        );
        assert_eq!(view.title().chars().count(), 256);
        value["progress"] = json!([5, 50]);
        assert_eq!(
            Captured::from_capture(&value).expect("valid").progress(),
            None
        );
    }

    #[test]
    fn incomplete_or_malformed_captures_are_not_classified() {
        let mut truncated = fixture();
        truncated["truncated"] = json!(true);
        assert!(Captured::from_capture(&truncated).is_err());
        let mut wide = fixture();
        wide["columns"] = json!(513);
        assert!(Captured::from_capture(&wide).is_err());
        let mut short = fixture();
        short["lines"] = json!([{"row":0,"wrapped":false,"cells":[{"run":8}]}]);
        assert!(Captured::from_capture(&short).is_err());
        let mut uncovered = fixture();
        uncovered["lines"][2] = json!({"row":2,"wrapped":false,"cells":[{"run":7}]});
        assert!(Captured::from_capture(&uncovered).is_err());
        let mut overrun = fixture();
        overrun["lines"][2] =
            json!({"row":2,"wrapped":false,"cells":[{"text":"a","run":2},{"run":6}]});
        assert!(Captured::from_capture(&overrun).is_err());
        assert!(Captured::from_capture(&json!({"text":"blocked"})).is_err());
    }
}
