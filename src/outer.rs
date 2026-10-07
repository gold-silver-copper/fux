//! What fux knows of each client's own terminal, learned by asking it: its
//! colours (OSC 10 and 11), its palette entries 0 to 15 (OSC 4), and colour scheme (dark or light, `CSI ? 996 n`),
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
//! The server asks in the client's paint stream, as tmux asks its own
//! terminal: written to the terminal itself once the client has handed it
//! over, else relayed by the client. The answers come back with the keys,
//! read from the terminal or relayed, where the decoder tells them apart
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
use crate::view::View;
pub use fux_vt::keys::colour::{Colours, Rgb, Scheme};
use std::time::Instant;

/// What the server asks a client's terminal when the client attaches:
/// whether it knows mode 2031 (DECRQM), its foreground and background, its
/// palette entries 0 to 15 (each its own OSC 4, as not every terminal reads
/// several in one),
/// whether it draws underline styles (`STYLES`), its kitty keyboard flags,
/// and last the primary device attributes, which every terminal answers: once
/// that answer is in, any other the terminal will give is in too
/// (terminals answer in order), so the decoder stops waiting for them. The
/// kitty spec detects the protocol so: an answer to `CSI ? u` before DA1's.
pub const QUERIES: &[u8] = b"\x1b[?2031$p\x1b]10;?\x1b\\\x1b]11;?\x1b\\\
\x1b]4;0;?\x1b\\\x1b]4;1;?\x1b\\\x1b]4;2;?\x1b\\\x1b]4;3;?\x1b\\\
\x1b]4;4;?\x1b\\\x1b]4;5;?\x1b\\\x1b]4;6;?\x1b\\\x1b]4;7;?\x1b\\\
\x1b]4;8;?\x1b\\\x1b]4;9;?\x1b\\\x1b]4;10;?\x1b\\\x1b]4;11;?\x1b\\\
\x1b]4;12;?\x1b\\\x1b]4;13;?\x1b\\\x1b]4;14;?\x1b\\\x1b]4;15;?\x1b\\\
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
/// The palette entries 0 to 15, asked one by one: part of [`QUERIES`] and
/// [`COLOUR_QUERIES`].
pub const PALETTE_QUERIES: &[u8] =
    b"\x1b]4;0;?\x1b\\\x1b]4;1;?\x1b\\\x1b]4;2;?\x1b\\\x1b]4;3;?\x1b\\\
\x1b]4;4;?\x1b\\\x1b]4;5;?\x1b\\\x1b]4;6;?\x1b\\\x1b]4;7;?\x1b\\\
\x1b]4;8;?\x1b\\\x1b]4;9;?\x1b\\\x1b]4;10;?\x1b\\\x1b]4;11;?\x1b\\\
\x1b]4;12;?\x1b\\\x1b]4;13;?\x1b\\\x1b]4;14;?\x1b\\\x1b]4;15;?\x1b\\";
/// Pushes disambiguate and alternate keys (see the module documentation).
pub const KITTY_PUSH: &[u8] = b"\x1b[>5u";
/// Asked again after the terminal reports that its scheme changed: a theme
/// that changes changes the palette too.
pub const COLOUR_QUERIES: &[u8] = b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\
\x1b]4;0;?\x1b\\\x1b]4;1;?\x1b\\\x1b]4;2;?\x1b\\\x1b]4;3;?\x1b\\\
\x1b]4;4;?\x1b\\\x1b]4;5;?\x1b\\\x1b]4;6;?\x1b\\\x1b]4;7;?\x1b\\\
\x1b]4;8;?\x1b\\\x1b]4;9;?\x1b\\\x1b]4;10;?\x1b\\\x1b]4;11;?\x1b\\\
\x1b]4;12;?\x1b\\\x1b]4;13;?\x1b\\\x1b]4;14;?\x1b\\\x1b]4;15;?\x1b\\\x1b[c";

/// Saves the terminal's title (xterm's title stack, `CSI 22 ; 0 t`), sent
/// before fux first sets it. It is popped when titles are turned off, and
/// when the attachment ends: by the server as it gives a terminal it took
/// back, else by the client as it leaves (`client::restore`).
pub const TITLE_PUSH: &[u8] = b"\x1b[22;0t";
/// Restores the title [`TITLE_PUSH`] saved.
pub const TITLE_POP: &[u8] = b"\x1b[23;0t";

/// Whether a paint leaves the terminal's title saved: `Some(true)` if its
/// last [`TITLE_PUSH`] or [`TITLE_POP`] is a push, `Some(false)` if a pop,
/// `None` if it has neither. One pass from the end, stopping at escapes
/// alone: a paint is mostly text, and every paint is looked at.
pub fn title_saved_by(paint: &[u8]) -> Option<bool> {
    let mut end = paint.len();
    while let Some(at) = paint.get(..end)?.iter().rposition(|&b| b == 0x1b) {
        let rest = paint.get(at..)?;
        if rest.starts_with(TITLE_PUSH) {
            return Some(true);
        }
        if rest.starts_with(TITLE_POP) {
            return Some(false);
        }
        end = at;
    }
    None
}
/// The bell, as a client's terminal is rung.
pub const BELL: &[u8] = b"\x07";
/// The least time between two bells sent to one terminal, so that a
/// program ringing without end (`yes $'\a'`) cannot flood it.
pub const BELL_GAP: std::time::Duration = std::time::Duration::from_millis(250);

/// Palette entries 0 to 15 as a terminal said them (OSC 4), `None` for an
/// entry it did not.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Palette(pub [Option<Rgb>; 16]);

impl Palette {
    /// Whether the terminal said any entry.
    pub fn known(&self) -> bool {
        self.0.iter().any(Option::is_some)
    }
}
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
    /// Its palette entries 0 to 15, answered to the panes it answers for.
    pub palette: Palette,
}

impl Terminal {
    /// Whether the terminal said anything of its colours.
    fn said(&self) -> bool {
        self.colours.known() || self.palette.known()
    }
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
            Reply::Palette { index, rgb } => {
                if let Some(entry) = terminal.palette.0.get_mut(usize::from(index)) {
                    *entry = Some(rgb);
                    self.learned(client);
                }
            }
            Reply::Mode { .. } | Reply::KittyFlags(_) => {}
        }
    }

    /// What client `client`'s terminal said is the last known now.
    fn learned(&mut self, client: ClientId) {
        let Some((colours, palette)) = self
            .views
            .get(&client)
            .map(|v| (v.terminal.colours, v.terminal.palette))
        else {
            return;
        };
        for (last, said) in self.last_palette.0.iter_mut().zip(palette.0) {
            *last = said.or(*last);
        }
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
        let told = |client: &ClientId| self.views.get(client).is_some_and(|v| v.terminal.said());
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

    /// The palette the panes of `tab` are answered with, as `colours_for`:
    /// the answering client's whole, so that another client's replaces it,
    /// entries it did not say cleared.
    pub(crate) fn palette_for(&self, tab: Option<TabId>) -> Palette {
        self.colour_client(tab)
            .and_then(|c| self.views.get(&c))
            .map_or(self.last_palette, |v| v.terminal.palette)
    }

    /// Pane `id`'s program rang the bell: each client showing its workspace
    /// is rung, at most once every [`BELL_GAP`], and one not showing its
    /// tab has the tab marked in its bar until it shows it.
    pub(crate) fn ring(&mut self, id: PaneId, now: Instant) {
        if !self.config.bell {
            return;
        }
        let Some((ws, tab)) = self.locate(id) else {
            return;
        };
        for view in self.views.values_mut() {
            if view.workspace != ws {
                continue;
            }
            if view.tab() != Some(tab) && view.bells.insert(tab) {
                view.dirty = true;
            }
            if view
                .last_bell
                .is_none_or(|last| now.saturating_duration_since(last) >= BELL_GAP)
            {
                view.last_bell = Some(now);
                self.outbox.push(Outgoing::Bytes(view.id, BELL.to_vec()));
            }
        }
    }

    /// What client `client`'s terminal is sent before it is painted: the
    /// tab it shows loses its bell mark, and with `titles` on its title
    /// becomes its focused pane's, or the tab's name for a pane with none;
    /// turned off, the title fux saved is restored.
    pub fn before_paint(&mut self, client: ClientId) -> Vec<u8> {
        let mut out = Vec::new();
        let Some(view) = self.views.get(&client) else {
            return out;
        };
        let tab = view.tab();
        // The title, made only when it is not the one the terminal has:
        // `Some(None)` for the same.
        let wanted = self.config.titles.then(|| {
            let title = self.title_of(view);
            let same = view
                .title
                .as_deref()
                .is_some_and(|shown| shown.chars().eq(clean(title)));
            (!same).then(|| clean(title).collect::<String>())
        });
        let Some(view) = self.views.get_mut(&client) else {
            return out;
        };
        if let Some(tab) = tab {
            view.bells.remove(&tab);
        }
        match wanted {
            Some(Some(title)) => {
                if !view.title_pushed {
                    out.extend_from_slice(TITLE_PUSH);
                    view.title_pushed = true;
                }
                out.extend_from_slice(b"\x1b]2;");
                out.extend_from_slice(title.as_bytes());
                out.extend_from_slice(b"\x1b\\");
                view.title = Some(title);
            }
            None if view.title_pushed => {
                out.extend_from_slice(TITLE_POP);
                view.title_pushed = false;
                view.title = None;
            }
            Some(None) | None => {}
        }
        out
    }

    /// The title a view's terminal is given: its focused pane's, else its
    /// tab's name, without control characters (`clean`).
    fn title_of<'a>(&'a self, view: &View) -> &'a str {
        let pane = view
            .focus()
            .and_then(|f| self.panes.get(&f))
            .map(|p| p.title.as_str())
            .filter(|t| !t.is_empty());
        let tab = view
            .tab()
            .and_then(|t| self.tab(t))
            .map(|t| t.name.as_str());
        pane.or(tab).unwrap_or_default()
    }
}

/// A title without its control characters, as a terminal is given it.
fn clean(title: &str) -> impl Iterator<Item = char> + '_ {
    title.chars().filter(|c| !c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    type Outcome = Result<(), String>;

    use crate::session::testing::run;

    fn session() -> Result<(Session, ClientId), String> {
        crate::session::testing::attached(10, 40)
    }

    /// What pane `pane`'s program was answered, taken.
    fn answered(s: &mut Session, pane: u32) -> String {
        let bytes = s
            .panes
            .get_mut(&PaneId(pane))
            .map(|p| p.input.drain_all())
            .unwrap_or_default();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// What pane `pane`'s program is answered to `OSC 4 ; n ; ?`.
    fn entry(s: &mut Session, pane: u32, n: u8) -> String {
        let _ = answered(s, pane);
        s.output(PaneId(pane), format!("\x1b]4;{n};?\x07").as_bytes());
        answered(s, pane)
    }

    #[test]
    fn a_panes_palette_query_is_answered_with_its_clients_palette() -> Outcome {
        let (mut s, c) = session()?;
        // Nothing said yet: xterm's red.
        assert_eq!(entry(&mut s, 1, 1), "\x1b]4;1;rgb:cdcd/0000/0000\x07");
        s.input(
            c,
            b"\x1b]4;1;rgb:1111/2222/3333\x1b\\\x1b]4;12;rgb:ff/80/00\x1b\\\x1b[?62c",
        );
        assert_eq!(entry(&mut s, 1, 1), "\x1b]4;1;rgb:1111/2222/3333\x07");
        assert_eq!(entry(&mut s, 1, 12), "\x1b]4;12;rgb:ffff/8080/0000\x07");
        // Entries the terminal did not say, and those past 15: xterm's.
        assert_eq!(entry(&mut s, 1, 2), "\x1b]4;2;rgb:0000/cdcd/0000\x07");
        // The program's own colour wins, and nothing the host gave is
        // painted.
        let before = crate::render::compose(&s, c);
        s.output(PaneId(1), b"\x1b]4;1;#00ff00\x07");
        assert_eq!(entry(&mut s, 1, 1), "\x1b]4;1;rgb:0000/ffff/0000\x07");
        s.output(PaneId(1), b"\x1b]104;1\x07");
        assert_eq!(entry(&mut s, 1, 1), "\x1b]4;1;rgb:1111/2222/3333\x07");
        assert_eq!(crate::render::compose(&s, c), before);
        Ok(())
    }

    #[test]
    fn another_client_typing_replaces_the_palette_whole() -> Outcome {
        let (mut s, c) = session()?;
        s.input(c, b"\x1b]4;1;rgb:1111/2222/3333\x1b\\\x1b[?62c");
        let d = s.attach(10, 40, None).map_err(|e| e.to_string())?;
        s.input(d, b"\x1b]4;2;rgb:4444/5555/6666\x1b\\\x1b[?62c");
        s.input(c, b"x");
        assert_eq!(entry(&mut s, 1, 1), "\x1b]4;1;rgb:1111/2222/3333\x07");
        // d types: its palette, whole; entry 1, which it did not say, is
        // xterm's again.
        s.input(d, b"y");
        assert_eq!(entry(&mut s, 1, 2), "\x1b]4;2;rgb:4444/5555/6666\x07");
        assert_eq!(entry(&mut s, 1, 1), "\x1b]4;1;rgb:cdcd/0000/0000\x07");
        Ok(())
    }

    /// What the session sent client `c`'s terminal outside its paints,
    /// taken.
    fn sent(s: &mut Session, c: ClientId) -> Vec<Vec<u8>> {
        let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut s.outbox)
            .into_iter()
            .partition(|o| matches!(o, Outgoing::Bytes(to, _) if *to == c));
        s.outbox = rest;
        mine.into_iter()
            .filter_map(|o| match o {
                Outgoing::Bytes(_, bytes) => Some(bytes),
                Outgoing::Exit(..) | Outgoing::Shutdown(_) => None,
            })
            .collect()
    }

    /// Client `c`'s bar, as it would be painted.
    fn tab_label(s: &Session, c: ClientId) -> String {
        crate::render::compose(s, c)
            .map(|g| g.row_text(g.rows.saturating_sub(1)))
            .unwrap_or_default()
    }

    #[test]
    fn a_bell_rings_the_clients_showing_its_workspace_and_marks_its_tab() -> Outcome {
        let (mut s, c) = session()?;
        let _ = sent(&mut s, c);
        s.output(PaneId(1), b"\x07");
        assert_eq!(sent(&mut s, c), [BELL.to_vec()]);
        // Again at once: not rung twice within the gap.
        s.output(PaneId(1), b"\x07\x07");
        assert!(sent(&mut s, c).is_empty());
        std::thread::sleep(BELL_GAP);
        s.output(PaneId(1), b"\x07");
        assert_eq!(sent(&mut s, c), [BELL.to_vec()]);
        // In a tab the client does not show: its tab is marked until shown.
        run(&mut s, "new-tab -t +1 -n other")?;
        run(&mut s, "select-tab -c c1 -t @2")?;
        s.settle();
        let _ = s.before_paint(c);
        std::thread::sleep(BELL_GAP);
        s.output(PaneId(1), b"\x07");
        assert_eq!(sent(&mut s, c), [BELL.to_vec()]);
        assert!(tab_label(&s, c).contains(" main! "), "{}", tab_label(&s, c));
        run(&mut s, "select-tab -c c1 -t @1")?;
        let _ = s.before_paint(c);
        run(&mut s, "select-tab -c c1 -t @2")?;
        let _ = s.before_paint(c);
        assert!(!tab_label(&s, c).contains('!'), "{}", tab_label(&s, c));
        // A client on another workspace is not rung.
        run(&mut s, "select-tab -c c1 -t @1")?;
        let _ = s.before_paint(c);
        run(&mut s, "new-workspace -n elsewhere")?;
        let d = s.attach(10, 40, None).map_err(|e| e.to_string())?;
        run(&mut s, "select-workspace -c c2 -t elsewhere")?;
        let _ = sent(&mut s, d);
        std::thread::sleep(BELL_GAP);
        s.output(PaneId(1), b"\x07");
        assert_eq!(sent(&mut s, c), [BELL.to_vec()]);
        assert!(sent(&mut s, d).is_empty());
        // Off: no bell and no mark.
        run(&mut s, "set bell off")?;
        std::thread::sleep(BELL_GAP);
        s.output(PaneId(1), b"\x07");
        assert!(sent(&mut s, c).is_empty());
        assert!(!tab_label(&s, c).contains('!'));
        Ok(())
    }

    #[test]
    fn titles_follow_the_focused_pane_and_are_restored_when_turned_off() -> Outcome {
        let (mut s, c) = session()?;
        // Off by default: nothing.
        s.output(PaneId(1), b"\x1b]2;vim notes\x07");
        assert!(s.before_paint(c).is_empty());
        run(&mut s, "set titles on")?;
        let first = s.before_paint(c);
        assert_eq!(first, [TITLE_PUSH, b"\x1b]2;vim notes\x1b\\"].concat());
        // Unchanged: nothing more; changed: the title alone.
        assert!(s.before_paint(c).is_empty());
        s.output(PaneId(1), b"\x1b]2;make\x07");
        assert_eq!(s.before_paint(c), b"\x1b]2;make\x1b\\");
        // A pane with no title: its tab's name.
        run(&mut s, "split -h -t %1")?;
        run(&mut s, "select-pane -c c1 -t %2")?;
        run(&mut s, "rename -t @1 notes")?;
        assert_eq!(s.before_paint(c), b"\x1b]2;notes\x1b\\");
        // Off again: the title fux saved is restored, once.
        run(&mut s, "set titles off")?;
        assert_eq!(s.before_paint(c), TITLE_POP);
        assert!(s.before_paint(c).is_empty());
        Ok(())
    }

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
        // The palette is asked with the colours, before them.
        let at = |bytes: &[u8], part: &[u8]| {
            (0..bytes.len()).find(|&i| bytes.get(i..).is_some_and(|r| r.starts_with(part)))
        };
        let palette = at(QUERIES, PALETTE_QUERIES);
        assert!(palette.is_some_and(|p| at(QUERIES, STYLES).is_some_and(|s| p < s)));
        assert!(at(COLOUR_QUERIES, PALETTE_QUERIES).is_some());
        assert!(COLOUR_QUERIES.ends_with(b"\x1b[c"));
        assert!(STYLES.ends_with(b"\x1b[0m"));
    }

    /// `title_saved_by` decides as the later of a search for each from the
    /// end did, which it replaced: over every paint of up to four pieces,
    /// the sequences, their beginnings, a lone escape and text.
    #[test]
    fn the_last_title_push_or_pop_decides() {
        let last = |paint: &[u8], needle: &[u8]| {
            (0..paint.len())
                .rev()
                .find(|&at| paint.get(at..).is_some_and(|rest| rest.starts_with(needle)))
        };
        let pieces: [&[u8]; 7] = [
            TITLE_PUSH,
            TITLE_POP,
            b"\x1b",
            b"\x1b[22;0",
            b"\x1b[23",
            b"t",
            b"text",
        ];
        let mut paints: Vec<Vec<u8>> = vec![Vec::new()];
        for _ in 0..4 {
            let longer: Vec<Vec<u8>> = paints
                .iter()
                .filter(|p| p.len() < 64)
                .flat_map(|p| {
                    pieces
                        .iter()
                        .map(move |piece| [p.as_slice(), piece].concat())
                })
                .collect();
            paints.extend(longer);
        }
        paints.sort();
        paints.dedup();
        for paint in paints {
            let (push, pop) = (last(&paint, TITLE_PUSH), last(&paint, TITLE_POP));
            let expected = (push.is_some() || pop.is_some()).then_some(push > pop);
            assert_eq!(title_saved_by(&paint), expected, "{paint:?}");
        }
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
