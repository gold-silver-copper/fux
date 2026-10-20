//! `encoders`: fux-vt's input encoders beside libghostty-vt's. Both
//! terminals are put in the same modes by the same bytes (mouse tracking and
//! its encoding, focus reporting, bracketed paste, cursor keys, the kitty
//! keyboard flags, modifyOtherKeys); then each key, mouse event, focus
//! change and paste is encoded by both, each reading the modes from its own
//! terminal (`Screen::encode_*`, ghostty's `set_options_from_terminal`), and
//! the bytes are compared. A difference is a failure unless it is one of
//! the recorded verdicts below, each with who decides it.
//!
//! The comparison is in `beside`, built with the `engines` feature (it
//! links libghostty-vt); the verdicts are here.
#[cfg(feature = "engines")]
mod beside;
#[cfg(feature = "engines")]
pub use beside::run;

/// Differences decided in fux-vt's favour or recorded as a choice, and the
/// source that decides each.
#[cfg(feature = "engines")]
pub const VERDICTS: &[(&str, &str)] = &[
    (
        "utf8-button",
        "UTF-8 mouse mode (1005), a button code of 96 or more: ctlseqs says \"Cb will be UTF-8 \
         encoded\", as fux-vt does; ghostty writes it as one raw byte",
    ),
    (
        "utf8-limit",
        "UTF-8 mouse mode (1005), a position past 2015: ctlseqs gives 2015 as its last and not \
         what comes past it, so xterm 411 decides: fux-vt clamps it and sends a 0 byte, as xterm's \
         `EmitMousePosition` does; ghostty encodes it in three bytes",
    ),
    (
        "default-limit",
        "the default mouse encoding, a position past 223: ctlseqs gives 223 as its last and not \
         what comes past it, so xterm 411 decides: fux-vt clamps it and sends a 0 byte, as xterm's \
         `EmitMousePosition` does; ghostty sends nothing",
    ),
    (
        "legacy-extras",
        "no keyboard mode set: ghostty sends keys legacy bytes cannot carry as CSI u (Ctrl-I, \
         Ctrl-Shift-letters, Ctrl-punctuation) and modified Enter, Tab and Escape as CSI 27 ; m ; \
         k ~; xterm with modifyOtherKeys off sends the legacy bytes, as fux-vt does: a program \
         that asked for nothing gets nothing it did not ask for",
    ),
    (
        "f3",
        "F3 with modifiers, outside the kitty protocol: xterm's PC-style function keys send CSI 1 \
         ; m R, as fux-vt does; ghostty sends kitty's CSI 13 ; m ~",
    ),
    (
        "ctrl-backspace-bs",
        "Backspace with Ctrl and another modifier is BS (8): real xterm 411 sends ESC BS for \
         Ctrl-Alt and Ctrl-Alt-Shift (`tests/data/keys/xterm-keys.txt` lines 420 and \
         422: `C-M-BSpace \\e\\x08`, `C-M-S-BSpace \\e\\x08`), and under modifyOtherKeys 2 \
         `CSI 27 ; m ; 8 ~` (lines 1659, 1660 and 1662: C-S `\\e[27;6;8~`, C-M \
         `\\e[27;7;8~`, C-M-S `\\e[27;8;8~`), as fux-vt does; ghostty sends DEL (127). Plain \
         Ctrl-Backspace is BS in all three (lines 417 and 1657: `\\x08`)",
    ),
    (
        "modify-other-keys",
        "modifyOtherKeys 2: xterm sends every modified key as CSI 27 ; m ; k ~, Ctrl-letters, \
         Shift-letters and Alt-Escape included, but Ctrl alone on Backspace and Shift alone on a \
         character Ctrl makes no control of (`!`, `)`, `+`), which types its text, as real xterm \
         411 sends them (`tests/data/keys/xterm-keys.txt`) and fux-vt does; ghostty keeps the \
         legacy bytes of the first and escapes the second",
    ),
    (
        "ctrl-alt-question",
        "Ctrl-Alt-? with no kitty flags and no modifyOtherKeys 2: real xterm 411 sends ESC ?, \
         its Ctrl-? being DEL by its own translation, which Alt passes by (\
         `tests/data/keys/xterm-keys.txt`: `CMS slash legacy C-M-? \\e?`), as fux-vt does; \
         ghostty sends ESC DEL",
    ),
    (
        "shift-tab",
        "Shift-Tab under modifyOtherKeys 2: real xterm 411 sends CSI Z, ISO_Left_Tab's, with \
         Shift alone (`tests/data/keys/xterm-keys.txt`: `S Tab legacy mok=2 S-Tab \
         \\e[Z`), as fux-vt does; ghostty sends CSI 27 ; 2 ; 9 ~",
    ),
    (
        "kitty-flags-without-disambiguate",
        "kitty flags without disambiguate (1) or all keys (8): the spec gives these keys no other \
         encoding, and fux-vt sends their legacy bytes; ghostty sends kitty's forms",
    ),
    (
        "kitty-press-type",
        "report event types (2): a press may leave out its event type (\"CSI key-code;modifier \
         # this is a press event\") and fux-vt does, as kitty does; ghostty writes ;1:1",
    ),
    (
        "paste-controls",
        "a paste holding NUL, BS, ENQ, EOT, ESC, DEL or the tty's special characters (^C ^\\ ^U \
         ^Z ^Q ^S ^W ^V ^R ^O): ghostty, as xterm 411 (`removeControls`, `button.c`), makes each \
         a space; fux-vt passes them as pasted, removing only what would end a bracketed paste: \
         xterm's set depends on the pane's tty, and a host's paste comes from its own terminal, \
         which filtered it",
    ),
    (
        "paste-newline",
        "a bracketed paste holding LF: fux-vt sends it as CR, as xterm 411 sends every paste \
         (`button.c`: each `\\n` a `\\r` unless `pasteLiteralNL`); ghostty keeps the LF inside \
         the brackets (as it sends CR unbracketed)",
    ),
    (
        "paste-c1-end",
        "a bracketed paste holding U+009B 201 ~ (the C1 form of its end): fux-vt removes it, as \
         it removes ESC [ 201 ~; ghostty keeps it",
    ),
];
