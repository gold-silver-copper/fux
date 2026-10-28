//! DEC checksum contributions, including fux-vt's documented departures:
//! blank cells count as spaces, wide tails as ESC, and combining marks add
//! their code points. Rendition bits follow DEC STD 070 DECRQCRA; see the
//! evidence beside opt_in::rectangle_checksums_sum_the_cells.

pub const QUERY: &[u8] = b"\x1b[7;0;1;1;1;1*y\x1b[8;0;1;2;1;2*y";
pub const CELLS: &[(&str, u16, u16)] = &[
    ("a", 0x61u16, 0x20u16),
    ("ab", 0x61, 0x62),
    ("\x1b[1ma", 0xe1, 0x20),
    ("\x1b[4ma", 0x71, 0x20),
    ("\x1b[5ma", 0xa1, 0x20),
    ("\x1b[7ma", 0x81, 0x20),
    ("\x1b[8ma", 0x69, 0x20),
    ("\x1b[1;4;5;7;8ma", 0x159, 0x20),
    ("\x1b[31;42;2;3;9ma", 0x61, 0x20),
    ("界", 0x1b, 0x1b),
    ("\x1b[1m界", 0x9b, 0x1b),
    ("e\u{301}\u{302}", 0x668, 0x20),
    ("é", 0xe9, 0x20),
    ("€", 0x1b, 0x20),
    ("\x1b(0q", 0x1d, 0x20),
    ("\x1b(%5W", 0x97, 0x20),
    ("ab\r\x1b[1;7m\x1b[K", 0x20, 0x20),
    ("a漢", 0x1b, 0x1b),
];
