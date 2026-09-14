//! Copy-mode key and selection handling, split out of the controller's key dispatcher.
//! These are `Controller` methods; `key` in the parent module dispatches into them.
use super::*;

impl Controller {
    pub(super) fn selection_dragging(&self) -> bool {
        matches!(&self.mode, Mode::Copy(copy) if copy.dragging())
    }

    pub(super) fn copy_key(&mut self, key: CopyKey) -> Option<Request> {
        let dragging = self.selection_dragging();
        let outcome = match &mut self.mode {
            Mode::Copy(copy) => copy.key(key),
            _ => CopyOutcome::Continue,
        };
        match outcome {
            CopyOutcome::Continue => {}
            CopyOutcome::Copied(text) => {
                self.end_interaction();
                self.copied = Some(text);
            }
            CopyOutcome::Finished => self.end_interaction(),
        }
        self.capture
            .cancel_left_if(dragging && !self.selection_dragging());
        None
    }

    pub(super) fn scroll_copy(&mut self, delta: i64) -> Option<Request> {
        if let Mode::Copy(copy) = &mut self.mode {
            copy.scroll(delta);
        }
        None
    }
}
