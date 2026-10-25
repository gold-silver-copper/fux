//! What fux knows of each client's own terminal, learned by asking it in
//! the client's paint stream: its colours (OSC 10, 11), palette entries 0
//! to 15 (OSC 4) and colour scheme (`CSI ? 996 n`, reports with mode 2031),
//! the kitty keyboard protocol (`CSI ? u`), underline styles (`styles!`),
//! sixel and its cell size. The answers come back with the keys, which the
//! decoder tells apart ([`crate::decode::Reply`]); a terminal that answers
//! nothing costs nothing. Programs in panes asking their terminal, fux,
//! the same are answered from it.
//!
//! A kitty terminal gets disambiguate (1) and alternate keys (4) pushed
//! (`CSI > 5 u`), so that Shift-Enter, Ctrl-I and Alt-[ are told apart, and
//! Alt-Shift-1 is `M-!` and Ctrl on a Cyrillic layout is Ctrl and a Latin
//! letter. Not "report all keys as escapes" (8): if fux died without
//! popping it, the shell would be left unusable. The client pops the flags
//! as it leaves (`client::LEAVE`).
//!
//! The terminal that answers for a pane is the client's that last typed
//! into its tab, while attached; else any client's showing the tab; else
//! none: a pane no attached terminal answers for is not answered colours
//! it did not set, as no one knows what it is shown on.
use crate::decode::Reply;
use crate::id::{ClientId, PaneId, TabId};
use crate::protocol::ToAttached;
use crate::session::{Outgoing, Session};
use crate::view::View;
pub use fux_vt::Rgb;
pub use fux_vt::keys::colour::{Colours, Scheme};
use fux_vt::{Graphics, ModeStatus};
use std::collections::BTreeMap;
use std::time::Instant;

/// The colours asked one by one, each its own OSC: the foreground, the
/// background, and the palette entries 0 to 15. Part of [`QUERIES`] and
/// [`COLOUR_QUERIES`].
macro_rules! colours {
    () => {
        concat!(
            "\x1b]10;?\x1b\\\x1b]11;?\x1b\\",
            "\x1b]4;0;?\x1b\\\x1b]4;1;?\x1b\\\x1b]4;2;?\x1b\\\x1b]4;3;?\x1b\\",
            "\x1b]4;4;?\x1b\\\x1b]4;5;?\x1b\\\x1b]4;6;?\x1b\\\x1b]4;7;?\x1b\\",
            "\x1b]4;8;?\x1b\\\x1b]4;9;?\x1b\\\x1b]4;10;?\x1b\\\x1b]4;11;?\x1b\\",
            "\x1b]4;12;?\x1b\\\x1b]4;13;?\x1b\\\x1b]4;14;?\x1b\\\x1b]4;15;?\x1b\\",
        )
    };
}
/// How fux learns whether a terminal draws underline styles (kitty's
/// `4:n`): either answer is enough (`decode::Reply::UnderlineStyles`).
/// XTGETTCAP for `Smulx`, which Ghostty, kitty, WezTerm, foot and iTerm2
/// answer from their terminfo; and the pen, as neovim asks it: a curly
/// underline set, then DECRQSS, which VTE answers with `4:3` in it. A
/// terminal that answers neither is painted plain underlines, the safe way,
/// as one that does not know `4:3` may draw none or italics.
macro_rules! styles {
    () => {
        "\x1bP+q536d756c78\x1b\\\x1b[0m\x1b[4:3m\x1bP$qm\x1b\\\x1b[0m"
    };
}
/// What the server asks a client's terminal as it attaches: mode 2031
/// (DECRQM), its colours (each palette entry its own OSC 4, as not every
/// terminal reads several in one), underline styles, its cell size in
/// pixels (`CSI 16 t`), its kitty flags, and last DA1, which every terminal
/// answers, after the others (terminals answer in order): it says whether
/// the terminal draws sixel, and ends the decoder's waiting.
pub const QUERIES: &[u8] = concat!(
    "\x1b[?2031$p",
    colours!(),
    styles!(),
    "\x1b[16t\x1b[?u\x1b[c"
)
.as_bytes();
/// Pushes disambiguate and alternate keys (see the module documentation),
/// and asks the flags again: their answer, disambiguate set, is the
/// terminal's word that it sends Escape as `CSI 27 u`, which the decoder
/// then never waits out ("Detection of support": set the flags, then
/// query).
pub const KITTY_PUSH: &[u8] = b"\x1b[>5u\x1b[?u";
/// Asked again after the terminal reports that its scheme changed: a theme
/// that changes changes the palette too.
pub const COLOUR_QUERIES: &[u8] = concat!(colours!(), "\x1b[c").as_bytes();

/// Saves the terminal's title (xterm's title stack, `CSI 22 ; 0 t`). The
/// attach client saves it as it enters (`client::ENTER`) and restores it as
/// it leaves, however the attachment ends; titles turned off restore it
/// and save it again.
pub const TITLE_PUSH: &[u8] = b"\x1b[22;0t";
/// Restores the title [`TITLE_PUSH`] saved.
pub const TITLE_POP: &[u8] = b"\x1b[23;0t";

/// The bell, as a client's terminal is rung.
pub const BELL: &[u8] = b"\x07";
/// The least time between two bells sent to one terminal, so that a
/// program ringing without end (`yes $'\a'`) cannot flood it.
pub const BELL_GAP: std::time::Duration = std::time::Duration::from_millis(250);

/// Palette entries 0 to 15 as a terminal said them (OSC 4), `None` for an
/// entry it did not.
pub type Palette = [Option<Rgb>; 16];
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
    /// Whether the terminal draws underline styles (see `styles!`): the
    /// client is painted them (`render::sgr`), else plain underlines.
    pub underline_styles: bool,
    /// Its palette entries 0 to 15, answered to the panes it answers for.
    pub palette: Palette,
    /// Whether it draws sixel graphics (DA1's `4`), and the size of its cells
    /// in pixels (`CSI 16 t`): pictures in panes are shown on it, and the
    /// panes it answers for are told so (sixel only with a cell size, which
    /// pictures are laid out in).
    pub sixel: bool,
    pub cell: Option<fux_vt::CellSize>,
}

impl Terminal {
    /// What it draws, as the panes it answers for are told.
    pub fn graphics(&self) -> Graphics {
        Graphics {
            cell: self.cell,
            sixel: self.sixel,
        }
    }

    /// Whether the terminal said anything of its colours.
    fn said(&self) -> bool {
        self.colours.known() || self.palette.iter().any(Option::is_some)
    }
}

impl Session {
    /// Writes `bytes` to client `client`'s terminal, in its paint stream:
    /// questions, whose answers the client's decoder expects, or a mode it
    /// sets.
    pub(crate) fn ask(&mut self, client: ClientId, bytes: &'static [u8]) {
        if self.views.contains_key(&client) {
            (self.outbox).push(Outgoing::To(client, ToAttached::Paint(bytes.into())));
        }
    }

    /// An answer or report from client `client`'s terminal.
    pub(crate) fn terminal_reply(&mut self, client: ClientId, reply: Reply) {
        let Some(view) = self.views.get_mut(&client) else {
            return;
        };
        let terminal = &mut view.terminal;
        match reply {
            Reply::Foreground(rgb) => terminal.colours.foreground = Some(rgb),
            Reply::Background(rgb) => {
                terminal.colours.background = Some(rgb);
                self.tell_change(client);
            }
            Reply::Scheme(scheme) => {
                terminal.colours.scheme = Some(scheme);
                if terminal.asked > 0 {
                    terminal.asked = terminal.asked.saturating_sub(1);
                    return;
                }
                // A change: the colours are asked again, and the panes told
                // once they are in; at once if the terminal never said them.
                let unknown = terminal.colours.background.is_none();
                terminal.change = Some(scheme);
                if unknown {
                    self.tell_change(client);
                } else {
                    self.ask(client, COLOUR_QUERIES);
                }
            }
            Reply::Mode { mode: 2031, status } => {
                // Known, and set or reset; or always set (DECRQM, ctlseqs).
                let queries = match status {
                    ModeStatus::Set | ModeStatus::Reset => REPORTS_ON,
                    ModeStatus::PermanentlySet => SCHEME_QUERY,
                    ModeStatus::NotRecognized | ModeStatus::PermanentlyReset => return,
                };
                terminal.asked = terminal.asked.saturating_add(1);
                self.ask(client, queries);
            }
            // DA1, the last answer to every round of questions: a change
            // waiting on colours the terminal did not give is told now.
            Reply::Attributes { sixel } => {
                if terminal.sixel != sixel {
                    terminal.sixel = sixel;
                    view.dirty = true;
                }
                self.tell_change(client);
            }
            // A cell smaller than a sixel band is no size a picture can be
            // laid out in: refused, and said, not made one.
            Reply::CellSize { width, height } => {
                terminal.cell = fux_vt::CellSize::new(width, height);
                if terminal.cell.is_none() {
                    view.notify(
                        format!(
                            "your terminal's cell, {width}x{height} pixels, is smaller than a sixel band \
                             (2x6): no pictures, and no mouse reports in pixels"
                        ),
                        true,
                    );
                }
                view.dirty = true;
            }
            // The terminal speaks the kitty protocol: its keys are read
            // exactly from now on.
            Reply::KittyFlags(_) if !terminal.kitty => {
                terminal.kitty = true;
                self.ask(client, KITTY_PUSH);
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
                if let Some(entry) = terminal.palette.get_mut(usize::from(index)) {
                    *entry = Some(rgb);
                }
            }
            Reply::Mode { .. } | Reply::KittyFlags(_) => {}
        }
    }

    /// Tells the panes whose colours come from client `client` of the scheme
    /// change it reported, if one is waiting: each whose program set mode
    /// 2031 gets `CSI ? 997 ; n n`.
    fn tell_change(&mut self, client: ClientId) {
        let Some(scheme) = self
            .views
            .get_mut(&client)
            .and_then(|v| v.terminal.change.take())
        else {
            return;
        };
        let listening: Vec<PaneId> = (self.parts.panes().iter())
            .filter(|(_, p)| p.screen().mode(fux_vt::Mode::ColorSchemeUpdates))
            .map(|(id, _)| *id)
            .filter(|id| {
                let tab = self.locate(*id).map(|(_, t)| t);
                self.colour_client(tab) == Some(client)
            })
            .collect();
        for id in listening {
            if let Some(mut pane) = self.parts.pane_mut(id) {
                // A program that does not read loses it, as a reply.
                let _ = pane.input.push(scheme.report());
            }
        }
    }

    /// Client `client` typed into the tab it shows: its terminal answers for
    /// the tab's panes.
    pub(crate) fn typed(&mut self, client: ClientId) {
        if let Some(tab) = self.shown_tab_mut(client) {
            tab.typist = Some(client);
        }
    }

    /// The client whose terminal answers for the panes of `tab`: the one that
    /// last typed into it, while attached, if its terminal said anything;
    /// else the first showing it whose terminal did.
    fn colour_client(&self, tab: Option<TabId>) -> Option<ClientId> {
        let tab = tab?;
        let told = |client: &ClientId| self.views.get(client).is_some_and(|v| v.terminal.said());
        let typist = self.tab(tab)?.typist.filter(told);
        typist.or_else(|| (self.views.keys().copied()).find(|c| self.shows(*c, tab) && told(c)))
    }

    /// What the panes of `tab` are answered with, from the terminal of
    /// `colour_client(tab)`: its colours (see the module documentation); its
    /// palette whole, so that another client's replaces it, entries it did
    /// not say cleared; and what they are told their terminal draws, which
    /// is nothing in a tab no client shows, so a program started there draws
    /// in text, which a capture reads. With no such terminal, nothing.
    pub(crate) fn host_for(&self, tab: Option<TabId>) -> (Colours, Palette, Graphics) {
        let answering = (self.colour_client(tab)).and_then(|c| self.views.get(&c));
        let answering = answering.map(|v| &v.terminal);
        let shown = tab.is_some_and(|t| self.views.keys().any(|c| self.shows(*c, t)));
        (
            answering.map_or_else(Colours::default, |t| t.colours),
            answering.map_or([None; 16], |t| t.palette),
            (answering.filter(|_| shown)).map_or_else(Graphics::default, Terminal::graphics),
        )
    }

    /// Pane `id`'s program rang the bell: each client showing its workspace
    /// is rung, at most once every [`BELL_GAP`], and one not showing its
    /// tab has the tab marked in its bar until it shows it.
    pub(crate) fn ring(&mut self, id: PaneId, now: Instant) {
        if !self.config.bell {
            return;
        }
        let Some(ws) =
            (self.parts.workspaces_mut().iter_mut()).find(|w| w.tabs().iter().any(|t| t.holds(id)))
        else {
            return;
        };
        // Each client showing the workspace, and whether it is shown the tab.
        let viewers: BTreeMap<ClientId, bool> = (ws.viewers.iter())
            .map(|c| (*c, ws.tab_of(*c).is_some_and(|t| t.holds(id))))
            .collect();
        let Some(tab) = ws.tabs_mut().iter_mut().find(|t| t.holds(id)) else {
            return;
        };
        let mut rung = Vec::new();
        for view in self.views.values_mut() {
            let Some(&shown) = viewers.get(&view.id) else {
                continue;
            };
            if let Some(seat) = tab.seat_mut(view.id).filter(|s| !shown && !s.rang) {
                seat.rang = true;
                view.dirty = true;
            }
            if view
                .last_bell
                .is_none_or(|last| now.saturating_duration_since(last) >= BELL_GAP)
            {
                view.last_bell = Some(now);
                rung.push(view.id);
            }
        }
        rung.into_iter().for_each(|client| self.ask(client, BELL));
    }

    /// What client `client`'s terminal is sent before it is painted: the
    /// tab it shows loses its bell mark, and with `titles` on its title
    /// becomes its focused pane's, or the tab's name for a pane with none;
    /// turned off, its own title is restored, and saved again for leaving.
    pub fn before_paint(&mut self, client: ClientId) -> Vec<u8> {
        let mut out = Vec::new();
        let Some(view) = self.views.get(&client) else {
            return out;
        };
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
        if let Some(seat) = self.shown_tab_mut(client).and_then(|t| t.seat_mut(client)) {
            seat.rang = false;
        }
        let Some(view) = self.views.get_mut(&client) else {
            return out;
        };
        match wanted {
            Some(Some(title)) => {
                out.extend_from_slice(b"\x1b]2;");
                out.extend_from_slice(title.as_bytes());
                out.extend_from_slice(b"\x1b\\");
                view.title = Some(title);
            }
            None if view.title.take().is_some() => {
                out.extend_from_slice(TITLE_POP);
                out.extend_from_slice(TITLE_PUSH);
            }
            Some(None) | None => {}
        }
        out
    }

    /// The title a view's terminal is given: its focused pane's, else its
    /// tab's name, without control characters (`clean`).
    fn title_of<'a>(&'a self, view: &View) -> &'a str {
        let pane = (self.focused(view.id))
            .and_then(|f| self.parts.panes().get(&f))
            .map(|p| p.title().as_str())
            .filter(|t| !t.is_empty());
        let tab = self.shown_tab(view.id).map(|t| t.name());
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
        let bytes = (s.parts.pane_mut(PaneId::of(pane)))
            .map(|mut p| p.input.drain_all())
            .unwrap_or_default();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// What pane `pane`'s program is answered to `OSC 4 ; n ; ?`.
    fn entry(s: &mut Session, pane: u32, n: u8) -> String {
        let _ = answered(s, pane);
        s.output(PaneId::of(pane), format!("\x1b]4;{n};?\x07").as_bytes());
        answered(s, pane)
    }

    #[test]
    fn a_panes_palette_query_is_answered_with_its_clients_palette() -> Outcome {
        let (mut s, c) = session()?;
        // Nothing said yet: no answer.
        assert_eq!(entry(&mut s, 1, 1), "");
        s.input(
            c,
            b"\x1b]4;1;rgb:1111/2222/3333\x1b\\\x1b]4;12;rgb:ff/80/00\x1b\\\x1b[?62c",
        );
        assert_eq!(entry(&mut s, 1, 1), "\x1b]4;1;rgb:1111/2222/3333\x07");
        assert_eq!(entry(&mut s, 1, 12), "\x1b]4;12;rgb:ffff/8080/0000\x07");
        // Entries the terminal did not say, and those past 15: none.
        assert_eq!(entry(&mut s, 1, 2), "");
        assert_eq!(entry(&mut s, 1, 17), "");
        // The program's own colour wins, and nothing the host gave is
        // painted.
        let before = crate::render::compose(&s, c);
        s.output(PaneId::of(1), b"\x1b]4;1;#00ff00\x07");
        assert_eq!(entry(&mut s, 1, 1), "\x1b]4;1;rgb:0000/ffff/0000\x07");
        s.output(PaneId::of(1), b"\x1b]104;1\x07");
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
        // not answered.
        s.input(d, b"y");
        assert_eq!(entry(&mut s, 1, 2), "\x1b]4;2;rgb:4444/5555/6666\x07");
        assert_eq!(entry(&mut s, 1, 1), "");
        Ok(())
    }

    #[test]
    fn a_pane_no_terminal_answers_for_is_told_only_its_own_colours() -> Outcome {
        let (mut s, c) = session()?;
        s.input(
            c,
            b"\x1b]11;rgb:1111/2222/3333\x1b\\\x1b]4;1;rgb:1111/2222/3333\x1b\\\x1b[?62c",
        );
        let ask = |s: &mut Session, pane| {
            let _ = answered(s, pane);
            s.output(PaneId::of(pane), b"\x1b]11;?\x07\x1b]4;1;?\x07");
            answered(s, pane)
        };
        let theirs = "\x1b]11;rgb:1111/2222/3333\x07\x1b]4;1;rgb:1111/2222/3333\x07";
        assert_eq!(ask(&mut s, 1), theirs);
        // A workspace c1 neither shows nor typed into: no terminal's.
        run(&mut s, "opts str other name new-workspace")?;
        assert_eq!(ask(&mut s, 2), "");
        s.output(PaneId::of(2), b"\x1b]11;#445566\x07");
        assert_eq!(ask(&mut s, 2), "\x1b]11;rgb:4444/5555/6666\x07");
        Ok(())
    }

    /// What the session sent client `c`'s terminal outside its paints,
    /// taken.
    fn sent(s: &mut Session, c: ClientId) -> Vec<Vec<u8>> {
        let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut s.outbox)
            .into_iter()
            .partition(|o| matches!(o, Outgoing::To(to, ToAttached::Paint(_)) if *to == c));
        s.outbox = rest;
        mine.into_iter()
            .filter_map(|o| match o {
                Outgoing::To(_, ToAttached::Paint(bytes)) => Some(bytes.into_owned()),
                Outgoing::To(
                    _,
                    ToAttached::Welcome(..)
                    | ToAttached::Exit(_)
                    | ToAttached::Ended(_)
                    | ToAttached::Run(_)
                    | ToAttached::Keys(_),
                )
                | Outgoing::Shutdown { .. } => None,
            })
            .collect()
    }

    /// Client `c`'s bar, as it would be painted.
    fn tab_label(s: &Session, c: ClientId) -> String {
        crate::render::compose(s, c)
            .map(|g| g.row_text(g.rows.saturating_sub(1)))
            .unwrap_or_default()
    }

    /// As if `BELL_GAP` had passed since every terminal was rung.
    fn past_the_gap(s: &mut Session) {
        s.views.values_mut().for_each(|v| v.last_bell = None);
    }

    #[test]
    fn a_bell_rings_the_clients_showing_its_workspace_and_marks_its_tab() -> Outcome {
        let (mut s, c) = session()?;
        let _ = sent(&mut s, c);
        s.output(PaneId::of(1), b"\x07");
        assert_eq!(sent(&mut s, c), [BELL.to_vec()]);
        // Again at once: not rung twice within the gap.
        s.output(PaneId::of(1), b"\x07\x07");
        assert!(sent(&mut s, c).is_empty());
        past_the_gap(&mut s);
        s.output(PaneId::of(1), b"\x07");
        assert_eq!(sent(&mut s, c), [BELL.to_vec()]);
        // In a tab the client does not show: its tab is marked until shown.
        run(&mut s, "opts str +1 workspace in str other name new-tab")?;
        run(&mut s, "str c1 client str @2 tab show-tab")?;
        s.settle();
        let _ = s.before_paint(c);
        past_the_gap(&mut s);
        s.output(PaneId::of(1), b"\x07");
        assert_eq!(sent(&mut s, c), [BELL.to_vec()]);
        assert!(tab_label(&s, c).contains(" main! "), "{}", tab_label(&s, c));
        run(&mut s, "str c1 client str @1 tab show-tab")?;
        let _ = s.before_paint(c);
        run(&mut s, "str c1 client str @2 tab show-tab")?;
        let _ = s.before_paint(c);
        assert!(!tab_label(&s, c).contains('!'), "{}", tab_label(&s, c));
        // A client on another workspace is not rung.
        run(&mut s, "str c1 client str @1 tab show-tab")?;
        let _ = s.before_paint(c);
        run(&mut s, "opts str elsewhere name new-workspace")?;
        let d = s.attach(10, 40, None).map_err(|e| e.to_string())?;
        run(
            &mut s,
            "str c2 client str elsewhere workspace show-workspace",
        )?;
        let _ = sent(&mut s, d);
        past_the_gap(&mut s);
        s.output(PaneId::of(1), b"\x07");
        assert_eq!(sent(&mut s, c), [BELL.to_vec()]);
        assert!(sent(&mut s, d).is_empty());
        // Off: no bell and no mark.
        run(&mut s, "str bell str off set")?;
        past_the_gap(&mut s);
        s.output(PaneId::of(1), b"\x07");
        assert!(sent(&mut s, c).is_empty());
        assert!(!tab_label(&s, c).contains('!'));
        Ok(())
    }

    #[test]
    fn titles_follow_the_focused_pane_and_are_restored_when_turned_off() -> Outcome {
        let (mut s, c) = session()?;
        // Off by default: nothing.
        s.output(PaneId::of(1), b"\x1b]2;vim notes\x07");
        assert!(s.before_paint(c).is_empty());
        run(&mut s, "str titles str on set")?;
        let first = s.before_paint(c);
        assert_eq!(first, b"\x1b]2;vim notes\x1b\\");
        // Unchanged: nothing more; changed: the title alone.
        assert!(s.before_paint(c).is_empty());
        s.output(PaneId::of(1), b"\x1b]2;make\x07");
        assert_eq!(s.before_paint(c), b"\x1b]2;make\x1b\\");
        // A pane with no title: its tab's name.
        run(&mut s, "str %1 pane horizontal split")?;
        run(&mut s, "str c1 client str %2 pane focus-pane")?;
        run(&mut s, "str @1 tab str notes rename")?;
        assert_eq!(s.before_paint(c), b"\x1b]2;notes\x1b\\");
        // Off again: the terminal's own title is restored, once, and saved
        // again for the client's leaving.
        run(&mut s, "str titles str off set")?;
        assert_eq!(s.before_paint(c), [TITLE_POP, TITLE_PUSH].concat());
        assert!(s.before_paint(c).is_empty());
        Ok(())
    }
}
