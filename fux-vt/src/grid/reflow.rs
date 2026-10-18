//! Reflow: the primary screen resized with its lines re-wrapped
//! (`Grid::reflowed`), laid out in one walk over the rows and copied in a
//! second, each through `Reflow`.
use super::*;

impl Grid {
    /// The primary screen resized with reflow: every logical line, a run of
    /// soft-wrapped rows and the row that ends it, history included, is
    /// re-wrapped at `cols`, without splitting wide glyphs. The cursor stays
    /// on its character. When there are more rows than fit, blank lines
    /// below the cursor go first, then lines scroll into history, oldest
    /// dropped first past its budget; rows below the screen once
    /// the cursor's row is at its top are dropped. The scroll region is
    /// reset, as xterm does on resize. Like `resized`, it builds replacement
    /// storage first, so allocation failure leaves this grid unchanged.
    ///
    /// Two walks over the rows, one to lay them out and one to copy them,
    /// so no line is ever gathered in memory of its own. The k-th row of a
    /// reflowed line keeps the identity of the line's k-th row before, if it
    /// had one; every row takes `version`.
    pub fn reflowed(&self, size: Size, next: &mut u64, version: u64) -> Result<Self, Error> {
        Self::check_size(size)?;
        let cols = size.cols();
        // The cursor and the saved cursor go with their characters; one
        // waiting to wrap is laid out one past its glyph.
        let at = |cursor: Cursor| {
            self.history_len()
                .checked_add(usize::from(cursor.row))
                .map(|row| (row, usize::from(past(cursor.col, cursor.pending_wrap))))
        };
        let marks = [at(self.cursor), at(self.saved.cursor)];
        let layout = self.reflow(usize::from(cols), marks, &mut Layout)?;
        // Blank lines below the cursor are dropped before any line scrolls
        // into history: a mostly empty screen keeps its text on screen.
        let screen = usize::from(size.rows());
        // A mark no line held goes to the start of the last row.
        let last = Mark::Blank(layout.rows.saturating_sub(1), 0);
        let [cursor, saved] = layout.marks.map(|mark| mark.unwrap_or(last));
        let (Mark::Text(cursor_row, _) | Mark::Blank(cursor_row, _)) = cursor;
        // Only as many as the screen's own lines are past the new height:
        // rows in history stay there, and a resize never brings one back
        // above the cursor to fill the place of dropped blank lines.
        let drop = layout
            .trailing_blank
            .min(
                layout
                    .rows
                    .saturating_sub(layout.screen_line)
                    .saturating_sub(screen),
            )
            .min(layout.rows.saturating_sub(cursor_row.saturating_add(1)));
        let total = layout.rows.saturating_sub(drop);
        let live_top = total.saturating_sub(screen).min(cursor_row);
        let end = total.min(live_top.checked_add(screen).ok_or(Error::Capacity)?);
        // A row on the screen, the first if above it, the last if below;
        // one past the last column of the text is the last column, waiting
        // to wrap, and any column past the text's end at most the last.
        let placed = |cursor: Cursor, mark: Mark| {
            let (row, col, pending_wrap) = match mark {
                Mark::Text(row, col) => (row, col, col >= usize::from(cols)),
                Mark::Blank(row, col) => (row, col, false),
            };
            Cursor {
                pending_wrap,
                ..cursor.placed(
                    (
                        u16::try_from(row.saturating_sub(live_top)).unwrap_or(u16::MAX),
                        u16::try_from(col).unwrap_or(u16::MAX),
                    ),
                    size,
                )
            }
        };
        let mut replacement = Self {
            // The cursor's row is on screen: `live_top` is at most its row,
            // and the screen reaches past it.
            cursor: placed(self.cursor, cursor),
            // The saved cursor moves with its character as the cursor
            // does, so DECRC (as 1049 leaves the alternate screen) finds it.
            saved: Saved {
                cursor: placed(self.saved.cursor, saved),
                ..self.saved
            },
            ..self.successor(size)
        };
        replacement.reserve_screen()?;
        let mut copy = Fill {
            grid: &mut replacement,
            rows: 0..end,
            screen: live_top,
            next,
            version,
            cells: vec![BLANK; usize::from(cols)],
            written: 0,
            text: Text::default(),
            links: None,
        };
        self.reflow(usize::from(cols), marks, &mut copy)?;
        // Blank rows under the last line, if the lines do not fill the screen.
        while replacement.order.len() < screen {
            let meta = Meta::new(next_id(next), version, false, 0);
            replacement.push_screen_row(meta, &[], None, None);
        }
        replacement.fit_cursor();
        Ok(replacement)
    }

    /// Lays every retained row out again at `width` columns, one logical
    /// line after another, giving each run of cells and each finished row
    /// to `target`. `marks` are retained rows and columns, the cursor's and
    /// the saved cursor's, each laid out as the cursor is.
    fn reflow(
        &self,
        width: usize,
        marks: [Option<(usize, usize)>; 2],
        target: &mut impl Reflow,
    ) -> Result<Reflowed, Error> {
        let retained = self.retained_len();
        let mut pass = Pass {
            width,
            marks,
            out: Reflowed {
                rows: 0,
                marks: [None; 2],
                trailing_blank: 0,
                screen_line: 0,
            },
            target,
        };
        // Whether any row may be half as long, which only then is asked.
        let halved = self.doubled || self.history.has_renditions();
        let double = |index: usize| halved && self.rendition_at(index).is_double();
        let mut start = 0;
        while start < retained {
            // A line runs through its wrapped rows to the row that ends it.
            // Its length leaves out its blank tail, which can run back over
            // several rows: the rows' widths up to the last with text, and
            // that row's text. A row half as long is a line of its own, as
            // wide as it was, never joined to the rows around it nor split:
            // the row it is laid out in takes its rendition.
            let (mut end, mut before, mut length) = (start, 0usize, 0);
            loop {
                let (wrapped, width, text) = self.facts_at(end);
                if text > 0 {
                    length = before.saturating_add(text);
                }
                before = before.saturating_add(width);
                let next = end.saturating_add(1);
                if !wrapped || next >= retained || double(end) || double(next) {
                    break;
                }
                end = next;
            }
            if (start..=end).contains(&self.history_len()) {
                pass.out.screen_line = pass.out.rows;
            }
            let rendition = if double(start) {
                self.rendition_at(start)
            } else {
                LineRendition::Single
            };
            self.lay_out_runs((start, end), (length, rendition), &mut pass)?;
            start = end.saturating_add(1);
        }
        Ok(pass.out)
    }

    /// The rendition of retained row `index`.
    fn rendition_at(&self, index: usize) -> LineRendition {
        self.row_at(index)
            .map_or(LineRendition::Single, |row| row.rendition)
    }

    /// Lays out the line of rows `start` through `end`, a run of cells at a
    /// time: as many of a row's as fit in the row being laid out, short of a
    /// wide glyph that would end in its last column, or a spacer, which no
    /// cell is laid out from. At a width under two, wide glyphs are left out,
    /// as one column cannot show them. So a line costs its rows and the rows it is laid
    /// out in, not its cells; one whose rows stay as they are (every line
    /// but a few, when only the rows change) costs a run a row. The line's
    /// text is `length` cells long. A line of `rendition` other than single
    /// width, one row, keeps as much as fits in half the width, and loses
    /// the rest, as DECDWL loses it (DEC STD 070, DECDWL, notes 1 and 2).
    fn lay_out_runs(
        &self,
        (start, end): (usize, usize),
        (length, rendition): (usize, LineRendition),
        pass: &mut Pass<'_, impl Reflow>,
    ) -> Result<(), Error> {
        let length = if rendition.is_double() {
            length.min((pass.width / 2).max(1))
        } else {
            length
        };
        let mut line = Run {
            used: 0,
            next: start,
            end,
            prompt: false,
            pending: false,
            offsets: [None; 2],
        };
        let first = pass.out.rows;
        let mut base = 0usize;
        for i in start..=end {
            let Some(row) = self.row_at(i) else {
                continue;
            };
            let len = row.width;
            // The row the row's first cell goes to is where the prompt
            // starts after, or the line's last, if no cell goes after it.
            line.pending |= row.prompt;
            for (offset, mark) in line.offsets.iter_mut().zip(pass.marks) {
                if let Some((row_index, col)) = mark
                    && row_index == i
                {
                    *offset = base.checked_add(col);
                }
            }
            let mut taken = len.min(length.saturating_sub(base));
            if i != end && row.ends_in_spacer() {
                // A cursor on a spacer goes with the glyph after it.
                let spacer = base.saturating_add(len).saturating_sub(1);
                for offset in &mut line.offsets {
                    if *offset == Some(spacer) {
                        *offset = spacer.checked_add(1);
                    }
                }
                taken = taken.min(len.saturating_sub(1));
            }
            // At one column, the runs between wide glyphs, which it cannot
            // show.
            let mut from = 0;
            loop {
                let to = if pass.width < 2 {
                    (from..taken)
                        .find(|&c| {
                            row.stored(c)
                                .is_some_and(|c| c.is_wide() || c.is_wide_continuation())
                        })
                        .unwrap_or(taken)
                } else {
                    taken
                };
                self.lay_out_run(row, base, from..to, &mut line, pass)?;
                if to >= taken {
                    break;
                }
                from = to.saturating_add(1);
            }
            base = base.saturating_add(len);
        }
        line.prompt |= line.pending;
        let out = &mut pass.out;
        for (offset, mark) in line.offsets.iter_mut().zip(&mut out.marks) {
            if let Some(offset) = offset.take() {
                // At the end of the line's text, after its last glyph; or
                // as far past it on the last row, in its blanks.
                *mark = Some(match offset.saturating_sub(length) {
                    0 => Mark::Text(out.rows, line.used),
                    past => Mark::Blank(out.rows, line.used.saturating_add(past)),
                });
            }
        }
        let id = self.id_at(line.next, end);
        pass.target
            .row(out.rows, line.used, (false, rendition), id, line.prompt)?;
        out.rows = out.rows.saturating_add(1);
        out.trailing_blank = if length == 0 {
            out.trailing_blank
                .saturating_add(out.rows.saturating_sub(first))
        } else {
            0
        };
        Ok(())
    }

    /// Lays out cells `cells` of `row`, which starts `base` cells into its
    /// line, after the line's cells laid out so far.
    fn lay_out_run(
        &self,
        row: Row<'_>,
        base: usize,
        cells: Range<usize>,
        line: &mut Run,
        pass: &mut Pass<'_, impl Reflow>,
    ) -> Result<(), Error> {
        let (width, taken) = (pass.width, cells.end);
        let mut at = cells.start;
        while at < taken {
            if line.used >= width {
                self.end_row(line, pass)?;
            }
            let room = width.saturating_sub(line.used);
            let take = room.min(taken.saturating_sub(at));
            let next = at.saturating_add(take);
            // A wide glyph ending the run in the last column goes to the
            // next row, leaving that column blank.
            let last = next.saturating_sub(1);
            let padded = take == room && row.stored(last).is_some_and(Compact::is_wide);
            if padded {
                line.place(row, base, at..last, pass);
                self.end_row(line, pass)?;
                line.place(row, base, last..next, pass);
            } else {
                line.place(row, base, at..next, pass);
            }
            at = next;
        }
        Ok(())
    }

    /// Ends the row being laid out, its line going on in the next.
    fn end_row(&self, line: &mut Run, pass: &mut Pass<'_, impl Reflow>) -> Result<(), Error> {
        let id = self.id_at(line.next, line.end);
        line.next = line.next.saturating_add(1);
        pass.target.row(
            pass.out.rows,
            line.used,
            (true, LineRendition::Single),
            id,
            line.prompt,
        )?;
        pass.out.rows = pass.out.rows.saturating_add(1);
        line.used = 0;
        line.prompt = false;
        Ok(())
    }

    /// The identity of retained row `index`, if it is at most `end`: the
    /// line's row whose place a row laid out takes.
    fn id_at(&self, index: usize, end: usize) -> Option<RowId> {
        if index > end {
            return None;
        }
        self.row_at(index).map(|row| row.id)
    }

    /// Retained row `index`'s facts a line's layout reads: whether it is
    /// soft-wrapped, its width, and how many of its cells come before its
    /// blank tail. A history row keeps none of its blank tail; a screen
    /// row's is looked for back from its `used` mark, past which every
    /// cell is blank.
    fn facts_at(&self, index: usize) -> (bool, usize, usize) {
        let Some(row) = index.checked_sub(self.history.len()) else {
            let kept = self.history.kept(index);
            return kept.map_or((false, 0, 0), |k| {
                (k.wrapped(), usize::from(k.width()), k.len())
            });
        };
        let width = usize::from(self.size.cols());
        let Some((slot, m)) =
            (self.order.get(row)).and_then(|&slot| Some((slot, self.meta.get(slot)?)))
        else {
            return (false, width, 0);
        };
        let cells = self.slice(slot);
        let text = cells
            .get(..usize::from(m.used).min(cells.len()))
            .unwrap_or_default()
            .iter()
            .rposition(|c| !c.is_blank(0))
            .map_or(0, |last| last.saturating_add(1));
        (m.wrapped, width, text)
    }
}

/// The links of a run of cells: their row's, if it has any, and where the
/// cells are in it.
type RunLinks<'a> = Option<(&'a RowLinks, Range<usize>)>;

/// Where a reflow's rows go: `Layout` only counts them; `Fill` writes the
/// ones kept into the replacement grid.
trait Reflow {
    /// `cells`, of a row whose text is in `text`, with links `links`, are
    /// at `col` on of reflowed row
    /// `row`, where they fit; the row's cells past them that the run
    /// covers, which it does not keep, are blank.
    fn run(&mut self, row: usize, col: usize, cells: &[Compact], text: &Text, links: RunLinks<'_>);
    /// Reflowed row `row` is finished, its cells from `used` on blank;
    /// `wrapped` if its line goes on, and its rendition; the identity of its
    /// line's row in the same place before, if any, and whether a prompt
    /// starts on it.
    fn row(
        &mut self,
        row: usize,
        used: usize,
        wrapped: (bool, LineRendition),
        id: Option<RowId>,
        prompt: bool,
    ) -> Result<(), Error>;
}

/// A reflow under way: the width it lays out at, the marks it places, what
/// it has laid out, and where the rows go.
struct Pass<'a, T> {
    width: usize,
    marks: [Option<(usize, usize)>; 2],
    out: Reflowed,
    target: &'a mut T,
}

/// A line being laid out a run at a time (`Grid::lay_out_runs`).
#[derive(Debug)]
struct Run {
    /// The cells laid out in the row being laid out.
    used: usize,
    /// The line's row whose identity the next row laid out takes, and its
    /// last row.
    next: usize,
    end: usize,
    /// Whether a prompt starts on the row being laid out, or on the row
    /// the next cell placed goes to.
    prompt: bool,
    pending: bool,
    /// Where the marks are in the line, each taken once placed.
    offsets: [Option<usize>; 2],
}

impl Run {
    /// Lays out cells `cells` of `row`, which starts `base` cells into the
    /// line, in the row being laid out, after its cells so far: they fit.
    fn place(
        &mut self,
        row: Row<'_>,
        base: usize,
        cells: Range<usize>,
        pass: &mut Pass<'_, impl Reflow>,
    ) {
        if cells.is_empty() || cells.end > row.width {
            return;
        }
        // The cells the row keeps of the run; the rest are blank.
        let kept = row.cells.len();
        let run = row
            .cells
            .get(cells.start.min(kept)..cells.end.min(kept))
            .unwrap_or_default();
        let start = base.saturating_add(cells.start);
        let end = base.saturating_add(cells.end);
        for (offset, mark) in self.offsets.iter_mut().zip(&mut pass.out.marks) {
            if let Some(offset) = offset.take_if(|offset| (start..end).contains(offset)) {
                *mark = Some(Mark::Text(
                    pass.out.rows,
                    self.used.saturating_add(offset.saturating_sub(start)),
                ));
            }
        }
        self.prompt |= self.pending;
        self.pending = false;
        let links = row.links.map(|links| (links, cells.clone()));
        pass.target
            .run(pass.out.rows, self.used, run, row.text, links);
        self.used = self.used.saturating_add(cells.len());
    }
}

/// Where a mark, the cursor or the saved cursor, is laid out: a reflowed
/// row and a column in it.
#[derive(Clone, Copy)]
enum Mark {
    /// On a glyph of its line's text, or just after the last: one past the
    /// last column waits to wrap.
    Text(usize, usize),
    /// In the blanks past the end of its line's text, as far past it as
    /// before: in the last column at most, never waiting to wrap, as
    /// resizing without reflow keeps it.
    Blank(usize, usize),
}

/// The shape of a reflow: how many rows, where the cursor and the saved
/// cursor are, and how many rows at the end hold blank lines.
struct Reflowed {
    rows: usize,
    /// `None` for a mark no line held.
    marks: [Option<Mark>; 2],
    trailing_blank: usize,
    /// The laid-out row the screen's line begins on: the line holding the
    /// screen's first row before.
    screen_line: usize,
}

struct Layout;
impl Reflow for Layout {
    fn run(&mut self, _: usize, _: usize, _: &[Compact], _: &Text, _: RunLinks<'_>) {}
    fn row(
        &mut self,
        _: usize,
        _: usize,
        _: (bool, LineRendition),
        _: Option<RowId>,
        _: bool,
    ) -> Result<(), Error> {
        Ok(())
    }
}

/// Writes reflowed rows `rows` into `grid`, which has none yet: each row is
/// laid out in `cells`, `text` and `links`, then goes into the grid's
/// history if it is above `screen`, else onto its screen. A cluster held in
/// its old row's text is stored again in its new row's.
struct Fill<'a> {
    grid: &'a mut Grid,
    rows: Range<usize>,
    screen: usize,
    next: &'a mut u64,
    version: u64,
    cells: Vec<Compact>,
    /// How far into `cells` the row being laid out was written: past it,
    /// they are blank.
    written: usize,
    text: Text,
    links: Option<RowLinks>,
}
impl Reflow for Fill<'_> {
    fn run(&mut self, row: usize, col: usize, cells: &[Compact], text: &Text, links: RunLinks<'_>) {
        if !self.rows.contains(&row) {
            return;
        }
        let Some(dst) = col
            .checked_add(cells.len())
            .and_then(|end| self.cells.get_mut(col..end))
        else {
            return;
        };
        self.written = self.written.max(col.saturating_add(cells.len()));
        work!(cells, cells.len());
        crate::copy_from(dst, cells);
        // The clusters held in the row's text are stored again, as placing
        // the cells one at a time stores them.
        if !text.is_empty() {
            let spilled = (cells.iter().enumerate())
                .filter(|(_, cell)| cell.is_spilled())
                .map(|(i, cell)| (col.saturating_add(i), *cell));
            let mut line = Line {
                cells: &mut self.cells,
                text: &mut self.text,
            };
            line.store_again(spilled, text);
        }
        if let Some((links, from)) = links {
            for (at, link) in links.within(from) {
                let at = col.saturating_add(at.start)..col.saturating_add(at.end);
                // The row's links, none until its first.
                self.links.get_or_insert_default().set(at, Some(link));
            }
        }
    }
    fn row(
        &mut self,
        row: usize,
        used: usize,
        (wrapped, rendition): (bool, LineRendition),
        id: Option<RowId>,
        prompt: bool,
    ) -> Result<(), Error> {
        if !self.rows.contains(&row) {
            return Ok(());
        }
        let id = id.unwrap_or_else(|| next_id(self.next));
        let cols = self.grid.size.cols();
        // The row's `used` mark is where its cells laid out end, so the
        // next reflow finds its text, and recycling clears it, from there.
        let used = u16::try_from(used).map_or(cols, |used| used.min(cols));
        // Past the mark, every cell is blank: no half of a wide glyph.
        if let Some(cells) = self.cells.get_mut(..usize::from(used)) {
            repair_wide(cells);
        }
        let text = std::mem::take(&mut self.text);
        let links = self.links.take();
        let written = std::mem::take(&mut self.written);
        let meta = Meta {
            prompt,
            rendition,
            ..Meta::new(id, self.version, wrapped, used)
        };
        let history = row < self.screen;
        let cells = (self.cells.as_slice(), written);
        (self.grid).keep_row(meta, cells, (Some(text), links), history)?;
        work!(cells, written);
        if let Some(cells) = self.cells.get_mut(..written) {
            cells.fill(BLANK);
        }
        Ok(())
    }
}
