#![no_main]
//! Input: four bytes for the area's width and height (a high byte below 0x80
//! is a small size, `lo % 64`; ff is u16::MAX), a tree of splits and panes,
//! then two-byte resizes.
use fux::keys::Direction;
use fux::layout::{self, Axis, MIN, Node, PaneId, Placement, Rect};
use libfuzzer_sys::fuzz_target;

struct Bytes<'a> {
    rest: &'a [u8],
    panes: u32,
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
    fn weight(&mut self) -> u32 {
        let b = self.next();
        match b % 8 {
            0 => 0,
            1 => 1,
            2 => layout::WEIGHT,
            3 => u32::MAX,
            4 => u32::MAX - 1,
            _ => u32::from(b) * 37,
        }
    }
    /// A pane, or a split of two to four children, at most four deep and
    /// with at most 16 panes.
    fn node(&mut self, depth: u32) -> Node {
        let b = self.next();
        if depth >= 4 || self.panes >= 16 || b & 0x80 == 0 {
            self.panes += 1;
            return Node::Pane(PaneId(self.panes));
        }
        let axis = if b & 1 == 0 {
            Axis::Horizontal
        } else {
            Axis::Vertical
        };
        let count = 2 + (b >> 1) % 3;
        let children = (0..count)
            .map(|_| {
                let weight = self.weight();
                (weight, self.node(depth + 1))
            })
            .collect();
        Node::Split { axis, children }
    }
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
fn check(root: &Node, area: Rect, placement: &Placement) {
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
        pieces.push(if s.vertical {
            Rect {
                x: s.x,
                y: s.y,
                w: 1,
                h: s.len,
            }
        } else {
            Rect {
                x: s.x,
                y: s.y,
                w: s.len,
                h: 1,
            }
        });
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
    // Disjoint, so never more than the area; and when the whole tree fits,
    // exactly the area: the panes and separators tile it. (Where it does not
    // fit, a split without room for even its first child shows nothing.)
    let whole = u64::from(area.w) * u64::from(area.h);
    assert!(covered <= whole);
    if area.w >= min_len(root, Axis::Horizontal) && area.h >= min_len(root, Axis::Vertical) {
        assert_eq!(covered, whole, "the area is not tiled: {placement:?}");
    }
}

/// The smallest length `node` can have along `axis`, as the layout docs
/// define it: a pane needs MIN; a split along the axis needs its children's
/// and a separator between each two; across it, its largest child's.
fn min_len(node: &Node, axis: Axis) -> u16 {
    match node {
        Node::Pane(_) => MIN,
        Node::Split {
            axis: own,
            children,
        } => {
            let mins = children.iter().map(|(_, c)| min_len(c, axis));
            if *own == axis {
                let separators = u16::try_from(children.len() - 1).unwrap_or(u16::MAX);
                mins.fold(separators, u16::saturating_add)
            } else {
                mins.max().unwrap_or(MIN)
            }
        }
    }
}

const DIRECTIONS: [Direction; 4] = [
    Direction::Left,
    Direction::Right,
    Direction::Up,
    Direction::Down,
];

fuzz_target!(|data: &[u8]| {
    let mut bytes = Bytes {
        rest: data,
        panes: 0,
    };
    let (x, y) = (u16::from(bytes.next() % 4), u16::from(bytes.next() % 4));
    let (w, h) = (bytes.length(), bytes.length());
    // The area may sit anywhere its far edge still fits a u16 screen.
    let area = Rect {
        x: x.min(u16::MAX - w),
        y: y.min(u16::MAX - h),
        w,
        h,
    };
    let mut root = bytes.node(0);
    // fux keeps its trees normalized after every change.
    layout::normalize(&mut root);
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
