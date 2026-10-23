//! A tab's layout: a tree of splits whose leaves are panes, and the
//! rectangles it gives each pane at a given size.
use crate::id::PaneId;
use crate::keys::Direction;
use crate::wire::wire;
use std::num::{NonZeroU32, NonZeroU64};
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
    /// The other axis: that of a split's child splits.
    pub fn cross(self) -> Axis {
        match self {
            Axis::Horizontal => Axis::Vertical,
            Axis::Vertical => Axis::Horizontal,
        }
    }
}

/// A tab's layout: one pane, or a split along an axis. Only a whole tree
/// has an axis, `A`: a subtree inside a split, a `Tree<()>`, is along the
/// other axis, so no split nests in one along its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tree<A = Axis> {
    Pane(PaneId),
    Split(A, Split),
}

/// Two or more children side by side, each with its share: only this module
/// makes or changes one, never leaving it fewer than two.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Split(Vec<(NonZeroU32, Tree<()>)>);

/// The weight a new child starts with.
const WEIGHT: NonZeroU32 = match NonZeroU32::new(1000) {
    Some(weight) => weight,
    None => NonZeroU32::MIN,
};

/// `n` as a weight, which is never less than 1.
fn weight(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).unwrap_or(NonZeroU32::MIN)
}

/// The smallest pane, in each dimension.
pub const MIN: u16 = 2;

/// The separators between `children` side by side. Past what a u16 screen
/// holds the count saturates, as the lengths added to it do.
fn separators(children: usize) -> u16 {
    u16::try_from(children.saturating_sub(1)).unwrap_or(u16::MAX)
}

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
    /// Whether it and `other` have a cell in common.
    pub fn meets(&self, other: Rect) -> bool {
        !self.is_empty()
            && !other.is_empty()
            && self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
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
    /// `w` by `h` at (`y`, `x`), cut short where it would pass the last
    /// cell a u16 counts.
    pub fn new(x: u16, y: u16, w: u16, h: u16) -> Rect {
        let w = w.min(u16::MAX.saturating_sub(x));
        let h = h.min(u16::MAX.saturating_sub(y));
        Rect { x, y, w, h }
    }
    /// It without its outermost cells: what a frame drawn on them holds.
    pub fn inner(self) -> Rect {
        Rect {
            x: self.x.saturating_add(1),
            y: self.y.saturating_add(1),
            w: self.w.saturating_sub(2),
            h: self.h.saturating_sub(2),
        }
    }
    /// It moved and cut to fit in `area`, of which (0, 0) is the corner: no
    /// larger than it, and inside.
    pub fn fitted(self, area: Rect) -> Rect {
        let (w, h) = (self.w.min(area.w), self.h.min(area.h));
        Rect {
            x: area.x.saturating_add(self.x.min(area.w.saturating_sub(w))),
            y: area.y.saturating_add(self.y.min(area.h.saturating_sub(h))),
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
    /// Each pane's rect: the tiled ones, then the floating ones, bottom to
    /// top, each inside its frame.
    pub panes: Vec<(PaneId, Rect)>,
    pub separators: Vec<Separator>,
    /// The floating panes' frames, in their order: the last `frames.len()`
    /// of `panes` are theirs.
    pub frames: Vec<Rect>,
    /// Room for each split's sizes while it is placed; empty between
    /// placements.
    scratch: Vec<u16>,
}

impl Placement {
    /// Nothing placed, the buffers kept.
    pub fn clear(&mut self) {
        self.panes.clear();
        self.separators.clear();
        self.frames.clear();
    }
    pub fn rect(&self, pane: PaneId) -> Option<Rect> {
        self.panes.iter().find(|(p, _)| *p == pane).map(|(_, r)| *r)
    }
    /// The tiled panes.
    pub fn tiled(&self) -> &[(PaneId, Rect)] {
        let tiled = self.panes.len().saturating_sub(self.frames.len());
        self.panes.get(..tiled).unwrap_or_default()
    }
    /// The floating panes, bottom to top, each with its frame.
    pub fn floating(&self) -> impl Iterator<Item = (PaneId, Rect, Rect)> {
        let tiled = self.panes.len().saturating_sub(self.frames.len());
        let floating = self.panes.iter().skip(tiled).zip(&self.frames);
        floating.map(|((p, r), f)| (*p, *r, *f))
    }
}

/// The least a floating pane's frame is each way: a pane at its smallest,
/// and the border round it.
pub const FRAME_MIN: u16 = MIN + 2;

/// A pane floating over a tab's tiled panes: its frame, border included, on
/// a pane area whose corner is (0, 0). On an area too small for it, it is
/// moved and cut to fit (`Rect::fitted`); on one too small for a pane, it
/// is not shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Float {
    pub pane: PaneId,
    pub frame: Rect,
}

/// A tab's panes: those its tree tiles, and those floating over them, the
/// last on top.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    pub tiled: Option<Tree>,
    pub floating: Vec<Float>,
}

impl Layout {
    pub fn tiled(tree: Option<Tree>) -> Layout {
        Layout {
            tiled: tree,
            floating: Vec::new(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.tiled.is_none() && self.floating.is_empty()
    }
    pub fn contains(&self, pane: PaneId) -> bool {
        self.tiles(pane) || self.floats(pane)
    }
    /// Whether `pane` is one of its tiled panes.
    pub fn tiles(&self, pane: PaneId) -> bool {
        self.tiled.as_ref().is_some_and(|t| t.contains(pane))
    }
    /// Whether `pane` is one of its floating panes.
    pub fn floats(&self, pane: PaneId) -> bool {
        self.floating.iter().any(|f| f.pane == pane)
    }
    pub fn float(&self, pane: PaneId) -> Option<&Float> {
        self.floating.iter().find(|f| f.pane == pane)
    }
    /// The floating pane on top.
    pub fn top(&self) -> Option<PaneId> {
        self.floating.last().map(|f| f.pane)
    }
    /// The first tiled pane, else the top floating one.
    pub fn first_pane(&self) -> Option<PaneId> {
        (self.tiled.as_ref().and_then(Tree::first_pane)).or_else(|| self.top())
    }
    /// Every pane: the tiled ones in tree order, then the floating ones,
    /// bottom to top.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.for_each_pane(&mut |pane| out.push(pane));
        out
    }
    pub fn for_each_pane(&self, f: &mut impl FnMut(PaneId)) {
        if let Some(tree) = &self.tiled {
            tree.for_each_pane(f);
        }
        self.floating.iter().for_each(|float| f(float.pane));
    }
    /// The panes of `pane`'s layer, tiled or floating, in order.
    pub fn layer(&self, pane: PaneId) -> Vec<PaneId> {
        match self.floats(pane) {
            true => self.floating.iter().map(|f| f.pane).collect(),
            false => self.tiled.as_ref().map_or_else(Vec::new, Tree::panes),
        }
    }
    /// Removes a pane, tiled or floating.
    pub fn remove(&mut self, pane: PaneId) -> bool {
        let floated = self.floating.len();
        self.floating.retain(|f| f.pane != pane);
        remove(&mut self.tiled, pane) || self.floating.len() != floated
    }
    /// Swaps two panes' places, whichever layers they are in.
    pub fn swap(&mut self, a: PaneId, b: PaneId) {
        if let Some(tree) = &mut self.tiled {
            swap(tree, a, b);
        }
        for float in &mut self.floating {
            float.pane = match float.pane {
                p if p == a => b,
                p if p == b => a,
                p => p,
            };
        }
    }
    /// Puts floating pane `pane` on top.
    pub fn raise(&mut self, pane: PaneId) {
        if let Some(float) = self.float(pane).copied() {
            self.floating.retain(|f| f.pane != pane);
            self.floating.push(float);
        }
    }
    /// A pane floating in `frame`: taken out of the tree, on top, if it
    /// was tiled; moved there if it floated.
    pub fn float_at(&mut self, pane: PaneId, frame: Rect) {
        match self.floating.iter_mut().find(|f| f.pane == pane) {
            Some(float) => float.frame = frame,
            None => {
                remove(&mut self.tiled, pane);
                self.floating.push(Float { pane, frame });
            }
        }
    }
    /// The frame a new floating pane takes on `area`: three quarters of it
    /// each way, centred, and a little down and right of each one there is,
    /// so that none hides another whole.
    pub fn new_frame(&self, area: Rect) -> Rect {
        let three_quarters = |len: u16| (len / 4).saturating_mul(3).max(FRAME_MIN);
        let (w, h) = (three_quarters(area.w), three_quarters(area.h));
        let n = u16::try_from(self.floating.len()).unwrap_or(u16::MAX);
        let x = (area.w.saturating_sub(w) / 2).saturating_add(n.saturating_mul(2));
        let y = (area.h.saturating_sub(h) / 2).saturating_add(n);
        Rect::new(x, y, w, h).fitted(area)
    }
    /// Places its tiled panes in `area`, and with `floating` its floating
    /// ones over them, into `out`, whatever it held; `focus`, if one of
    /// them, on top, where whoever focuses it sees it.
    pub fn place_into(
        &self,
        area: Rect,
        floating: bool,
        focus: Option<PaneId>,
        out: &mut Placement,
    ) {
        match &self.tiled {
            Some(tree) => place_into(tree, area, out),
            None => out.clear(),
        }
        let shown = self.floating.iter().filter(|_| floating);
        let focused = |f: &&Float| Some(f.pane) == focus;
        let others = shown.clone().filter(|f| !focused(f));
        for float in others.chain(shown.filter(focused)) {
            let frame = float.frame.fitted(area);
            if frame.w >= FRAME_MIN && frame.h >= FRAME_MIN {
                out.panes.push((float.pane, frame.inner()));
                out.frames.push(frame);
            }
        }
    }
}

impl<A> Tree<A> {
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
            Tree::Split(_, Split(children)) => {
                children.iter().for_each(|(_, c)| c.for_each_pane(f))
            }
        }
    }
    /// The first pane in tree order.
    pub fn first_pane(&self) -> Option<PaneId> {
        match self {
            Tree::Pane(p) => Some(*p),
            Tree::Split(_, Split(children)) => children.first()?.1.first_pane(),
        }
    }
    pub fn contains(&self, pane: PaneId) -> bool {
        match self {
            Tree::Pane(p) => *p == pane,
            Tree::Split(_, Split(children)) => children.iter().any(|(_, c)| c.contains(pane)),
        }
    }
    fn is_pane(&self, pane: PaneId) -> bool {
        matches!(self, Tree::Pane(p) if *p == pane)
    }
}

impl Tree<()> {
    /// The smallest length this subtree can have along `axis`, a split at
    /// its root being along `own`.
    fn min_len(&self, own: Axis, axis: Axis) -> u16 {
        let Tree::Split((), Split(children)) = self else {
            return MIN;
        };
        let mins = children.iter().map(|(_, c)| c.min_len(own.cross(), axis));
        if own == axis {
            mins.fold(separators(children.len()), u16::saturating_add)
        } else {
            mins.max().unwrap_or(MIN)
        }
    }
}

impl Split {
    /// Its children, in order.
    pub fn children(&self) -> impl Iterator<Item = &Tree<()>> {
        self.0.iter().map(|(_, child)| child)
    }
    /// `target` and `new` beside it, on `side`, sharing alike.
    fn pair(target: PaneId, new: PaneId, side: Side) -> Split {
        let (target, new) = ((WEIGHT, Tree::Pane(target)), (WEIGHT, Tree::Pane(new)));
        Split(match side {
            Side::After => vec![target, new],
            Side::Before => vec![new, target],
        })
    }
    /// Puts `with` in place of the child at `at`.
    fn replace(&mut self, at: usize, with: impl IntoIterator<Item = (NonZeroU32, Tree<()>)>) {
        let mut rest = std::mem::take(&mut self.0).into_iter();
        let mut placed: Vec<_> = rest.by_ref().take(at).collect();
        placed.extend(with);
        placed.extend(rest.skip(1));
        self.0 = placed;
    }
    /// Its children, their weights scaled to make up `share` together: the
    /// share they had as one child of a split along their axis.
    fn scaled(self, share: NonZeroU32) -> impl Iterator<Item = (NonZeroU32, Tree<()>)> {
        let total: u64 = self.0.iter().map(|(w, _)| u64::from(w.get())).sum();
        let total = NonZeroU64::new(total).unwrap_or(NonZeroU64::MIN);
        self.0.into_iter().map(move |(w, child)| {
            // A u32 by a u32 is exact in a u64, and a part of `share` fits
            // a u32.
            let scaled = u64::from(share.get()).saturating_mul(u64::from(w.get())) / total;
            let scaled = u32::try_from(scaled).unwrap_or(u32::MAX);
            (weight(scaled), child)
        })
    }
    /// What is left of a split once it has one child: the child.
    fn only(&mut self) -> Option<Tree<()>> {
        if self.0.len() > 1 {
            return None;
        }
        self.0.pop().map(|(_, only)| only)
    }
    /// `split`, in this split, along `own`.
    fn insert(&mut self, own: Axis, target: PaneId, new: PaneId, axis: Axis, side: Side) -> bool {
        let Some(index) = self.0.iter().position(|(_, c)| c.is_pane(target)) else {
            return (self.0.iter_mut()).any(|(_, c)| match c {
                Tree::Split((), split) => split.insert(own.cross(), target, new, axis, side),
                Tree::Pane(_) => false,
            });
        };
        let Some((share, child)) = self.0.get_mut(index) else {
            return false;
        };
        if own != axis {
            // Across this split, the pane becomes a split along `axis`.
            *child = Tree::Split((), Split::pair(target, new, side));
            return true;
        }
        // Along it, the new pane takes half the target's share.
        let half = weight(share.get() / 2);
        let target = (
            weight(share.get().saturating_sub(half.get())),
            Tree::Pane(target),
        );
        let new = (half, Tree::Pane(new));
        match side {
            Side::After => self.replace(index, [target, new]),
            Side::Before => self.replace(index, [new, target]),
        }
        true
    }
    /// `remove`, in this split, along `own`: a child split left with one
    /// child gives way to it, merging into this one if it is a split.
    fn remove(&mut self, own: Axis, pane: PaneId) -> bool {
        if let Some(index) = self.0.iter().position(|(_, c)| c.is_pane(pane)) {
            self.replace(index, []);
            return true;
        }
        for index in 0..self.0.len() {
            let Some((share, Tree::Split((), split))) = self.0.get_mut(index) else {
                continue;
            };
            if !split.remove(own.cross(), pane) {
                continue;
            }
            let share = *share;
            match split.only() {
                Some(Tree::Pane(p)) => self.replace(index, [(share, Tree::Pane(p))]),
                Some(Tree::Split((), inner)) => self.replace(index, inner.scaled(share)),
                None => {}
            }
            return true;
        }
        false
    }
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
}

impl Tree {
    /// Adds `new` beside `target`, on `side` of it, along `axis`. A split
    /// already along `axis` takes it as a sibling sharing the target's
    /// weight; otherwise the target becomes a new split of the two. False if
    /// `target` is not in the tree.
    pub fn split(&mut self, target: PaneId, new: PaneId, axis: Axis, side: Side) -> bool {
        match self {
            Tree::Pane(p) if *p == target => {
                *self = Tree::Split(axis, Split::pair(target, new, side));
                true
            }
            Tree::Pane(_) => false,
            Tree::Split(own, split) => split.insert(*own, target, new, axis, side),
        }
    }
}

/// Moves `pane` beside `target`, on the side `direction` points to, as
/// `Tree::split` puts a new one, in a tree holding both. False, the tree as
/// it was, otherwise.
pub fn move_beside(
    root: &mut Option<Tree>,
    pane: PaneId,
    target: PaneId,
    direction: Direction,
) -> bool {
    let both = root
        .as_ref()
        .is_some_and(|r| r.contains(pane) && r.contains(target));
    if !both || pane == target {
        return false;
    }
    // `target` is left, so the tree is too.
    remove(root, pane);
    root.as_mut()
        .is_some_and(|r| r.split(target, pane, Axis::of(direction), Side::of(direction)))
}

/// Removes a pane. A split left with one child becomes that child, merging
/// into the split around it when that is along its axis. The tree is `None`
/// once its last pane is gone.
pub fn remove(root: &mut Option<Tree>, pane: PaneId) -> bool {
    match root {
        Some(Tree::Pane(p)) if *p == pane => *root = None,
        None | Some(Tree::Pane(_)) => return false,
        Some(Tree::Split(axis, split)) => {
            if !split.remove(*axis, pane) {
                return false;
            }
            match split.only() {
                Some(Tree::Pane(p)) => *root = Some(Tree::Pane(p)),
                Some(Tree::Split((), inner)) => *root = Some(Tree::Split(axis.cross(), inner)),
                None => {}
            }
        }
    }
    true
}

/// Adds `pane` after the whole tree, side by side with it, as one more
/// child of a split along `Horizontal` or with the tree's weight.
pub fn append(root: &mut Option<Tree>, pane: PaneId) {
    let new = (WEIGHT, Tree::Pane(pane));
    let children = match root.take() {
        None => return *root = Some(Tree::Pane(pane)),
        Some(Tree::Pane(p)) => vec![(WEIGHT, Tree::Pane(p)), new],
        Some(Tree::Split(Axis::Horizontal, split)) => {
            split.scaled(WEIGHT).chain(std::iter::once(new)).collect()
        }
        Some(Tree::Split(Axis::Vertical, split)) => vec![(WEIGHT, Tree::Split((), split)), new],
    };
    *root = Some(Tree::Split(Axis::Horizontal, Split(children)));
}

/// Swaps two panes' places in one tree, or, given two trees in turn, each
/// takes the other's place.
pub fn swap<A>(tree: &mut Tree<A>, a: PaneId, b: PaneId) {
    match tree {
        Tree::Pane(p) if *p == a => *p = b,
        Tree::Pane(p) if *p == b => *p = a,
        Tree::Pane(_) => {}
        Tree::Split(_, Split(children)) => children.iter_mut().for_each(|(_, c)| swap(c, a, b)),
    }
}

/// Shares `len` cells among `children` by weight, into `sizes`, which are
/// zeros, giving each at least its minimum in `mins`; `len` is at least
/// their minimums together (`place_split` lays out a split too small for
/// them without it). `mins` is spent: it ends up 0 for the children that
/// shared what was left, rather than being fixed at their minimum.
fn distribute(len: u16, children: &[(NonZeroU32, Tree<()>)], sizes: &mut [u16], mins: &mut [u16]) {
    let weight = |i: usize| u64::from(children.get(i).map_or(1, |(w, _)| w.get()));
    // A child whose share falls short of its minimum is fixed at it, and the
    // rest share again. A minimum is never 0, so a size of 0 is a child not
    // fixed yet.
    loop {
        // The fixed children have their minimums, which together fit.
        let fixed_len: u32 = sizes.iter().map(|s| u32::from(*s)).sum();
        let Some(free_len) = u32::from(len).checked_sub(fixed_len) else {
            return;
        };
        let free_weight: u64 = sizes
            .iter()
            .zip(0..)
            .filter(|(s, _)| **s == 0)
            .map(|(_, i)| weight(i))
            .sum();
        let free_weight = NonZeroU64::new(free_weight).unwrap_or(NonZeroU64::MIN);
        // A u32 by a u32 is exact in a u64.
        let exact = |i: usize| u64::from(free_len).saturating_mul(weight(i));
        let share = |i: usize| exact(i) / free_weight;
        let mut changed = false;
        for ((size, min), i) in sizes.iter_mut().zip(mins.iter()).zip(0..) {
            if *size == 0 && share(i) < u64::from(*min) {
                *size = *min;
                changed = true;
            }
        }
        if changed {
            continue;
        }
        // A share is a fraction of `free_len`, which fits `len`.
        let cells = |i: usize| u16::try_from(share(i)).unwrap_or(len);
        let used = sizes
            .iter()
            .zip(0..)
            .filter(|(s, _)| **s == 0)
            // The shares are parts of `free_len`.
            .fold(0u32, |used, (_, i)| {
                used.saturating_add(u32::from(cells(i)))
            });
        // Fewer cells are left over than there are children sharing: one
        // each to the largest fractions of a cell, the first on a tie.
        let left = free_len.saturating_sub(used);
        for (min, size) in mins.iter_mut().zip(sizes.iter()) {
            if *size == 0 {
                *min = 0;
            }
        }
        let fraction = |i: usize| exact(i) % free_weight;
        for (size, i) in sizes.iter_mut().zip(0..) {
            if mins.get(i) != Some(&0) {
                continue;
            }
            let ahead = mins
                .iter()
                .zip(0..)
                .filter(|(m, j)| {
                    **m == 0
                        && (fraction(*j) > fraction(i) || fraction(*j) == fraction(i) && *j < i)
                })
                .count();
            let extra = u32::try_from(ahead).is_ok_and(|ahead| ahead < left);
            *size = cells(i);
            if extra && let Some(grown) = size.checked_add(1) {
                *size = grown;
            }
        }
        return;
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
    if !area.is_empty() {
        match root {
            Tree::Pane(p) => out.panes.push((*p, area)),
            Tree::Split(axis, split) => place_split(*axis, split, area, out),
        }
    }
}

/// Places a split's children, side by side along `axis`, in `area`.
fn place_split(axis: Axis, Split(children): &Split, area: Rect, out: &mut Placement) {
    let along = area.len(axis);
    let separators = separators(children.len());
    // The children's sizes, then their minimums, above those of the splits
    // this one is inside, until it is placed.
    let base = out.scratch.len();
    out.scratch.extend(children.iter().map(|_| 0));
    out.scratch
        .extend(children.iter().map(|(_, c)| c.min_len(axis.cross(), axis)));
    let (sizes, mins) = out
        .scratch
        .get_mut(base..)
        .and_then(|s| s.split_at_mut_checked(children.len()))
        .unwrap_or_default();
    let needed = mins.iter().fold(separators, |a, m| a.saturating_add(*m));
    let room = along.checked_sub(separators).filter(|_| along >= needed);
    if let Some(room) = room {
        distribute(room, children, sizes, mins);
    } else {
        // Too small for all: children in order while they fit, each after
        // the first needing a separator; the last one shown takes what is
        // left.
        let mut left = along;
        let mut last = None;
        for (i, (size, min)) in sizes.iter_mut().zip(mins.iter()).enumerate() {
            let cost = min.saturating_add(u16::from(i > 0));
            let Some(rest) = left.checked_sub(cost) else {
                break;
            };
            left = rest;
            *size = *min;
            last = Some(i);
        }
        // What is left fits: the sizes add up to at most `along`.
        if let Some(size) = last.and_then(|i| sizes.get_mut(i)) {
            *size = size.saturating_add(left);
        }
    }
    // Each child shown is cut from what is left of `area`, after a
    // separator if one was before it.
    let mut rest = area;
    let mut first = true;
    for (i, (_, child)) in children.iter().enumerate() {
        let size = out.scratch.get(base..).and_then(|s| s.get(i)).copied();
        let Some(size) = size.filter(|s| *s > 0) else {
            continue;
        };
        if !std::mem::replace(&mut first, false) {
            let (line, after) = rest.split(axis, 1);
            out.separators.push(Separator { axis, rect: line });
            rest = after;
        }
        let (rect, after) = rest.split(axis, size);
        match child {
            Tree::Pane(p) => out.panes.push((*p, rect)),
            Tree::Split((), split) => place_split(axis.cross(), split, rect, out),
        }
        rest = after;
    }
    out.scratch.truncate(base);
}

/// The pane in `direction` from `from`, among the placed panes: rectangles
/// beyond `from`'s edge, ranked by the distance between centres across the
/// direction, then by the distance along it, then by ID.
pub fn neighbor(panes: &[(PaneId, Rect)], from: PaneId, direction: Direction) -> Option<PaneId> {
    let source = panes.iter().find(|(p, _)| *p == from)?.1;
    let (sx, sy) = source.centre2();
    panes
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

/// Resizes `pane` by `amount` cells in `direction`, as placed in `area`:
/// the nearest split along the direction's axis that has a sibling on that
/// side moves the border between them, growing the pane's side. Only when
/// no split has one does the nearest split along the axis shrink the pane
/// from its far side instead, moving that border the same way. Weights
/// become the new cell sizes.
pub fn resize(
    root: &mut Tree,
    area: Rect,
    pane: PaneId,
    direction: Direction,
    amount: u16,
) -> bool {
    let Tree::Split(axis, split) = root else {
        return false;
    };
    match split.resize_by(*axis, area, pane, direction, amount, true) {
        Some(moved) => moved,
        None => split.resize_by(*axis, area, pane, direction, amount, false) == Some(true),
    }
}

/// `root` with `pane` resized to `len` cells along `axis`, as placed in
/// `area`, by moving the borders `resize` moves; `None` if no way of moving
/// them gets it there.
pub fn resize_to(root: &Tree, area: Rect, pane: PaneId, axis: Axis, len: u16) -> Option<Tree> {
    let size = |tree: &Tree| place(tree, area).rect(pane).map(|r| r.len(axis));
    let mut tree = root.clone();
    let directions = match axis {
        Axis::Horizontal => [Direction::Left, Direction::Right],
        Axis::Vertical => [Direction::Up, Direction::Down],
    };
    // A border can stop short of the length, at another pane's least; the
    // next border then takes up the rest, a few times at most.
    for _ in 0..4 {
        let now = size(&tree)?;
        if now == len {
            return Some(tree);
        }
        let amount = now.abs_diff(len);
        // The pane grows (or shrinks from its far side) itself; or, to
        // shrink, a neighbour grows into it: a pane between two has no far
        // side of its own to give from.
        let placed = place(&tree, area);
        let growing = directions.iter().map(|&direction| (pane, direction));
        // Each of the axis's directions with the other, back from it.
        let [start, end] = directions;
        let neighbours = [(start, end), (end, start)]
            .into_iter()
            .filter(|_| now > len)
            .filter_map(|(direction, back)| {
                Some((neighbor(&placed.panes, pane, direction)?, back))
            });
        let tried = growing.chain(neighbours).filter_map(|(which, direction)| {
            let mut moved = tree.clone();
            resize(&mut moved, area, which, direction, amount).then_some(())?;
            Some((size(&moved)?.abs_diff(len), moved))
        });
        match tried.min_by_key(|(off, _)| *off) {
            Some((off, moved)) if off < amount => tree = moved,
            _ => return None,
        }
    }
    (size(&tree)? == len).then_some(tree)
}

impl Split {
    /// `resize`, at this split, along `axis`, and those below it: with
    /// `toward`, only a split with a sibling on the direction's side acts;
    /// without, the nearest along the axis does, from whichever side it can.
    /// Whether a border moved, once a split could act (one whose siblings
    /// are all at their least passes to the split around it); `None` if
    /// none could.
    fn resize_by(
        &mut self,
        axis: Axis,
        area: Rect,
        pane: PaneId,
        direction: Direction,
        amount: u16,
        toward: bool,
    ) -> Option<bool> {
        let index = self.0.iter().position(|(_, c)| c.contains(pane))?;
        // Deeper splits first: the border nearest the pane moves.
        let mut placement = Placement::default();
        if !area.is_empty() {
            place_split(axis, self, area, &mut placement);
        }
        let children = &mut self.0;
        let mut sizes: Vec<u16> = Vec::with_capacity(children.len());
        let mut inner = Rect::default();
        for (i, (_, child)) in children.iter().enumerate() {
            let rect = placed_span(axis, child, area, &placement);
            sizes.push(rect.len(axis));
            if i == index {
                inner = rect;
            }
        }
        let below = match children.get_mut(index) {
            Some((_, Tree::Split((), child))) => {
                child.resize_by(axis.cross(), inner, pane, direction, amount, toward)
            }
            Some((_, Tree::Pane(_))) | None => None,
        };
        if below == Some(true) || axis != Axis::of(direction) {
            return below;
        }
        let toward_start = Side::of(direction) == Side::Before;
        // Grow toward the direction, taking from the siblings that way,
        // nearest first, each down to its least; else shrink from the far
        // side, moving the other border the same way.
        let after = index.saturating_add(1);
        let (grow, shrink): (usize, Vec<usize>) =
            match (toward_start, index > 0, after < children.len()) {
                (true, true, _) => (index, (0..index).rev().collect()),
                (false, _, true) => (index, (after..children.len()).collect()),
                _ if toward => return below,
                // A split has two children or more: the pane has a sibling.
                (true, false, _) => (after, vec![index]),
                (false, _, false) => (index.saturating_sub(1), vec![index]),
            };
        // What the grown side gains, the others give, within `area`.
        let mut moved: u16 = 0;
        for i in shrink {
            let min = children
                .get(i)
                .map_or(MIN, |(_, c)| c.min_len(axis.cross(), axis));
            if let Some(size) = sizes.get_mut(i) {
                let given = amount.saturating_sub(moved).min(size.saturating_sub(min));
                *size = size.saturating_sub(given);
                moved = moved.saturating_add(given);
            }
        }
        let Some(grown) = sizes.get_mut(grow).filter(|_| moved > 0) else {
            return Some(false);
        };
        *grown = grown.saturating_add(moved);
        for ((share, _), size) in children.iter_mut().zip(&sizes) {
            *share = weight(u32::from(*size));
        }
        Some(true)
    }
}

/// The rect a child of a split along `axis` in `area` has in `placement`:
/// along the axis, the span of its placed panes, none if none is; across,
/// all of `area`.
fn placed_span(axis: Axis, child: &Tree<()>, area: Rect, placement: &Placement) -> Rect {
    let mut span: Option<Range<u16>> = None;
    child.for_each_pane(&mut |p| {
        if let Some(r) = placement.rect(p) {
            let start = r.start(axis);
            let end = start.saturating_add(r.len(axis));
            span = Some(
                span.as_ref()
                    .map_or(start..end, |s| s.start.min(start)..s.end.max(end)),
            );
        }
    });
    // Inside `area`, as the panes are.
    let Range { start, end } = span.unwrap_or_default();
    area.along(axis, start, end.saturating_sub(start))
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
    /// `0 | 1`.
    fn tree() -> Tree {
        let mut root = Tree::Pane(p(0));
        root.split(p(0), p(1), Axis::Horizontal, Side::After);
        root
    }

    /// A pane between two shrinks: a neighbour grows into it.
    #[test]
    fn a_middle_pane_shrinks_to_a_length() {
        let mut root = tree();
        root.split(p(1), p(2), Axis::Horizontal, Side::After);
        let area = area(90, 20);
        let width = |t: &Tree| place(t, area).rect(p(1)).map(|r| r.w);
        assert!(width(&root).is_some_and(|w| w > 12));
        let smaller = resize_to(&root, area, p(1), Axis::Horizontal, 12);
        assert_eq!(smaller.as_ref().and_then(width), Some(12));
        let wider = smaller.and_then(|t| resize_to(&t, area, p(1), Axis::Horizontal, 40));
        assert_eq!(wider.as_ref().and_then(width), Some(40));
    }

    #[test]
    fn splitting_shares_the_space_with_one_separator() {
        let root = tree();
        let node = &root;
        let placed = place(node, area(80, 24));
        assert_eq!(
            placed.panes,
            vec![
                (
                    p(0),
                    Rect {
                        x: 0,
                        y: 0,
                        w: 40,
                        h: 24
                    }
                ),
                (
                    p(1),
                    Rect {
                        x: 41,
                        y: 0,
                        w: 39,
                        h: 24
                    }
                )
            ]
        );
        assert_eq!(
            placed.separators,
            vec![Separator {
                axis: Axis::Horizontal,
                rect: Rect {
                    x: 40,
                    y: 0,
                    w: 1,
                    h: 24
                }
            }]
        );
        // A second split of the right pane shares its weight: 1000:500:500.
        let mut root = tree();
        assert!(root.split(p(1), p(2), Axis::Horizontal, Side::After));
        let node = &root;
        let widths: Vec<u16> = place(node, area(80, 24))
            .panes
            .iter()
            .map(|(_, r)| r.w)
            .collect();
        assert_eq!(widths, vec![39, 20, 19]);
    }

    #[test]
    fn nested_splits_place_every_pane_without_overlap() {
        let mut root = tree();
        root.split(p(1), p(2), Axis::Horizontal, Side::After);
        root.split(p(2), p(3), Axis::Vertical, Side::After);
        root.split(p(1), p(4), Axis::Vertical, Side::Before);
        let node = &root;
        assert_eq!(node.panes(), vec![p(0), p(4), p(1), p(2), p(3)]);
        let placed = place(node, area(81, 25));
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
        let mut root = tree();
        root.split(p(0), p(1), Axis::Horizontal, Side::After);
        root.split(p(1), p(2), Axis::Vertical, Side::After);
        let node = &root;
        let mut used = place(node, area(120, 40));
        for (w, h) in [(80, 24), (3, 3), (0, 9), (200, 60)] {
            place_into(node, area(w, h), &mut used);
            assert_eq!(used, place(node, area(w, h)), "{w}x{h}");
        }
        place_into(&Tree::Pane(p(7)), area(10, 10), &mut used);
        assert_eq!(used.panes, vec![(p(7), area(10, 10))]);
        assert!(used.separators.is_empty());
    }

    #[test]
    fn a_small_area_keeps_minimums_and_hides_what_does_not_fit() {
        let mut root = tree();
        for n in 2..10 {
            root.split(p(n - 1), p(n), Axis::Horizontal, Side::After);
        }
        let node = &root;
        let placed = place(node, area(10, 3));
        assert!(placed.panes.iter().all(|(_, r)| r.w >= MIN && r.h >= MIN));
        let used: usize = placed
            .panes
            .iter()
            .map(|(_, r)| usize::from(r.w))
            .sum::<usize>()
            + placed.separators.len();
        assert_eq!(used, 10);
        assert!(placed.panes.len() < 9);
        assert!(place(node, area(0, 5)).panes.is_empty());
    }

    #[test]
    fn removal_collapses_and_merges() {
        let mut tree = tree();
        tree.split(p(1), p(2), Axis::Vertical, Side::After);
        tree.split(p(2), p(3), Axis::Horizontal, Side::After);
        let mut root = Some(tree);
        assert!(remove(&mut root, p(1)));
        // (0 | (1 / (2 | 3))) minus 1 is (0 | 2 | 3), merged along one axis.
        assert!(matches!(&root, Some(Tree::Split(Axis::Horizontal, Split(c))) if c.len() == 3));
        remove(&mut root, p(2));
        remove(&mut root, p(3));
        assert_eq!(root, Some(Tree::Pane(p(0))));
        assert!(remove(&mut root, p(0)));
        assert_eq!(root, None);
        assert!(!remove(&mut root, p(0)));
    }

    /// A pane moves only within a tree holding both it and its target;
    /// otherwise nothing changes, and an empty tree takes nothing.
    #[test]
    fn moving_beside_needs_both_panes_in_the_tree() {
        let mut root = Some(tree());
        assert!(move_beside(&mut root, p(0), p(1), Direction::Down));
        assert_eq!(root.as_ref().map(Tree::panes), Some(vec![p(1), p(0)]));
        let before = root.clone();
        assert!(!move_beside(&mut root, p(0), p(9), Direction::Down));
        assert!(!move_beside(&mut root, p(9), p(0), Direction::Down));
        assert!(!move_beside(&mut root, p(0), p(0), Direction::Down));
        assert_eq!(root, before);
        let mut empty = None;
        assert!(!move_beside(&mut empty, p(0), p(1), Direction::Down));
        assert_eq!(empty, None);
    }

    /// Floating panes are placed over the tiled ones, inside their frames,
    /// the focused one on top: a frame past the area's edge moves in, one
    /// larger than it is cut, and on an area too small for a pane none is
    /// placed.
    #[test]
    fn floating_panes_fit_the_area_they_float_over() {
        let mut layout = Layout::tiled(Some(tree()));
        layout.float_at(p(2), Rect::new(70, 20, 20, 10));
        layout.float_at(p(3), Rect::new(0, 0, 200, 4));
        let mut placed = Placement::default();
        layout.place_into(area(80, 24), true, Some(p(2)), &mut placed);
        let floating: Vec<_> = placed.floating().collect();
        let (moved, cut) = (Rect::new(60, 14, 20, 10), Rect::new(0, 0, 80, 4));
        assert_eq!(
            floating,
            vec![(p(3), cut.inner(), cut), (p(2), moved.inner(), moved)]
        );
        assert_eq!(placed.tiled().len(), 2);
        layout.place_into(area(80, 24), false, None, &mut placed);
        assert!(placed.frames.is_empty() && placed.panes.len() == 2);
        layout.place_into(area(3, 3), true, None, &mut placed);
        assert!(placed.frames.is_empty());
    }

    #[test]
    fn directional_neighbours_prefer_alignment_then_distance_then_id() {
        // 0 | (1 / 2)
        let mut root = tree();
        root.split(p(1), p(2), Axis::Vertical, Side::After);
        let node = &root;
        let placed = place(node, area(80, 24));
        assert_eq!(neighbor(&placed.panes, p(0), Direction::Right), Some(p(1)));
        assert_eq!(neighbor(&placed.panes, p(2), Direction::Left), Some(p(0)));
        assert_eq!(neighbor(&placed.panes, p(1), Direction::Down), Some(p(2)));
        assert_eq!(neighbor(&placed.panes, p(2), Direction::Up), Some(p(1)));
        assert_eq!(neighbor(&placed.panes, p(0), Direction::Left), None);
        assert_eq!(neighbor(&placed.panes, p(1), Direction::Right), None);
    }

    #[test]
    fn resizing_moves_the_nearest_border_and_respects_minimums() {
        let mut root = tree();
        let node = &mut root;
        let a = area(81, 24);
        assert!(resize(node, a, p(0), Direction::Right, 10));
        let placed = place(node, a);
        assert_eq!(placed.rect(p(0)).map(|r| r.w), Some(50));
        assert_eq!(placed.rect(p(1)).map(|r| r.w), Some(30));
        // Left from the leftmost pane shrinks it, moving its right border.
        assert!(resize(node, a, p(0), Direction::Left, 5));
        assert_eq!(place(node, a).rect(p(0)).map(|r| r.w), Some(45));
        // Never below the minimum.
        assert!(resize(node, a, p(1), Direction::Left, 200));
        assert_eq!(place(node, a).rect(p(0)).map(|r| r.w), Some(MIN));
        // No split along the vertical axis: nothing to move.
        assert!(!resize(node, a, p(0), Direction::Up, 1));
    }

    /// In nested splits the border that moves is the nearest one on the
    /// side asked for, however deep the pane is: `0 | (1 / (2 | 3))`,
    /// left from 2 moves the border between 0 and the rest, not 2's own
    /// right border (which would shrink 2 while 0 stood still).
    #[test]
    fn resizing_in_nested_splits_moves_the_border_on_that_side() {
        let mut root = tree();
        root.split(p(1), p(2), Axis::Vertical, Side::After);
        root.split(p(2), p(3), Axis::Horizontal, Side::After);
        let node = &mut root;
        let a = area(81, 24);
        let before = place(node, a);
        let width = |placed: &Placement, n| placed.rect(p(n)).map(|r| r.w);
        assert!(resize(node, a, p(2), Direction::Left, 5));
        let after = place(node, a);
        assert_eq!(width(&after, 0), width(&before, 0).map(|w| w - 5));
        assert_eq!(width(&after, 1), width(&before, 1).map(|w| w + 5));
        assert_eq!(
            after.rect(p(2)).map(|r| r.x),
            before.rect(p(2)).map(|r| r.x - 5)
        );
        // Right from 3, at the right edge: no split has a sibling there, so
        // 3 shrinks from its left, the nearest border along the axis.
        let before = after;
        assert!(resize(node, a, p(3), Direction::Right, 2));
        let after = place(node, a);
        assert_eq!(width(&after, 3), width(&before, 3).map(|w| w - 2));
        assert_eq!(width(&after, 0), width(&before, 0));
    }

    /// A border held at a pane's least passes the resize on: to the
    /// siblings beyond, then to the split around; with none left, nothing
    /// moves, and the pane does not shrink from its far side instead.
    #[test]
    fn a_border_at_its_least_passes_the_resize_on() {
        let width = |t: &Tree, a, n| place(t, a).rect(p(n)).map(|r| r.w);
        let a = area(100, 24);
        let mut row = tree();
        row.split(p(1), p(2), Axis::Horizontal, Side::After);
        let wide = resize_to(&row, a, p(2), Axis::Horizontal, 80);
        let widths = wide.map(|t| [0, 1, 2].map(|n| width(&t, a, n)));
        assert_eq!(widths, Some([Some(16), Some(MIN), Some(80)]));
        // `0 | (1 / (2 | 3))`, 2 at its least: left from 3 moves 0's border.
        let mut nested = tree();
        nested.split(p(1), p(2), Axis::Vertical, Side::After);
        nested.split(p(2), p(3), Axis::Horizontal, Side::After);
        assert!(resize(&mut nested, a, p(3), Direction::Left, 100));
        let before = [0, 3].map(|n| width(&nested, a, n));
        assert!(resize(&mut nested, a, p(3), Direction::Left, 10));
        assert_eq!(width(&nested, a, 0), before[0].map(|w| w - 10));
        assert_eq!(width(&nested, a, 3), before[1].map(|w| w + 10));
        let mut pair = tree();
        assert!(resize(&mut pair, a, p(1), Direction::Left, 100));
        let held = [0, 1].map(|n| width(&pair, a, n));
        assert!(!resize(&mut pair, a, p(1), Direction::Left, 1));
        assert_eq!([0, 1].map(|n| width(&pair, a, n)), held);
    }

    #[test]
    fn swapping_exchanges_places() {
        let mut root = tree();
        let node = &mut root;
        swap(node, p(0), p(1));
        assert_eq!(node.panes(), vec![p(1), p(0)]);
    }
}
