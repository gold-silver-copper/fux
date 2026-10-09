//! The modes a program sets and resets by number. One table names them
//! ([`Mode::of`]), one says where each is kept ([`Mode::kind`]), and
//! `Screen::mode` and `Screen::set_mode` read and set every one by its kind.

use crate::{Feature, MouseProtocolEncoding, MouseProtocolMode, Options};

/// A mode a program sets and resets by number: an ANSI mode (SM and RM,
/// `CSI Ps h` / `l`) or a DEC private mode (DECSET and DECRST, `CSI ? Ps h`
/// / `l`). [`Screen::mode`](crate::Screen::mode) says whether it is set, as
/// DECRQM reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Mode {
    /// IRM (`CSI 4 h`): a glyph printed moves what is there right.
    Insert,
    /// LNM (`CSI 20 h`): LF, VT and FF return the carriage too.
    NewLine,
    /// DECCKM (`CSI ? 1 h`): cursor keys send their application sequences.
    ApplicationCursor,
    /// DECSCLM, smooth scroll (`CSI ? 4 h`). State only.
    SmoothScroll,
    /// DECSCNM, reverse video (`CSI ? 5 h`). State only.
    ReverseVideo,
    /// DECOM (`CSI ? 6 h`): lines are addressed from the top margin.
    Origin,
    /// DECAWM (`CSI ? 7 h`): a glyph past the last column wraps. Default.
    Autowrap,
    /// DECARM (`CSI ? 8 h`): permanently reset, as in xterm 411.
    AutoRepeat,
    /// Mouse mode 9, X10: button presses.
    MousePress,
    /// DECTCEM (`CSI ? 25 h`): the cursor is shown. Default.
    ShowCursor,
    /// Reverse wraparound (`CSI ? 45 h`): BS and CUB go back over soft wraps.
    ReverseWrap,
    /// The alternate screen (`CSI ? 47 h`).
    AlternateScreen,
    /// DECNKM (`CSI ? 66 h`), the keypad mode `ESC =` and `ESC >` set too.
    ApplicationKeypad,
    /// DECBKM (`CSI ? 67 h`), the backarrow key sending BS. State only.
    BackarrowSendsBackspace,
    /// DECLRMM (`CSI ? 69 h`): DECSLRM sets left and right margins.
    LeftRightMargins,
    /// Mouse mode 1000: presses and releases.
    MousePressRelease,
    /// Mouse mode 1002: also motion while a button is down.
    MouseButtonMotion,
    /// Mouse mode 1003: also motion with no button down.
    MouseAnyMotion,
    /// Focus reporting (`CSI ? 1004 h`), as `Screen::encode_focus` makes it.
    FocusReporting,
    /// Mouse encoding 1005: coordinates in UTF-8.
    MouseUtf8,
    /// Mouse encoding 1006, SGR.
    MouseSgr,
    /// Extended reverse wraparound (`CSI ? 1045 h`): back over any line.
    ExtendedReverseWrap,
    /// The alternate screen (`CSI ? 1047 h`), cleared on leaving it.
    AlternateScreenCleared,
    /// `CSI ? 1048 h` / `l`: DECSC and DECRC, never set.
    SaveCursor,
    /// The alternate screen (`CSI ? 1049 h`), cleared, the cursor saved.
    AlternateScreenSaveCursor,
    /// Bracketed paste (`CSI ? 2004 h`).
    BracketedPaste,
    /// Synchronized output (`CSI ? 2026 h`): a frame the host is to hold.
    SynchronizedOutput,
    /// Colour-scheme change reports (`CSI ? 2031 h`), which the host sends.
    ColorSchemeUpdates,
    /// In-band resize (`CSI ? 2048 h`): a size report at each resize.
    InBandResize,
}

// Each mode is a bit of a `Modes`.
const _: () = assert!((Mode::InBandResize as u32) < u32::BITS);

impl Mode {
    /// The mode SM and RM (`private` false: IRM and LNM alone) or DECSET
    /// and DECRST name by `n`, with `options`: without theirs, 2031 and
    /// 2048 are not recognized.
    #[inline(always)]
    pub(crate) fn of(n: u16, private: bool, options: &Options) -> Option<Self> {
        Some(match (private, n) {
            (false, 4) => Self::Insert,
            (false, 20) => Self::NewLine,
            (false, _) => return None,
            (true, 1) => Self::ApplicationCursor,
            (true, 4) => Self::SmoothScroll,
            (true, 5) => Self::ReverseVideo,
            (true, 6) => Self::Origin,
            (true, 7) => Self::Autowrap,
            (true, 8) => Self::AutoRepeat,
            (true, 9) => Self::MousePress,
            (true, 25) => Self::ShowCursor,
            (true, 45) => Self::ReverseWrap,
            (true, 47) => Self::AlternateScreen,
            (true, 66) => Self::ApplicationKeypad,
            (true, 67) => Self::BackarrowSendsBackspace,
            (true, 69) => Self::LeftRightMargins,
            (true, 1000) => Self::MousePressRelease,
            (true, 1002) => Self::MouseButtonMotion,
            (true, 1003) => Self::MouseAnyMotion,
            (true, 1004) => Self::FocusReporting,
            (true, 1005) => Self::MouseUtf8,
            (true, 1006) => Self::MouseSgr,
            (true, 1045) => Self::ExtendedReverseWrap,
            (true, 1047) => Self::AlternateScreenCleared,
            (true, 1048) => Self::SaveCursor,
            (true, 1049) => Self::AlternateScreenSaveCursor,
            (true, 2004) => Self::BracketedPaste,
            (true, 2026) => Self::SynchronizedOutput,
            (true, 2031) if options.has(Feature::ColorSchemeUpdates) => Self::ColorSchemeUpdates,
            (true, 2048) if options.has(Feature::InBandResize) => Self::InBandResize,
            (true, _) => return None,
        })
    }

    /// Where its state is kept, and what setting it does.
    #[inline(always)]
    pub(crate) fn kind(self) -> Kind {
        match self {
            Self::Origin => Kind::Origin,
            Self::AlternateScreen => Kind::Screen(Switch::Plain),
            Self::AlternateScreenCleared => Kind::Screen(Switch::ClearedOnLeaving),
            Self::AlternateScreenSaveCursor => Kind::Screen(Switch::SavingCursor),
            Self::SaveCursor => Kind::SaveCursor,
            Self::AutoRepeat => Kind::Reset,
            Self::MousePress => Kind::Mouse(MouseProtocolMode::Press),
            Self::MousePressRelease => Kind::Mouse(MouseProtocolMode::PressRelease),
            Self::MouseButtonMotion => Kind::Mouse(MouseProtocolMode::ButtonMotion),
            Self::MouseAnyMotion => Kind::Mouse(MouseProtocolMode::AnyMotion),
            Self::MouseUtf8 => Kind::Encoding(MouseProtocolEncoding::Utf8),
            Self::MouseSgr => Kind::Encoding(MouseProtocolEncoding::Sgr),
            Self::Insert
            | Self::NewLine
            | Self::ApplicationCursor
            | Self::SmoothScroll
            | Self::ReverseVideo
            | Self::Autowrap
            | Self::ShowCursor
            | Self::ReverseWrap
            | Self::ApplicationKeypad
            | Self::BackarrowSendsBackspace
            | Self::FocusReporting
            | Self::ExtendedReverseWrap
            | Self::BracketedPaste
            | Self::ColorSchemeUpdates => Kind::Flag,
            Self::LeftRightMargins => Kind::Margins,
            Self::SynchronizedOutput => Kind::Frame,
            Self::InBandResize => Kind::SizeReport,
        }
    }

    /// Whether XTSAVE saves it and XTRESTORE restores it: not 1048, an
    /// action, nor DECARM, permanently reset.
    pub(crate) fn savable(self) -> bool {
        !matches!(self.kind(), Kind::SaveCursor | Kind::Reset)
    }

    const fn bit(self) -> u32 {
        1u32.wrapping_shl(self as u32)
    }
}

/// Where a mode's state is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A bit of the screen's [`Modes`], and no more.
    Flag,
    /// DECLRMM: a bit; reset, the margins go back to the screen's edges.
    Margins,
    /// Synchronized output: a bit; set, a frame begins.
    Frame,
    /// In-band resize: a bit; set, a size report is due.
    SizeReport,
    /// The shown grid's DECOM.
    Origin,
    /// Whether the alternate screen is shown.
    Screen(Switch),
    /// 1048: an action, never set, and not saved by XTSAVE.
    SaveCursor,
    /// DECARM: permanently reset, and not saved by XTSAVE.
    Reset,
    /// The mouse reporting, the latest set winning.
    Mouse(MouseProtocolMode),
    /// The mouse encoding, the latest set winning.
    Encoding(MouseProtocolEncoding),
}

/// How a mode switches to the alternate screen and back (xterm's ctlseqs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Switch {
    /// 47.
    Plain,
    /// 1047: cleared on leaving it.
    ClearedOnLeaving,
    /// 1049: DECSC, then cleared; and back, then DECRC.
    SavingCursor,
}

/// A set of modes, a bit each.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Modes(u32);

impl Modes {
    /// The modes set on a new screen, and after RIS.
    pub(crate) const DEFAULT: Self = Self(Mode::Autowrap.bit() | Mode::ShowCursor.bit());

    /// The modes DECSTR puts back as they are by default: those of the
    /// VT520 manual's table (p. 5-150) and DEC STD 070's Soft Terminal
    /// Reset (p. 4-37); reverse wraparound, as xterm 411's DECSTR ends
    /// both; and synchronized output, in neither table, so that `tput
    /// init` and `tput reset`, which send DECSTR, never leave a frame
    /// waiting.
    pub(crate) const SOFT_RESET: Self = Self(
        Mode::ShowCursor.bit()
            | Mode::SynchronizedOutput.bit()
            | Mode::Autowrap.bit()
            | Mode::ReverseWrap.bit()
            | Mode::ExtendedReverseWrap.bit()
            | Mode::Insert.bit()
            | Mode::ApplicationCursor.bit()
            | Mode::ApplicationKeypad.bit()
            | Mode::LeftRightMargins.bit(),
    );

    pub(crate) fn contains(self, mode: Mode) -> bool {
        self.0 & mode.bit() != 0
    }

    pub(crate) fn set(&mut self, mode: Mode, on: bool) {
        self.0 = (self.0 & !mode.bit()) | if on { mode.bit() } else { 0 };
    }

    /// The modes in `which` back as they are by default.
    pub(crate) fn reset(&mut self, which: Self) {
        self.0 = (self.0 & !which.0) | (Self::DEFAULT.0 & which.0);
    }
}
