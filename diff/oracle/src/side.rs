//! The two sides: the working tree's fux-vt (`work`) and the base commit's
//! (`base`), each read into the model through the same public API.
//!
//! Both are one macro, `side!`, so they are read the same way. The base is
//! the newest `master` on `origin` (or a commit `diff/oracle.sh` is given),
//! so both sides share one API and the macro takes no per-side code. A
//! branch that changes fux-vt's public API can be compared with a temporary
//! local shim here for the base side; never commit one. Its matches over
//! fux-vt's enums are exhaustive, so a variant fux-vt adds makes the side
//! that has it fail to build, not compare wrongly.

/// One side: `side!(work, fux_vt, "work")` expands to `pub mod work`,
/// which reads the crate `fux_vt` into the model (`model::State`, its
/// cells, rows, events and errors) through fux-vt's public API, with
/// `NAME` `"work"` naming the side in a report. With `checked` after the
/// name, `side!(work, fux_vt, "work", checked)`, its `Side::faults` are
/// what `Parser::check` finds: the side's fux-vt has the checker.
macro_rules! side {
    (@faults checked, $terminal:expr) => {{
        let report = $terminal.parser.check();
        (!report.is_clean()).then(|| report.to_string())
    }};
    (@faults, $terminal:expr) => {{
        let _ = $terminal;
        None
    }};
    (
        $module:ident,
        $vt:ident,
        $name:literal $(, $checked:ident)? $(,)?
    ) => {
        pub mod $module {
            use crate::model::{
                self, Blink, Call, Cell, Color, Error, Heard, Lookup, Marked, Row, Setup, State,
                Stop, Style, Underline,
            };
            use $vt as vt;

            /// The row `offset` up from the bottom.
            fn back(s: &vt::Screen, offset: usize) -> Option<vt::Row<'_>> {
                s.rows().nth_back(offset)
            }

            /// A parser, and the marks taken of its screen.
            #[derive(Clone)]
            pub struct Terminal {
                parser: vt::Parser,
                marks: Vec<vt::Mark>,
                /// The oldest row seen at the last lookup, to ask for again
                /// once it may be gone.
                oldest: Option<vt::RowId>,
            }

            fn color(c: vt::Color) -> Color {
                match c {
                    vt::Color::Default => Color::Default,
                    vt::Color::Idx(n) => Color::Idx(n),
                    vt::Color::Rgb(vt::Rgb { r, g, b }) => Color::Rgb(r, g, b),
                }
            }

            fn underline(u: vt::UnderlineStyle) -> Underline {
                match u {
                    vt::UnderlineStyle::None => Underline::None,
                    vt::UnderlineStyle::Single => Underline::Single,
                    vt::UnderlineStyle::Double => Underline::Double,
                    vt::UnderlineStyle::Curly => Underline::Curly,
                    vt::UnderlineStyle::Dotted => Underline::Dotted,
                    vt::UnderlineStyle::Dashed => Underline::Dashed,
                }
            }

            fn blink(b: vt::Blink) -> Blink {
                match b {
                    vt::Blink::None => Blink::None,
                    vt::Blink::Slow => Blink::Slow,
                    vt::Blink::Rapid => Blink::Rapid,
                }
            }

            fn style(a: vt::Attributes) -> Style {
                Style {
                    fg: color(a.foreground()),
                    bg: color(a.background()),
                    underline_color: color(a.underline_color()),
                    bold: a.bold(),
                    dim: a.dim(),
                    italic: a.italic(),
                    underline: a.underline(),
                    underline_style: underline(a.underline_style()),
                    inverse: a.inverse(),
                    blink: blink(a.blink()),
                    hidden: a.hidden(),
                    strikeout: a.strikeout(),
                }
            }

            fn error(e: vt::Error) -> Error {
                Error(format!("{e:?}: {e}"))
            }

            /// The screen's rows and columns (the working tree's `Size`).
            fn size(s: &vt::Screen) -> (u16, u16) {
                s.size().into()
            }

            /// Every mode, by name: all but DECHCCM, which is permanent and
            /// which the DECRQM probe compares. The match names each, so a
            /// mode fux-vt adds fails to build until it is read here too.
            fn modes(s: &vt::Screen) -> Vec<(String, bool)> {
                crate::side::every_mode!(vt::Mode, HorizontalCursorCoupling;
                    Insert, NewLine, ApplicationCursor, SmoothScroll, ReverseVideo, Origin,
                    Autowrap, AutoRepeat, MousePress, CursorBlink, ShowCursor, MoreFix,
                    ReverseWrap, AlternateScreen, ApplicationKeypad, BackarrowSendsBackspace,
                    LeftRightMargins, SixelDisplay, MousePressRelease, MouseHighlight,
                    MouseButtonMotion, MouseAnyMotion, FocusReporting, MouseUtf8, MouseSgr,
                    AlternateScroll, MouseUrxvt, MouseSgrPixels, ExtendedReverseWrap,
                    AlternateScreenCleared, SaveCursor, AlternateScreenSaveCursor,
                    BracketedPaste, SynchronizedOutput, ColorSchemeUpdates, InBandResize,
                    GraphemeClustering)
                .into_iter()
                .map(|m| (format!("{m:?}"), s.mode(m)))
                .collect()
            }

            /// FNV-1a over `bytes`, folded into `h`.
            fn fold(h: u64, bytes: &[u8]) -> u64 {
                (bytes.iter()).fold(h, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
            }

            /// A row's cells, hashed: each one's text, halves and style, the
            /// style as the model reads it, so the two sides hash alike.
            fn cells_hash(row: vt::Row<'_>) -> u64 {
                let mut h = crate::side::Fnv(0xcbf2_9ce4_8422_2325);
                for c in row.cells() {
                    std::hash::Hash::hash(&(c.contents(), c.is_wide(), c.is_wide_continuation()), &mut h);
                    std::hash::Hash::hash(&style(c.attributes()), &mut h);
                }
                std::hash::Hasher::finish(&h)
            }

            /// A picture's pixels, hashed at an 8 by 8 grid of them (all of
            /// them is millions, every step), each read as `sixel::encode`
            /// gives it alone.
            fn pixels_hash(p: &vt::sixel::Picture) -> u64 {
                let (w, h) = (p.width(), p.height());
                let mut hash = 0xcbf2_9ce4_8422_2325;
                let mut pixel = Vec::new();
                for y in 0..8u32 {
                    for x in 0..8u32 {
                        let at = (x.saturating_mul(w) / 8, y.saturating_mul(h) / 8);
                        pixel.clear();
                        vt::sixel::encode(p, (at.0, at.1, 1, 1), &mut pixel);
                        hash = fold(hash, &pixel);
                    }
                }
                hash
            }

            fn rgb(c: Option<vt::Rgb>) -> Option<(u8, u8, u8)> {
                c.map(|c| (c.r, c.g, c.b))
            }

            /// The number in a `RowId` or `Mark`, which shows it only in
            /// its `Debug` form; `u64::MAX` if it has none.
            fn number(value: impl std::fmt::Debug) -> u64 {
                let text = format!("{value:?}");
                let digits: String = text.chars().filter(char::is_ascii_digit).collect();
                digits.parse().unwrap_or(u64::MAX)
            }

            fn options(setup: &Setup) -> vt::Options {
                let mut flags = *setup;
                // `Setup::flags` is in `Feature::ALL`'s order.
                let on = vt::Feature::ALL.into_iter().zip(flags.flags());
                let options: vt::Options = on.filter_map(|(f, (_, on))| on.then_some(f)).collect();
                options.with_identity(
                    setup
                        .identity
                        .map(|(name, version)| vt::Identity { name, version }),
                )
            }

            /// Everything the parser gives the host, in order.
            struct Sink<'a>(&'a mut Vec<Heard>);

            impl vt::Sink for Sink<'_> {
                fn reply(&mut self, bytes: &[u8]) {
                    self.0.push(Heard::Reply(bytes.to_vec()));
                }
                fn event(&mut self, event: vt::Event<'_>) {
                    self.0.push(match event {
                        vt::Event::Title(t) => Heard::Title(t.bytes.to_vec()),
                        vt::Event::IconName(t) => Heard::IconName(t.bytes.to_vec()),
                        vt::Event::Bell => Heard::Bell,
                        vt::Event::Notification(n) => Heard::Notification(n.bytes.to_vec()),
                        vt::Event::Clipboard { selection, data } => Heard::Clipboard {
                            selection: selection.to_vec(),
                            data: data.bytes.to_vec(),
                        },
                        vt::Event::ColorQuery { number, bel } => Heard::ColorQuery { number, bel },
                        vt::Event::CommandStarted => Heard::CommandStarted,
                        vt::Event::CommandEnded(status) => Heard::CommandEnded(status),
                    });
                }
                fn unhandled(&mut self, sequence: vt::Unhandled<'_>) {
                    self.0.push(match sequence {
                        vt::Unhandled::Csi {
                            params,
                            intermediates,
                            action,
                        } => Heard::Csi {
                            params: params.groups().map(<[u16]>::to_vec).collect(),
                            intermediates: intermediates.to_vec(),
                            action,
                        },
                        vt::Unhandled::Escape {
                            intermediates,
                            action,
                        } => Heard::Escape {
                            intermediates: intermediates.to_vec(),
                            action,
                        },
                    });
                }
            }

            impl crate::Side for Terminal {
                const NAME: &'static str = $name;

                fn faults(&self) -> Option<String> {
                    side!(@faults $($checked)?, self)
                }

                fn new(rows: u16, cols: u16, history_bytes: usize, setup: &Setup) -> Result<Self, Error> {
                    // A size of none is no `Size`: refused as such.
                    let size = vt::Size::new(rows, cols).map_err(|_| crate::side::zero_size())?;
                    let parser =
                        vt::Parser::with_options(size, history_bytes, options(setup)).map_err(error)?;
                    Ok(Terminal {
                        parser,
                        marks: Vec::new(),
                        oldest: None,
                    })
                }

                fn process(&mut self, bytes: &[u8], heard: &mut Vec<Heard>) -> Result<(), Error> {
                    let refused = self.parser.refused();
                    let mut rest = bytes;
                    while let Some((taken, _)) =
                        self.parser.process_until_stop(rest, &mut Sink(heard)).map_err(error)?
                    {
                        rest = rest.get(taken..).unwrap_or_default();
                    }
                    match self.parser.refused() == refused {
                        true => Ok(()),
                        false => Err(error(vt::Error::Capacity)),
                    }
                }

                fn process_until_stop(
                    &mut self,
                    bytes: &[u8],
                    heard: &mut Vec<Heard>,
                ) -> Result<Option<(usize, Stop)>, Error> {
                    let stop = self.parser.process_until_stop(bytes, &mut Sink(heard));
                    let why = |stop| match stop {
                        vt::Stop::Begun => Stop::Begun,
                        vt::Stop::Ended => Stop::Ended,
                        vt::Stop::Budget => Stop::Budget,
                    };
                    stop.map(|s| s.map(|(taken, stop)| (taken, why(stop))))
                        .map_err(error)
                }

                fn resize(&mut self, rows: u16, cols: u16) -> Result<(), Error> {
                    let size = vt::Size::new(rows, cols).map_err(|_| crate::side::zero_size())?;
                    self.parser.resize(size).map_err(error)
                }

                fn host(&mut self, call: Call) {
                    let p = &mut self.parser;
                    match call {
                        Call::HistoryBytes(n) => p.set_history_bytes(n),
                        Call::ClearHistory => p.clear_history(),
                        Call::Palette(n) => {
                            let mut palette = [None; 16];
                            if let Some(entry) = palette.get_mut(usize::from(n % 16)) {
                                *entry = Some(vt::Rgb { r: n, g: !n, b: n / 2 });
                            }
                            p.set_host_palette(palette);
                        }
                        Call::Graphics(w, h) => p.set_host_graphics(vt::CellSize::new(w, h).map_or(vt::Graphics::Text, vt::Graphics::Sixel)),
                    }
                }

                fn state(&self) -> State {
                    let s = self.parser.screen();
                    let (rows, cols) = size(s);
                    let retained = s.history_len().saturating_add(usize::from(rows));
                    State {
                        size: (rows, cols),
                        cursor: s.cursor_position(),
                        pending_wrap: s.pending_wrap(),
                        modes: modes(s),
                        cursor_shape: s.cursor_shape().number(),
                        mouse: format!("{:?}", s.mouse_protocol_mode()),
                        encoding: format!("{:?}", s.mouse_protocol_encoding()),
                        kitty_keyboard_flags: s.kitty_keyboard_flags(),
                        key_mode: format!("{:?}", s.key_mode()),
                        history_len: s.history_len(),
                        mark: number(s.mark()),
                        resize_report: self.parser.resize_report(),
                        colors_changed: s.colors_changed(),
                        rows_end_there: retained
                            .checked_sub(1)
                            .is_none_or(|last| back(s, last).is_some())
                            && back(s, retained).is_none(),
                        window: (s.window().rows(), s.window().cols()),
                        palette: (0..=u8::MAX).map(|i| rgb(s.palette_color(i))).collect(),
                        dynamic: (10..=19).map(|n| rgb(s.dynamic_color(n))).collect(),
                        pictures: (s.pictures())
                            .map(|p| {
                                let size = (p.picture.width(), p.picture.height());
                                (p.row, p.col, p.cols, p.rows, size, pixels_hash(p.picture))
                            })
                            .collect(),
                        inactive: {
                            // The live rows alone: the main screen's history
                            // is the rows' own comparison's while it shows.
                            let other = if s.mode(vt::Mode::AlternateScreen) {
                                s.main_rows()
                            } else {
                                s.alternate_rows()
                            };
                            let history = other.len().saturating_sub(usize::from(rows));
                            (other.skip(history))
                                .map(|r| (number(r.id()), r.continues(), cells_hash(r)))
                                .collect()
                        },
                    }
                }

                fn retained(&self) -> usize {
                    let s = self.parser.screen();
                    s.history_len().saturating_add(usize::from(size(s).0))
                }

                fn row(&self, offset: usize, out: &mut Row) -> bool {
                    out.clear();
                    let s = self.parser.screen();
                    let Some(row) = back(s, offset) else {
                        return false;
                    };
                    out.id = number(row.id());
                    out.version = row.version();
                    out.continues = row.continues();
                    out.len = row.len();
                    out.is_empty = row.is_empty();
                    out.has_links = row.has_links();
                    let (rows, _) = size(s);
                    // The row as the window shows it, if it is a live row.
                    let shown = usize::from(rows)
                        .checked_sub(offset.saturating_add(1))
                        .and_then(|y| u16::try_from(y).ok())
                        .map(|y| s.window().row(y));
                    let mut cell_agrees = true;
                    let mut window_agrees = shown.is_none_or(|w| {
                        w.is_some_and(|w| w.id() == row.id() && w.continues() == row.continues())
                    });
                    for (col, c) in row.cells().enumerate() {
                        out.text.push_str(c.contents());
                        out.cells.push(Cell {
                            end: out.text.len(),
                            wide: c.is_wide(),
                            continuation: c.is_wide_continuation(),
                            style: style(c.attributes()),
                        });
                        let l = row.link(col);
                        cell_agrees &= row.cell(col) == Some(c);
                        if let Some(Some(w)) = shown {
                            window_agrees &= w.cell(col) == Some(c) && w.link(col) == l;
                        }
                        if let Some(l) = l {
                            out.links.push((col, model::Link { uri: l.uri().to_owned(), key: l.key() }));
                        }
                    }
                    cell_agrees &= row.cell(row.len()).is_none();
                    if !cell_agrees {
                        out.disagreeing.push("Row::cell");
                    }
                    if !window_agrees {
                        out.disagreeing.push("Window::row");
                    }
                    true
                }

                fn take_mark(&mut self, slot: usize) {
                    let mark = self.parser.screen().mark();
                    match self.marks.get_mut(slot) {
                        Some(kept) => *kept = mark,
                        None => self.marks.push(mark),
                    }
                }

                fn marks(&self) -> Vec<Marked> {
                    let s = self.parser.screen();
                    self.marks
                        .iter()
                        .map(|&mark| {
                            let full_refresh = s.full_refresh_since(mark);
                            Marked {
                                changed: s.mark() != mark,
                                full_refresh,
                                dirty: (s.rows())
                                    .filter(|r| full_refresh || r.changed_since(mark))
                                    .map(|r| number(r.id()))
                                    .collect(),
                            }
                        })
                        .collect()
                }

                fn lookups(&mut self, offsets: &[usize]) -> Vec<Lookup> {
                    let s = self.parser.screen();
                    let look = |id: vt::RowId| {
                        let index = s.row_index(id);
                        Lookup {
                            id: number(id),
                            index,
                            version: index.and_then(|i| s.rows().nth(i)).map(|r| r.version()),
                        }
                    };
                    let mut out: Vec<Lookup> = self.oldest.into_iter().map(look).collect();
                    out.extend(
                        offsets
                            .iter()
                            .filter_map(|&offset| back(s, offset))
                            .map(|r| look(r.id())),
                    );
                    let retained = s.history_len().saturating_add(usize::from(size(s).0));
                    self.oldest = retained
                        .checked_sub(1)
                        .and_then(|last| back(s, last))
                        .map(|r| r.id());
                    out
                }
            }

            fn to_color(c: &Color) -> vt::Color {
                match c {
                    Color::Idx(n) => vt::Color::Idx(*n),
                    Color::Rgb(r, g, b) => vt::Color::Rgb(vt::Rgb {
                        r: *r,
                        g: *g,
                        b: *b,
                    }),
                    Color::Default => vt::Color::Default,
                }
            }

            fn attributes(s: &Style) -> vt::Attributes {
                vt::Attributes::new(to_color(&s.fg), to_color(&s.bg))
                    .with_bold(s.bold)
                    .with_dim(s.dim)
                    .with_italic(s.italic)
                    .with_underline(s.underline)
                    .with_underline_style(match s.underline_style {
                        Underline::Single => vt::UnderlineStyle::Single,
                        Underline::Double => vt::UnderlineStyle::Double,
                        Underline::Curly => vt::UnderlineStyle::Curly,
                        Underline::Dotted => vt::UnderlineStyle::Dotted,
                        Underline::Dashed => vt::UnderlineStyle::Dashed,
                        Underline::None => vt::UnderlineStyle::None,
                    })
                    .with_inverse(s.inverse)
                    .with_blink(match s.blink {
                        Blink::Slow => vt::Blink::Slow,
                        Blink::Rapid => vt::Blink::Rapid,
                        Blink::None => vt::Blink::None,
                    })
                    .with_hidden(s.hidden)
                    .with_strikeout(s.strikeout)
                    .with_underline_color(to_color(&s.underline_color))
            }

            /// A `Cell` as its own accessors show it.
            fn stored(c: &vt::CellRef<'_>) -> String {
                format!(
                    "{:?} wide {} continuation {} {:?}",
                    c.contents(),
                    c.is_wide(),
                    c.is_wide_continuation(),
                    style(c.attributes())
                )
            }

            /// The standalone types and constants.
            pub struct Types;

            impl crate::cells::Standalone for Types {
                type Run = vt::Cells;

                fn constants() -> Vec<(&'static str, String)> {
                    let mut out = vec![
                        ("OSC_PAYLOAD_LIMIT", vt::OSC_PAYLOAD_LIMIT.to_string()),
                        ("Error::Capacity", error(vt::Error::Capacity).0),
                        (
                            "Options::default is Options::new",
                            (vt::Options::default() == vt::Options::new()).to_string(),
                        ),
                        (
                            "Attributes::default",
                            format!("{:?}", style(vt::Attributes::default())),
                        ),
                        ("blank cell", stored(&vt::CellRef::default())),
                        (
                            "wide continuation",
                            stored(&vt::CellRef::wide_continuation()),
                        ),
                        (
                            "MouseProtocolMode::default",
                            format!("{:?}", vt::MouseProtocolMode::default()),
                        ),
                        (
                            "MouseProtocolEncoding::default",
                            format!("{:?}", vt::MouseProtocolEncoding::default()),
                        ),
                    ];
                    for n in 0..8u16 {
                        out.push((
                            "UnderlineStyle::from_number, number",
                            format!(
                                "{n}: {:?}",
                                vt::UnderlineStyle::from_number(n)
                                    .map(|u| (underline(u), u.number()))
                            ),
                        ));
                    }
                    out
                }

                fn continues_cluster(cluster: &str, c: char) -> bool {
                    vt::continues_cluster(cluster, c)
                }

                fn run(len: usize) -> vt::Cells {
                    vt::Cells::new(len).unwrap_or_default()
                }

                fn apply(run: &mut vt::Cells, edit: &crate::cells::Edit) -> String {
                    use crate::cells::Edit;
                    match edit {
                        Edit::Set {
                            i,
                            text,
                            wide,
                            style,
                        } => {
                            let cell = vt::CellRef::new(text, *wide, attributes(style));
                            format!("{} {}", run.set(*i, cell), stored(&cell))
                        }
                        Edit::Continuation { i } => {
                            run.set(*i, vt::CellRef::wide_continuation());
                            String::new()
                        }
                        Edit::SetAttributes { i, style } => {
                            run.set_attributes(*i, attributes(style));
                            String::new()
                        }
                        Edit::Fill {
                            start,
                            end,
                            text,
                            wide,
                            style,
                        } => {
                            let cell = vt::CellRef::new(text, *wide, attributes(style));
                            run.fill(*start..*end, cell);
                            stored(&cell)
                        }
                        Edit::Reset { len } => format!("{:?}", run.reset(*len).map_err(error)),
                        Edit::Copy { i, from } => {
                            let was = run.clone();
                            match was.get(*from) {
                                Some(cell) => format!("{}", run.set(*i, cell)),
                                None => "no cell".into(),
                            }
                        }
                        Edit::Collect => match vt::Cells::new(run.len()) {
                            Ok(mut collected) => {
                                let kept: Vec<bool> = (run.range(0..run.len()).enumerate())
                                    .map(|(i, cell)| collected.set(i, cell))
                                    .collect();
                                let equal = collected == *run;
                                *run = collected;
                                format!("{equal} {kept:?}")
                            }
                            Err(e) => error(e).0,
                        },
                    }
                }

                fn read(run: &vt::Cells, out: &mut Row) {
                    out.clear();
                    out.len = run.len();
                    out.is_empty = run.is_empty();
                    let mut range_agrees = run.range(0..run.len()).len() == run.len();
                    for (i, c) in (0..run.len()).filter_map(|i| run.get(i)).enumerate() {
                        out.text.push_str(c.contents());
                        out.cells.push(Cell {
                            end: out.text.len(),
                            wide: c.is_wide(),
                            continuation: c.is_wide_continuation(),
                            style: style(c.attributes()),
                        });
                        range_agrees &= run.range(i..i.saturating_add(1)).eq(Some(c));
                    }
                    if run.get(run.len()).is_some() {
                        out.disagreeing.push("Cells::get");
                    }
                    let (from, to) = (
                        run.len() / 3,
                        (run.len().saturating_mul(2) / 3).saturating_add(2),
                    );
                    range_agrees &= run.range(from..to).eq((from..to).filter_map(|i| run.get(i)));
                    if !range_agrees {
                        out.disagreeing.push("Cells::range");
                    }
                }
            }
        }
    };
}

/// The modes of `Mode` (`$mode`) listed, but `$left`: the match has no
/// catch-all, so a mode that is not listed fails to build.
macro_rules! every_mode {
    ($mode:path, $left:ident; $($m:ident),* $(,)?) => {{
        use $mode as Mode;
        let _named = |m: Mode| match m {
            Mode::$left $(| Mode::$m)* => (),
        };
        [$(Mode::$m),*]
    }};
}
use every_mode;

side!(work, fux_vt, "work", checked);
side!(base, base_vt, "base");

/// The commit's error for a size of no rows or no columns, as read.
/// FNV-1a as a `Hasher`: the sides hash what they read with it, the same
/// whichever fux-vt they are.
pub(crate) struct Fnv(pub(crate) u64);

impl std::hash::Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0 ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
}

fn zero_size() -> crate::model::Error {
    crate::model::Error("ZeroSize: terminal dimensions must be nonzero".to_owned())
}
