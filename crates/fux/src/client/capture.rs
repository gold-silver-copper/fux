//! Ownership of gesture tails after a local interaction ends or changes mode.
use crate::proto::attach::MouseEvent;

#[derive(Default)]
pub(super) struct Capture {
    left: bool,
    auxiliary: Option<u16>,
}
impl Capture {
    pub fn adopt(&mut self, button: u16) {
        match button {
            0 => self.left = true,
            1 | 2 => self.auxiliary = Some(button),
            _ => {}
        }
    }
    pub fn cancel_left_if(&mut self, cancelled: bool) {
        self.left |= cancelled;
    }
    pub fn finish_left(&mut self, released: bool) {
        self.left = !released;
    }
    pub fn remember_local_press(&mut self, mouse: MouseEvent) {
        if !mouse.release && !mouse.motion() && !mouse.wheel() && matches!(mouse.button(), 1 | 2) {
            self.adopt(mouse.button());
        }
    }
    /// Captured tails never reach another owner. A fresh press recovers from a
    /// lost release, while wheel events neither end capture nor become tails.
    pub fn consume(&mut self, mouse: MouseEvent) -> bool {
        if self.left {
            if mouse.release {
                if super::drag::Drag::accepts(mouse) {
                    self.left = false;
                }
                return true;
            }
            if mouse.motion() {
                return true;
            }
            if !mouse.wheel() {
                self.left = false;
            }
        }
        if self.auxiliary == Some(mouse.button()) && !mouse.wheel() {
            if mouse.release || !mouse.motion() {
                self.auxiliary = None;
            }
            if mouse.release || mouse.motion() {
                return true;
            }
        }
        false
    }
    #[cfg(test)]
    pub fn left_pending(&self) -> bool {
        self.left
    }
    #[cfg(test)]
    pub fn auxiliary(&self) -> Option<u16> {
        self.auxiliary
    }
}
