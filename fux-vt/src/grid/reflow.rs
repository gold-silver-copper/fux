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
    /// dropped first past the history limit; rows below the screen once
    /// the cursor's row is at its top are dropped. The scroll region is
    /// reset, as xterm does on resize. Like `resized`, it builds replacement
    /// storage first, so allocation failure leaves this grid unchanged.
    ///
    /// Two walks over the rows, one to lay them out and one to copy them,
    /// so no line is ever gathered in memory of its own. The k-th row of a
    /// reflowed line keeps the identity of the line's k-th row before, if it
    /// had one; every row takes `version`.
    pub fn reflowed(&self, size: Size, next: &mut u64, version: u64) -> Result<Self, Error> {
        Self::check_size(size, self.history_limit)?;
        let cols = size.cols();
        // The cursor and the saved cursor go with their characters; one
        // waiting to wrap is laid out one past its glyph.
        let at = |cursor: Cursor| {
            self.history_len()
                .checked_add(usize::from(cursor.row))
                .map(|row| (row, usize::from(past(cursor.col, cursor.pending_wrap))))
        };
        let marks = [at(self.cursor), at(self.saved)];
        let layout = self.reflow(usize::from(cols), marks, &mut Layout)?;
        // Blank lines below the cursor are dropped before any line scrolls
        // into history: a mostly empty screen keeps its text on screen.
        let screen = usize::from(size.rows());
        let [(cursor_row, cursor_col), (saved_row, saved_col)] = layout.marks;
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
        let base = live_top.saturating_sub(self.history_limit);
        let end = total.min(live_top.checked_add(screen).ok_or(Error::Capacity)?);
        // A row on the screen, the first if above it, the last if below;
        // one past the last column is the last column, waiting to wrap.
        let placed = |cursor: Cursor, (row, col): (usize, usize)| Cursor {
            pending_wrap: col >= usize::from(cols),
            ..cursor.placed(
                (
                    u16::try_from(row.saturating_sub(live_top)).unwrap_or(u16::MAX),
                    u16::try_from(col).unwrap_or(u16::MAX),
                ),
                size,
            )
        };
        let mut replacement = Self {
            // The cursor's row is on screen: `live_top` is at most its row,
            // and the screen reaches past it.
            cursor: placed(self.cursor, (cursor_row, cursor_col)),
            // The saved cursor moves with its character as the cursor
            // does, so DECRC (as 1049 leaves the alternate screen) finds it.
            saved: placed(self.saved, (saved_row, saved_col)),
            ..self.successor(size)
        };
        replacement.reserve_screen()?;
        let mut copy = Fill {
            grid: &mut replacement,
            rows: base..end,
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
            let meta = Meta::new(next_id(next), version, cols, false, 0);
            replacement.push_screen_row(meta, &[], None, None);
        }
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
                marks: [(0, 0); 2],
                trailing_blank: 0,
                screen_line: 0,
            },
            found: [false; 2],
            target,
        };
        let mut start = 0;
        while start < retained {
            // A line runs through its wrapped rows to the row that ends it.
            let mut end = start;
            while end.checked_add(1).is_some_and(|next| next < retained) && self.wrapped_at(end) {
                end = end.saturating_add(1);
            }
            if (start..=end).contains(&self.history_len()) {
                pass.out.screen_line = pass.out.rows;
            }
            self.lay_out_runs(start, end, &mut pass)?;
            start = end.saturating_add(1);
        }
        let mut out = pass.out;
        for (mark, found) in out.marks.iter_mut().zip(pass.found) {
            if !found {
                *mark = (out.rows.saturating_sub(1), 0);
            }
        }
        Ok(out)
    }

    /// Lays out the line of rows `start` through `end`, a run of cells at a
    /// time: as many of a row's as fit in the row being laid out, short of a
    /// wide glyph that would end in its last column, or a spacer, which no
    /// cell is laid out from. At a width under two, wide glyphs are left out,
    /// as one column cannot show them. So a line costs its rows and the rows it is laid
    /// out in, not its cells; one whose rows stay as they are (every line
    /// but a few, when only the rows change) costs a run a row. The line's
    /// length comes from its rows' widths and `used` marks.
    fn lay_out_runs(
        &self,
        start: usize,
        end: usize,
        pass: &mut Pass<'_, impl Reflow>,
    ) -> Result<(), Error> {
        let length = self.text_length(start, end);
        let mut line = Run {
            used: 0,
            next: start,
            end,
            prompt: false,
            pending: false,
            offsets: [None; 2],
            placed: [false; 2],
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
            if i != end && self.spacer(row, i) {
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
        for (((offset, mark), placed), found) in line
            .offsets
            .iter()
            .zip(&mut out.marks)
            .zip(&mut line.placed)
            .zip(pass.found.iter_mut())
        {
            if let Some(offset) = offset
                && !*placed
            {
                // At or past the end of the line's text: as far past it
                // on the last row, at most waiting to wrap after the
                // last column.
                let past = offset.saturating_sub(length);
                *mark = (out.rows, line.used.saturating_add(past).min(pass.width));
                *placed = true;
            }
            *found |= *placed;
        }
        let id = self.id_at(line.next, end);
        pass.target
            .row(out.rows, line.used, false, id, line.prompt)?;
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
        pass.target
            .row(pass.out.rows, line.used, true, id, line.prompt)?;
        pass.out.rows = pass.out.rows.saturating_add(1);
        line.used = 0;
        line.prompt = false;
        Ok(())
    }

    /// The length of the line from row `start` through `end` without its
    /// blank tail, which can run back over several rows: found from the
    /// rows' widths and their cells back from their `used` marks.
    fn text_length(&self, start: usize, end: usize) -> usize {
        let mut length = (start..=end).fold(0usize, |sum, i| sum.saturating_add(self.width_at(i)));
        for i in (start..=end).rev() {
            length = length.saturating_sub(self.width_at(i));
            let text = self.text_end(i);
            if text > 0 {
                return length.saturating_add(text);
            }
        }
        0
    }

    /// Whether `row`, retained row `index`, soft-wrapped, ends in a spacer:
    /// a blank that a wide glyph starting the next row did not fit in.
    fn spacer(&self, row: Row<'_>, index: usize) -> bool {
        row.width
            .checked_sub(1)
            .and_then(|col| row.stored(col))
            .is_some_and(|c| c.is_blank(0))
            && self
                .row_at(index.saturating_add(1))
                .and_then(|next| next.stored(0))
                .is_some_and(Compact::is_wide)
    }

    /// The identity of retained row `index`, if it is at most `end`: the
    /// line's row whose place a row laid out takes.
    fn id_at(&self, index: usize, end: usize) -> Option<RowId> {
        if index > end {
            return None;
        }
        self.row_at(index).map(|row| row.id)
    }

    /// Whether retained row `index` is soft-wrapped.
    fn wrapped_at(&self, index: usize) -> bool {
        match index.checked_sub(self.history.len()) {
            None => self.history.kept(index).is_some_and(|kept| kept.wrapped()),
            Some(row) => self
                .screen_slot(row)
                .and_then(|slot| self.meta.get(slot))
                .is_some_and(|m| m.wrapped),
        }
    }

    /// Retained row `index`'s width: how many cells it has.
    fn width_at(&self, index: usize) -> usize {
        match index.checked_sub(self.history.len()) {
            None => self
                .history
                .kept(index)
                .map_or(0, |kept| usize::from(kept.width())),
            Some(row) => self
                .screen_slot(row)
                .and_then(|slot| self.meta.get(slot))
                .map_or(0, |m| usize::from(m.width)),
        }
    }

    /// How many of retained row `index`'s cells come before its blank
    /// tail: looked for back from its `used` mark, past which every cell
    /// is blank. A history row keeps none of its blank tail.
    fn text_end(&self, index: usize) -> usize {
        let Some(row) = index.checked_sub(self.history.len()) else {
            return self.history.kept(index).map_or(0, |kept| kept.len());
        };
        let Some(slot) = self.screen_slot(row) else {
            return 0;
        };
        let used = self.meta.get(slot).map_or(0, |m| usize::from(m.used));
        let cells = self.slice(slot);
        cells
            .get(..used.min(cells.len()))
            .unwrap_or_default()
            .iter()
            .rposition(|c| !c.is_blank(0))
            .map_or(0, |last| last.saturating_add(1))
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
    /// `wrapped` if its line goes on, the identity of its line's row in the
    /// same place before, if any, and whether a prompt starts on it.
    fn row(
        &mut self,
        row: usize,
        used: usize,
        wrapped: bool,
        id: Option<RowId>,
        prompt: bool,
    ) -> Result<(), Error>;
}

/// A reflow under way: the width it lays out at, the marks it places, what
/// it has laid out, which marks it has found, and where the rows go.
struct Pass<'a, T> {
    width: usize,
    marks: [Option<(usize, usize)>; 2],
    out: Reflowed,
    found: [bool; 2],
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
    /// Where the marks are in the line, and whether they have been placed.
    offsets: [Option<usize>; 2],
    placed: [bool; 2],
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
        for ((offset, mark), placed) in self
            .offsets
            .iter()
            .zip(&mut pass.out.marks)
            .zip(&mut self.placed)
        {
            if let Some(offset) = *offset
                && (start..end).contains(&offset)
            {
                *mark = (
                    pass.out.rows,
                    self.used.saturating_add(offset.saturating_sub(start)),
                );
                *placed = true;
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

/// The shape of a reflow: how many rows, where the cursor and the saved
/// cursor are, and how many rows at the end hold blank lines.
struct Reflowed {
    rows: usize,
    marks: [(usize, usize); 2],
    trailing_blank: usize,
    /// The laid-out row the screen's line begins on: the line holding the
    /// screen's first row before.
    screen_line: usize,
}

struct Layout;
impl Reflow for Layout {
    fn run(&mut self, _: usize, _: usize, _: &[Compact], _: &Text, _: RunLinks<'_>) {}
    fn row(&mut self, _: usize, _: usize, _: bool, _: Option<RowId>, _: bool) -> Result<(), Error> {
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
impl Fill<'_> {
    /// The links of the row being laid out, none at first.
    fn links(&mut self) -> &mut RowLinks {
        self.links.get_or_insert_default()
    }
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
        crate::copy_from(dst, cells);
        // The clusters held in the row's text are stored again, left to
        // right, as placing the cells one at a time stores them: until
        // then, a cell locates nothing in the new row's text.
        if !text.is_empty() && cells.iter().any(Compact::is_spilled) {
            for (cell, src) in dst.iter_mut().zip(cells) {
                if src.is_spilled() {
                    *cell = src.blanked();
                }
            }
            let mut line = Line {
                cells: &mut self.cells,
                text: &mut self.text,
            };
            for (i, cell) in cells.iter().enumerate() {
                if cell.is_spilled() {
                    line.set(col.saturating_add(i), *cell, text.of(cell));
                }
            }
        }
        if let Some((links, from)) = links {
            for (at, link) in links.within(from) {
                let at = col.saturating_add(at.start)..col.saturating_add(at.end);
                self.links().set(at, Some(link));
            }
        }
    }
    fn row(
        &mut self,
        row: usize,
        used: usize,
        wrapped: bool,
        id: Option<RowId>,
        prompt: bool,
    ) -> Result<(), Error> {
        if !self.rows.contains(&row) {
            return Ok(());
        }
        let id = match id {
            Some(id) => id,
            None => next_id(self.next),
        };
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
        if row < self.screen {
            let cells = trimmed(self.cells.get(..written).unwrap_or_default());
            self.grid.history.push(Arriving {
                id,
                version: self.version,
                wrapped,
                prompt,
                cells,
                width: cols,
            })?;
            if !text.is_empty() || links.is_some() {
                self.grid.history.attach(Some(text.exact()), links);
            }
        } else {
            let mut meta = Meta::new(id, self.version, cols, wrapped, used);
            meta.prompt = prompt;
            self.grid
                .push_screen_row(meta, &self.cells, Some(text), links);
        }
        if let Some(cells) = self.cells.get_mut(..written) {
            cells.fill(BLANK);
        }
        Ok(())
    }
}
