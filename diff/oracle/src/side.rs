//! The two sides: the working tree's fux-vt (`work`) and the pinned
//! commit's (`base`), each read into the model through its own public API.
//!
//! Both are one macro, `side!`, so they are read the same way. Where the
//! two APIs part (a method renamed, a type reshaped), give the macro an
//! argument for that part, as `diff/src/terminal.rs`'s `stack!` does, and
//! read both into the same model; say so in the README's list of adapters.
//!
//! Adapters today: `Options::palette` and `Screen::colors_changed`, which
//! the pinned commit has not. The working tree is given the option the
//! case asks for; the commit, which has no palette, reports the option as
//! asked and no colour changed. The palette's sequences are exempt
//! (`exempt`), so with the option on the working tree sees none of them,
//! and anything else that changed a colour would differ.
//!
//! And the cell a host makes: the commit's `Cell` (`Cell::new`, `None`
//! past 17 bytes; `Cells::set_cell` and `set_text`), the working tree's
//! `CellRef` (`CellRef::new`, given only what `Cell::new` takes;
//! `Cells::set`), and `CLUSTER_CAPACITY`, on `Cell` in the commit.

macro_rules! side {
    (
        $module:ident,
        $vt:ident,
        $name:literal,
        with_palette: $with_palette:expr,
        palette: $palette:expr,
        colors_changed: $colors_changed:expr,
        cell: $cell:ty,
        new_cell: $new_cell:expr,
        blank: $blank:expr,
        continuation: $continuation:expr,
        set_cell: $set_cell:expr,
        set_text: $set_text:expr,
        cluster_capacity: $cluster_capacity:expr $(,)?
    ) => {
        pub mod $module {
            use crate::model::{
                self, Blink, Cell, Color, Encoding, Error, Heard, Lookup, Marked, Mouse, Row, Seen,
                Setup, State, Style, Underline,
            };
            use $vt as vt;

            /// A parser, and the marks taken of its screen.
            #[derive(Clone)]
            pub struct Terminal {
                parser: vt::Parser,
                /// `Options::palette` as the case asked for it.
                palette: bool,
                marks: Vec<vt::Mark>,
                /// The oldest row seen at the last lookup, to ask for again
                /// once it may be gone.
                oldest: Option<vt::RowId>,
            }

            fn color(c: vt::Color) -> Color {
                match c {
                    vt::Color::Default => Color::Default,
                    vt::Color::Idx(n) => Color::Idx(n),
                    vt::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
                    other => Color::Other(format!("{other:?}")),
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
                    other => Underline::Other(format!("{other:?}")),
                }
            }

            fn blink(b: vt::Blink) -> Blink {
                match b {
                    vt::Blink::None => Blink::None,
                    vt::Blink::Slow => Blink::Slow,
                    vt::Blink::Rapid => Blink::Rapid,
                    other => Blink::Other(format!("{other:?}")),
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

            /// A cell's style through its own accessors rather than its
            /// attributes.
            fn getters(c: &vt::CellRef<'_>) -> Style {
                Style {
                    fg: color(c.fgcolor()),
                    bg: color(c.bgcolor()),
                    underline_color: color(c.underline_color()),
                    bold: c.bold(),
                    dim: c.dim(),
                    italic: c.italic(),
                    underline: c.underline(),
                    underline_style: underline(c.underline_style()),
                    inverse: c.inverse(),
                    blink: blink(c.blink()),
                    hidden: c.hidden(),
                    strikeout: c.strikeout(),
                }
            }

            fn link(l: vt::Hyperlink<'_>) -> model::Link {
                model::Link {
                    uri: l.uri().to_owned(),
                    id: l.id().map(str::to_owned),
                    key: l.key(),
                }
            }

            fn error(e: vt::Error) -> Error {
                Error(format!("{e:?}: {e}"))
            }

            /// The number in a `RowId` or `Mark`, which shows it only in
            /// its `Debug` form; `u64::MAX` if it has none.
            fn number(value: impl std::fmt::Debug) -> u64 {
                let text = format!("{value:?}");
                let digits: String = text.chars().filter(char::is_ascii_digit).collect();
                digits.parse().unwrap_or(u64::MAX)
            }

            fn options(setup: &Setup) -> vt::Options {
                let options = vt::Options::new()
                    .with_events(setup.events)
                    .with_extended_replies(setup.extended_replies)
                    .with_mode_reports(setup.mode_reports)
                    .with_in_band_resize(setup.in_band_resize)
                    .with_size_reports(setup.size_reports)
                    .with_color_scheme_updates(setup.color_scheme_updates)
                    .with_kitty_keyboard(setup.kitty_keyboard)
                    .with_reflow(setup.reflow)
                    .with_hyperlinks(setup.hyperlinks)
                    .with_prompt_marks(setup.prompt_marks)
                    .with_rectangle_checksums(setup.rectangle_checksums)
                    .with_setting_reports(setup.setting_reports);
                let with_palette: fn(vt::Options, bool) -> vt::Options = $with_palette;
                with_palette(options, setup.palette).with_identity(
                    setup
                        .identity
                        .map(|(name, version)| vt::Identity { name, version }),
                )
            }

            /// The options back, as the parser reports them; `palette` as
            /// the case asked for it.
            fn setup(o: vt::Options, palette: bool) -> Setup {
                let reported: fn(vt::Options, bool) -> bool = $palette;
                Setup {
                    palette: reported(o, palette),
                    events: o.events,
                    extended_replies: o.extended_replies,
                    mode_reports: o.mode_reports,
                    in_band_resize: o.in_band_resize,
                    size_reports: o.size_reports,
                    color_scheme_updates: o.color_scheme_updates,
                    kitty_keyboard: o.kitty_keyboard,
                    reflow: o.reflow,
                    hyperlinks: o.hyperlinks,
                    prompt_marks: o.prompt_marks,
                    rectangle_checksums: o.rectangle_checksums,
                    setting_reports: o.setting_reports,
                    identity: o.identity.map(|i| (i.name, i.version)),
                }
            }

            /// Everything the parser gives the host, in order.
            struct Sink<'a>(&'a mut Vec<Heard>);

            impl vt::Sink for Sink<'_> {
                fn reply(&mut self, bytes: &[u8]) {
                    self.0.push(Heard::Reply(bytes.to_vec()));
                }
                fn event(&mut self, event: vt::Event<'_>) {
                    self.0.push(match event {
                        vt::Event::Title(t) => Heard::Title(t.to_vec()),
                        vt::Event::IconName(t) => Heard::IconName(t.to_vec()),
                        vt::Event::Bell => Heard::Bell,
                        vt::Event::Clipboard { selection, data } => Heard::Clipboard {
                            selection: selection.to_vec(),
                            data: data.to_vec(),
                        },
                        vt::Event::ColorQuery { number, bel } => Heard::ColorQuery { number, bel },
                        other => Heard::OtherEvent(format!("{other:?}")),
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
                        other => Heard::OtherUnhandled(format!("{other:?}")),
                    });
                }
            }

            impl crate::Side for Terminal {
                const NAME: &'static str = $name;

                fn new(rows: u16, cols: u16, history: usize, setup: &Setup) -> Result<Self, Error> {
                    let parser = vt::Parser::with_options(rows, cols, history, options(setup))
                        .map_err(error)?;
                    Ok(Terminal {
                        parser,
                        palette: setup.palette,
                        marks: Vec::new(),
                        oldest: None,
                    })
                }

                fn process(&mut self, bytes: &[u8], heard: &mut Vec<Heard>) -> Result<(), Error> {
                    self.parser
                        .process_with(bytes, &mut Sink(heard))
                        .map_err(error)
                }

                fn process_until_frame(
                    &mut self,
                    bytes: &[u8],
                    heard: &mut Vec<Heard>,
                ) -> Result<Option<usize>, Error> {
                    self.parser
                        .process_until_frame(bytes, &mut Sink(heard))
                        .map_err(error)
                }

                fn resize(&mut self, rows: u16, cols: u16) -> Result<(), Error> {
                    self.parser.resize(rows, cols).map_err(error)
                }

                fn state(&self) -> State {
                    let s = self.parser.screen();
                    let pen = s.attributes();
                    let (rows, _) = s.size();
                    let retained = s.history_len().saturating_add(usize::from(rows));
                    State {
                        size: s.size(),
                        cursor: s.cursor_position(),
                        pending_wrap: s.pending_wrap(),
                        hide_cursor: s.hide_cursor(),
                        application_cursor: s.application_cursor(),
                        application_keypad: s.application_keypad(),
                        bracketed_paste: s.bracketed_paste(),
                        synchronized_output: s.synchronized_output(),
                        in_band_resize: s.in_band_resize(),
                        color_scheme_updates: s.color_scheme_updates(),
                        focus_reporting: s.focus_reporting(),
                        cursor_shape: s.cursor_shape(),
                        alternate_screen: s.alternate_screen(),
                        autowrap: s.autowrap(),
                        insert_mode: s.insert_mode(),
                        origin_mode: s.origin_mode(),
                        scroll_region: s.scroll_region(),
                        mouse: match s.mouse_protocol_mode() {
                            vt::MouseProtocolMode::None => Mouse::None,
                            vt::MouseProtocolMode::Press => Mouse::Press,
                            vt::MouseProtocolMode::PressRelease => Mouse::PressRelease,
                            vt::MouseProtocolMode::ButtonMotion => Mouse::ButtonMotion,
                            vt::MouseProtocolMode::AnyMotion => Mouse::AnyMotion,
                            other => Mouse::Other(format!("{other:?}")),
                        },
                        encoding: match s.mouse_protocol_encoding() {
                            vt::MouseProtocolEncoding::Default => Encoding::Default,
                            vt::MouseProtocolEncoding::Utf8 => Encoding::Utf8,
                            vt::MouseProtocolEncoding::Sgr => Encoding::Sgr,
                            other => Encoding::Other(format!("{other:?}")),
                        },
                        kitty_keyboard_flags: s.kitty_keyboard_flags(),
                        modify_other_keys: s.modify_other_keys(),
                        pen: style(pen),
                        pen_getters_agree: s.bgcolor() == pen.background()
                            && s.inverse() == pen.inverse(),
                        hyperlink: s
                            .hyperlink()
                            .map(|(uri, id)| (uri.to_owned(), id.map(str::to_owned))),
                        history_len: s.history_len(),
                        storage_cells: s.storage_cells(),
                        mark: number(s.mark()),
                        resize_report: self.parser.resize_report(),
                        options: setup(self.parser.options(), self.palette),
                        colors_changed: {
                            let changed: fn(&vt::Screen) -> bool = $colors_changed;
                            changed(s)
                        },
                        rows_end_there: retained
                            .checked_sub(1)
                            .is_none_or(|last| s.row_from_bottom(last).is_some())
                            && s.row_from_bottom(retained).is_none(),
                    }
                }

                fn retained(&self) -> usize {
                    let s = self.parser.screen();
                    s.history_len().saturating_add(usize::from(s.size().0))
                }

                fn row(&self, offset: usize, out: &mut Row) -> bool {
                    out.clear();
                    let s = self.parser.screen();
                    let Some(row) = s.row_from_bottom(offset) else {
                        return false;
                    };
                    out.id = number(row.id());
                    out.version = row.version();
                    out.wrapped = row.wrapped();
                    out.prompt = row.starts_prompt();
                    out.len = row.len();
                    out.is_empty = row.is_empty();
                    out.text_len = row.text_len();
                    out.has_links = row.has_links();
                    let (rows, _) = s.size();
                    // The row's place on the screen, if it is a live row.
                    let live = usize::from(rows)
                        .checked_sub(offset.saturating_add(1))
                        .and_then(|y| u16::try_from(y).ok());
                    let mut cell_agrees = true;
                    let (mut screen_cell_agrees, mut screen_link_agrees) = (true, true);
                    for (col, c) in row.cells().enumerate() {
                        out.text.push_str(c.contents());
                        let st = style(c.attributes());
                        let getters_agree = getters(&c) == st;
                        out.cells.push(Cell {
                            end: out.text.len(),
                            has_contents: c.has_contents(),
                            wide: c.is_wide(),
                            continuation: c.is_wide_continuation(),
                            style: st,
                            getters_agree,
                        });
                        let l = row.link(col);
                        cell_agrees &= row.cell(col) == Some(c);
                        if let Some(y) = live {
                            let x = u16::try_from(col).ok();
                            screen_cell_agrees &= x.and_then(|x| s.cell(y, x)) == Some(c);
                            screen_link_agrees &= x.and_then(|x| s.link(y, x)) == l;
                        }
                        if let Some(l) = l {
                            out.links.push((col, link(l)));
                        }
                    }
                    cell_agrees &= row.cell(row.len()).is_none();
                    if !cell_agrees {
                        out.disagreeing.push("Row::cell");
                    }
                    if let Some(y) = live {
                        let past = u16::try_from(row.len()).unwrap_or(u16::MAX);
                        screen_cell_agrees &= s.cell(y, past).is_none();
                        if !screen_cell_agrees {
                            out.disagreeing.push("Screen::cell");
                        }
                        if !screen_link_agrees {
                            out.disagreeing.push("Screen::link");
                        }
                        if s.row_wrapped(y) != row.wrapped() {
                            out.disagreeing.push("Screen::row_wrapped");
                        }
                        if s.starts_prompt(y) != row.starts_prompt() {
                            out.disagreeing.push("Screen::starts_prompt");
                        }
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
                        .map(|&mark| Marked {
                            changed: s.changed_since(mark),
                            full_refresh: s.full_refresh_since(mark),
                            dirty: s.dirty_rows_since(mark).map(|r| number(r.id())).collect(),
                            dirty_live: s
                                .dirty_live_rows_since(mark)
                                .map(|(y, r)| (y, number(r.id())))
                                .collect(),
                        })
                        .collect()
                }

                fn lookups(&mut self, offsets: &[usize]) -> Vec<Lookup> {
                    let s = self.parser.screen();
                    let look = |id: vt::RowId| Lookup {
                        id: number(id),
                        offset: s.offset_for_row(id),
                        version: s.row_by_id(id).map(|r| r.version()),
                    };
                    let mut out: Vec<Lookup> = self.oldest.into_iter().map(look).collect();
                    out.extend(
                        offsets
                            .iter()
                            .filter_map(|&offset| s.row_from_bottom(offset))
                            .map(|r| look(r.id())),
                    );
                    let retained = s.history_len().saturating_add(usize::from(s.size().0));
                    self.oldest = retained
                        .checked_sub(1)
                        .and_then(|last| s.row_from_bottom(last))
                        .map(|r| r.id());
                    out
                }

                fn copy(&self, ask: &model::Selection, cells: bool) -> Seen {
                    let w = self.parser.screen().window(ask.offset, ask.rows, ask.cols);
                    let mut seen = Seen {
                        rows: w.rows(),
                        cols: w.cols(),
                        offset: w.offset(),
                        wrapped: Vec::new(),
                        row_ids: Vec::new(),
                        cells: Vec::new(),
                        text: w
                            .text(ask.from, ask.to, ask.max_cells, ask.max_bytes)
                            .map_err(error),
                    };
                    if cells {
                        for y in 0..=w.rows() {
                            seen.wrapped.push(w.row_wrapped(y));
                            seen.row_ids
                                .push(w.row(y).map(|r| (number(r.id()), r.len())));
                            let mut line = String::new();
                            for x in 0..=w.cols() {
                                match w.cell(y, x) {
                                    Some(c) => line.push_str(&format!(
                                        "[{:?} {} {} {:?}]",
                                        c.contents(),
                                        c.is_wide(),
                                        c.is_wide_continuation(),
                                        style(c.attributes())
                                    )),
                                    None => line.push_str("[-]"),
                                }
                            }
                            seen.cells.push(line);
                        }
                    }
                    seen
                }
            }

            fn to_color(c: &Color) -> vt::Color {
                match c {
                    Color::Idx(n) => vt::Color::Idx(*n),
                    Color::Rgb(r, g, b) => vt::Color::Rgb(*r, *g, *b),
                    Color::Default | Color::Other(_) => vt::Color::Default,
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
                        Underline::None | Underline::Other(_) => vt::UnderlineStyle::None,
                    })
                    .with_inverse(s.inverse)
                    .with_blink(match s.blink {
                        Blink::Slow => vt::Blink::Slow,
                        Blink::Rapid => vt::Blink::Rapid,
                        Blink::None | Blink::Other(_) => vt::Blink::None,
                    })
                    .with_hidden(s.hidden)
                    .with_strikeout(s.strikeout)
                    .with_underline_color(to_color(&s.underline_color))
            }

            /// A `Cell` as its own accessors show it.
            fn stored(c: &$cell) -> String {
                format!(
                    "contents {} wide {} continuation {} {:?}",
                    c.has_contents(),
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
                        ("UNICODE_VERSION", format!("{:?}", vt::UNICODE_VERSION)),
                        ("URI_LIMIT", vt::URI_LIMIT.to_string()),
                        ("ID_LIMIT", vt::ID_LIMIT.to_string()),
                        ("OSC_PAYLOAD_LIMIT", vt::OSC_PAYLOAD_LIMIT.to_string()),
                        ("CLUSTER_CAPACITY", $cluster_capacity.to_string()),
                        ("Identity::MAX_LEN", vt::Identity::MAX_LEN.to_string()),
                        (
                            "Options::default is Options::new",
                            (vt::Options::default() == vt::Options::new()).to_string(),
                        ),
                        (
                            "Attributes::default",
                            format!("{:?}", style(vt::Attributes::default())),
                        ),
                        ("blank cell", stored(&$blank)),
                        ("wide continuation", stored(&$continuation)),
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
                    for len in [0usize, 1, 2, 3, 10, 100, 1000] {
                        out.push((
                            "Cells::text_limit",
                            format!("{len}: {}", vt::Cells::text_limit(len)),
                        ));
                    }
                    for e in [
                        vt::Error::ZeroSize,
                        vt::Error::Capacity,
                        vt::Error::IdentityExhausted,
                        vt::Error::CopyLimit,
                        vt::Error::InvalidRange,
                    ] {
                        out.push(("Error", error(e).0));
                    }
                    out
                }

                fn continues_cluster(cluster: &str, c: char) -> bool {
                    vt::continues_cluster(cluster, c)
                }

                fn run(len: usize) -> vt::Cells {
                    vt::Cells::new(len)
                }

                fn apply(run: &mut vt::Cells, edit: &crate::cells::Edit) -> String {
                    use crate::cells::Edit;
                    match edit {
                        Edit::SetText {
                            i,
                            text,
                            wide,
                            style,
                        } => format!("{}", $set_text(run, *i, text, *wide, attributes(style))),
                        Edit::SetCell {
                            i,
                            text,
                            wide,
                            style,
                        } => match $new_cell(text, *wide, attributes(style)) {
                            Some(cell) => {
                                let shown = stored(&cell);
                                $set_cell(run, *i, cell);
                                shown
                            }
                            None => "no cell".into(),
                        },
                        Edit::Continuation { i } => {
                            $set_cell(run, *i, $continuation);
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
                        } => match $new_cell(text, *wide, attributes(style)) {
                            Some(cell) => {
                                run.fill(*start..*end, cell);
                                stored(&cell)
                            }
                            None => "no cell".into(),
                        },
                        Edit::Resize {
                            len,
                            text,
                            wide,
                            style,
                        } => match $new_cell(text, *wide, attributes(style)) {
                            Some(cell) => {
                                run.resize(*len, cell);
                                stored(&cell)
                            }
                            None => "no cell".into(),
                        },
                        Edit::Copy { i, from } => {
                            let was = run.clone();
                            match was.get(*from) {
                                Some(cell) => format!("{}", run.set(*i, cell)),
                                None => "no cell".into(),
                            }
                        }
                        Edit::Collect => {
                            let collected: vt::Cells = run.iter().collect();
                            let equal = collected == *run;
                            *run = collected;
                            format!("{equal}")
                        }
                    }
                }

                fn read(run: &vt::Cells, out: &mut Row) {
                    out.clear();
                    out.len = run.len();
                    out.is_empty = run.is_empty();
                    out.text_len = run.text_len();
                    let mut get_agrees = run.get(run.len()).is_none();
                    for (i, c) in run.iter().enumerate() {
                        out.text.push_str(c.contents());
                        let st = style(c.attributes());
                        let getters_agree = getters(&c) == st;
                        out.cells.push(Cell {
                            end: out.text.len(),
                            has_contents: c.has_contents(),
                            wide: c.is_wide(),
                            continuation: c.is_wide_continuation(),
                            style: st,
                            getters_agree,
                        });
                        get_agrees &= run.get(i) == Some(c);
                    }
                    if !get_agrees {
                        out.disagreeing.push("Cells::get");
                    }
                    let (from, to) = (
                        run.len() / 3,
                        (run.len().saturating_mul(2) / 3).saturating_add(2),
                    );
                    if !run
                        .range(from..to)
                        .eq(run.iter().skip(from).take(to.saturating_sub(from)))
                    {
                        out.disagreeing.push("Cells::range");
                    }
                }
            }
        }
    };
}

side!(
    work,
    fux_vt,
    "work",
    with_palette: |o, on| o.with_palette(on),
    palette: |o, _| o.palette,
    colors_changed: |s| s.colors_changed(),
    cell: vt::CellRef<'_>,
    new_cell: {
        fn made(text: &str, wide: bool, a: vt::Attributes) -> Option<vt::CellRef<'_>> {
            (text.len() <= 17).then(|| vt::CellRef::new(text, wide, a))
        }
        made
    },
    blank: vt::CellRef::default(),
    continuation: vt::CellRef::wide_continuation(),
    set_cell: |run: &mut vt::Cells, i, cell| {
        run.set(i, cell);
    },
    set_text: |run: &mut vt::Cells, i, text, wide, attributes| run
        .set(i, vt::CellRef::new(text, wide, attributes)),
    cluster_capacity: vt::CLUSTER_CAPACITY,
);
side!(
    base,
    base_vt,
    "base",
    with_palette: |o, _| o,
    palette: |_, asked| asked,
    colors_changed: |_| false,
    cell: vt::Cell,
    new_cell: vt::Cell::new,
    blank: vt::Cell::default(),
    continuation: vt::Cell::wide_continuation(),
    set_cell: vt::Cells::set_cell,
    set_text: vt::Cells::set_text,
    cluster_capacity: vt::Cell::CLUSTER_CAPACITY,
);
