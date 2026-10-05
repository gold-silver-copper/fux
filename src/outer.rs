//! What fux knows of each client's own terminal, learned by asking it: its
//! colours (OSC 10 and 11) and colour scheme (dark or light, `CSI ? 996 n`),
//! whether it reports changes to the scheme (mode 2031), whether it
//! speaks the kitty keyboard protocol (`CSI ? u`), and whether it draws
//! underline styles (`STYLES`). Programs in panes ask the same of their
//! terminal, which is fux, and are answered from what the client terminal
//! said; the styles are what fux paints the client.
//!
//! A terminal that speaks the kitty protocol gets disambiguate (1) and
//! alternate keys (4) pushed (`CSI > 5 u`), so that fux reads keys as
//! exactly as the terminal can tell them (`decode`): disambiguate tells
//! apart what legacy bytes do not (Shift-Enter, Ctrl-I and Tab, Escape and
//! Alt-[), and alternate keys give the shifted key, so that Alt-Shift-1 is
//! `M-!` as from a legacy terminal, and the base-layout key, so that Ctrl
//! on a Cyrillic layout is Ctrl and a Latin letter. Not report all keys as
//! escapes (8), and with it associated text (16): then every key, plain
//! text and Enter included, would come as an escape, and if fux died
//! without popping them the shell would be left unusable, which is why the
//! spec keeps Enter, Tab and Backspace legacy under disambiguate; and fux
//! would have to rebuild text from key codes and layouts the terminal
//! already knows. Not event types (2): fux uses no repeats or releases.
//! The client pops the flags as it leaves (`client::LEAVE`).
//!
//! The client is a dumb pipe, so the server asks in the client's paint
//! stream, as tmux asks its own terminal, and the answers come back in the
//! client's input, where the decoder tells them from keys
//! ([`crate::decode::Reply`]). A terminal that answers nothing costs
//! nothing: what is not known is not answered, as before.
//!
//! Which client's terminal answers for a pane: the client that last typed
//! into the pane's tab, while it is attached; else any client showing the
//! tab; else the last colours any client's terminal gave. A pane in a tab no
//! one shows, or with no client attached, is answered with those: a program
//! started there (delta in a background tab, a build in a detached session)
//! then picks the theme it would pick in the foreground, rather than its
//! default, and it is the best guess at the terminal the user will come back
//! to. Before any terminal has answered, nothing is answered.
use crate::command::{ClientId, TabId};
use crate::decode::Reply;
use crate::layout::PaneId;
use crate::session::{Outgoing, Session};
pub use fux_vt::keys::colour::{Colours, Rgb, Scheme};
use std::time::Instant;

/// What the server asks a client's terminal when the client attaches:
/// whether it knows mode 2031 (DECRQM), its foreground and background,
/// whether it draws underline styles (`STYLES`), its kitty keyboard flags,
/// and last the primary device attributes, which every terminal answers: once
/// that answer is in, any other the terminal will give is in too
/// (terminals answer in order), so the decoder stops waiting for them. The
/// kitty spec detects the protocol so: an answer to `CSI ? u` before DA1's.
pub const QUERIES: &[u8] = b"\x1b[?2031$p\x1b]10;?\x1b\\\x1b]11;?\x1b\\\
\x1bP+q536d756c78\x1b\\\x1b[0m\x1b[4:3m\x1bP$qm\x1b\\\x1b[0m\x1b[?u\x1b[c";
/// How fux learns whether a terminal draws underline styles (kitty's
/// `4:n`, `references/modern/kitty_underlines.html`): it asks two ways,
/// and either answer is enough (`decode::Reply::UnderlineStyles`).
///
/// - XTGETTCAP for `Smulx` (`DCS + q 536d756c78 ST`, the name in hex),
///   the terminfo capability that sets a style. A terminal that answers
///   XTGETTCAP answers from its own terminfo: Ghostty, kitty, WezTerm,
///   foot and iTerm2 have `Smulx` in theirs; xterm answers that it has
///   none (`DCS 0 + r`). Ghostty draws styles but reports a curly
///   underline as plain 4 to DECRQSS, so this is how it is known.
/// - The pen, as neovim asks it (`tui_query_extended_underline`, neovim
///   0.12.5): a curly underline set, then DECRQSS (`DCS $ q m ST`); a
///   terminal that kept the style answers with `4:3` in it, as VTE does,
///   which answers no XTGETTCAP. xterm drops `4:3`; then the pen is reset.
///
/// A terminal that answers neither (Apple's Terminal; alacritty, which
/// draws styles but answers neither question) is painted plain
/// underlines, as before: the safe way, as a terminal that does not know
/// `4:3` draws no underline at all (xterm, avt) or reads the colon as a
/// semicolon, underline and italic. Neither question changes what a
/// terminal shows, and both come before DA1, whose answer ends the
/// waiting for them.
pub const STYLES: &[u8] = b"\x1bP+q536d756c78\x1b\\\x1b[0m\x1b[4:3m\x1bP$qm\x1b\\\x1b[0m";
/// Pushes disambiguate and alternate keys (see the module documentation).
pub const KITTY_PUSH: &[u8] = b"\x1b[>5u";
/// Asked again after the terminal reports that its scheme changed.
pub const COLOUR_QUERIES: &[u8] = b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[c";
/// Turns on the terminal's scheme reports and asks for the scheme now. The
/// client turns the reports off on every way out (`client::LEAVE`).
pub const REPORTS_ON: &[u8] = b"\x1b[?2031h\x1b[?996n";
/// Asks for the scheme alone, of a terminal whose reports are always on.
pub const SCHEME_QUERY: &[u8] = b"\x1b[?996n";

/// One client's terminal, as fux has learned it.
#[derive(Debug, Default)]
pub struct Terminal {
    pub colours: Colours,
    /// Scheme answers (`CSI ? 996 n`) asked for and not given yet: a
    /// `CSI ? 997` that comes while one is due is the answer, any other a
    /// report of a change.
    asked: u8,
    /// A change the terminal reported, told to the panes once its colours
    /// have been asked again, so that a program that asks at the report
    /// gets the new ones.
    change: Option<Scheme>,
    /// Whether the kitty keyboard flags were pushed.
    pub kitty: bool,
    /// Whether the terminal draws underline styles (see `STYLES`): the
    /// client is painted them (`render::sgr`), else plain underlines.
    pub underline_styles: bool,
}

impl Session {
    /// Asks client `client`'s terminal `queries`, which end with DA1, in its
    /// paint stream; its decoder expects the answers.
    fn ask(&mut self, client: ClientId, queries: &[u8], now: Instant) {
        if let Some(view) = self.views.get_mut(&client) {
            view.decoder.expect(now);
            self.outbox.push(Outgoing::Bytes(client, queries.to_vec()));
        }
    }

    /// Asks a newly attached client's terminal what fux wants to know of it.
    pub(crate) fn ask_terminal(&mut self, client: ClientId) {
        self.ask(client, QUERIES, Instant::now());
    }

    /// An answer or report from client `client`'s terminal.
    pub(crate) fn terminal_reply(&mut self, client: ClientId, reply: Reply) {
        let Some(view) = self.views.get_mut(&client) else {
            return;
        };
        let terminal = &mut view.terminal;
        match reply {
            Reply::Colour { number, rgb } => {
                match number {
                    10 => terminal.colours.foreground = Some(rgb),
                    11 => terminal.colours.background = Some(rgb),
                    _ => return,
                }
                self.learned(client);
                if number == 11 {
                    self.tell_change(client);
                }
            }
            Reply::Scheme(scheme) => {
                terminal.colours.scheme = Some(scheme);
                if terminal.asked > 0 {
                    terminal.asked = terminal.asked.saturating_sub(1);
                    self.learned(client);
                    return;
                }
                // A change: the colours are asked again, and the panes told
                // once they are in; at once if the terminal never said them.
                let unknown = terminal.colours.background.is_none();
                terminal.change = Some(scheme);
                self.learned(client);
                if unknown {
                    self.tell_change(client);
                } else {
                    self.ask(client, COLOUR_QUERIES, Instant::now());
                }
            }
            Reply::Mode { mode: 2031, status } => {
                // 1 or 2: known, and set or reset; 3: always set
                // (DECRQM, ctlseqs).
                let queries = match status {
                    1 | 2 => REPORTS_ON,
                    3 => SCHEME_QUERY,
                    _ => return,
                };
                terminal.asked = terminal.asked.saturating_add(1);
                self.outbox.push(Outgoing::Bytes(client, queries.to_vec()));
            }
            // DA1, the last answer to every round of questions: a change
            // waiting on colours the terminal did not give is told now.
            Reply::Attributes => self.tell_change(client),
            // The terminal speaks the kitty protocol: its keys are read
            // exactly from now on.
            Reply::KittyFlags(_) if !terminal.kitty => {
                terminal.kitty = true;
                self.outbox
                    .push(Outgoing::Bytes(client, KITTY_PUSH.to_vec()));
            }
            // From now on the client is painted underline styles; what it
            // shows already, painted plain, is painted again
            // (`render::paint_into`).
            Reply::UnderlineStyles => {
                if !terminal.underline_styles {
                    terminal.underline_styles = true;
                    view.dirty = true;
                }
            }
            // fux asks no palette entries.
            Reply::Mode { .. } | Reply::KittyFlags(_) | Reply::Palette { .. } => {}
        }
    }

    /// What client `client`'s terminal said is the last known now.
    fn learned(&mut self, client: ClientId) {
        let Some(colours) = self.views.get(&client).map(|v| v.terminal.colours) else {
            return;
        };
        let last = &mut self.last_colours;
        last.foreground = colours.foreground.or(last.foreground);
        last.background = colours.background.or(last.background);
        last.scheme = colours.scheme.or(last.scheme);
    }

    /// Tells the panes whose colours come from client `client`, or from no
    /// client in particular, of the scheme change it reported, if one is
    /// waiting: each whose program set mode 2031 gets `CSI ? 997 ; n n`.
    fn tell_change(&mut self, client: ClientId) {
        let Some(scheme) = self
            .views
            .get_mut(&client)
            .and_then(|v| v.terminal.change.take())
        else {
            return;
        };
        let listening: Vec<PaneId> = self
            .panes
            .iter()
            .filter(|(_, p)| p.screen().color_scheme_updates())
            .map(|(id, _)| *id)
            .filter(|id| {
                let tab = self.locate(*id).map(|(_, t)| t);
                self.colour_client(tab).is_none_or(|c| c == client)
            })
            .collect();
        for id in listening {
            if let Some(pane) = self.panes.get_mut(&id) {
                // A program that does not read loses it, as a reply.
                let _ = pane.input.push(scheme.report());
            }
        }
    }

    /// Client `client` typed into the tab it shows: its terminal answers for
    /// the tab's panes.
    pub(crate) fn typed(&mut self, client: ClientId) {
        if let Some(tab) = self.views.get(&client).and_then(|v| v.tab()) {
            self.typists.insert(tab, client);
        }
    }

    /// The client whose terminal answers for the panes of `tab`: the one that
    /// last typed into it, while attached, if its terminal said anything;
    /// else the first showing it whose terminal did.
    fn colour_client(&self, tab: Option<TabId>) -> Option<ClientId> {
        let tab = tab?;
        let told = |client: &ClientId| {
            self.views
                .get(client)
                .is_some_and(|v| v.terminal.colours.known())
        };
        self.typists
            .get(&tab)
            .filter(|c| told(c))
            .copied()
            .or_else(|| {
                self.views
                    .values()
                    .filter(|v| v.tab() == Some(tab))
                    .map(|v| v.id)
                    .find(told)
            })
    }

    /// The colours the panes of `tab` are answered with (see the module
    /// documentation).
    pub(crate) fn colours_for(&self, tab: Option<TabId>) -> Colours {
        self.colour_client(tab)
            .and_then(|c| self.views.get(&c))
            .map_or(self.last_colours, |v| v.terminal.colours)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// XParseColor's scaling: each channel's digits are a fraction of the
    /// largest number of that many digits.
    #[test]
    fn colour_specifications_scale_to_sixteen_bits() {
        let rgb = |r, g, b| Some(Rgb { r, g, b });
        assert_eq!(Rgb::parse(b"rgb:ffff/0000/8080"), rgb(0xffff, 0, 0x8080));
        assert_eq!(Rgb::parse(b"rgb:ff/00/80"), rgb(0xffff, 0, 0x8080));
        assert_eq!(Rgb::parse(b"rgb:f/0/8"), rgb(0xffff, 0, 0x8888));
        // 0x800 * 0xffff / 0xfff, truncated.
        assert_eq!(Rgb::parse(b"rgb:fff/000/800"), rgb(0xffff, 0, 0x8007));
        assert_eq!(
            Rgb::parse(b"rgb:1E1E/1e1e/1E1E"),
            rgb(0x1e1e, 0x1e1e, 0x1e1e)
        );
        for bad in [
            &b"rgb:ff/00"[..],
            b"rgb:ff/00/80/00",
            b"rgb:fffff/0/0",
            b"rgb://",
            b"rgb:g/0/0",
            b"#ffffff",
            b"rgba:ff/ff/ff/ff",
        ] {
            assert_eq!(Rgb::parse(bad), None, "{bad:?}");
        }
    }

    /// The questions about underline styles are asked at attach, before the
    /// kitty query and DA1, whose answer ends the waiting for theirs, and
    /// leave the pen reset.
    #[test]
    fn the_questions_about_styles_come_before_da1() {
        assert!(QUERIES.ends_with(&[STYLES, b"\x1b[?u\x1b[c"].concat()));
        assert!(STYLES.ends_with(b"\x1b[0m"));
    }

    #[test]
    fn answers_take_the_form_they_were_asked_in() {
        let colours = Colours {
            foreground: Some(Rgb {
                r: 0xffff,
                g: 0xffff,
                b: 0xffff,
            }),
            background: Some(Rgb {
                r: 0x1e1e,
                g: 0,
                b: 0xabcd,
            }),
            scheme: None,
        };
        assert_eq!(
            colours.answer(11, true).as_deref(),
            Some(&b"\x1b]11;rgb:1e1e/0000/abcd\x07"[..])
        );
        assert_eq!(
            colours.answer(10, false).as_deref(),
            Some(&b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\"[..])
        );
        assert_eq!(colours.answer(12, true), None, "the cursor colour");
        assert_eq!(Colours::default().answer(11, true), None);
        assert!(!Colours::default().known());
        assert_eq!(Scheme::Dark.report(), b"\x1b[?997;1n");
        assert_eq!(Scheme::Light.report(), b"\x1b[?997;2n");
    }
}
