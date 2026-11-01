//! What a reply answers of panes, and of everything there is, as data the
//! client renders as text or as JSON (`json`), each from the one value.
use crate::code::Code;
use crate::command::Version;
use crate::id::{ClientId, PaneId};
use crate::json::Json;
use crate::wire::{closed_wire, wire, wire_enum};
use fux_vt::{Attributes, Blink, Color, Rgb, UnderlineStyle};
use fuxix::process::{Signal, Status};

wire! {
    /// What one pane answers, of an action or a wait on it.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum PaneAnswer {
        /// An action's (keys, a mouse event, a signal, a program started):
        /// its pane's version before it acted, or as it starts, which a
        /// wait for its effect waits to differ from.
        Acted(version: Version),
        /// A wait's, its pane changed: its version now, what changed, and
        /// how its program ended, if it ended since.
        Changed {
            version: Version,
            changed: Vec<Change>,
            ended: Option<Ended>,
        },
        /// A wait's, its pane closed.
        Closed(pane: PaneId),
        /// A wait's, nothing changed in time: the version it is still at.
        TimedOut { since: Version, limit: std::time::Duration },
    }
}

wire! {
    /// One pane's answer, or all the members that answered, in watch order.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum PaneResults {
        One(answer: PaneAnswer),
        Many(answers: Vec<PaneAnswer>),
    }
}

impl PaneResults {
    pub fn all(&self) -> &[PaneAnswer] {
        match self {
            Self::One(one) => std::slice::from_ref(one),
            Self::Many(all) => all,
        }
    }
}

/// What changed of a pane a wait answers: its program started again
/// (whose other counts say nothing of the last one's), or its screen, a
/// bell, a shell command's end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Respawned,
    Screen,
    Bell,
    Done,
}

impl Change {
    pub const ALL: [Change; 4] = [
        Change::Respawned,
        Change::Screen,
        Change::Bell,
        Change::Done,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Change::Respawned => "respawned",
            Change::Screen => "screen",
            Change::Bell => "bell",
            Change::Done => "done",
        }
    }
}

closed_wire!(Change, "change");
wire_enum!(Ended { Exited(code), Signalled(signal), Lost });
crate::wire::shaped!(enum Ended { Exited(code: i32), Signalled(signal: i32), Lost });

impl PaneAnswer {
    pub fn pane(&self) -> PaneId {
        match self {
            PaneAnswer::Acted(version)
            | PaneAnswer::Changed { version, .. }
            | PaneAnswer::TimedOut { since: version, .. } => version.pane,
            PaneAnswer::Closed(pane) => *pane,
        }
    }

    /// The version it holds, which a wait on it waits from: none once its
    /// pane closed.
    pub fn version(&self) -> Option<Version> {
        match self {
            PaneAnswer::Acted(version)
            | PaneAnswer::Changed { version, .. }
            | PaneAnswer::TimedOut { since: version, .. } => Some(*version),
            PaneAnswer::Closed(_) => None,
        }
    }

    /// The codes of the failures one may be (`failure`), which carry it
    /// to a `try`'s handler.
    pub const FAILURES: [Code; 2] = [Code::Closed, Code::Timeout];

    /// The code of the failure it is, if it is one: one of `FAILURES`.
    pub fn failure(&self) -> Option<Code> {
        match self {
            PaneAnswer::Acted(_) | PaneAnswer::Changed { .. } => None,
            PaneAnswer::Closed(_) => Some(Code::Closed),
            PaneAnswer::TimedOut { .. } => Some(Code::Timeout),
        }
    }

    /// Why this outcome failed, if it did.
    pub fn why(&self) -> Option<String> {
        Some(match self {
            PaneAnswer::Acted(_) | PaneAnswer::Changed { .. } => return None,
            PaneAnswer::Closed(id) => format!("{id}: the pane closed"),
            PaneAnswer::TimedOut { since, limit } => format!(
                "{}: unchanged for {}s, still at version {since}",
                since.pane,
                limit.as_secs_f64()
            ),
        })
    }

    /// A line that names the pane: `%2:0.57.1.3.0:RUN screen done exited
    /// 0`, `%2 closed`, `%2:0.57.1.3.0:RUN timeout`.
    pub fn text(&self) -> String {
        struct Text<'a>(&'a PaneAnswer);
        impl std::fmt::Display for Text<'_> {
            fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.write_text(out)
            }
        }
        Text(self).to_string()
    }

    pub fn write_text(&self, out: &mut dyn std::fmt::Write) -> std::fmt::Result {
        match self {
            PaneAnswer::Acted(version) => write!(out, "{version}"),
            PaneAnswer::Changed {
                version,
                changed,
                ended,
            } => {
                write!(out, "{version}")?;
                for change in changed {
                    write!(out, " {}", change.name())?;
                }
                if let Some(ended) = ended {
                    write!(out, " {}", Told(*ended))?;
                }
                Ok(())
            }
            PaneAnswer::Closed(pane) => write!(out, "{pane} closed"),
            PaneAnswer::TimedOut { since, .. } => write!(out, "{since} timeout"),
        }
    }

    /// As JSON, without allocating a second owned graph.
    pub fn write_json(&self, out: &mut dyn std::fmt::Write) -> std::fmt::Result {
        write!(out, "{{\"pane\":\"{}\"", self.pane())?;
        if let Some(version) = self.version() {
            write!(out, ",\"version\":\"{version}\"")?;
        }
        if let PaneAnswer::Changed { changed, ended, .. } = self {
            out.write_str(",\"changed\":[")?;
            for (i, change) in changed.iter().enumerate() {
                if i > 0 {
                    out.write_char(',')?;
                }
                crate::json::write_str(out, change.name())?;
            }
            out.write_char(']')?;
            match ended {
                Some(Ended::Exited(code)) => write!(out, ",\"exited\":{code},\"signal\":null")?,
                Some(Ended::Signalled(signal)) => {
                    write!(out, ",\"exited\":null,\"signal\":{signal}")?
                }
                Some(Ended::Lost) => out.write_str(",\"exited\":null,\"signal\":null")?,
                None => {}
            }
        }
        write!(
            out,
            ",\"status\":{}}}",
            self.failure().map_or(0, |code| code.status().get())
        )
    }
}
/// How a pane's program ended: as the system says, or with its status
/// lost (its leader no unreaped child whose status can be read), which is
/// no exit code and no signal, and is said as unknown.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ended {
    Exited(i32),
    Signalled(i32),
    Lost,
}

impl Ended {
    /// How a wait or an event says it: `exited`, its code, `null` if it
    /// was lost, or `signal`, the signal that killed it.
    pub fn field(self) -> (&'static str, crate::json::Json) {
        use crate::json::Json;
        match self {
            Ended::Exited(code) => ("exited", Json::int(code)),
            Ended::Signalled(signal) => ("signal", Json::int(signal)),
            Ended::Lost => ("exited", Json::Null),
        }
    }
}

impl From<Status> for Ended {
    fn from(status: Status) -> Self {
        match status {
            Status::Exited(code) => Ended::Exited(code),
            Status::Signalled(signal) => Ended::Signalled(signal),
        }
    }
}

/// The status a shell reports for a process: its exit code, or 128 plus the
/// signal that killed it, whose numbers are small; none for a status lost.
pub fn shell_status(ended: Ended) -> Option<i32> {
    match ended {
        Ended::Exited(code) => Some(code),
        Ended::Signalled(signal) => Some(128i32.saturating_add(signal)),
        Ended::Lost => None,
    }
}

/// How an end is told: `exited 3`, `killed by SIGSEGV` (`killed by
/// signal 64` for one POSIX does not name), or `ended, its status lost`.
pub fn told(ended: Ended) -> String {
    Told(ended).to_string()
}

pub(crate) struct Told(pub Ended);
impl std::fmt::Display for Told {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Ended::Lost => out.write_str("ended, its status lost"),
            Ended::Exited(code) => write!(out, "exited {code}"),
            Ended::Signalled(raw) => match Signal::from_raw(raw) {
                Some(signal) => write!(out, "killed by SIG{}", signal.name()),
                None => write!(out, "killed by signal {raw}"),
            },
        }
    }
}

wire! {
    /// A capture, read once: what it read, its screen's cursor (its row
    /// and column, if it shows on the screen read), its lines,
    /// trimmed as its view asked, and the styles their runs name; and
    /// whether it was read with styles (its text shows them as SGR, its
    /// JSON lists them) and marks. Read `since` a version, its lines are
    /// those with a row changed since, and `changed_rows` holds the row
    /// each begins on.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Captured {
        pub of: Of,
        pub screen_cursor: Option<(u16, u16)>,
        pub lines: Vec<Line>,
        /// The styles runs name, in the order first used.
        pub styles: Vec<Attributes>,
        pub styled: bool,
        pub marks: bool,
        pub changed_rows: Option<Vec<u32>>,
    }
}

wire! {
    /// What a capture read: a pane, its observation read with it, what
    /// the capture read beside its lines (`read`: the journal's `cursor`
    /// first) and its pending wrap; or a client's terminal, of its size.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Of {
        Pane {
            pane: Box<crate::inspect::Pane>,
            read: Vec<(String, Json)>,
            /// A glyph went into the last column: the next one goes on the
            /// next line, though the cursor shows on this one's last cell.
            wrap_pending: bool,
        },
        Client {
            client: ClientId,
            rows: u16,
            cols: u16,
        },
    }
}

wire! {
    /// A line's text, and a run for each change of style along it. Text
    /// past its runs is in the default style: a plain capture's lines
    /// have none.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Line {
        pub text: String,
        pub runs: Vec<Run>,
    }
}

wire! {
    /// A run of a line's cells of one style: the bytes of its text and
    /// the cells they fill, and its style's place in the capture's
    /// `styles`.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Run {
        pub bytes: u32,
        pub cells: u32,
        pub style: u32,
    }
}

wire_enum!(Color { Default, Idx(n), Rgb(rgb) });
wire_enum!(Blink { None, Slow, Rapid });

crate::wire::shaped!(enum Color { Default, Idx(n: u8), Rgb(rgb: fux_vt::Rgb) });
crate::wire::shaped!(struct fux_vt::Rgb { r: u8, g: u8, b: u8 });
crate::wire::shaped!(
    enum Blink {
        None,
        Slow,
        Rapid,
    }
);

/// As its `Wire` writes it; its fields are fux-vt's own.
#[cfg(test)]
impl crate::wire::Reach for Attributes {
    fn reach(out: &mut crate::wire::Shapes) {
        let flags = ["bold", "dim", "italic", "inverse", "hidden", "strikeout"];
        let mut fields = vec![
            ("foreground", "Color"),
            ("background", "Color"),
            ("underline_color", "Color"),
        ];
        fields.extend(flags.map(|flag| (flag, "bool")));
        fields.extend([("underline_style", "u16"), ("blink", "Blink")]);
        if out.first::<Self>("Attributes", &[("Attributes", &fields)]) {
            Color::reach(out);
            Blink::reach(out);
        }
    }
}

/// The style's colours, its flags and its underline's style and blink, as
/// the vocabulary of `Attributes` gives and takes them.
impl crate::wire::Wire for Attributes {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        let a = *self;
        [a.foreground(), a.background(), a.underline_color()]
            .iter()
            .for_each(|c| c.put(out));
        [
            a.bold(),
            a.dim(),
            a.italic(),
            a.inverse(),
            a.hidden(),
            a.strikeout(),
        ]
        .iter()
        .for_each(|f| f.put(out));
        a.underline_style().number().put(out);
        a.blink().put(out);
    }
    fn get(r: &mut crate::wire::Reader<'_>) -> Result<Self, crate::wire::Error> {
        let [fg, bg, under] = [Color::get(r)?, Color::get(r)?, Color::get(r)?];
        let mut flag = || bool::get(r);
        let a = Attributes::new(fg, bg)
            .with_underline_color(under)
            .with_bold(flag()?)
            .with_dim(flag()?)
            .with_italic(flag()?)
            .with_inverse(flag()?)
            .with_hidden(flag()?)
            .with_strikeout(flag()?);
        let style = UnderlineStyle::from_number(u16::get(r)?);
        let style = style.ok_or(crate::wire::Error::Bad("underline style"))?;
        Ok(a.with_underline_style(style).with_blink(Blink::get(r)?))
    }
}

/// The SGR that sets `a` from nothing, as painted (`SgrForm::Paint`), with
/// underline styles if the client's terminal draws them (`styles`,
/// `outer::styles!`).
pub fn sgr(out: &mut Vec<u8>, a: Attributes, styles: bool) {
    out.extend_from_slice(b"\x1b[0");
    a.write_sgr(out, fux_vt::SgrForm::Paint { styles });
    out.push(b'm');
}

/// Highlighted: reverse video, or a background colour.
pub fn highlighted(a: &Attributes) -> bool {
    a.inverse() || a.background() != Color::Default
}

/// Marks closed and opened to go from text marked `on` to text marked
/// `to`, each highlighted and faint: `⟦` and `⟧` around the highlighted,
/// `⟨` and `⟩` around the faint, inside any highlighting.
fn remark(
    out: &mut dyn std::fmt::Write,
    (lit, faint): (bool, bool),
    to: (bool, bool),
) -> Result<(bool, bool), std::fmt::Error> {
    if faint && (!to.1 || lit != to.0) {
        out.write_str("⟩")?;
    }
    if lit && !to.0 {
        out.write_str("⟧")?;
    }
    if to.0 && !lit {
        out.write_str("⟦")?;
    }
    if to.1 && (!faint || lit != to.0) {
        out.write_str("⟨")?;
    }
    Ok(to)
}

impl Captured {
    /// The default-history styled capture, as the tests measure it: 9931
    /// lines of 100 cells, each cell in a style of its own.
    #[cfg(any(test, feature = "testing"))]
    pub fn default_styled(version: Version) -> Captured {
        let line = Line {
            text: std::iter::repeat_n('x', 100).collect(),
            runs: (0..100)
                .map(|style| Run {
                    bytes: 1,
                    cells: 1,
                    style,
                })
                .collect(),
        };
        Captured {
            of: Of::Pane {
                pane: Box::new(crate::inspect::Pane {
                    id: version.pane,
                    tab: crate::id::TabId::of(1),
                    version,
                    name: None,
                    label: String::new(),
                    title: String::new(),
                    rows: 40,
                    cols: 120,
                    row: None,
                    col: None,
                    facts: None,
                    cwd: None,
                }),
                read: Vec::new(),
                wrap_pending: false,
            },
            screen_cursor: None,
            lines: std::iter::repeat_n(line, 9931).collect(),
            styles: (0u8..100)
                .map(|n| Attributes::new(Color::Idx(n), Color::Default))
                .collect(),
            styled: true,
            marks: false,
            changed_rows: None,
        }
    }

    pub fn text_scratch(&self) -> usize {
        if self.styled { 128 } else { 0 }
    }

    pub fn write_text(&self, out: &mut dyn std::fmt::Write) -> std::fmt::Result {
        // All flags and three RGB colours fit in the declared scratch space.
        let mut sgr = Vec::new();
        sgr.try_reserve_exact(self.text_scratch())
            .map_err(|_| std::fmt::Error)?;
        let mut rows = self.changed_rows.iter().flatten();
        for (i, line) in self.lines.iter().enumerate() {
            if i > 0 {
                out.write_char('\n')?;
            }
            // Read `since`, each line after its row, as `grep -n` writes it.
            if let Some(row) = rows.next() {
                write!(out, "{row}:")?;
            }
            self.write_line(out, line, self.styled, &mut sgr)?;
        }
        Ok(())
    }

    /// JSON directly from the capture: no second graph of owned run objects.
    pub fn write_json(&self, out: &mut dyn std::fmt::Write) -> std::fmt::Result {
        match &self.of {
            Of::Pane {
                pane,
                read,
                wrap_pending,
            } => {
                let (id, tab, version) = (pane.id, pane.tab, pane.version);
                write!(
                    out,
                    "{{\"id\":\"{id}\",\"tab\":\"{tab}\",\"version\":\"{version}\""
                )?;
                let mut field = |key: &str, value: &Json| {
                    out.write_char(',')?;
                    crate::json::write_str(out, key)?;
                    out.write_char(':')?;
                    value.write(out)
                };
                for (key, value) in pane.fields() {
                    field(key, &value)?;
                }
                for (key, value) in read {
                    field(key, value)?;
                }
                write!(out, ",\"wrap_pending\":{wrap_pending}")?;
            }
            Of::Client { client, rows, cols } => write!(
                out,
                "{{\"client\":\"{client}\",\"screen_rows\":{rows},\"screen_cols\":{cols}"
            )?,
        }
        out.write_str(",\"screen_cursor\":")?;
        match self.screen_cursor {
            Some((y, x)) => write!(out, "[{y},{x}]")?,
            None => out.write_str("null")?,
        }
        out.write_str(",\"lines\":[")?;
        struct Escaped<'a>(&'a mut dyn std::fmt::Write);
        impl std::fmt::Write for Escaped<'_> {
            fn write_str(&mut self, text: &str) -> std::fmt::Result {
                crate::json::escaped(self.0, text)
            }
        }
        for (i, line) in self.lines.iter().enumerate() {
            if i > 0 {
                out.write_char(',')?;
            }
            out.write_char('"')?;
            self.write_line(&mut Escaped(out), line, false, &mut Vec::new())?;
            out.write_char('"')?;
        }
        out.write_char(']')?;
        if let Some(rows) = &self.changed_rows {
            out.write_str(",\"changed_rows\":[")?;
            for (i, row) in rows.iter().enumerate() {
                if i > 0 {
                    out.write_char(',')?;
                }
                write!(out, "{row}")?;
            }
            out.write_char(']')?;
        }
        if self.styled {
            out.write_str(",\"styles\":[")?;
            for (i, line) in self.lines.iter().enumerate() {
                if i > 0 {
                    out.write_char(',')?;
                }
                out.write_char('[')?;
                let mut start = 0u32;
                let mut first = true;
                for run in self.runs(line) {
                    let (_, cells, style) = run.ok_or(std::fmt::Error)?;
                    if style != Attributes::default() {
                        if !first {
                            out.write_char(',')?;
                        }
                        first = false;
                        write!(out, "{{\"col\":{start},\"len\":{cells}")?;
                        describe(out, style)?;
                        out.write_char('}')?;
                    }
                    start = start.saturating_add(cells);
                }
                out.write_char(']')?;
            }
            out.write_char(']')?;
        }
        out.write_char('}')
    }

    /// A line's runs, each its text, cells and style: `None` for one past
    /// the line's text or the capture's styles.
    pub fn runs<'a>(
        &'a self,
        line: &'a Line,
    ) -> impl Iterator<Item = Option<(&'a str, u32, Attributes)>> + 'a {
        let mut rest = line.text.as_str();
        line.runs.iter().map(move |run| {
            let (text, after) = rest.split_at_checked(usize::try_from(run.bytes).ok()?)?;
            rest = after;
            let style = self.styles.get(usize::try_from(run.style).ok()?)?;
            Some((text, run.cells, *style))
        })
    }

    /// Write a line without allocating its plain or escaped intermediate
    /// text; with `sgr`, its styles as SGR, and its marks in no style.
    pub fn write_line(
        &self,
        out: &mut dyn std::fmt::Write,
        line: &Line,
        sgr: bool,
        scratch: &mut Vec<u8>,
    ) -> std::fmt::Result {
        let mut pen = Attributes::default();
        let mut on = (false, false);
        let mut taken = 0usize;
        for run in self.runs(line) {
            let (text, _, style) = run.ok_or(std::fmt::Error)?;
            taken = taken.saturating_add(text.len());
            let marked = (self.marks && highlighted(&style), self.marks && style.dim());
            if marked != on {
                if pen != Attributes::default() {
                    out.write_str("\x1b[0m")?;
                    pen = Attributes::default();
                }
                on = remark(out, on, marked)?;
            }
            if sgr && style != pen {
                scratch.clear();
                self::sgr(scratch, style, true);
                out.write_str(std::str::from_utf8(scratch).map_err(|_| std::fmt::Error)?)?;
                pen = style;
            }
            out.write_str(text)?;
        }
        if pen != Attributes::default() {
            out.write_str("\x1b[0m")?;
        }
        remark(out, on, (false, false))?;
        out.write_str(line.text.get(taken..).ok_or(std::fmt::Error)?)
    }
}

/// The shared vocabulary for styled capture runs, streamed or projected locally.
pub enum StyleValue {
    Index(u8),
    Rgb(Rgb),
    Flag,
}
pub fn style_fields(a: Attributes) -> impl Iterator<Item = (&'static str, StyleValue)> {
    let colors = [("fg", a.foreground()), ("bg", a.background())]
        .into_iter()
        .filter_map(|(key, color)| match color {
            Color::Default => None,
            Color::Idx(n) => Some((key, StyleValue::Index(n))),
            Color::Rgb(rgb) => Some((key, StyleValue::Rgb(rgb))),
        });
    let flags = a
        .sgr_flags()
        .map(|(_, name)| name)
        .chain(a.underline().then_some("underline"))
        .chain((a.blink() != fux_vt::Blink::None).then_some("blink"))
        .map(|name| (name, StyleValue::Flag));
    colors.chain(flags)
}

fn describe(out: &mut dyn std::fmt::Write, a: Attributes) -> std::fmt::Result {
    for (name, value) in style_fields(a) {
        match value {
            StyleValue::Index(n) => write!(out, ",\"{name}\":{n}")?,
            StyleValue::Rgb(Rgb { r, g, b }) => {
                write!(out, ",\"{name}\":\"#{r:02x}{g:02x}{b:02x}\"")?
            }
            StyleValue::Flag => write!(out, ",\"{name}\":true")?,
        }
    }
    Ok(())
}
