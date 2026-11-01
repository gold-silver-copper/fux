//! A tab's layout: a tree of splits and stacks whose leaves are panes, and
//! the rectangles it gives each pane at a given size.
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
//!
//! A stack lays every child out on its whole rectangle, and shows each
//! client one: its own choice, kept in the stack ([`Shown`]), the first
//! until it chooses. Every child is sized, shown or not, so a choice
//! resizes no terminal.
use crate::id::{ClientId, PaneId};
use crate::wire::wire;
use fux_vt::keys::Direction;
use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::ops::Range;

/// What a client is shown of a stack, or focuses in a tab: its current
/// child, and the one before it, another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shown<I> {
    pub current: I,
    pub last: Option<I>,
}

impl<I: Copy + Eq> Shown<I> {
    pub fn on(current: I) -> Shown<I> {
        Shown {
            current,
            last: None,
        }
    }

    pub fn map<J>(self, f: impl Fn(I) -> J) -> Shown<J> {
        Shown {
            current: f(self.current),
            last: self.last.map(f),
        }
    }

    /// Shows `id`, the current one becoming the last: whether it changed.
    pub fn show(&mut self, id: I) -> bool {
        let changed = self.current != id;
        if changed {
            self.last = Some(std::mem::replace(&mut self.current, id));
        }
        changed
    }

    /// Of the children `was` before a change, those `kept` are left, and
    /// `first` heads them all: a current one gone becomes the last, else
    /// its neighbour (the next one left, else the one before), else
    /// `first`. Whether the current one changed.
    pub fn keep(&mut self, was: &[I], kept: impl Fn(I) -> bool, first: Option<I>) -> bool {
        self.last = self.last.filter(|last| kept(*last));
        if kept(self.current) {
            return false;
        }
        let at = was.iter().position(|c| *c == self.current).unwrap_or(0);
        let (before, after) = was.split_at_checked(at).unwrap_or((was, &[]));
        let mut near = after.iter().chain(before.iter().rev()).copied();
        let next = self.last.take().or_else(|| near.find(|c| kept(*c)));
        match next.or(first) {
            Some(next) => {
                self.current = next;
                true
            }
            None => false,
        }
    }
}

wire! {
    /// How a split arranges its children.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Axis {
        /// Side by side, divided by vertical separators (`str horizontal axis split`).
        Horizontal,
        /// Stacked, divided by horizontal separators (`str vertical axis split`).
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

/// A tab's layout: one pane, a split of two, or a stack of alternatives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tree {
    Pane(PaneId),
    Split(Box<Split>),
    Stack(Box<Stack>),
}

wire! {
    /// One node of a flat layout. Node zero is the root; children name
    /// indices in the same list. Every node must be reached exactly once.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Node {
        Pane(pane: PaneId),
        Split {
            axis: Axis,
            first_share: NonZeroU32,
            second_share: NonZeroU32,
            first: u32,
            second: u32,
        },
        /// Two or more children, each on its whole rectangle.
        Stack { children: Vec<u32> },
    }
}

/// Whether a flat layout of `len` nodes may be given: none is no layout
/// (`bad_value`), from the word or the server alike.
pub fn admit_nodes(len: usize) -> Result<(), crate::code::Error> {
    match len {
        0 => Err(crate::code::fail(
            crate::code::Raise::BadValue,
            "a layout needs a node",
        )),
        _ => Ok(()),
    }
}

/// The native tree of a flat layout of exactly `panes`, a leaf each: one
/// that is no tree, or holds others, is `bad_value`. Its leaves are checked
/// before it is walked, so it is no deeper than a tree of those panes that
/// edits make; it is walked and built with stacks of its own.
pub fn from_nodes(nodes: &[Node], panes: &[PaneId]) -> Result<Tree, crate::code::Error> {
    use crate::code::{Raise, fail};
    admit_nodes(nodes.len())?;
    let bad = |why: &str| fail(Raise::BadValue, why);
    let sorted = |mut leaves: Vec<PaneId>| {
        leaves.sort_unstable();
        leaves
    };
    let leaves = (nodes.iter()).filter_map(|n| match n {
        Node::Pane(pane) => Some(*pane),
        Node::Split { .. } | Node::Stack { .. } => None,
    });
    if sorted(leaves.collect()) != sorted(panes.to_vec()) {
        return Err(bad(
            "layout must contain every current tab pane exactly once",
        ));
    }
    // Each node, parents before their children.
    let mut order = Vec::with_capacity(nodes.len());
    let mut seen = vec![false; nodes.len()];
    let mut pending = vec![0usize];
    while let Some(at) = pending.pop() {
        let visited = seen
            .get_mut(at)
            .ok_or_else(|| bad("layout child index is out of range"))?;
        if *visited {
            return Err(bad("layout nodes must be reached exactly once"));
        }
        *visited = true;
        order.push(at);
        match nodes.get(at) {
            Some(Node::Split { first, second, .. }) => {
                pending.extend([*second as usize, *first as usize]);
            }
            Some(Node::Stack { children }) if children.len() < 2 => {
                return Err(bad("a stack needs two or more children"));
            }
            Some(Node::Stack { children }) => {
                pending.extend(children.iter().rev().map(|c| *c as usize));
            }
            Some(Node::Pane(_)) | None => {}
        }
    }
    if seen.iter().any(|v| !v) {
        return Err(bad("layout contains unreachable nodes"));
    }
    // Children before their parents, each taken once: the walk above found
    // a tree.
    let mut built: Vec<Option<Tree>> = std::iter::repeat_with(|| None).take(nodes.len()).collect();
    let take = |built: &mut Vec<Option<Tree>>, at: u32| {
        (built.get_mut(at as usize))
            .and_then(Option::take)
            .ok_or_else(|| bad("layout nodes must be reached exactly once"))
    };
    for &at in order.iter().rev() {
        let tree = match nodes.get(at) {
            Some(Node::Pane(pane)) => Tree::Pane(*pane),
            Some(Node::Split {
                axis,
                first_share,
                second_share,
                first,
                second,
            }) => Tree::Split(Box::new(Split {
                axis: *axis,
                shares: Shares::new(*first_share, *second_share),
                first: take(&mut built, *first)?,
                second: take(&mut built, *second)?,
            })),
            Some(Node::Stack { children }) => {
                let mut children = children.iter().map(|c| take(&mut built, *c));
                let first = children
                    .next()
                    .ok_or_else(|| bad("a stack needs two or more children"))??;
                let rest = children.collect::<Result<_, _>>()?;
                Tree::Stack(Box::new(Stack {
                    first,
                    rest,
                    shown: BTreeMap::new(),
                }))
            }
            None => return Err(bad("layout child index is out of range")),
        };
        if let Some(slot) = built.get_mut(at) {
            *slot = Some(tree);
        }
    }
    take(&mut built, 0)
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

/// Trees on one rectangle, two or more, each client shown one: its own
/// current child, the first until it chooses another.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stack {
    first: Tree,
    rest: Vec<Tree>,
    /// Each client's choice, by child index: one with none is shown the
    /// first.
    shown: BTreeMap<ClientId, Shown<usize>>,
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
    /// Whether `other` holds exactly the same panes, in any order.
    pub fn same_panes(&self, other: &Tree) -> bool {
        let sorted = |tree: &Tree| {
            let mut panes = tree.panes();
            panes.sort_unstable();
            panes
        };
        sorted(self) == sorted(other)
    }

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
            Tree::Stack(stack) => stack.children().for_each(|c| c.for_each_pane(f)),
        }
    }
    /// The panes `client` is shown, in tree order: at each stack, those
    /// of its child.
    pub fn shown_panes(&self, client: ClientId) -> Vec<PaneId> {
        fn visit(tree: &Tree, client: ClientId, out: &mut Vec<PaneId>) {
            match tree {
                Tree::Pane(p) => out.push(*p),
                Tree::Split(split) => {
                    visit(&split.first, client, out);
                    visit(&split.second, client, out);
                }
                Tree::Stack(stack) => visit(stack.current(client), client, out),
            }
        }
        let mut out = Vec::new();
        visit(self, client, &mut out);
        out
    }
    /// The first pane in tree order.
    pub fn first_pane(&self) -> PaneId {
        match self {
            Tree::Pane(p) => *p,
            Tree::Split(split) => split.first.first_pane(),
            Tree::Stack(stack) => stack.first.first_pane(),
        }
    }
    /// The first pane `client` is shown in it.
    pub fn first_shown(&self, client: ClientId) -> PaneId {
        match self {
            Tree::Pane(p) => *p,
            Tree::Split(split) => split.first.first_shown(client),
            Tree::Stack(stack) => stack.current(client).first_shown(client),
        }
    }
    pub fn contains(&self, pane: PaneId) -> bool {
        match self {
            Tree::Pane(p) => *p == pane,
            Tree::Split(split) => split.first.contains(pane) || split.second.contains(pane),
            Tree::Stack(stack) => stack.children().any(|c| c.contains(pane)),
        }
    }

    /// The stack nearest `pane` that holds it, if one does.
    pub fn stack_of(&self, pane: PaneId) -> Option<&Stack> {
        match self {
            Tree::Pane(_) => None,
            Tree::Split(split) => {
                (split.first.stack_of(pane)).or_else(|| split.second.stack_of(pane))
            }
            Tree::Stack(stack) => {
                let child = stack.children().find(|c| c.contains(pane))?;
                child.stack_of(pane).or(Some(stack))
            }
        }
    }

    /// Shows `client` `pane` at each stack that holds it: whether that
    /// changed what it is shown.
    pub fn reveal(&mut self, client: ClientId, pane: PaneId) -> bool {
        match self {
            Tree::Pane(_) => false,
            Tree::Split(split) => match split.first.contains(pane) {
                true => split.first.reveal(client, pane),
                false => split.second.reveal(client, pane),
            },
            Tree::Stack(stack) => {
                let Some(at) = stack.children().position(|c| c.contains(pane)) else {
                    return false;
                };
                let shown = stack.shown.entry(client).or_insert(Shown::on(0));
                let changed = shown.show(at);
                let deeper = (stack.children_mut().nth(at)).is_some_and(|c| c.reveal(client, pane));
                changed || deeper
            }
        }
    }

    /// Forgets `client`'s choices.
    pub fn forget(&mut self, client: ClientId) {
        match self {
            Tree::Pane(_) => {}
            Tree::Split(split) => {
                split.first.forget(client);
                split.second.forget(client);
            }
            Tree::Stack(stack) => {
                stack.shown.remove(&client);
                stack.children_mut().for_each(|c| c.forget(client));
            }
        }
    }

    /// The smallest length it can have along `axis`: a stack's, the
    /// largest of its children's, as each is laid out on all of it.
    fn min_len(&self, axis: Axis) -> u16 {
        match self {
            Tree::Pane(_) => MIN,
            Tree::Split(split) => {
                let (a, b) = (split.first.min_len(axis), split.second.min_len(axis));
                match split.axis == axis {
                    true => a.saturating_add(1).saturating_add(b),
                    false => a.max(b),
                }
            }
            Tree::Stack(stack) => (stack.children().map(|c| c.min_len(axis)))
                .max()
                .unwrap_or(MIN),
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
            Tree::Stack(stack) => (stack.children_mut())
                .find(|c| c.contains(target))
                .map_or(Err(NoPane), |c| {
                    c.split((target, new), (axis, side), shares)
                }),
        }
    }

    /// Puts `new` in a stack with `target`, after it: in the stack that
    /// holds it as a child, else a new one of the two.
    pub fn stack(&mut self, (target, new): (PaneId, PaneId)) -> Result<(), NoPane> {
        match self {
            Tree::Pane(p) if *p == target => {
                let stack = Stack {
                    first: Tree::Pane(target),
                    rest: vec![Tree::Pane(new)],
                    shown: BTreeMap::new(),
                };
                *self = Tree::Stack(Box::new(stack));
                Ok(())
            }
            Tree::Pane(_) => Err(NoPane),
            Tree::Split(split) => {
                (split.first.stack((target, new))).or_else(|_| split.second.stack((target, new)))
            }
            Tree::Stack(stack) => {
                let leaf = Tree::Pane(target);
                let at = stack.children().position(|c| *c == leaf);
                match at {
                    Some(at) => {
                        stack.insert(at.saturating_add(1), Tree::Pane(new));
                        Ok(())
                    }
                    None => (stack.children_mut())
                        .find(|c| c.contains(target))
                        .map_or(Err(NoPane), |c| c.stack((target, new))),
                }
            }
        }
    }

    /// The tree without `pane`: its sibling takes their split's place, and
    /// all its room; a stack left with one child becomes it.
    pub fn remove(&self, pane: PaneId) -> Removed {
        let split = match self {
            Tree::Pane(p) if *p == pane => return Removed::Emptied,
            Tree::Pane(_) => return Removed::Absent,
            Tree::Split(split) => split,
            Tree::Stack(stack) => return stack.remove(pane),
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
            Tree::Stack(stack) => stack.children_mut().for_each(|c| c.swap(a, b)),
        }
    }

    /// Its nodes, flat, in preorder: the root first, as `from_nodes` takes
    /// them.
    pub fn nodes(&self) -> Vec<Node> {
        self.flat().0
    }

    /// What `client` is shown at each stack, as indices of `nodes`: the
    /// stack's, then its child's and the one before.
    pub fn choices(&self, client: ClientId) -> Vec<(u32, Shown<u32>)> {
        let (nodes, stacks) = self.flat();
        let at = |i: usize| u32::try_from(i).ok();
        (stacks.into_iter())
            .filter_map(|(index, stack)| {
                let Some(Node::Stack { children }) = nodes.get(index) else {
                    return None;
                };
                let child = |i: usize| children.get(i).copied();
                let shown = stack.shown(client);
                let current = child(shown.current)?;
                Some((
                    at(index)?,
                    Shown {
                        current,
                        last: shown.last.and_then(child),
                    },
                ))
            })
            .collect()
    }

    /// Its nodes in preorder, and each stack with its index there: walked
    /// with a stack of its own, as deep as it is.
    fn flat(&self) -> (Vec<Node>, Vec<(usize, &Stack)>) {
        let (mut nodes, mut stacks) = (Vec::new(), Vec::new());
        // Each tree left, and the node and child it is of.
        let mut work: Vec<(&Tree, Option<(usize, usize)>)> = vec![(self, None)];
        while let Some((tree, parent)) = work.pop() {
            let at = nodes.len();
            let index = u32::try_from(at).unwrap_or(u32::MAX);
            match parent.and_then(|(p, child)| Some((nodes.get_mut(p)?, child))) {
                Some((Node::Split { first, .. }, 0)) => *first = index,
                Some((Node::Split { second, .. }, _)) => *second = index,
                Some((Node::Stack { children }, child)) => {
                    if let Some(c) = children.get_mut(child) {
                        *c = index;
                    }
                }
                Some((Node::Pane(_), _)) | None => {}
            }
            let node = match tree {
                Tree::Pane(pane) => Node::Pane(*pane),
                Tree::Split(split) => {
                    work.extend([
                        (&split.second, Some((at, 1))),
                        (&split.first, Some((at, 0))),
                    ]);
                    let (first_share, second_share) = split.shares.get();
                    Node::Split {
                        axis: split.axis,
                        first_share,
                        second_share,
                        first: 0,
                        second: 0,
                    }
                }
                Tree::Stack(stack) => {
                    let children: Vec<&Tree> = stack.children().collect();
                    work.extend(
                        (children.iter().enumerate().rev()).map(|(i, c)| (*c, Some((at, i)))),
                    );
                    stacks.push((at, &**stack));
                    Node::Stack {
                        children: vec![0; children.len()],
                    }
                }
            };
            nodes.push(node);
        }
        (nodes, stacks)
    }
}

impl Stack {
    /// Its children, in order.
    pub fn children(&self) -> impl Iterator<Item = &Tree> {
        std::iter::once(&self.first).chain(&self.rest)
    }
    fn children_mut(&mut self) -> impl Iterator<Item = &mut Tree> {
        std::iter::once(&mut self.first).chain(&mut self.rest)
    }
    fn len(&self) -> usize {
        self.rest.len().saturating_add(1)
    }
    /// What `client` is shown here: its choice, else the first child.
    pub fn shown(&self, client: ClientId) -> Shown<usize> {
        self.shown.get(&client).copied().unwrap_or(Shown::on(0))
    }
    /// The child `client` is shown.
    pub fn current(&self, client: ClientId) -> &Tree {
        (self.children().nth(self.shown(client).current)).unwrap_or(&self.first)
    }

    /// Puts `child` at `at`, each choice staying with its child.
    fn insert(&mut self, at: usize, child: Tree) {
        let at = at.clamp(1, self.len());
        let mut rest = std::mem::take(&mut self.rest).into_iter();
        self.rest = rest.by_ref().take(at.saturating_sub(1)).collect();
        self.rest.push(child);
        self.rest.extend(rest);
        for shown in self.shown.values_mut() {
            *shown = shown.map(|i| if i >= at { i.saturating_add(1) } else { i });
        }
    }

    /// The stack without `pane`: a child it empties goes, and each client
    /// shown that child is shown the one it was before, else its neighbour.
    fn remove(&self, pane: PaneId) -> Removed {
        let found = (self.children().enumerate())
            .map(|(i, c)| (i, c.remove(pane)))
            .find(|(_, removed)| *removed != Removed::Absent);
        let mut stack = self.clone();
        match found {
            None | Some((_, Removed::Absent)) => Removed::Absent,
            Some((at, Removed::Left(rest))) => {
                if let Some(child) = stack.children_mut().nth(at) {
                    *child = rest;
                }
                Removed::Left(Tree::Stack(Box::new(stack)))
            }
            Some((at, Removed::Emptied)) => {
                let was: Vec<usize> = (0..self.len()).collect();
                for shown in stack.shown.values_mut() {
                    shown.keep(&was, |i| i != at, Some(0));
                    *shown = shown.map(|i| if i > at { i.saturating_sub(1) } else { i });
                }
                let Stack { first, rest, shown } = stack;
                let children = std::iter::once(first).chain(rest).enumerate();
                let mut left = children.filter(|(i, _)| *i != at).map(|(_, c)| c);
                let Some(first) = left.next() else {
                    return Removed::Emptied;
                };
                let rest: Vec<Tree> = left.collect();
                match rest.is_empty() {
                    true => Removed::Left(first),
                    false => Removed::Left(Tree::Stack(Box::new(Stack { first, rest, shown }))),
                }
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

/// The rectangles of every pane that fits in `area`, every child of a
/// stack on its whole rectangle, and the separators.
pub fn place(root: &Tree, area: Rect) -> Placement {
    let mut out = Placement::default();
    place_into(root, area, None, &mut out);
    out
}

/// Places `root` in `area` into `out`, whatever it held, reusing its
/// buffers: a placement made into again allocates nothing. With `client`,
/// only what it is shown: at each stack, its child.
pub fn place_into(root: &Tree, area: Rect, client: Option<ClientId>, out: &mut Placement) {
    out.clear();
    place_tree(root, area, client, out);
}

fn place_tree(tree: &Tree, area: Rect, client: Option<ClientId>, out: &mut Placement) {
    if area.is_empty() {
        return;
    }
    match tree {
        Tree::Pane(p) => out.panes.push((*p, area)),
        Tree::Split(split) => {
            let [first, line, second] = split.parts(area);
            place_tree(&split.first, first, client, out);
            if !line.is_empty() {
                let axis = split.axis;
                out.separators.push(Separator { axis, rect: line });
                place_tree(&split.second, second, client, out);
            }
        }
        Tree::Stack(stack) => match client {
            Some(c) => place_tree(stack.current(c), area, client, out),
            None => (stack.children()).for_each(|child| place_tree(child, area, client, out)),
        },
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
        let split = match self {
            Tree::Pane(_) => return None,
            Tree::Split(split) => split,
            Tree::Stack(stack) => {
                let child = stack.children_mut().find(|c| c.contains(shift.pane))?;
                return child.shift(area, shift, left);
            }
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
        let split = match self {
            Tree::Pane(_) => return,
            Tree::Split(split) => split,
            Tree::Stack(stack) => {
                let stretch = |c: &mut Tree| c.stretch(area, (axis, len), (edge, pane));
                return stack.children_mut().for_each(stretch);
            }
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
    fn flat_layouts_require_one_reachable_tree_of_the_tabs_panes() -> Result<(), crate::code::Error>
    {
        use crate::code::Code;
        let branch = |first, second| Node::Split {
            axis: Axis::Horizontal,
            first_share: NonZeroU32::MIN,
            second_share: NonZeroU32::MIN,
            first,
            second,
        };
        let leaf = Node::Pane(p(1));
        let bad = Code::BadValue;
        for (nodes, code) in [
            (vec![], bad),
            (vec![Node::Pane(p(2))], bad),
            (vec![branch(0, 1), leaf.clone()], bad),
            (vec![branch(1, 1), leaf.clone()], bad),
            (vec![branch(1, 3), leaf.clone(), leaf.clone()], bad),
            (vec![leaf.clone(), leaf.clone()], bad),
        ] {
            assert_eq!(
                from_nodes(&nodes, &[p(1)]).map(drop).map_err(|e| e.code()),
                Err(code)
            );
        }
        let chain = |depth: u32| {
            let mut nodes = Vec::new();
            for level in 0..depth {
                nodes.push(branch(level * 2 + 1, level * 2 + 2));
                nodes.push(Node::Pane(p(level)));
            }
            nodes.push(Node::Pane(p(depth)));
            nodes
        };
        // As deep as its panes let it be, as edits make it.
        let panes: Vec<PaneId> = (0..=1000).map(p).collect();
        assert_eq!(from_nodes(&chain(1000), &panes)?.panes(), panes);
        let shares = Shares::new(NonZeroU32::MIN, NonZeroU32::MAX);
        let nodes = vec![
            Node::Split {
                axis: Axis::Vertical,
                first_share: NonZeroU32::MIN,
                second_share: NonZeroU32::MAX,
                first: 2,
                second: 1,
            },
            Node::Pane(p(2)),
            Node::Pane(p(1)),
        ];
        let root = from_nodes(&nodes, &[p(1), p(2)])?;
        assert_eq!(root.panes(), vec![p(1), p(2)]);
        assert!(
            matches!(root, Tree::Split(split) if split.axis() == Axis::Vertical && split.shares() == shares)
        );
        Ok(())
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

    #[test]
    fn placing_into_a_used_placement_is_placing_afresh() {
        let root = made(&[(0, Axis::Horizontal), (1, Axis::Vertical)]);
        let mut used = place(&root, area(120, 40));
        for (w, h) in [(80, 24), (3, 3), (0, 9), (200, 60)] {
            place_into(&root, area(w, h), None, &mut used);
            assert_eq!(used, place(&root, area(w, h)), "{w}x{h}");
        }
        place_into(&Tree::Pane(p(7)), area(10, 10), None, &mut used);
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

        let a = area(100, 24);
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

    #[test]
    fn resizing_to_a_length_preserves_siblings_and_minimums() {
        let row = made(&[(0, Axis::Horizontal), (1, Axis::Horizontal)]);
        assert_eq!(
            resize_to(&row, area(100, 24), p(2), Axis::Horizontal, 80).map(|t| widths(
                &t,
                area(100, 24),
                &[0, 1, 2]
            )),
            Some(vec![Some(16), Some(MIN), Some(80)])
        );
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

    #[test]
    fn a_stack_lays_every_child_on_its_room_and_keeps_each_clients_choice() -> Result<(), String> {
        use crate::code::Code;
        let (c1, c2) = (ClientId::of(1), ClientId::of(2));
        // `0 | [1, 2 / 3, 4]`.
        let mut root = tree();
        assert_eq!(root.stack((p(1), p(2))), Ok(()));
        assert_eq!(root.stack((p(2), p(4))), Ok(()));
        let way = (Axis::Vertical, Side::After);
        assert_eq!(root.split((p(2), p(3)), way, Shares::HALVED), Ok(()));
        assert_eq!(root.panes(), [0, 1, 2, 3, 4].map(p));
        let rebuilt = from_nodes(&root.nodes(), &root.panes()).map_err(|e| e.to_string())?;
        assert_eq!(rebuilt, root);
        let mut deep = Tree::Pane(p(0));
        (1..70).for_each(|n| deep.append(p(n)));
        let leaves = deep
            .nodes()
            .iter()
            .filter(|n| matches!(n, Node::Pane(_)))
            .count();
        assert_eq!(
            leaves, 70,
            "a tree deeper than a flat layout may be is flat"
        );
        let few = |children| vec![Node::Stack { children }, Node::Pane(p(1))];
        for nodes in [few(vec![]), few(vec![1])] {
            let refused = from_nodes(&nodes, &[p(1)]).map(drop).map_err(|e| e.code());
            assert_eq!(refused, Err(Code::BadValue));
        }
        // Each child has the stack's whole room, shown or not, and the
        // most any needs.
        let placed = place(&root, area(81, 24));
        let right = placed.rect(p(1));
        assert!(right.is_some_and(|r| r.x == 41 && r.w == 40 && r.h == 24));
        assert_eq!(placed.rect(p(4)), right);
        assert_eq!(placed.rect(p(2)).map(|r| (r.x, r.h)), Some((41, 12)));
        assert!(shows_all(&root, area(81, 5)) && !shows_all(&root, area(81, 4)));
        // Each client is shown its own child, the first until it chooses.
        let shown = |root: &Tree, c| root.shown_panes(c);
        assert!(root.reveal(c2, p(3)));
        assert_eq!(
            (shown(&root, c1), shown(&root, c2)),
            (vec![p(0), p(1)], [0, 2, 3].map(p).to_vec())
        );
        let mut placed = Placement::default();
        place_into(&root, area(81, 24), Some(c2), &mut placed);
        assert_eq!(placed.panes.len(), 3);
        let (stack, one, both) = (2, 3, 4);
        assert_eq!(root.choices(c1), [(stack, Shown::on(one))]);
        let chosen = Shown {
            current: both,
            last: Some(one),
        };
        assert_eq!(root.choices(c2), [(stack, chosen)]);
        // A child added keeps each choice on its child; one gone shows each
        // client shown it the one before, else its neighbour.
        assert_eq!(root.stack((p(1), p(5))), Ok(()));
        assert!(root.reveal(c1, p(4)));
        assert_eq!(shown(&root, c2), [0, 2, 3].map(p));
        let without = |root: &Tree, pane| match root.remove(p(pane)) {
            Removed::Left(rest) => Ok(rest),
            other @ (Removed::Absent | Removed::Emptied) => Err(format!("{other:?}")),
        };
        let root = without(&without(&root, 1)?, 4)?;
        assert_eq!(shown(&root, c1), [0, 2, 3].map(p));
        assert_eq!(shown(&root, c2), [0, 2, 3].map(p));
        // A stack left with one child is it.
        let root = without(&root, 5)?;
        assert!(
            root.nodes()
                .iter()
                .all(|n| !matches!(n, Node::Stack { .. }))
        );
        assert_eq!(root.panes(), [0, 2, 3].map(p));
        Ok(())
    }
}
