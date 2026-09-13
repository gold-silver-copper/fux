//! Pointer policy for the command popup, using only its most recently painted hits.
use super::{hints::HintPanel, render::HitRegions};
use crate::{commands::Action, proto::attach::MouseEvent};
use ratatui_core::layout::Rect;

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Ignore,
    Dismiss,
    Scroll(i32),
    Command(Action),
}

#[derive(Default)]
pub struct Popup {
    bounds: Option<Rect>,
    actions: Vec<(Rect, Action)>,
    captured: Option<u16>,
}
impl Popup {
    /// A selected command can switch parsers before the initiating release arrives.
    pub fn take_capture(&mut self) -> Option<u16> {
        self.captured.take()
    }

    pub fn painted(&mut self, hits: &HitRegions, panel: Option<&HintPanel>) {
        self.bounds = panel.and(hits.panel);
        self.actions = panel.map_or_else(Vec::new, |panel| {
            hits.entries
                .iter()
                .filter_map(|(rect, index)| {
                    panel.command_action(*index).map(|action| (*rect, action))
                })
                .collect()
        });
    }
    /// Retain release ownership after dismissal; never swallow a fresh subsequent press.
    pub fn consume_tail(&mut self, mouse: MouseEvent) -> bool {
        if self.captured != Some(mouse.button()) || mouse.wheel() {
            return false;
        }
        if mouse.release {
            self.captured = None;
            return true;
        }
        if !mouse.motion() {
            self.captured = None;
            return false;
        }
        true
    }
    pub fn mouse(&mut self, mouse: MouseEvent) -> Outcome {
        if self.consume_tail(mouse) || mouse.release || mouse.motion() {
            return Outcome::Ignore;
        }
        let point = (mouse.column.saturating_sub(1), mouse.row.saturating_sub(1)).into();
        let inside = self.bounds.is_some_and(|rect| rect.contains(point));
        if mouse.wheel() {
            return if inside && mouse.button() <= 1 {
                Outcome::Scroll(if mouse.button() == 0 { -3 } else { 3 })
            } else {
                Outcome::Ignore
            };
        }
        if mouse.button() <= 2 {
            self.captured = Some(mouse.button());
        }
        if mouse.button() != 0 {
            return Outcome::Ignore;
        }
        if !inside {
            return Outcome::Dismiss;
        }
        self.actions
            .iter()
            .find(|(rect, _)| rect.contains(point))
            .map_or(Outcome::Ignore, |(_, action)| Outcome::Command(*action))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            code: 0,
            column,
            row,
            release: false,
        }
    }
    #[test]
    fn ignored_auxiliary_presses_own_their_releases_after_dismissal() {
        for code in [1, 2] {
            let mut popup = Popup::default();
            let press = MouseEvent {
                code,
                ..click(2, 2)
            };
            assert_eq!(popup.mouse(press), Outcome::Ignore);
            // Keyboard dismissal stops popup routing before the release arrives.
            assert!(popup.consume_tail(MouseEvent {
                release: true,
                ..press
            }));
            assert!(!popup.consume_tail(press), "a fresh press must recover");
        }
    }

    #[test]
    fn painted_actions_headings_outside_clicks_and_release_ownership() {
        let mut popup = Popup {
            bounds: Some(Rect::new(10, 2, 10, 5)),
            actions: vec![(Rect::new(10, 3, 10, 1), Action::SplitSide)],
            captured: None,
        };
        assert_eq!(popup.mouse(click(12, 3)), Outcome::Ignore); // heading
        assert_eq!(
            popup.mouse(click(12, 4)),
            Outcome::Command(Action::SplitSide)
        );
        assert!(popup.consume_tail(MouseEvent {
            release: true,
            ..click(12, 4)
        }));
        assert!(!popup.consume_tail(click(2, 2)));
        assert_eq!(popup.mouse(click(2, 2)), Outcome::Dismiss);
        assert!(
            !popup.consume_tail(click(2, 2)),
            "fresh click works even if release was lost"
        );
        assert_eq!(
            popup.mouse(MouseEvent {
                code: 64,
                ..click(12, 4)
            }),
            Outcome::Scroll(-3)
        );
        assert_eq!(
            popup.mouse(MouseEvent {
                code: 66,
                ..click(12, 4)
            }),
            Outcome::Ignore
        );
        assert_eq!(
            popup.mouse(MouseEvent {
                code: 65,
                ..click(2, 2)
            }),
            Outcome::Ignore
        );
    }
}
