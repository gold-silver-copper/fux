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
    fn tree(&mut self) -> Option<Tree> {
        let mut root = Some(Tree::Pane(pane(1)));
        let count = self.next() % 16;
        for new in 2..=u32::from(count) + 1 {
            let (which, how) = (self.next(), self.next());
            let panes = root.as_ref().map(Tree::panes).unwrap_or_default();
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
            layout::split(&mut root, target, pane(new), axis, side);
        }
        root
    }
}

fn pane(n: u32) -> PaneId {
    format!("%{n}").parse().expect("a pane's number")
}

/// A rect's cells as x and y ranges, in u32 so that no end overflows.
fn span(r: &Rect) -> (std::ops::Range<u32>, std::ops::Range<u32>) {
    let (x, y) = (u32::from(r.x), u32::from(r.y));
    (x..x + u32::from(r.w), y..y + u32::from(r.h))
}

fn overlap(a: &Rect, b: &Rect) -> bool {
    let ((ax, ay), (bx, by)) = (span(a), span(b));
    ax.start < bx.end && bx.start < ax.end && ay.start < by.end && by.start < ay.end
}

/// Everything `place` promises about `placement` of `root` in `area`.
fn check(root: &Tree, area: Rect, placement: &Placement) {
    if area.w == 0 || area.h == 0 {
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
        assert!(r.w > 0 && r.h > 0, "{id} placed empty: {r:?}");
        // As large as the minimum, where the area has room for it.
        assert!(
            r.w >= MIN.min(area.w) && r.h >= MIN.min(area.h),
            "{id} too small: {r:?} in {area:?}"
        );
        pieces.push(*r);
    }
    for s in &placement.separators {
        assert!(s.len > 0, "an empty separator: {s:?}");
        pieces.push(s.rect());
    }
    let (ax, ay) = span(&area);
    let mut covered = 0u64;
    for (i, piece) in pieces.iter().enumerate() {
        let (px, py) = span(piece);
        assert!(
            px.start >= ax.start && px.end <= ax.end && py.start >= ay.start && py.end <= ay.end,
            "{piece:?} is outside {area:?}"
        );
        for other in &pieces[i + 1..] {
            assert!(!overlap(piece, other), "{piece:?} overlaps {other:?}");
        }
        covered += u64::from(piece.w) * u64::from(piece.h);
    }
    // Disjoint, so never more than the area. Room for 16 panes side by side
    // is room for any tree of them, and every pane shown; and with every
    // pane shown they tile the area exactly. (Where it is too small, a split
    // without room for even its first child shows nothing.)
    let whole = u64::from(area.w) * u64::from(area.h);
    assert!(covered <= whole);
    let room = 16 * MIN + 15;
    if area.w >= room && area.h >= room {
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
    let area = Rect {
        x: x.min(u16::MAX - w),
        y: y.min(u16::MAX - h),
        w,
        h,
    };
    let Some(mut root) = bytes.tree() else {
        return;
    };
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
