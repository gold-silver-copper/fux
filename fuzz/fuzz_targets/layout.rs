#![no_main]
//! Input: four bytes for the area's width and height (a high byte below 0x80
//! is a small size, `lo % 64`; ff is u16::MAX), the splits that grow a tree
//! from one pane, then two-byte resizes.
use fux::id::PaneId;
use fux::keys::Direction;
use fux::layout::{self, Axis, MIN, Placement, Rect, Side, Tree};
use libfuzzer_sys::fuzz_target;

struct Bytes<'a> {
    rest: &'a [u8],
}
impl Bytes<'_> {
    fn next(&mut self) -> u8 {
        let Some((&b, rest)) = self.rest.split_first() else {
            return 0;
        };
        self.rest = rest;
        b
    }
    fn length(&mut self) -> u16 {
        let (hi, lo) = (self.next(), self.next());
        match hi {
            0xff => u16::MAX,
            0x80.. => u16::from_be_bytes([hi, lo]),
            _ => u16::from(lo % 64),
        }
    }
    /// A tree of up to 16 panes, grown from one by splits, each two
    /// bytes: the pane to split, then the axis and side.
    fn tree(&mut self) -> Tree {
        let mut root = Tree::Pane(pane(1));
        let count = self.next() % 16;
        for new in 2..=u32::from(count) + 1 {
            let (which, how) = (self.next(), self.next());
            let panes = root.panes();
            let target = panes[usize::from(which) % panes.len()];
            let axis = if how & 1 == 0 {
                Axis::Horizontal
            } else {
                Axis::Vertical
            };
            let side = if how & 2 == 0 {
                Side::After
            } else {
                Side::Before
            };
            root.split(target, pane(new), axis, side);
        }
        root
    }
}

fn pane(n: u32) -> PaneId {
    format!("%{n}").parse().expect("a pane's number")
}

fn overlap(a: &Rect, b: &Rect) -> bool {
    a.x() < b.right() && b.x() < a.right() && a.y() < b.bottom() && b.y() < a.bottom()
}

/// Everything `place` promises about `placement` of `root` in `area`.
fn check(root: &Tree, area: Rect, placement: &Placement) {
    if area.is_empty() {
        assert!(placement.panes.is_empty() && placement.separators.is_empty());
        return;
    }
    let panes = root.panes();
    let mut pieces: Vec<Rect> = Vec::new();
    for (id, r) in &placement.panes {
        assert!(panes.contains(id), "{id} is not in the tree");
        assert_eq!(
            placement.panes.iter().filter(|(p, _)| p == id).count(),
            1,
            "{id} placed twice"
        );
        assert!(!r.is_empty(), "{id} placed empty: {r:?}");
        // As large as the minimum, where the area has room for it.
        assert!(
            r.w() >= MIN.min(area.w()) && r.h() >= MIN.min(area.h()),
            "{id} too small: {r:?} in {area:?}"
        );
        pieces.push(*r);
    }
    for s in &placement.separators {
        assert!(!s.rect.is_empty(), "an empty separator: {s:?}");
        pieces.push(s.rect);
    }
    let mut covered = 0u64;
    for (i, piece) in pieces.iter().enumerate() {
        assert!(
            piece.x() >= area.x()
                && piece.right() <= area.right()
                && piece.y() >= area.y()
                && piece.bottom() <= area.bottom(),
            "{piece:?} is outside {area:?}"
        );
        for other in &pieces[i + 1..] {
            assert!(!overlap(piece, other), "{piece:?} overlaps {other:?}");
        }
        covered += u64::from(piece.area());
    }
    // Disjoint, so never more than the area. Room for 16 panes side by side
    // is room for any tree of them, and every pane shown; and with every
    // pane shown they tile the area exactly. (Where it is too small, a split
    // without room for even its first child shows nothing.)
    let whole = u64::from(area.area());
    assert!(covered <= whole);
    let room = 16 * MIN + 15;
    if area.w() >= room && area.h() >= room {
        assert_eq!(placement.panes.len(), panes.len(), "a pane is hidden");
    }
    if placement.panes.len() == panes.len() {
        assert_eq!(covered, whole, "the area is not tiled: {placement:?}");
    }
}

const DIRECTIONS: [Direction; 4] = [
    Direction::Left,
    Direction::Right,
    Direction::Up,
    Direction::Down,
];

fuzz_target!(|data: &[u8]| {
    let mut bytes = Bytes { rest: data };
    let (x, y) = (u16::from(bytes.next() % 4), u16::from(bytes.next() % 4));
    let (w, h) = (bytes.length(), bytes.length());
    // The area may sit anywhere its far edge still fits a u16 screen.
    let (x, y) = (x.min(u16::MAX - w), y.min(u16::MAX - h));
    let screen = Rect::screen(y + h, x + w);
    let area = screen
        .split(Axis::Vertical, y)
        .1
        .split(Axis::Horizontal, x)
        .1;
    let mut root = bytes.tree();
    let placement = layout::place(&root, area);
    check(&root, area, &placement);
    let ids = root.panes();
    for (from, _) in &placement.panes {
        for direction in DIRECTIONS {
            if let Some(to) = layout::neighbor(&placement, *from, direction) {
                assert!(
                    to != *from && placement.rect(to).is_some(),
                    "{from} {direction:?} gives {to}"
                );
            }
        }
    }
    while !bytes.rest.is_empty() {
        let (which, how) = (bytes.next(), bytes.next());
        let Some(&pane) = ids.get(usize::from(which) % ids.len().max(1)) else {
            break;
        };
        let direction = DIRECTIONS[usize::from(how % 4)];
        let amount = u16::from(how >> 2);
        layout::resize(&mut root, area, pane, direction, amount);
        let placement = layout::place(&root, area);
        check(&root, area, &placement);
    }
});
