//! Stable, inspectable fixture format. Missing cells mean empty/default cells.
use fux_vt::{Color, Screen};
use std::fmt::Write;

pub fn flags(bold: bool, dim: bool, italic: bool, underline: bool, inverse: bool) -> u8 {
    u8::from(bold)
        | u8::from(dim) << 1
        | u8::from(italic) << 2
        | u8::from(underline) << 3
        | u8::from(inverse) << 4
}
pub fn attributes(fg: Color, bg: Color, flags: u8) -> String {
    format!("{fg:?}/{bg:?}/{flags}")
}
pub fn header(
    size: (u16, u16),
    cursor: (u16, u16),
    modes: (bool, bool, bool, bool),
    mouse: &str,
    encoding: &str,
    attributes: &str,
) -> String {
    format!(
        "size={size:?} cursor={cursor:?} modes={modes:?} mouse={mouse} encoding={encoding} attrs={attributes}\n"
    )
}
pub fn cell(
    out: &mut String,
    index: usize,
    text: &str,
    wide: bool,
    continuation: bool,
    attributes: &str,
) {
    if text.is_empty() && !wide && !continuation && attributes == "Default/Default/0" {
        return;
    }
    let _ = writeln!(
        out,
        "cell={index} text={text:?} wide={wide} continuation={continuation} attrs={attributes}"
    );
}
pub fn screen(s: &Screen) -> String {
    let attrs = s.attributes();
    let a = attributes(
        attrs.foreground,
        attrs.background,
        flags(
            attrs.bold(),
            attrs.dim(),
            attrs.italic(),
            attrs.underline(),
            attrs.inverse(),
        ),
    );
    let mut out = header(
        s.size(),
        s.cursor_position(),
        (
            s.hide_cursor(),
            s.application_cursor(),
            s.bracketed_paste(),
            s.alternate_screen(),
        ),
        &format!("{:?}", s.mouse_protocol_mode()),
        &format!("{:?}", s.mouse_protocol_encoding()),
        &a,
    );
    let rows = s.history_len().saturating_add(usize::from(s.size().0));
    // Oldest first.
    for (index, from_bottom) in (0..rows).rev().enumerate() {
        if let Some(row) = s.row_from_bottom(from_bottom) {
            let _ = writeln!(out, "row={index} wrapped={}", row.wrapped);
            for (i, c) in row.cells.iter().enumerate() {
                cell(
                    &mut out,
                    i,
                    c.contents(),
                    c.is_wide(),
                    c.is_wide_continuation(),
                    &attributes(
                        c.fgcolor(),
                        c.bgcolor(),
                        flags(c.bold(), c.dim(), c.italic(), c.underline(), c.inverse()),
                    ),
                );
            }
        }
    }
    out
}
