//! A tab's layout: a binary tree of splits whose leaves are panes, and the
//! rectangles it gives each pane at a given size.
//!
//! Each split divides its room between its two children by two exact
//! shares, and only these change them:
//! - a split puts a new pane beside one, the two dividing its room as
//!   asked;
//! - a close puts the closed pane's sibling in their split's place, taking
//!   all its room, every other share as it was;
//! - a border moved (`resize`, `resize_to`) gives each split it moves the
//!   cells its children then have as their shares.
//!
//! A screen's size changes no share: each split shares out what it has.
use crate::id::PaneId;
use crate::keys::Direction;
use crate::wire::wire;
use std::num::NonZeroU32;
use std::ops::Range;

wire! {
    /// How a split arranges its children.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Axis {
        /// Side by side, divided by vertical separators (`horizontal split`).
        Horizontal,
        /// Stacked, divided by horizontal separators (`vertical split`).
        Vertical,
    }
}

impl Axis {
    pub fn of(direction: Direction) -> Axis {
        match direction {
            Direction::Left | Direction::Right => Axis::Horizontal,
            Direction::Up | Direction::Down => Axis::Vertical,
        }
    }
}

/// A tab's layout: one pane, or a split of two.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tree {
    Pane(PaneId),
    Split(Box<Split>),
}

/// Two trees side by side along `axis`, a separator between, sharing what
/// room there is by `shares`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Split {
    axis: Axis,
    shares: Shares,
    first: Tree,
    second: Tree,
}

/// How two divide their room, one to the other, at their least: two
/// splits that divide it alike are alike.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shares(NonZeroU32, NonZeroU32);

/// The greatest number dividing both, 0 for none.
fn gcd(mut a: u32, mut b: u32) -> u32 {
    while let Some(rest) = a.checked_rem(b) {
        (a, b) = (b, rest);
    }
    a
}

impl Shares {
    /// In halves: what a split divides a pane's room by unless told.
    pub const HALVED: Shares = Shares(NonZeroU32::MIN, NonZeroU32::MIN);

    /// `a` to `b`, at their least.
    pub fn new(a: NonZeroU32, b: NonZeroU32) -> Shares {
        let least = |n: NonZeroU32| {
            let n = n.get().checked_div(gcd(a.get(), b.get())).unwrap_or(1);
            NonZeroU32::new(n).unwrap_or(NonZeroU32::MIN)
        };
        Shares(least(a), least(b))
    }

    /// As many as there are cells, `a` to `b`, each at least 1.
    fn of(a: u16, b: u16) -> Shares {
        let share = |n: u16| NonZeroU32::new(u32::from(n)).unwrap_or(NonZeroU32::MIN);
        Shares::new(share(a), share(b))
    }

    pub fn get(self) -> (NonZeroU32, NonZeroU32) {
        (self.0, self.1)
    }

    fn swapped(self) -> Shares {
        Shares(self.1, self.0)
    }
}

/// A pane a layout change names is not in the tree, which is as it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoPane;

/// What a tree is without a pane (`Tree::remove`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Removed {
    /// The pane is not in it.
    Absent,
    /// What is left.
    Left(Tree),
    /// It was the tree's only pane: nothing is left.
    Emptied,
}

/// The smallest pane, in each dimension.
pub const MIN: u16 = 2;

/// A rectangle of cells on a screen. Its right and bottom edges are within a
/// u16, as a screen's are: one is made only as a whole screen
/// ([`Rect::screen`]) or cut from another ([`Rect::split`], [`Rect::corner`],
/// [`Rect::lines`]), so every position inside one is exact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    x: u16,
    y: u16,
    w: u16,
    h: u16,
}

impl Rect {
    /// A whole screen of `rows` by `cols`.
    pub const fn screen(rows: u16, cols: u16) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: cols,
            h: rows,
        }
    }
    pub fn x(&self) -> u16 {
        self.x
    }
    pub fn y(&self) -> u16 {
        self.y
    }
    pub fn w(&self) -> u16 {
        self.w
    }
    pub fn h(&self) -> u16 {
        self.h
    }
    // Exact: a rect is inside a screen.
    pub fn right(&self) -> u16 {
        self.x.saturating_add(self.w)
    }
    pub fn bottom(&self) -> u16 {
        self.y.saturating_add(self.h)
    }
    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }
    /// How many cells it has; exact in a u32.
    pub fn area(&self) -> u32 {
        u32::from(self.w).saturating_mul(u32::from(self.h))
    }
    /// Its rows on the screen, top to bottom.
    pub fn rows(&self) -> Range<u16> {
        self.y..self.bottom()
    }
    /// Its columns on the screen, left to right.
    pub fn cols(&self) -> Range<u16> {
        self.x..self.right()
    }
    /// Each of its rows, as a rect one row high.
    pub fn lines(self) -> impl Iterator<Item = Rect> {
        self.rows().map(move |y| Rect { y, h: 1, ..self })
    }
    /// Where (y, x) of the rect is on the screen; none outside it.
    pub fn at(&self, y: u16, x: u16) -> Option<(u16, u16)> {
        (y < self.h && x < self.w).then(|| (self.y.saturating_add(y), self.x.saturating_add(x)))
    }
    /// Whether (y, x) on the screen is inside the rect.
    pub fn contains(&self, y: u16, x: u16) -> bool {
        self.rows().contains(&y) && self.cols().contains(&x)
    }
    /// Whether (y, x) on the screen is inside the rect or beside it, edges
    /// and corners: within a cell of it.
    pub fn near(&self, y: u16, x: u16) -> bool {
        y.saturating_add(1) >= self.y
            && y <= self.bottom()
            && x.saturating_add(1) >= self.x
            && x <= self.right()
    }
    /// (y, x) on the screen as a position of the rect, moved to its nearest
    /// cell if outside; (0, 0) in an empty one.
    pub fn clamp(&self, y: u16, x: u16) -> (u16, u16) {
        let inside =
            |at: u16, start: u16, len: u16| at.saturating_sub(start).min(len.saturating_sub(1));
        (inside(y, self.y, self.h), inside(x, self.x, self.w))
    }
    /// Its length along `axis`.
    pub fn len(&self, axis: Axis) -> u16 {
        match axis {
            Axis::Horizontal => self.w,
            Axis::Vertical => self.h,
        }
    }
    /// Its first cell along `axis`.
    fn start(&self, axis: Axis) -> u16 {
        match axis {
            Axis::Horizontal => self.x,
            Axis::Vertical => self.y,
        }
    }
    /// The rect `len` cells long from `start` along `axis`, and as it is
    /// across.
    fn along(mut self, axis: Axis, start: u16, len: u16) -> Rect {
        match axis {
            Axis::Horizontal => (self.x, self.w) = (start, len),
            Axis::Vertical => (self.y, self.h) = (start, len),
        }
        self
    }
    /// Its first `len` cells along `axis`, or all of it if it has fewer,
    /// and the rest.
    pub fn split(self, axis: Axis, len: u16) -> (Rect, Rect) {
        let (start, all) = (self.start(axis), self.len(axis));
        let len = len.min(all);
        let rest = self.along(axis, start.saturating_add(len), all.saturating_sub(len));
        (self.along(axis, start, len), rest)
    }
    /// Its bottom-right corner, at most `h` by `w`.
    pub fn corner(self, h: u16, w: u16) -> Rect {
        let (h, w) = (h.min(self.h), w.min(self.w));
        Rect {
            x: self.right().saturating_sub(w),
            y: self.bottom().saturating_sub(h),
            w,
            h,
        }
    }
    /// Twice the centre, so that it is whole.
    fn centre2(&self) -> (u32, u32) {
        (
            u32::from(self.x).saturating_add(u32::from(self.right())),
            u32::from(self.y).saturating_add(u32::from(self.bottom())),
        )
    }
}

/// A separator line, a rect one cell wide or high: vertical between
/// side-by-side children, horizontal between stacked ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Separator {
    /// The axis of the split it divides: a `Horizontal` split's line is
    /// vertical.
    pub axis: Axis,
    pub rect: Rect,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Placement {
    pub panes: Vec<(PaneId, Rect)>,
    pub separators: Vec<Separator>,
}

impl Placement {
    /// Nothing placed, the buffers kept.
    pub fn clear(&mut self) {
        self.panes.clear();
        self.separators.clear();
    }
    pub fn rect(&self, pane: PaneId) -> Option<Rect> {
        self.panes.iter().find(|(p, _)| *p == pane).map(|(_, r)| *r)
    }
}

/// Whether `root` placed in `area` shows every one of its panes.
pub fn shows_all(root: &Tree, area: Rect) -> bool {
    let placed = place(root, area);
    root.panes().iter().all(|p| placed.rect(*p).is_some())
}

/// Which side of its target a split puts the new pane: before it, left or
/// above, or after it, right or below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Before,
    After,
}

impl Side {
    /// The side `direction` points to, along its axis.
    pub fn of(direction: Direction) -> Side {
        match direction {
            Direction::Left | Direction::Up => Side::Before,
            Direction::Right | Direction::Down => Side::After,
        }
    }
    fn other(self) -> Side {
        match self {
            Side::Before => Side::After,
            Side::After => Side::Before,
        }
    }
}

impl Tree {
    /// Every pane, in tree order: for when a list is needed, as to step
    /// through it by index.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.for_each_pane(&mut |pane| out.push(pane));
        out
    }
    /// Calls `f` with every pane, in tree order.
    pub fn for_each_pane(&self, f: &mut impl FnMut(PaneId)) {
        match self {
            Tree::Pane(p) => f(*p),
            Tree::Split(split) => {
                split.first.for_each_pane(f);
                split.second.for_each_pane(f);
            }
        }
    }
    /// The first pane in tree order.
    pub fn first_pane(&self) -> PaneId {
        match self {
            Tree::Pane(p) => *p,
            Tree::Split(split) => split.first.first_pane(),
        }
    }
    pub fn contains(&self, pane: PaneId) -> bool {
        match self {
            Tree::Pane(p) => *p == pane,
            Tree::Split(split) => split.first.contains(pane) || split.second.contains(pane),
        }
    }

    /// The smallest length it can have along `axis`.
    fn min_len(&self, axis: Axis) -> u16 {
        let Tree::Split(split) = self else {
            return MIN;
        };
        let (a, b) = (split.first.min_len(axis), split.second.min_len(axis));
        match split.axis == axis {
            true => a.saturating_add(1).saturating_add(b),
            false => a.max(b),
        }
    }

    /// Puts `new` beside `target`, on `side` of it, along `axis`: the
    /// target becomes a split of the two, dividing its room as `shares`
    /// says, target to new.
    pub fn split(
        &mut self,
        (target, new): (PaneId, PaneId),
        (axis, side): (Axis, Side),
        shares: Shares,
    ) -> Result<(), NoPane> {
        match self {
            Tree::Pane(p) if *p == target => {
                let (target, new) = (Tree::Pane(target), Tree::Pane(new));
                let (first, second, shares) = match side {
                    Side::After => (target, new, shares),
                    Side::Before => (new, target, shares.swapped()),
                };
                let split = Split {
                    axis,
                    shares,
                    first,
                    second,
                };
                *self = Tree::Split(Box::new(split));
                Ok(())
            }
            Tree::Pane(_) => Err(NoPane),
            Tree::Split(split) => (split.first.split((target, new), (axis, side), shares))
                .or_else(|_| split.second.split((target, new), (axis, side), shares)),
        }
    }

    /// The tree without `pane`: its sibling takes their split's place, and
    /// all its room.
    pub fn remove(&self, pane: PaneId) -> Removed {
        let split = match self {
            Tree::Pane(p) if *p == pane => return Removed::Emptied,
            Tree::Pane(_) => return Removed::Absent,
            Tree::Split(split) => split,
        };
        let with = |first, second| {
            let (axis, shares) = (split.axis, split.shares);
            Removed::Left(Tree::Split(Box::new(Split {
                axis,
                shares,
                first,
                second,
            })))
        };
        match (split.first.remove(pane), split.second.remove(pane)) {
            (Removed::Emptied, _) => Removed::Left(split.second.clone()),
            (_, Removed::Emptied) => Removed::Left(split.first.clone()),
            (Removed::Left(first), _) => with(first, split.second.clone()),
            (_, Removed::Left(second)) => with(split.first.clone(), second),
            (Removed::Absent, Removed::Absent) => Removed::Absent,
        }
    }

    /// Puts `pane` after the whole tree, side by side with it, taking as
    /// much as all the rest.
    pub fn append(&mut self, pane: PaneId) {
        let whole = std::mem::replace(self, Tree::Pane(pane));
        *self = Tree::Split(Box::new(Split {
            axis: Axis::Horizontal,
            shares: Shares::HALVED,
            first: whole,
            second: Tree::Pane(pane),
        }));
    }

    /// Swaps two panes' places in one tree, or, given two trees in turn,
    /// each takes the other's place.
    pub fn swap(&mut self, a: PaneId, b: PaneId) {
        match self {
            Tree::Pane(p) if *p == a => *p = b,
            Tree::Pane(p) if *p == b => *p = a,
            Tree::Pane(_) => {}
            Tree::Split(split) => {
                split.first.swap(a, b);
                split.second.swap(a, b);
            }
        }
    }
}

impl Split {
    pub fn axis(&self) -> Axis {
        self.axis
    }
    pub fn shares(&self) -> Shares {
        self.shares
    }
    /// Its children, first (left or above) and second.
    pub fn children(&self) -> (&Tree, &Tree) {
        (&self.first, &self.second)
    }

    /// Its children's lengths along its axis in `len` cells, a separator
    /// between: as its shares divide what there is, the first rounded
    /// half up, each at least its least. With too few for both, the first
    /// has all if it fits; 0 is a child not shown.
    fn lengths(&self, len: u16) -> (u16, u16) {
        let (min1, min2) = (
            self.first.min_len(self.axis),
            self.second.min_len(self.axis),
        );
        let room = len.checked_sub(1);
        let Some(room) = room.filter(|room| *room >= min1.saturating_add(min2)) else {
            return if len >= min1 { (len, 0) } else { (0, 0) };
        };
        let (a, b) = (
            u64::from(self.shares.0.get()),
            u64::from(self.shares.1.get()),
        );
        // Exact in a u64: a u16 by a u32, twice, and two u32s.
        let whole = a.saturating_add(b);
        let first = (u64::from(room).saturating_mul(a).saturating_mul(2))
            .saturating_add(whole)
            .checked_div(whole.saturating_mul(2))
            .unwrap_or_default();
        let most = room.saturating_sub(min2);
        let first = u16::try_from(first).unwrap_or(most).clamp(min1, most);
        (first, room.saturating_sub(first))
    }

    /// Where its first child, the separator and its second are in `area`:
    /// empty for what is not shown.
    fn parts(&self, area: Rect) -> [Rect; 3] {
        let (a, b) = self.lengths(area.len(self.axis));
        let (first, rest) = area.split(self.axis, a);
        let (line, rest) = rest.split(self.axis, u16::from(b > 0));
        let (second, _) = rest.split(self.axis, b);
        [first, line, second]
    }
}

/// The rectangles of every pane that fits in `area`, and the separators.
pub fn place(root: &Tree, area: Rect) -> Placement {
    let mut out = Placement::default();
    place_into(root, area, &mut out);
    out
}

/// Places `root` in `area` into `out`, whatever it held, reusing its
/// buffers: a placement made into again allocates nothing.
pub fn place_into(root: &Tree, area: Rect, out: &mut Placement) {
    out.clear();
    place_tree(root, area, out);
}

fn place_tree(tree: &Tree, area: Rect, out: &mut Placement) {
    if area.is_empty() {
        return;
    }
    match tree {
        Tree::Pane(p) => out.panes.push((*p, area)),
        Tree::Split(split) => {
            let [first, line, second] = split.parts(area);
            place_tree(&split.first, first, out);
            if !line.is_empty() {
                let axis = split.axis;
                out.separators.push(Separator { axis, rect: line });
                place_tree(&split.second, second, out);
            }
        }
    }
}

/// The pane in `direction` from `from`, among the placed panes: rectangles
/// beyond `from`'s edge, ranked by the distance between centres across the
/// direction, then by the distance along it, then by ID.
pub fn neighbor(placement: &Placement, from: PaneId, direction: Direction) -> Option<PaneId> {
    let source = placement.rect(from)?;
    let (sx, sy) = source.centre2();
    placement
        .panes
        .iter()
        .filter(|(p, _)| *p != from)
        .filter_map(|(p, r)| {
            let beyond = match direction {
                Direction::Left => r.right() <= source.x,
                Direction::Right => r.x >= source.right(),
                Direction::Up => r.bottom() <= source.y,
                Direction::Down => r.y >= source.bottom(),
            };
            if !beyond {
                return None;
            }
            let (cx, cy) = r.centre2();
            let (cross, forward) = match direction {
                Direction::Left | Direction::Right => (sy.abs_diff(cy), sx.abs_diff(cx)),
                Direction::Up | Direction::Down => (sx.abs_diff(cx), sy.abs_diff(cy)),
            };
            Some(((cross, forward, *p), *p))
        })
        .min_by_key(|(key, _)| *key)
        .map(|(_, p)| p)
}

/// Moves `pane` beside `target`, on the side `direction` points to, as
/// `Tree::split` puts a new one, in halves: the tree it makes, if it holds
/// both.
pub fn move_beside(
    root: &Tree,
    pane: PaneId,
    target: PaneId,
    direction: Direction,
) -> Option<Tree> {
    if pane == target || !root.contains(target) {
        return None;
    }
    let Removed::Left(mut moved) = root.remove(pane) else {
        return None;
    };
    let way = (Axis::of(direction), Side::of(direction));
    moved.split((target, pane), way, Shares::HALVED).ok()?;
    Some(moved)
}

/// Moves a border of `pane`, as placed in `area`, by `amount` cells toward
/// `direction`: its border on that side, the pane growing, then while the
/// panes beyond are at their least, the borders beyond it, nearest first.
/// With no border on that side, its border on the other moves, the pane
/// shrinking, to its least. How far it moved; `None` if the pane has no
/// border along the axis, is not in the tree, or the area hides a pane.
pub fn resize(
    root: &mut Tree,
    area: Rect,
    pane: PaneId,
    direction: Direction,
    amount: u16,
) -> Option<u16> {
    if !shows_all(root, area) {
        return None;
    }
    let (axis, toward) = (Axis::of(direction), Side::of(direction));
    let mut move_side = |side| {
        let mut left = amount;
        let shift = Shift {
            pane,
            axis,
            side,
            toward,
        };
        root.shift(area, shift, &mut left)
    };
    move_side(toward).or_else(|| move_side(toward.other()))
}

/// `root` with `pane` resized to `len` cells along `axis`, as placed in
/// `area`, by moving its border after it and then the one before, as
/// `resize` does; `None` if they do not get it there.
pub fn resize_to(root: &Tree, area: Rect, pane: PaneId, axis: Axis, len: u16) -> Option<Tree> {
    let size = |tree: &Tree| place(tree, area).rect(pane).map(|r| r.len(axis));
    if !shows_all(root, area) {
        return None;
    }
    let mut tree = root.clone();
    for side in [Side::After, Side::Before] {
        let now = size(&tree)?;
        // Out from the pane to grow it, in to shrink it.
        let toward = if now < len { side } else { side.other() };
        let mut left = now.abs_diff(len);
        if left > 0 {
            let shift = Shift {
                pane,
                axis,
                side,
                toward,
            };
            tree.shift(area, shift, &mut left);
        }
    }
    (size(&tree)? == len).then_some(tree)
}

/// A move of the borders on `side` of `pane` along `axis`, toward
/// `toward`: out from the pane if that is `side`, else in.
#[derive(Clone, Copy)]
struct Shift {
    pane: PaneId,
    axis: Axis,
    side: Side,
    toward: Side,
}

impl Tree {
    /// Carries out `shift` in this tree, placed in `area`, by up to `left`
    /// cells, which it counts down. Out from the pane, the border nearest
    /// it moves as far as it can, then the next, until none is left; in,
    /// the nearest alone. How far they moved; `None` if the pane has no
    /// border on that side here.
    fn shift(&mut self, area: Rect, shift: Shift, left: &mut u16) -> Option<u16> {
        let Tree::Split(split) = self else {
            return None;
        };
        let [first, _, second] = split.parts(area);
        let in_first = split.first.contains(shift.pane);
        let deeper = match (in_first, split.second.contains(shift.pane)) {
            (true, _) => split.first.shift(first, shift, left),
            (false, true) => split.second.shift(second, shift, left),
            (false, false) => return None,
        };
        // This split's border is on that side of the child holding it.
        let here = split.axis == shift.axis && in_first == (shift.side == Side::After);
        let outward = shift.side == shift.toward;
        if !here || deeper.is_some() && (!outward || *left == 0) {
            return deeper;
        }
        let moved = split.move_border(area, shift.pane, shift.toward, *left);
        *left = left.saturating_sub(moved);
        Some(deeper.unwrap_or(0).saturating_add(moved))
    }

    /// Makes it `len` cells along `axis`, from what it has in `area`, the
    /// change at its `edge`: in each split along the axis, the child
    /// holding `pane`, else the one at `edge`, takes it; one that would go
    /// below its least gives no more, and the other gives the rest. A
    /// split it changes takes its children's new lengths as its shares.
    fn stretch(&mut self, area: Rect, (axis, len): (Axis, u16), (edge, pane): (Side, PaneId)) {
        let Tree::Split(split) = self else {
            return;
        };
        if len == area.len(axis) {
            return;
        }
        let [r1, _, r2] = split.parts(area);
        if split.axis != axis {
            split.first.stretch(r1, (axis, len), (edge, pane));
            split.second.stretch(r2, (axis, len), (edge, pane));
            return;
        }
        let (l1, l2) = (r1.len(axis), r2.len(axis));
        let room = len.saturating_sub(1);
        let (min1, min2) = (split.first.min_len(axis), split.second.min_len(axis));
        let first_takes =
            split.first.contains(pane) || !split.second.contains(pane) && edge == Side::Before;
        let (n1, n2) = if first_takes {
            let n2 = l2.min(room.saturating_sub(min1));
            (room.saturating_sub(n2), n2)
        } else {
            let n1 = l1.min(room.saturating_sub(min2));
            (n1, room.saturating_sub(n1))
        };
        split.first.stretch(r1, (axis, n1), (edge, pane));
        split.second.stretch(r2, (axis, n2), (edge, pane));
        split.shares = Shares::of(n1, n2);
    }
}

impl Split {
    /// Moves its border, placed in `area`, toward `toward` by as much of
    /// `amount` as the child that way can give down to its least (as
    /// `Tree::stretch` takes it), the other taking it: the child holding
    /// `pane` changes there alone. How far it moved.
    fn move_border(&mut self, area: Rect, pane: PaneId, toward: Side, amount: u16) -> u16 {
        let [r1, _, r2] = self.parts(area);
        let axis = self.axis;
        let (l1, l2) = (r1.len(axis), r2.len(axis));
        let (n1, n2) = match toward {
            Side::After => {
                let d = amount.min(l2.saturating_sub(self.second.min_len(axis)));
                (l1.saturating_add(d), l2.saturating_sub(d))
            }
            Side::Before => {
                let d = amount.min(l1.saturating_sub(self.first.min_len(axis)));
                (l1.saturating_sub(d), l2.saturating_add(d))
            }
        };
        if n1 == l1 {
            return 0;
        }
        self.first.stretch(r1, (axis, n1), (Side::After, pane));
        self.second.stretch(r2, (axis, n2), (Side::Before, pane));
        self.shares = Shares::of(n1, n2);
        l1.abs_diff(n1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(n: u32) -> PaneId {
        PaneId::of(n)
    }
    fn area(w: u16, h: u16) -> Rect {
        Rect::screen(h, w)
    }
    /// The tree `ways` makes from pane 0, each splitting a pane for the
    /// next, after it, in halves.
    fn made(ways: &[(u32, Axis)]) -> Tree {
        let mut root = Tree::Pane(p(0));
        for (n, (target, axis)) in (1..).zip(ways) {
            let split = root.split((p(*target), p(n)), (*axis, Side::After), Shares::HALVED);
            assert_eq!(split, Ok(()));
        }
        root
    }
    /// `0 | 1`.
    fn tree() -> Tree {
        made(&[(0, Axis::Horizontal)])
    }
    /// `0 | (1 / (2 | 3))`.
    fn nested() -> Tree {
        made(&[
            (0, Axis::Horizontal),
            (1, Axis::Vertical),
            (2, Axis::Horizontal),
        ])
    }
    fn widths(t: &Tree, a: Rect, panes: &[u32]) -> Vec<Option<u16>> {
        let placed = place(t, a);
        panes
            .iter()
            .map(|n| placed.rect(p(*n)).map(|r| r.w))
            .collect()
    }

    #[test]
    fn splitting_shares_the_space_with_one_separator() {
        let placed = place(&tree(), area(80, 24));
        let rect = |x, w| Rect { x, y: 0, w, h: 24 };
        assert_eq!(
            placed.panes,
            vec![(p(0), rect(0, 40)), (p(1), rect(41, 39))]
        );
        let axis = Axis::Horizontal;
        assert_eq!(
            placed.separators,
            vec![Separator {
                axis,
                rect: rect(40, 1)
            }]
        );
        // A second split of the right pane halves its room.
        let row = made(&[(0, Axis::Horizontal), (1, Axis::Horizontal)]);
        assert_eq!(
            widths(&row, area(80, 24), &[0, 1, 2]),
            [40, 19, 19].map(Some)
        );
        // Shares divide the pane's room, before or after it, at their least.
        let two = NonZeroU32::MIN.saturating_add(1);
        let mut root = Tree::Pane(p(0));
        let way = (Axis::Horizontal, Side::Before);
        assert_eq!(root.split((p(0), p(1)), way, Shares::new(two, two)), Ok(()));
        let mut halved = Tree::Pane(p(0));
        assert_eq!(halved.split((p(0), p(1)), way, Shares::HALVED), Ok(()));
        assert_eq!(root, halved);
        assert_eq!(root.panes(), [p(1), p(0)]);
        assert_eq!(root.split((p(9), p(2)), way, Shares::HALVED), Err(NoPane));
    }

    #[test]
    fn nested_splits_place_every_pane_without_overlap() {
        let root = made(&[
            (0, Axis::Horizontal),
            (1, Axis::Horizontal),
            (2, Axis::Vertical),
            (1, Axis::Vertical),
        ]);
        assert_eq!(root.panes(), [0, 1, 4, 2, 3].map(p));
        let placed = place(&root, area(81, 25));
        assert_eq!(placed.panes.len(), 5);
        let mut covered = vec![0u8; 81 * 25];
        let separators = placed.separators.iter().map(|s| &s.rect);
        for r in placed.panes.iter().map(|(_, r)| r).chain(separators) {
            for (y, x) in r.rows().flat_map(|y| r.cols().map(move |x| (y, x))) {
                if let Some(c) = covered.get_mut(usize::from(y) * 81 + usize::from(x)) {
                    *c += 1;
                }
            }
        }
        assert!(
            covered.iter().all(|c| *c == 1),
            "every cell is a pane or a separator, once"
        );
    }

    /// A placement made into again, whatever it held, is a fresh one.
    #[test]
    fn placing_into_a_used_placement_is_placing_afresh() {
        let root = made(&[(0, Axis::Horizontal), (1, Axis::Vertical)]);
        let mut used = place(&root, area(120, 40));
        for (w, h) in [(80, 24), (3, 3), (0, 9), (200, 60)] {
            place_into(&root, area(w, h), &mut used);
            assert_eq!(used, place(&root, area(w, h)), "{w}x{h}");
        }
        place_into(&Tree::Pane(p(7)), area(10, 10), &mut used);
        assert_eq!(used.panes, vec![(p(7), area(10, 10))]);
        assert!(used.separators.is_empty());
    }

    #[test]
    fn a_small_area_keeps_minimums_and_hides_what_does_not_fit() {
        let ways: Vec<_> = (0..9).map(|n| (n, Axis::Horizontal)).collect();
        let root = made(&ways);
        let placed = place(&root, area(10, 3));
        assert!(placed.panes.iter().all(|(_, r)| r.w >= MIN && r.h >= MIN));
        let used: usize = (placed.panes.iter())
            .map(|(_, r)| usize::from(r.w))
            .sum::<usize>()
            + placed.separators.len();
        assert_eq!(used, 10);
        assert!(placed.panes.len() < 9);
        assert!(!shows_all(&root, area(10, 3)));
        assert!(shows_all(&root, area(29, 3)));
        assert!(place(&root, area(0, 5)).panes.is_empty());
    }

    /// A pane's sibling takes their split's place and all its room, every
    /// other share as it was; the last pane leaves nothing.
    #[test]
    fn a_close_gives_its_room_to_its_sibling() {
        let quarter = Shares::new(NonZeroU32::MIN, NonZeroU32::MIN.saturating_add(2));
        let way = |axis| (axis, Side::After);
        let mut root = Tree::Pane(p(0));
        let ways = [
            ((0, 1), Axis::Horizontal, quarter),
            ((1, 2), Axis::Vertical, Shares::HALVED),
            ((2, 3), Axis::Horizontal, Shares::HALVED),
        ];
        for ((target, new), axis, shares) in ways {
            assert_eq!(root.split((p(target), p(new)), way(axis), shares), Ok(()));
        }
        // `0 | (1 / (2 | 3))`, 0 a quarter: without 1, `0 | (2 | 3)`.
        let mut left = Tree::Pane(p(0));
        assert_eq!(
            left.split((p(0), p(2)), way(Axis::Horizontal), quarter),
            Ok(())
        );
        let halved = Shares::HALVED;
        assert_eq!(
            left.split((p(2), p(3)), way(Axis::Horizontal), halved),
            Ok(())
        );
        assert_eq!(root.remove(p(1)), Removed::Left(left.clone()));
        assert_eq!(
            widths(&left, area(81, 9), &[0, 2, 3]),
            [20, 30, 29].map(Some)
        );
        assert_eq!(Tree::Pane(p(0)).remove(p(0)), Removed::Emptied);
        assert_eq!(left.remove(p(9)), Removed::Absent);
    }

    /// A pane moves only within a tree holding both it and its target;
    /// otherwise nothing is made.
    #[test]
    fn moving_beside_needs_both_panes_in_the_tree() {
        let root = tree();
        let moved = move_beside(&root, p(0), p(1), Direction::Down);
        assert_eq!(moved.as_ref().map(Tree::panes), Some(vec![p(1), p(0)]));
        for (pane, target) in [(0, 9), (9, 0), (0, 0)] {
            assert_eq!(
                move_beside(&root, p(pane), p(target), Direction::Down),
                None
            );
        }
    }

    #[test]
    fn directional_neighbours_prefer_alignment_then_distance_then_id() {
        let root = made(&[(0, Axis::Horizontal), (1, Axis::Vertical)]);
        let placed = place(&root, area(80, 24));
        assert_eq!(neighbor(&placed, p(0), Direction::Right), Some(p(1)));
        assert_eq!(neighbor(&placed, p(2), Direction::Left), Some(p(0)));
        assert_eq!(neighbor(&placed, p(1), Direction::Down), Some(p(2)));
        assert_eq!(neighbor(&placed, p(2), Direction::Up), Some(p(1)));
        assert_eq!(neighbor(&placed, p(0), Direction::Left), None);
        assert_eq!(neighbor(&placed, p(1), Direction::Right), None);
    }

    #[test]
    fn resizing_moves_the_nearest_border_and_respects_minimums() {
        let mut root = tree();
        let a = area(81, 24);
        assert_eq!(resize(&mut root, a, p(0), Direction::Right, 10), Some(10));
        assert_eq!(widths(&root, a, &[0, 1]), [50, 30].map(Some));
        // The shares are the cells each then has.
        let share = |n| NonZeroU32::new(n).unwrap_or(NonZeroU32::MIN);
        let shares = Shares::new(share(5), share(3));
        assert!(matches!(&root, Tree::Split(split) if split.shares() == shares));
        // Left from the leftmost pane shrinks it, moving its right border.
        assert_eq!(resize(&mut root, a, p(0), Direction::Left, 5), Some(5));
        assert_eq!(widths(&root, a, &[0]), [Some(45)]);
        // Never below the minimum.
        assert_eq!(resize(&mut root, a, p(1), Direction::Left, 200), Some(43));
        assert_eq!(widths(&root, a, &[0]), [Some(MIN)]);
        // No split along the vertical axis: nothing to move.
        assert_eq!(resize(&mut root, a, p(0), Direction::Up, 1), None);
        // A pane not in the tree moves nothing.
        let held = root.clone();
        assert_eq!(resize(&mut root, a, p(9), Direction::Left, 1), None);
        assert_eq!(root, held);
    }

    /// The border that moves is the nearest on the side asked for, however
    /// deep the pane is, and only the panes touching it change: in
    /// `0 | (1 / (2 | 3))`, left from 2 moves 0's border, 1 and 2 growing
    /// and 3 as it was.
    #[test]
    fn resizing_in_nested_splits_moves_the_border_on_that_side() {
        let mut root = nested();
        let a = area(81, 24);
        let all = [0, 1, 2, 3];
        assert_eq!(widths(&root, a, &all), [40, 40, 20, 19].map(Some));
        assert_eq!(resize(&mut root, a, p(2), Direction::Left, 5), Some(5));
        assert_eq!(widths(&root, a, &all), [35, 45, 25, 19].map(Some));
        // Right from 3, at the right edge: no border there, so 3 shrinks
        // from its left.
        assert_eq!(resize(&mut root, a, p(3), Direction::Right, 2), Some(2));
        assert_eq!(widths(&root, a, &all), [35, 45, 27, 17].map(Some));
    }

    /// A border stopped at the least of the panes beyond passes the rest
    /// on to the next border out, the pane alone growing; with none left,
    /// nothing moves.
    #[test]
    fn a_border_at_its_least_passes_the_resize_on() {
        let a = area(100, 24);
        let row = made(&[(0, Axis::Horizontal), (1, Axis::Horizontal)]);
        let wide = resize_to(&row, a, p(2), Axis::Horizontal, 80);
        assert_eq!(
            wide.map(|t| widths(&t, a, &[0, 1, 2])),
            Some(vec![Some(16), Some(MIN), Some(80)])
        );
        // `0 | (1 / (2 | 3))`, 50 and 24 each below: 2 gives 22, then 0
        // gives 8, which 1 takes too.
        let mut nested = nested();
        assert_eq!(
            widths(&nested, a, &[0, 1, 2, 3]),
            [50, 49, 24, 24].map(Some)
        );
        assert_eq!(resize(&mut nested, a, p(3), Direction::Left, 30), Some(30));
        assert_eq!(widths(&nested, a, &[0, 1, 2, 3]), [42, 57, 2, 54].map(Some));
        let mut pair = tree();
        assert_eq!(resize(&mut pair, a, p(1), Direction::Left, 100), Some(48));
        let held = pair.clone();
        assert_eq!(resize(&mut pair, a, p(1), Direction::Left, 1), Some(0));
        assert_eq!(pair, held);
    }

    /// A pane between two shrinks to a length, and grows again, its
    /// neighbours taking and giving.
    #[test]
    fn a_middle_pane_resizes_to_a_length() {
        let row = made(&[(0, Axis::Horizontal), (1, Axis::Horizontal)]);
        let a = area(90, 20);
        assert_eq!(widths(&row, a, &[0, 1, 2]), [45, 22, 21].map(Some));
        let smaller = resize_to(&row, a, p(1), Axis::Horizontal, 12);
        let width = |t: &Tree| widths(t, a, &[0, 1, 2]);
        assert_eq!(
            smaller.as_ref().map(width),
            Some([45, 12, 31].map(Some).to_vec())
        );
        let wider = smaller.and_then(|t| resize_to(&t, a, p(1), Axis::Horizontal, 40));
        assert_eq!(
            wider.as_ref().map(width),
            Some([45, 40, 3].map(Some).to_vec())
        );
        assert_eq!(resize_to(&row, a, p(1), Axis::Horizontal, 88), None);
    }
}
