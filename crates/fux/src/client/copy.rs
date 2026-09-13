//! Viewer-private history browsing and selection over one pane. Positions refer to a specific
//! history offset; when a fresh view changes size or the offset the clamp moved, the selection is
//! cleared with visible feedback instead of copying different cells.

use crate::ids::PaneId;
use crate::proto::attach::ViewReply;
use crate::view::PaneView;

pub const SCROLL_STEP: u32 = 3;

// Process-wide IDs cannot collide when a viewer dismisses and recreates a session.
static NEXT_READ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[derive(Debug)]
struct PendingRead {
    id: u64,
    offset: u32,
}

#[derive(Debug)]
pub struct CopySession {
    pane: PaneId,
    view: PaneView,
    cursor: (u16, u16),
    anchor: Option<(u16, u16)>,
    dragging: bool,
    /// Offset the viewer wants; `view.offset` is what the server clamped to.
    wanted_offset: u32,
    history: u32,
    pending_read: Option<PendingRead>,
    refresh_needed: bool,
    live_size: (u16, u16),
    viewport_size: Option<(u16, u16)>,
    notice: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyKey {
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Anchor,
    Clear,
    Copy,
    Live,
    Quit,
    Escape,
}

pub enum CopyOutcome {
    /// Keep the mode; repaint if the view or selection changed.
    Continue,
    /// Copy this text and leave the mode.
    Copied(String),
    /// Leave the mode without copying.
    Finished,
}

impl CopySession {
    /// Starts at the live screen, cursor at the pane's cursor.
    #[must_use]
    pub fn new(pane: PaneId, view: PaneView) -> Self {
        let cursor = (
            view.cursor.row.min(view.rows.saturating_sub(1)),
            view.cursor.column.min(view.columns.saturating_sub(1)),
        );
        Self {
            pane,
            live_size: (view.rows, view.columns),
            viewport_size: None,
            view,
            cursor,
            anchor: None,
            dragging: false,
            wanted_offset: 0,
            history: 0,
            pending_read: None,
            refresh_needed: false,
            notice: None,
        }
    }

    /// Private history is clipped to this viewer's rectangle; shared PTY geometry
    /// follows the smallest attached viewer and can arrive in a later frame.
    pub fn set_viewport(&mut self, rows: u16, columns: u16) {
        let size = (rows, columns);
        if self.viewport_size != Some(size) {
            self.viewport_size = Some(size);
            if size != (self.view.rows, self.view.columns) {
                self.refresh_needed = true;
                self.clear_selection();
                self.notice = Some("Selection cleared: the history viewport changed");
            }
        }
    }

    pub fn same_buffer(&self, live: &PaneView) -> bool {
        self.view.modes.alternate_screen == live.modes.alternate_screen
    }

    fn viewport(mut view: PaneView, size: Option<(u16, u16)>) -> PaneView {
        let Some((rows, columns)) = size else {
            return view;
        };
        let rows = rows.min(view.rows);
        let columns = columns.min(view.columns);
        let first = view.rows.saturating_sub(rows);
        if (rows, columns) != (view.rows, view.columns) {
            let cells = view
                .cells
                .chunks(usize::from(view.columns.max(1)))
                .skip(usize::from(first))
                .take(usize::from(rows))
                .flat_map(|row| row.iter().take(usize::from(columns)).cloned())
                .collect();
            view.cells = cells;
            view.wrapped_rows = view
                .wrapped_rows
                .into_iter()
                .skip(usize::from(first))
                .take(usize::from(rows))
                .collect();
            view.cursor.row = view
                .cursor
                .row
                .saturating_sub(first)
                .min(rows.saturating_sub(1));
            view.cursor.column = view.cursor.column.min(columns.saturating_sub(1));
            view.rows = rows;
            view.columns = columns;
        }
        view
    }

    /// Owned viewport allocation, including cell text and row metadata.
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(
                self.view
                    .cells
                    .capacity()
                    .saturating_mul(std::mem::size_of::<crate::view::Cell>()),
            )
            .saturating_add(
                self.view
                    .cells
                    .iter()
                    .map(|cell| cell.text.capacity())
                    .sum::<usize>(),
            )
            .saturating_add(self.view.wrapped_rows.capacity())
            .saturating_add(self.view.title.capacity())
            .saturating_add(self.view.label.as_ref().map_or(0, String::capacity))
    }

    pub const fn pane(&self) -> PaneId {
        self.pane
    }
    pub fn view(&self) -> &PaneView {
        &self.view
    }
    pub fn cursor(&self) -> (u16, u16) {
        self.cursor
    }
    pub fn anchor(&self) -> Option<(u16, u16)> {
        self.anchor
    }
    pub fn offset(&self) -> u32 {
        self.view.offset
    }
    pub fn selecting(&self) -> bool {
        self.anchor.is_some()
    }
    pub fn notice(&self) -> Option<&'static str> {
        self.notice
    }
    /// The next history read to send, at most one outstanding.
    pub fn take_read(&mut self) -> Option<(u64, PaneId, u32)> {
        if self.pending_read.is_some() {
            return None;
        }
        if self.wanted_offset == self.view.offset && !self.refresh_needed {
            return None;
        }
        let request = NEXT_READ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.pending_read = Some(PendingRead {
            id: request,
            offset: self.wanted_offset,
        });
        self.refresh_needed = false;
        Some((request, self.pane, self.wanted_offset))
    }

    pub fn pending_matches(&self, request: u64, pane: PaneId) -> bool {
        self.pane == pane
            && self
                .pending_read
                .as_ref()
                .is_some_and(|pending| pending.id == request)
    }

    pub fn awaiting_read(&self) -> bool {
        self.pending_read.is_some()
    }

    /// Installs a reply. Returns false when the pane is gone and the mode must end.
    pub fn install(&mut self, reply: ViewReply) -> bool {
        if reply.pane != self.pane
            || self.pending_read.as_ref().map(|pending| pending.id) != Some(reply.request)
        {
            return true;
        }
        let sent_offset = self.pending_read.take().map(|pending| pending.offset);
        let Some(view) = reply
            .view
            .and_then(|view| PaneView::from_update(&view).ok())
        else {
            return false;
        };
        // The application may switch buffers before the live frame reaches this
        // viewer. A correlated request still cannot replace a different buffer.
        if !self.same_buffer(&view) {
            return false;
        }
        self.refresh_needed |= (view.rows, view.columns) != self.live_size;
        let view = Self::viewport(view, self.viewport_size);
        let resized = (view.rows, view.columns) != (self.view.rows, self.view.columns);
        let moved = view.offset != self.view.offset;
        if (resized || moved) && self.anchor.is_some() {
            self.clear_selection();
            self.notice = Some("Selection cleared: the view changed");
        }
        self.history = reply.history;
        if sent_offset == Some(self.wanted_offset) && view.offset != self.wanted_offset {
            // The clamp stopped moving: there is no more history in that direction.
            self.wanted_offset = view.offset;
        }
        self.view = view;
        self.clamp_cursor();
        true
    }

    fn clamp_cursor(&mut self) {
        self.cursor = (
            self.cursor.0.min(self.view.rows.saturating_sub(1)),
            self.cursor.1.min(self.view.columns.saturating_sub(1)),
        );
    }

    /// A newer live frame for the pane while browsing at offset zero: adopt it, keeping the
    /// selection only if the geometry is unchanged.
    pub fn refresh_live(&mut self, live: &PaneView) {
        let size = (live.rows, live.columns);
        if size != self.live_size {
            self.live_size = size;
            self.refresh_needed = true;
            self.clear_selection();
            self.notice = Some("Selection cleared: the pane was resized");
        }
        if self.view.offset != 0 || self.pending_read.is_some() {
            return;
        }
        let live = Self::viewport(live.clone(), self.viewport_size);
        if (live.rows, live.columns) != (self.view.rows, self.view.columns) && self.anchor.is_some()
        {
            self.clear_selection();
            self.notice = Some("Selection cleared: the pane was resized");
        }
        if self.anchor.is_some() && live.cells != self.view.cells {
            // New output replaced the selected cells; never copy text the user did not see.
            self.clear_selection();
            self.notice = Some("Selection cleared: new output arrived");
        }
        self.refresh_needed = false;
        self.view = live;
        self.clamp_cursor();
    }

    pub fn key(&mut self, key: CopyKey) -> CopyOutcome {
        self.notice = None;
        let rows = self.view.rows.saturating_sub(1);
        let columns = self.view.columns.saturating_sub(1);
        match key {
            CopyKey::Left => self.cursor.1 = self.cursor.1.saturating_sub(1),
            CopyKey::Right => self.cursor.1 = self.cursor.1.saturating_add(1).min(columns),
            CopyKey::Up => {
                if self.cursor.0 == 0 {
                    self.scroll(SCROLL_STEP as i64);
                } else {
                    self.cursor.0 -= 1;
                }
            }
            CopyKey::Down => {
                if self.cursor.0 >= rows {
                    self.scroll(-(SCROLL_STEP as i64));
                } else {
                    self.cursor.0 += 1;
                }
            }
            CopyKey::PageUp => self.scroll(i64::from(self.view.rows.max(1))),
            CopyKey::PageDown => self.scroll(-i64::from(self.view.rows.max(1))),
            CopyKey::Anchor => {
                self.dragging = false;
                self.anchor = Some(self.cursor);
            }
            CopyKey::Clear => self.clear_selection(),
            CopyKey::Copy => {
                if let Some(anchor) = self.anchor {
                    let text = self.view.text_between(anchor, self.cursor);
                    return CopyOutcome::Copied(text);
                }
            }
            CopyKey::Live => {
                self.wanted_offset = 0;
                if self.anchor.is_some() {
                    self.clear_selection();
                    self.notice = Some("Selection cleared: returned to live output");
                }
            }
            CopyKey::Quit | CopyKey::Escape => {
                self.clear_selection();
                return CopyOutcome::Finished;
            }
        }
        CopyOutcome::Continue
    }

    /// Wheel or explicit scrolling: positive moves into history. Scrolling invalidates a selection
    /// because the displayed cells change.
    pub fn scroll(&mut self, delta: i64) {
        self.dragging = false;
        let current = i64::from(self.wanted_offset);
        let target = (current + delta).clamp(0, i64::from(u32::MAX));
        let target = u32::try_from(target).unwrap_or(0);
        if target != self.wanted_offset {
            if self.anchor.is_some() {
                self.clear_selection();
                self.notice = Some("Selection cleared: scrolled");
            }
            self.wanted_offset = target;
        }
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
        self.dragging = false;
    }

    pub fn dragging(&self) -> bool {
        self.dragging
    }

    /// Pane-relative drag selection (zero-based content coordinates).
    pub fn drag(&mut self, row: u16, column: u16, release: bool) {
        self.notice = None;
        if self.view.rows == 0 || self.view.columns == 0 {
            return;
        }
        let point = (
            row.min(self.view.rows - 1),
            column.min(self.view.columns - 1),
        );
        if !self.dragging {
            self.anchor = Some(point);
        }
        self.cursor = point;
        self.dragging = !release;
    }

    pub fn hint(&self) -> String {
        let where_ = if self.view.offset == 0 {
            "live".to_owned()
        } else {
            format!("history -{} of {}", self.view.offset, self.history)
        };
        if let Some(notice) = self.notice {
            return format!("Copy · Esc finish · {where_} · {notice}");
        }
        if self.selecting() {
            format!(
                "Copy selection · Esc finish · {where_} · arrows/hjkl extend · y/Enter copy · c clear"
            )
        } else {
            format!(
                "Copy · Esc finish · {where_} · arrows/hjkl move · Space select · u/d PgUp/PgDn scroll · g live · q finish"
            )
        }
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    fn view(rows: u16, columns: u16, text: &str, offset: u32) -> PaneView {
        let mut parser = vt100::Parser::new(rows, columns, 0);
        parser.process(text.as_bytes());
        PaneView::from_screen(parser.screen(), "", offset, None).unwrap_or_default()
    }

    fn update(rows: u16, columns: u16, text: &str, offset: u32) -> crate::view::PaneUpdate {
        let mut parser = vt100::Parser::new(rows, columns, 0);
        parser.process(text.as_bytes());
        crate::view::PaneUpdate::full_from_screen(parser.screen(), "", offset, None)
            .unwrap_or_default()
    }

    #[test]
    fn a_reply_from_another_screen_buffer_cannot_replace_the_session() {
        for alternate in [false, true] {
            let original = if alternate {
                "\x1b[?1049hbefore"
            } else {
                "before"
            };
            let other = if alternate {
                "after"
            } else {
                "\x1b[?1049hafter"
            };
            let mut session = CopySession::new(PaneId(1), view(3, 10, original, 0));
            session.scroll(3);
            let (request, pane, _) = session
                .take_read()
                .unwrap_or_else(|| panic!("pending read"));
            assert!(
                !session.install(ViewReply {
                    request,
                    pane,
                    view: Some(Box::new(update(3, 10, other, 0))),
                    history: 0,
                }),
                "mismatched buffer reply must dismiss the old interaction"
            );
            assert_eq!(session.view().modes.alternate_screen, alternate);
        }
    }

    #[test]
    fn recreated_session_does_not_accept_an_old_reply() {
        let mut old = CopySession::new(PaneId(1), view(3, 6, "live", 0));
        old.scroll(3);
        let (old_request, pane, _) = old
            .take_read()
            .unwrap_or_else(|| panic!("missing fixture value"));
        let mut new = CopySession::new(pane, view(3, 6, "new", 0));
        new.scroll(6);
        let (new_request, _, _) = new
            .take_read()
            .unwrap_or_else(|| panic!("missing fixture value"));
        assert_ne!(new_request, old_request);
        new.install(ViewReply {
            request: old_request,
            pane,
            view: None,
            history: 0,
        });
        assert!(new.pending_matches(new_request, pane));
        assert_eq!(new.offset(), 0);
    }

    #[test]
    fn latest_scroll_intent_survives_an_older_reply() {
        let mut session = CopySession::new(PaneId(1), view(3, 6, "live", 0));
        session.scroll(3);
        let (request, pane, _) = session
            .take_read()
            .unwrap_or_else(|| panic!("missing fixture value"));
        session.scroll(6);
        session.install(ViewReply {
            request,
            pane,
            view: Some(Box::new(update(3, 6, "old", 3))),
            history: 30,
        });
        assert_eq!(session.take_read().map(|(_, _, offset)| offset), Some(9));
    }

    #[test]
    fn wheel_bursts_coalesce_and_only_the_latest_reply_clamps_intent() {
        let mut session = CopySession::new(PaneId(1), view(3, 6, "live", 0));
        session.scroll(3);
        let (first, pane, _) = session.take_read().unwrap_or_else(|| panic!("first read"));
        for _ in 0..100 {
            session.scroll(3);
            assert!(
                session.take_read().is_none(),
                "burst created parallel reads for one pane"
            );
        }
        session.install(ViewReply {
            request: first,
            pane,
            view: Some(Box::new(update(3, 6, "old", 2))),
            history: 2,
        });
        let (latest, _, offset) = session
            .take_read()
            .unwrap_or_else(|| panic!("coalesced read"));
        assert_eq!(offset, 303, "old clamp erased newer wheel intent");
        session.install(ViewReply {
            request: latest,
            pane,
            view: Some(Box::new(update(3, 6, "old", 2))),
            history: 2,
        });
        assert_eq!(session.offset(), 2);
        assert!(
            session.take_read().is_none(),
            "settled upper clamp caused a reread loop"
        );
        session.scroll(-1000);
        let (live, _, offset) = session.take_read().unwrap_or_else(|| panic!("live read"));
        assert_eq!(offset, 0);
        session.install(ViewReply {
            request: live,
            pane,
            view: Some(Box::new(update(3, 6, "live", 0))),
            history: 2,
        });
        assert!(
            session.take_read().is_none(),
            "settled lower clamp caused a reread loop"
        );
    }

    #[test]
    fn private_viewport_resize_refreshes_once_without_shared_pty_resize() {
        let mut session = CopySession::new(PaneId(1), view(23, 40, "live", 0));
        session.scroll(3);
        let (request, pane, _) = session
            .take_read()
            .unwrap_or_else(|| panic!("initial read"));
        session.install(ViewReply {
            request,
            pane,
            view: Some(Box::new(update(23, 40, "old", 3))),
            history: 80,
        });
        session.set_viewport(11, 30);
        session.refresh_live(&view(23, 40, "live", 0));
        let (request, _, offset) = session
            .take_read()
            .unwrap_or_else(|| panic!("viewport refresh"));
        assert_eq!(offset, 3);
        session.install(ViewReply {
            request,
            pane,
            view: Some(Box::new(update(23, 40, "old", 3))),
            history: 80,
        });
        assert_eq!((session.view.rows, session.view.columns), (11, 30));
        assert_eq!(session.view.cells.len(), 330);
        assert!(
            session.take_read().is_none(),
            "cropping must not cause a read loop"
        );
        session.set_viewport(23, 40);
        assert_eq!(session.take_read().map(|(_, _, offset)| offset), Some(3));
    }

    #[test]
    fn history_resize_requires_a_fresh_read_at_the_same_offset() {
        let mut session = CopySession::new(PaneId(1), view(3, 6, "live", 0));
        session.scroll(3);
        let (request, pane, _) = session
            .take_read()
            .unwrap_or_else(|| panic!("missing fixture value"));
        session.install(ViewReply {
            request,
            pane,
            view: Some(Box::new(update(3, 6, "old", 3))),
            history: 30,
        });
        session.refresh_live(&view(5, 9, "live", 0));
        assert_eq!(session.take_read().map(|(_, _, offset)| offset), Some(3));
    }

    #[test]
    fn escape_finishes_selection_in_one_press() {
        let mut session = CopySession::new(PaneId(1), view(3, 6, "live", 0));
        session.key(CopyKey::Anchor);
        assert!(matches!(
            session.key(CopyKey::Escape),
            CopyOutcome::Finished
        ));
        assert!(!session.selecting());
    }

    #[test]
    fn selection_copies_visible_text_and_clears_on_view_change() {
        let mut session = CopySession::new(PaneId(1), view(3, 6, "hello\r\nworld", 0));
        session.cursor = (0, 0);
        assert!(matches!(
            session.key(CopyKey::Anchor),
            CopyOutcome::Continue
        ));
        for _ in 0..4 {
            session.key(CopyKey::Right);
        }
        session.key(CopyKey::Down);
        match session.key(CopyKey::Copy) {
            CopyOutcome::Copied(text) => assert_eq!(text, "hello\nworld"),
            _ => panic!("expected copy"),
        }
        let mut session = CopySession::new(PaneId(1), view(3, 6, "hello", 0));
        session.key(CopyKey::Anchor);
        session.key(CopyKey::Up);
        assert!(session.anchor().is_none(), "scrolling clears the selection");
        let (request, pane, offset) = session
            .take_read()
            .unwrap_or_else(|| panic!("missing fixture value"));
        assert_eq!((pane, offset), (PaneId(1), 3));
        assert!(session.take_read().is_none(), "one read outstanding");
        let reply = ViewReply {
            request,
            pane: PaneId(1),
            view: Some(Box::new(update(3, 6, "older", 2))),
            history: 2,
        };
        assert!(session.install(reply));
        assert_eq!(session.offset(), 2);
        assert!(session.hint().contains("history -2 of 2"));
        session.key(CopyKey::Anchor);
        session.refresh_live(&view(3, 6, "changed", 0));
        assert!(
            session.selecting(),
            "live frames do not disturb history views"
        );
        session.key(CopyKey::Live);
        assert!(!session.selecting());
        let (request, _, _) = session
            .take_read()
            .unwrap_or_else(|| panic!("missing fixture value"));
        let gone = ViewReply {
            request,
            pane: PaneId(1),
            view: None,
            history: 0,
        };
        assert!(!session.install(gone));
    }

    #[test]
    fn new_output_under_a_live_selection_clears_it_with_feedback() {
        let mut session = CopySession::new(PaneId(1), view(3, 6, "hello", 0));
        session.key(CopyKey::Anchor);
        session.refresh_live(&view(3, 6, "bye", 0));
        assert!(!session.selecting());
        assert!(
            session
                .notice()
                .is_some_and(|notice| notice.contains("new output"))
        );
        session.refresh_live(&view(4, 8, "bye", 0));
        assert_eq!(session.view().rows, 4);
    }

    #[test]
    fn drag_selection_stays_within_the_pane() {
        let mut session = CopySession::new(PaneId(1), view(3, 6, "abcdef", 0));
        session.drag(0, 1, false);
        session.drag(9, 99, false);
        assert_eq!(session.anchor(), Some((0, 1)));
        assert_eq!(session.cursor(), (2, 5));
        session.drag(2, 5, true);
        match session.key(CopyKey::Copy) {
            CopyOutcome::Copied(text) => assert_eq!(text, "bcdef\n\n"),
            _ => panic!("expected copy"),
        }
        let mut session = CopySession::new(PaneId(1), view(3, 6, "abcdef", 0));
        session.drag(0, 1, false);
        session.drag(0, 4, true);
        match session.key(CopyKey::Copy) {
            CopyOutcome::Copied(text) => assert_eq!(text, "bcde"),
            _ => panic!("expected copy"),
        }
    }
}
