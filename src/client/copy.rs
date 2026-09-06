//! Viewer-private history browsing and selection over one pane. Positions refer to a specific
//! history offset; when a fresh view changes size or the offset the clamp moved, the selection is
//! cleared with visible feedback instead of copying different cells.

use crate::ids::PaneId;
use crate::proto::attach::ViewReply;
use crate::view::PaneView;

pub const SCROLL_STEP: u32 = 3;

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
    pending_read: Option<u64>,
    next_request: u64,
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
            view,
            cursor,
            anchor: None,
            dragging: false,
            wanted_offset: 0,
            history: 0,
            pending_read: None,
            next_request: 1,
            notice: None,
        }
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
    pub fn clear_notice(&mut self) {
        self.notice = None;
    }

    /// The next history read to send, at most one outstanding.
    pub fn take_read(&mut self) -> Option<(u64, PaneId, u32)> {
        if self.pending_read.is_some() {
            return None;
        }
        if self.wanted_offset == self.view.offset {
            return None;
        }
        let request = self.next_request;
        self.next_request = self.next_request.wrapping_add(1);
        self.pending_read = Some(request);
        Some((request, self.pane, self.wanted_offset))
    }

    pub fn awaiting_read(&self) -> bool {
        self.pending_read.is_some()
    }

    /// Installs a reply. Returns false when the pane is gone and the mode must end.
    pub fn install(&mut self, reply: ViewReply) -> bool {
        if reply.pane != self.pane || self.pending_read != Some(reply.request) {
            return true;
        }
        self.pending_read = None;
        let Some(view) = reply.view else {
            return false;
        };
        let resized = (view.rows, view.columns) != (self.view.rows, self.view.columns);
        let moved = view.offset != self.view.offset;
        if (resized || moved) && self.anchor.is_some() {
            self.anchor = None;
            self.notice = Some("Selection cleared: the view changed");
        }
        self.history = reply.history;
        if view.offset != self.wanted_offset {
            // The clamp stopped moving: there is no more history in that direction.
            self.wanted_offset = view.offset;
        }
        self.view = *view;
        self.cursor = (
            self.cursor.0.min(self.view.rows.saturating_sub(1)),
            self.cursor.1.min(self.view.columns.saturating_sub(1)),
        );
        true
    }

    /// A newer live frame for the pane while browsing at offset zero: adopt it, keeping the
    /// selection only if the geometry is unchanged.
    pub fn refresh_live(&mut self, live: &PaneView) {
        if self.view.offset != 0 || self.pending_read.is_some() {
            return;
        }
        if (live.rows, live.columns) != (self.view.rows, self.view.columns) {
            if self.anchor.is_some() {
                self.anchor = None;
                self.notice = Some("Selection cleared: the pane was resized");
            }
            self.cursor = (
                self.cursor.0.min(live.rows.saturating_sub(1)),
                self.cursor.1.min(live.columns.saturating_sub(1)),
            );
        }
        if self.anchor.is_some() && live.cells != self.view.cells {
            // New output replaced the selected cells; never copy text the user did not see.
            self.anchor = None;
            self.notice = Some("Selection cleared: new output arrived");
        }
        self.view = live.clone();
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
            CopyKey::Anchor => self.anchor = Some(self.cursor),
            CopyKey::Copy => {
                if let Some(anchor) = self.anchor {
                    let text = self.view.text_between(anchor, self.cursor);
                    return CopyOutcome::Copied(text);
                }
            }
            CopyKey::Live => {
                self.wanted_offset = 0;
                if self.anchor.is_some() {
                    self.anchor = None;
                    self.notice = Some("Selection cleared: returned to live output");
                }
            }
            CopyKey::Quit => return CopyOutcome::Finished,
            CopyKey::Escape => {
                if self.anchor.is_some() {
                    self.anchor = None;
                } else {
                    return CopyOutcome::Finished;
                }
            }
        }
        CopyOutcome::Continue
    }

    /// Wheel or explicit scrolling: positive moves into history. Scrolling invalidates a selection
    /// because the displayed cells change.
    pub fn scroll(&mut self, delta: i64) {
        let current = i64::from(self.wanted_offset);
        let target = (current + delta).clamp(0, i64::from(u32::MAX));
        let target = u32::try_from(target).unwrap_or(0);
        if target != self.wanted_offset {
            if self.anchor.is_some() {
                self.anchor = None;
                self.notice = Some("Selection cleared: scrolled");
            }
            self.wanted_offset = target;
        }
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
            return format!("Copy · {where_} · {notice} · Esc back");
        }
        if self.selecting() {
            format!("Copy selection · {where_} · arrows/hjkl extend · y/Enter copy · Esc clear")
        } else {
            format!(
                "Copy · {where_} · arrows/hjkl move · Space select · u/d PgUp/PgDn scroll · g live · q finish · Esc back"
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
        assert_eq!(
            session.take_read().map(|(_, pane, offset)| (pane, offset)),
            Some((PaneId(1), 3))
        );
        assert!(session.take_read().is_none(), "one read outstanding");
        let reply = ViewReply {
            request: 1,
            pane: PaneId(1),
            view: Some(Box::new(view(3, 6, "older", 2))),
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
        let gone = ViewReply {
            request: 2,
            pane: PaneId(1),
            view: None,
            history: 0,
        };
        assert!(session.take_read().is_some());
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
