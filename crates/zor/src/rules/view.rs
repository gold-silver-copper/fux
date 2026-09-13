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

/// Normalized screen rows and metadata consumed by rules, independent of transport encoding.
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
    /// Construct rule input from normalized rows; no transport schema is interpreted here.
    pub fn from_lines(
        rows: u16,
        columns: u16,
        mut lines: Vec<String>,
        title: String,
        progress: Option<Progress>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            (2..=512).contains(&rows) && (2..=512).contains(&columns),
            "invalid screen dimension"
        );
        anyhow::ensure!(
            lines.len() == usize::from(rows),
            "screen rows do not match lines"
        );
        for line in &mut lines {
            line.truncate(line.trim_end().len());
        }
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        let mut text = lines.join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        Ok(Self {
            rows,
            columns,
            lines,
            text,
            title: title.chars().take(MAX_TITLE_CHARS).collect(),
            progress,
        })
    }
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
