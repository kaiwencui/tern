//! The layout of one tab: the tree, the focus and the zoom, and every
//! operation the app applies to them.

use std::fmt;

use tern_types::{PaneId, Point, Rect};

use crate::tree::{self, Node, Split, Visit};
use crate::{normalized, Axis, Direction, Zone};

/// The most splits that may sit above any one pane. Real layouts stay far
/// below it; the bound keeps the recursive walks over a tree read from text
/// from exhausting the stack.
pub const MAX_DEPTH: usize = 256;

/// Names one split, and so its divider, for as long as that split exists.
/// Changing ratios or other parts of the tree leaves it valid; once the split
/// is gone (one of its sides closed, say) the handle matches nothing.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DividerHandle(u64);

/// A divider as laid out in one area.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Divider {
    pub handle: DividerHandle,
    /// The axis of the split it belongs to. With [`Axis::Horizontal`] the
    /// line runs top to bottom and drags left and right.
    pub axis: Axis,
    pub rect: Rect,
}

/// What is left after a pane leaves the layout.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Closed {
    /// Panes remain and this one has focus: the nearest pane beside the freed
    /// space if the pane that left had focus, otherwise the pane that already
    /// had it.
    Focus(PaneId),
    /// That was the last pane; the layout is now empty.
    Empty,
}

/// Why an operation was refused. A refused operation changes nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LayoutError {
    /// The pane is not in this layout.
    NotFound(PaneId),
    /// The pane is already in this layout, and a pane may appear only once.
    AlreadyPresent(PaneId),
    /// The pane would end up under more than [`MAX_DEPTH`] splits.
    TooDeep,
    /// [`Zone::Center`] names no side to add a pane on.
    CenterZone,
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LayoutError::NotFound(p) => write!(f, "pane {p} is not in this layout"),
            LayoutError::AlreadyPresent(p) => write!(f, "pane {p} is already in this layout"),
            LayoutError::TooDeep => {
                write!(f, "a pane cannot sit under more than {MAX_DEPTH} splits")
            }
            LayoutError::CenterZone => write!(f, "the center zone names no side to add a pane on"),
        }
    }
}

impl std::error::Error for LayoutError {}

/// The panes of one tab: a binary tree of splits, the focused pane, and
/// whether that pane is zoomed.
///
/// Every operation keeps these true: each pane appears once; a layout with
/// panes has a focused pane, and it is one of them; ratios lie in 0..=1; only
/// a layout of two or more panes is zoomed. Zoom always applies to the focused
/// pane, so moving focus while zoomed carries the zoom along. Any change to
/// which panes are where (split, close, take, insert, move) ends zoom.
///
/// Equality compares what [`Display`](fmt::Display) writes: the tree, the
/// ratios, the focus and the zoom. Divider handles and the focus history are
/// runtime details and do not count.
#[derive(Clone, Debug)]
pub struct Layout {
    pub(crate) root: Option<Node>,
    /// Some exactly when root is, and always one of its panes.
    pub(crate) focus: Option<PaneId>,
    pub(crate) zoomed: bool,
    /// Panes in the order they last had focus, most recent last. It breaks
    /// ties when choosing which pane to focus.
    pub(crate) history: Vec<PaneId>,
    /// The id the next split gets; ids back divider handles.
    pub(crate) next_split: u64,
}

impl PartialEq for Layout {
    fn eq(&self, other: &Layout) -> bool {
        self.root == other.root && self.focus == other.focus && self.zoomed == other.zoomed
    }
}

/// Where a divider sits and how far it may move.
struct Track {
    split: u64,
    axis: Axis,
    /// Where the split's first child starts.
    start: i32,
    /// The divider's width.
    line: i32,
    /// The length the two children share.
    avail: i32,
    /// The first child's length now.
    current: i32,
    /// The shortest first child that keeps every pane in it at the minimum,
    /// or more than avail when none does.
    lowest: i32,
    /// The longest first child that keeps every pane in the second child at
    /// the minimum, or below zero when none does.
    highest: i32,
}

impl Layout {
    /// A layout holding one pane, which has focus.
    pub fn new(pane: PaneId) -> Layout {
        Layout {
            root: Some(Node::Leaf(pane)),
            focus: Some(pane),
            zoomed: false,
            history: vec![pane],
            next_split: 0,
        }
    }

    pub(crate) fn empty() -> Layout {
        Layout {
            root: None,
            focus: None,
            zoomed: false,
            history: Vec::new(),
            next_split: 0,
        }
    }

    /// Every pane in visual order: depth first, left before right and top
    /// before bottom.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            root.panes(&mut out);
        }
        out
    }

    pub fn len(&self) -> usize {
        self.root.as_ref().map_or(0, Node::len)
    }

    /// True once the last pane has been closed or taken.
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    pub fn contains(&self, pane: PaneId) -> bool {
        self.root.as_ref().is_some_and(|r| r.contains(pane))
    }

    /// The focused pane; None only when the layout is empty.
    pub fn focused(&self) -> Option<PaneId> {
        self.focus
    }

    /// Focuses `pane`. Returns false, changing nothing, if it is not here.
    pub fn set_focus(&mut self, pane: PaneId) -> bool {
        if !self.contains(pane) {
            return false;
        }
        self.focus_on(pane);
        true
    }

    /// Focuses the next pane in visual order, wrapping around, and returns it.
    pub fn focus_next(&mut self) -> Option<PaneId> {
        self.cycle_focus(true)
    }

    /// Focuses the previous pane in visual order, wrapping around, and
    /// returns it.
    pub fn focus_previous(&mut self) -> Option<PaneId> {
        self.cycle_focus(false)
    }

    /// Focuses the pane adjacent to the focused one on that side, and returns
    /// it; None, changing nothing, if no pane is there.
    ///
    /// Of the panes across the neighboring divider, the one sharing the
    /// longest stretch of edge with the focused pane wins; on a tie, the one
    /// containing the focused pane's center line, then the most recently
    /// focused, then the first in visual order. While zoomed this looks at
    /// the layout as it would be unzoomed, and the zoom moves to the new pane.
    pub fn focus_direction(
        &mut self,
        direction: Direction,
        area: Rect,
        divider_px: i32,
    ) -> Option<PaneId> {
        let focus = self.focus?;
        let rects = self.tree_rects(area, divider_px);
        let current = rects.iter().find(|(p, _)| *p == focus)?.1;
        // The edges two neighbors share run across the direction of travel.
        let across = direction.axis().cross();
        let center = across.coord(current.center());
        let line = divider_px.max(0);
        let best = rects
            .iter()
            .rev()
            .filter(|(p, _)| *p != focus)
            .filter_map(|&(p, r)| {
                let gap = match direction {
                    Direction::Left => current.x - r.right(),
                    Direction::Right => r.x - current.right(),
                    Direction::Up => current.y - r.bottom(),
                    Direction::Down => r.y - current.bottom(),
                };
                let overlap = span_overlap(current, r, across);
                // Adjacent means only a divider lies between the two.
                if !(0..=line).contains(&gap) || overlap <= 0 {
                    return None;
                }
                let start = across.start(r);
                let on_center_line = start <= center && center < start + across.extent(r);
                Some((p, (overlap, on_center_line, self.recency(p))))
            })
            // Iterating in reverse makes max_by_key keep the first in visual
            // order on a full tie.
            .max_by_key(|&(_, key)| key)?;
        self.focus_on(best.0);
        Some(best.0)
    }

    pub fn is_zoomed(&self) -> bool {
        self.zoomed
    }

    /// Zooms the focused pane, or ends the zoom, and returns whether the
    /// layout is zoomed now. A lone pane has nothing to hide, so it is never
    /// zoomed.
    pub fn toggle_zoom(&mut self) -> bool {
        self.zoomed = !self.zoomed && self.len() > 1;
        self.zoomed
    }

    /// Puts `new_pane` beside `target` on the given side, sharing the target's
    /// space half and half, and focuses it.
    pub fn split(
        &mut self,
        target: PaneId,
        direction: Direction,
        new_pane: PaneId,
    ) -> Result<(), LayoutError> {
        self.add_beside(target, direction, new_pane)
    }

    /// Removes `pane`. Its sibling takes the freed space. If it had focus,
    /// focus moves to the nearest pane beside the freed space, the most
    /// recently focused one when several touch it.
    pub fn close(&mut self, pane: PaneId) -> Result<Closed, LayoutError> {
        let root = self.root.as_ref().ok_or(LayoutError::NotFound(pane))?;
        if root.is_pane(pane) {
            *self = Layout::empty();
            return Ok(Closed::Empty);
        }
        let (parent, in_first) = root.parent_of(pane).ok_or(LayoutError::NotFound(pane))?;
        let focus = match self.focus {
            Some(f) if f != pane => f,
            _ => {
                let sibling = if in_first {
                    &parent.second
                } else {
                    &parent.first
                };
                // The freed space lies on the sibling's leading side when the
                // pane came first.
                sibling.edge_pane(parent.axis, in_first, &|p| self.recency(p))
            }
        };
        if let Some(root) = self.root.as_mut() {
            root.remove(pane);
        }
        self.history.retain(|&p| p != pane);
        self.zoomed = false;
        self.focus_on(focus);
        Ok(Closed::Focus(focus))
    }

    /// Removes `pane` so it can move to another layout, exactly as [`close`]
    /// does, and returns whether any panes remain.
    ///
    /// [`close`]: Layout::close
    pub fn take(&mut self, pane: PaneId) -> Result<bool, LayoutError> {
        Ok(self.close(pane)? != Closed::Empty)
    }

    /// Adds `pane`, coming from elsewhere, beside `target` on the side the
    /// edge zone names, half and half, and focuses it. [`Zone::Center`] is
    /// refused: to swap panes between two layouts, call [`replace`] on each.
    ///
    /// [`replace`]: Layout::replace
    pub fn insert(&mut self, target: PaneId, zone: Zone, pane: PaneId) -> Result<(), LayoutError> {
        let side = zone.side().ok_or(LayoutError::CenterZone)?;
        self.add_beside(target, side, pane)
    }

    /// Adds every pane of `other`, a layout coming from elsewhere (a tab
    /// dropped on a pane), beside `target` on the side the edge zone names.
    /// The target and the newcomers share its space half and half; among
    /// themselves the newcomers keep the arrangement and the ratios they
    /// had, and the pane that had focus there has it here. Their zoom does
    /// not come along. An empty `other` adds nothing. [`Zone::Center`] is
    /// refused, as for [`insert`].
    ///
    /// [`insert`]: Layout::insert
    pub fn insert_layout(
        &mut self,
        target: PaneId,
        zone: Zone,
        other: &Layout,
    ) -> Result<(), LayoutError> {
        let side = zone.side().ok_or(LayoutError::CenterZone)?;
        let mut next = self.next_split;
        let root = self.root.as_mut().ok_or(LayoutError::NotFound(target))?;
        let depth = root.depth_of(target).ok_or(LayoutError::NotFound(target))?;
        let Some(mut added) = other.root.clone() else {
            return Ok(());
        };
        if let Some(pane) = other.panes().into_iter().find(|&p| root.contains(p)) {
            return Err(LayoutError::AlreadyPresent(pane));
        }
        // The newcomers end up one split below where the target was.
        if depth + added.height() >= MAX_DEPTH {
            return Err(LayoutError::TooDeep);
        }
        // Their splits get ids from this layout, so every divider handle
        // here still names one split.
        let id = next;
        next += 1;
        added.for_each_split_mut(&mut |split| {
            split.id = next;
            next += 1;
        });
        let leaf = root.leaf_mut(target).ok_or(LayoutError::NotFound(target))?;
        let (first, second) = if side.is_forward() {
            (Node::Leaf(target), added)
        } else {
            (added, Node::Leaf(target))
        };
        *leaf = Node::Split(Box::new(Split {
            id,
            axis: side.axis(),
            ratio: 0.5,
            first,
            second,
        }));
        self.next_split = next;
        self.zoomed = false;
        // They had focus elsewhere, in this order, after the panes here.
        self.history.extend(other.history.iter().copied());
        if let Some(focus) = other.focus {
            self.focus_on(focus);
        }
        Ok(())
    }

    /// Puts `new` in the place of `old`, keeping the shape, the ratios and
    /// the zoom. If `old` had focus, `new` has it now.
    pub fn replace(&mut self, old: PaneId, new: PaneId) -> Result<(), LayoutError> {
        if !self.contains(old) {
            return Err(LayoutError::NotFound(old));
        }
        if old == new {
            return Ok(());
        }
        if self.contains(new) {
            return Err(LayoutError::AlreadyPresent(new));
        }
        if let Some(root) = self.root.as_mut() {
            root.for_each_pane_mut(&mut |p| {
                if *p == old {
                    *p = new;
                }
            });
        }
        if self.focus == Some(old) {
            self.focus = Some(new);
        }
        for p in &mut self.history {
            if *p == old {
                *p = new;
            }
        }
        Ok(())
    }

    /// Moves `source` to `zone` of `target`, as for a dropped pane, and
    /// focuses it. [`Zone::Center`] swaps the two panes; an edge zone takes
    /// the source out (its sibling fills the gap) and puts it on that side of
    /// the target, half and half. Dropping a pane on itself changes nothing.
    pub fn move_pane(
        &mut self,
        source: PaneId,
        target: PaneId,
        zone: Zone,
    ) -> Result<(), LayoutError> {
        for pane in [source, target] {
            if !self.contains(pane) {
                return Err(LayoutError::NotFound(pane));
            }
        }
        if source == target {
            return Ok(());
        }
        // Work on a copy so a refused insertion leaves this layout untouched.
        let mut moved = self.clone();
        match zone.side() {
            None => {
                if let Some(root) = moved.root.as_mut() {
                    root.for_each_pane_mut(&mut |p| {
                        if *p == source {
                            *p = target;
                        } else if *p == target {
                            *p = source;
                        }
                    });
                }
            }
            Some(side) => {
                moved.close(source)?;
                moved.add_beside(target, side, source)?;
            }
        }
        // The same panes remain, so the old focus order still holds; the
        // focus that passed through a sibling on the way does not count.
        moved.history = std::mem::take(&mut self.history);
        moved.zoomed = false;
        moved.focus_on(source);
        *self = moved;
        Ok(())
    }

    /// Every visible pane with its rectangle, in visual order. While zoomed
    /// that is the focused pane alone, filling the area.
    pub fn rects(&self, area: Rect, divider_px: i32) -> Vec<(PaneId, Rect)> {
        match self.focus {
            Some(focus) if self.zoomed => vec![(focus, normalized(area))],
            _ => self.tree_rects(area, divider_px),
        }
    }

    /// Every visible divider, each split before the splits inside it. None
    /// while zoomed.
    pub fn dividers(&self, area: Rect, divider_px: i32) -> Vec<Divider> {
        let mut out = Vec::new();
        if !self.zoomed {
            self.visit(area, divider_px, |v| {
                if let Visit::Split { split, line, .. } = v {
                    out.push(Divider {
                        handle: DividerHandle(split.id),
                        axis: split.axis,
                        rect: line,
                    });
                }
            });
        }
        out
    }

    /// The visible pane under `point` and its rectangle; None over a divider
    /// or outside the area.
    pub fn pane_at(&self, area: Rect, divider_px: i32, point: Point) -> Option<(PaneId, Rect)> {
        self.rects(area, divider_px)
            .into_iter()
            .find(|(_, r)| r.contains(point))
    }

    /// The divider whose hit target holds `point`: the drawn line widened by
    /// `slop_px` on both sides. Where targets overlap, the line nearest the
    /// point wins, and on a tie the outer split.
    pub fn divider_at(
        &self,
        area: Rect,
        divider_px: i32,
        point: Point,
        slop_px: i32,
    ) -> Option<Divider> {
        let slop = i64::from(slop_px.max(0));
        let mut best: Option<(i64, Divider)> = None;
        for div in self.dividers(area, divider_px) {
            let r = div.rect;
            let (along, across) = match div.axis {
                Axis::Horizontal => ((point.y, r.y, r.bottom()), (point.x, r.x, r.right())),
                Axis::Vertical => ((point.x, r.x, r.right()), (point.y, r.y, r.bottom())),
            };
            if along.0 < along.1 || along.0 >= along.2 {
                continue;
            }
            let distance = pixel_distance(across.0, across.1, across.2);
            if distance <= slop && best.is_none_or(|(d, _)| distance < d) {
                best = Some((distance, div));
            }
        }
        best.map(|(_, div)| div)
    }

    /// Moves the divider so its line is centered on `pointer`, as far as it
    /// can go while every pane on either side stays at least `min_pane_px`
    /// long across the line (the panes inside each side keep their ratios). A
    /// side already below the minimum may grow but not shrink. Returns whether
    /// anything moved; a stale handle, or a zoomed layout, changes nothing.
    pub fn drag_divider(
        &mut self,
        handle: DividerHandle,
        area: Rect,
        divider_px: i32,
        pointer: Point,
        min_pane_px: i32,
    ) -> bool {
        if self.zoomed {
            return false;
        }
        let Some(track) = self.track(handle.0, area, divider_px, min_pane_px) else {
            return false;
        };
        let target = i64::from(track.axis.coord(pointer))
            - i64::from(track.start)
            - i64::from(track.line / 2);
        self.move_divider(&track, target)
    }

    /// Moves the divider on the `direction` side of `pane` by `amount_px` in
    /// that direction (a negative amount moves it the other way), so a
    /// positive amount grows the pane. The nearest enclosing split with a
    /// divider on that side is the one that moves. If the pane has none on
    /// that side, its nearest divider on the opposite side moves the same way
    /// instead, which shrinks the pane: the command always pushes an edge in
    /// the direction asked. Limits are those of [`drag_divider`]. Returns
    /// whether anything moved.
    ///
    /// [`drag_divider`]: Layout::drag_divider
    pub fn resize(
        &mut self,
        pane: PaneId,
        direction: Direction,
        amount_px: i32,
        area: Rect,
        divider_px: i32,
        min_pane_px: i32,
    ) -> bool {
        if self.zoomed {
            return false;
        }
        let Some(id) = self.resize_split(pane, direction) else {
            return false;
        };
        let Some(track) = self.track(id, area, divider_px, min_pane_px) else {
            return false;
        };
        let step = i64::from(amount_px);
        let target = i64::from(track.current) + if direction.is_forward() { step } else { -step };
        self.move_divider(&track, target)
    }

    /// Makes the units of every run of same-axis splits equally long in
    /// `area`: three panes side by side become thirds, not a half and two
    /// quarters. Lengths are equal to the pixel (they differ by at most one)
    /// when the area has room for every divider; in a smaller area the ratios
    /// just count units. In another area the shares stay proportional.
    pub fn equalize(&mut self, area: Rect, divider_px: i32) {
        if let Some(root) = self.root.as_mut() {
            tree::equalize(root, normalized(area), divider_px);
        }
    }

    fn focus_on(&mut self, pane: PaneId) {
        self.focus = Some(pane);
        self.history.retain(|&p| p != pane);
        self.history.push(pane);
    }

    /// How recently the pane had focus: larger is more recent.
    fn recency(&self, pane: PaneId) -> Option<usize> {
        self.history.iter().position(|&p| p == pane)
    }

    fn cycle_focus(&mut self, forward: bool) -> Option<PaneId> {
        let panes = self.panes();
        let at = panes.iter().position(|&p| Some(p) == self.focus)?;
        let n = panes.len();
        let next = panes[if forward {
            (at + 1) % n
        } else {
            (at + n - 1) % n
        }];
        self.focus_on(next);
        Some(next)
    }

    fn add_beside(
        &mut self,
        target: PaneId,
        side: Direction,
        pane: PaneId,
    ) -> Result<(), LayoutError> {
        let id = self.next_split;
        let root = self.root.as_mut().ok_or(LayoutError::NotFound(target))?;
        let depth = root.depth_of(target).ok_or(LayoutError::NotFound(target))?;
        if root.contains(pane) {
            return Err(LayoutError::AlreadyPresent(pane));
        }
        if depth >= MAX_DEPTH {
            return Err(LayoutError::TooDeep);
        }
        let leaf = root.leaf_mut(target).ok_or(LayoutError::NotFound(target))?;
        *leaf = Node::Split(Box::new(Split::beside(id, target, side, pane)));
        self.next_split += 1;
        self.zoomed = false;
        self.focus_on(pane);
        Ok(())
    }

    /// Walks the whole tree, ignoring zoom.
    fn visit<'a>(&'a self, area: Rect, divider_px: i32, mut f: impl FnMut(Visit<'a>)) {
        if let Some(root) = &self.root {
            tree::visit(root, normalized(area), divider_px, &mut f);
        }
    }

    /// Every pane's rectangle as if unzoomed.
    fn tree_rects(&self, area: Rect, divider_px: i32) -> Vec<(PaneId, Rect)> {
        let mut out = Vec::new();
        self.visit(area, divider_px, |v| {
            if let Visit::Pane(p, r) = v {
                out.push((p, r));
            }
        });
        out
    }

    /// The split whose divider [`resize`](Layout::resize) moves.
    fn resize_split(&self, pane: PaneId, direction: Direction) -> Option<u64> {
        let mut path = Vec::new();
        if !self.root.as_ref()?.path_to(pane, &mut path) {
            return None;
        }
        let axis = direction.axis();
        // A split's divider is on the pane's right (or bottom) side exactly
        // when the pane lies in its first child.
        let on_side = path
            .iter()
            .rev()
            .find(|(s, in_first)| s.axis == axis && *in_first == direction.is_forward());
        let any_side = || path.iter().rev().find(|(s, _)| s.axis == axis);
        on_side.or_else(any_side).map(|(s, _)| s.id)
    }

    fn track(&self, id: u64, area: Rect, divider_px: i32, min_pane_px: i32) -> Option<Track> {
        let mut found = None;
        self.visit(area, divider_px, |v| match v {
            Visit::Split { split, rect, .. } if split.id == id => found = Some((split, rect)),
            _ => {}
        });
        let (split, rect) = found?;
        let axis = split.axis;
        let (current, line, second) =
            tree::split_lengths(axis.extent(rect), split.ratio, divider_px);
        let avail = current + second;
        let min = min_pane_px.max(0);
        let lowest = tree::min_extent(&split.first, axis, min, avail, divider_px)
            .unwrap_or(avail.saturating_add(1));
        let highest =
            tree::min_extent(&split.second, axis, min, avail, divider_px).map_or(-1, |e| avail - e);
        Some(Track {
            split: id,
            axis,
            start: axis.start(rect),
            line,
            avail,
            current,
            lowest,
            highest,
        })
    }

    /// Moves a divider so the first child is `target` long, as far as the
    /// limits allow. Returns whether it moved.
    fn move_divider(&mut self, track: &Track, target: i64) -> bool {
        // A side already below the minimum may grow but not shrink further.
        let lower = track.lowest.min(track.current);
        let upper = track.highest.max(track.current);
        // Both bounds lie in 0..=avail, so the result fits in an i32.
        let length = target.clamp(i64::from(lower), i64::from(upper)) as i32;
        if length == track.current {
            return false;
        }
        let Some(split) = self.root.as_mut().and_then(|r| r.split_mut(track.split)) else {
            return false;
        };
        // avail is at least 1 here: two different lengths fit in 0..=avail.
        // The quotient times avail rounds back to exactly this length.
        split.ratio = f64::from(length) / f64::from(track.avail);
        true
    }
}

/// How long the two rectangles' spans along `axis` overlap; zero or less
/// when they do not.
fn span_overlap(a: Rect, b: Rect, axis: Axis) -> i32 {
    let (a0, b0) = (axis.start(a), axis.start(b));
    (a0 + axis.extent(a)).min(b0 + axis.extent(b)) - a0.max(b0)
}

/// How many pixels the pixel at `v` lies outside the span `lo..hi`. The
/// point comes from the caller and may be anywhere, so the difference is taken
/// in i64 where it cannot overflow.
fn pixel_distance(v: i32, lo: i32, hi: i32) -> i64 {
    let (v, lo, hi) = (i64::from(v), i64::from(lo), i64::from(hi));
    if v < lo {
        lo - v
    } else if v >= hi {
        v - hi + 1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(0, 0, 1001, 601);

    fn parse(text: &str) -> Layout {
        text.parse().unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    fn widths(layout: &Layout, area: Rect, divider: i32) -> Vec<i32> {
        layout
            .rects(area, divider)
            .iter()
            .map(|(_, r)| r.w)
            .collect()
    }

    fn heights(layout: &Layout, area: Rect, divider: i32) -> Vec<i32> {
        layout
            .rects(area, divider)
            .iter()
            .map(|(_, r)| r.h)
            .collect()
    }

    fn handles(layout: &Layout) -> Vec<DividerHandle> {
        layout.dividers(AREA, 1).iter().map(|d| d.handle).collect()
    }

    #[test]
    fn a_new_layout_is_one_focused_pane() {
        let layout = Layout::new(7);
        assert_eq!(layout.panes(), vec![7]);
        assert_eq!(layout.len(), 1);
        assert!(!layout.is_empty());
        assert!(layout.contains(7) && !layout.contains(8));
        assert_eq!(layout.focused(), Some(7));
        assert!(!layout.is_zoomed());
        assert_eq!(layout.rects(AREA, 1), vec![(7, AREA)]);
        assert!(layout.dividers(AREA, 1).is_empty());
    }

    #[test]
    fn split_puts_the_new_pane_on_the_given_side() {
        let area = Rect::new(10, 20, 101, 51);
        let cases = [
            (
                Direction::Left,
                "h0.5(*2,1)",
                [
                    (2, Rect::new(10, 20, 50, 51)),
                    (1, Rect::new(61, 20, 50, 51)),
                ],
            ),
            (
                Direction::Right,
                "h0.5(1,*2)",
                [
                    (1, Rect::new(10, 20, 50, 51)),
                    (2, Rect::new(61, 20, 50, 51)),
                ],
            ),
            (
                Direction::Up,
                "v0.5(*2,1)",
                [
                    (2, Rect::new(10, 20, 101, 25)),
                    (1, Rect::new(10, 46, 101, 25)),
                ],
            ),
            (
                Direction::Down,
                "v0.5(1,*2)",
                [
                    (1, Rect::new(10, 20, 101, 25)),
                    (2, Rect::new(10, 46, 101, 25)),
                ],
            ),
        ];
        for (direction, text, rects) in cases {
            let mut layout = Layout::new(1);
            layout.split(1, direction, 2).unwrap();
            assert_eq!(layout.to_string(), text);
            assert_eq!(layout.focused(), Some(2));
            assert_eq!(layout.rects(area, 1), rects.to_vec());
        }
    }

    #[test]
    fn split_refuses_bad_arguments_and_changes_nothing() {
        let mut layout = parse("h0.5(*1,2)");
        let before = layout.clone();
        assert_eq!(
            layout.split(9, Direction::Right, 3),
            Err(LayoutError::NotFound(9))
        );
        assert_eq!(
            layout.split(1, Direction::Right, 2),
            Err(LayoutError::AlreadyPresent(2))
        );
        assert_eq!(
            layout.split(1, Direction::Right, 1),
            Err(LayoutError::AlreadyPresent(1))
        );
        assert_eq!(layout, before);
        assert_eq!(layout.history, before.history);
        let mut empty = Layout::new(1);
        empty.close(1).unwrap();
        assert_eq!(
            empty.split(1, Direction::Right, 2),
            Err(LayoutError::NotFound(1))
        );
    }

    #[test]
    fn splits_nest_up_to_the_bound() {
        let mut layout = Layout::new(0);
        for i in 1..=MAX_DEPTH as PaneId {
            layout.split(i - 1, Direction::Right, i).unwrap();
        }
        let deepest = MAX_DEPTH as PaneId;
        let before = layout.clone();
        assert_eq!(
            layout.split(deepest, Direction::Down, deepest + 1),
            Err(LayoutError::TooDeep)
        );
        assert_eq!(layout, before);
        layout.split(0, Direction::Down, deepest + 1).unwrap();
        assert_eq!(layout.to_string().parse::<Layout>(), Ok(layout));
    }

    #[test]
    fn closing_gives_the_space_to_the_sibling() {
        let mut layout = parse("h0.3(*1,v0.5(2,3))");
        assert_eq!(layout.close(1), Ok(Closed::Focus(2)));
        assert_eq!(layout.to_string(), "v0.5(*2,3)");
        assert_eq!(
            layout.rects(AREA, 1),
            vec![
                (2, Rect::new(0, 0, 1001, 300)),
                (3, Rect::new(0, 301, 1001, 300))
            ]
        );
    }

    #[test]
    fn closing_the_focused_pane_focuses_the_nearest_pane() {
        // Pane 2 borders the gap; pane 3, though focused more recently, does not.
        let mut layout = parse("h0.5(*1,h0.5(2,3))");
        layout.set_focus(3);
        layout.set_focus(1);
        assert_eq!(layout.close(1), Ok(Closed::Focus(2)));

        let mut layout = parse("h0.5(h0.5(1,2),*3)");
        layout.set_focus(1);
        layout.set_focus(3);
        assert_eq!(layout.close(3), Ok(Closed::Focus(2)));

        // Both panes of a stack border the gap: the most recently focused
        // wins, and without history the first.
        let mut layout = parse("h0.5(*1,v0.5(2,3))");
        assert_eq!(layout.clone().close(1), Ok(Closed::Focus(2)));
        layout.set_focus(3);
        layout.set_focus(1);
        assert_eq!(layout.close(1), Ok(Closed::Focus(3)));
        assert_eq!(layout.focused(), Some(3));

        let mut layout = parse("v0.5(h0.5(1,2),*3)");
        layout.set_focus(2);
        layout.set_focus(3);
        assert_eq!(layout.close(3), Ok(Closed::Focus(2)));
    }

    #[test]
    fn closing_another_pane_keeps_the_focus() {
        let mut layout = parse("h0.5(*1,v0.5(2,3))");
        assert_eq!(layout.close(3), Ok(Closed::Focus(1)));
        assert_eq!(layout.to_string(), "h0.5(*1,2)");
    }

    #[test]
    fn closing_the_last_pane_empties_the_layout() {
        let mut layout = Layout::new(1);
        assert_eq!(layout.close(2), Err(LayoutError::NotFound(2)));
        assert_eq!(layout.close(1), Ok(Closed::Empty));
        assert!(layout.is_empty());
        assert_eq!(layout.len(), 0);
        assert_eq!(layout.focused(), None);
        assert!(layout.panes().is_empty());
        assert!(layout.rects(AREA, 1).is_empty());
        assert!(layout.dividers(AREA, 1).is_empty());
        assert_eq!(layout.pane_at(AREA, 1, Point { x: 1, y: 1 }), None);
        assert_eq!(layout.close(1), Err(LayoutError::NotFound(1)));
        assert_eq!(layout.focus_next(), None);
        assert_eq!(layout.focus_direction(Direction::Left, AREA, 1), None);
        assert!(!layout.toggle_zoom());
        assert!(!layout.set_focus(1));
        layout.equalize(AREA, 1);
        assert!(layout.is_empty());
    }

    #[test]
    fn take_reports_whether_panes_remain() {
        let mut layout = parse("h0.5(*1,2)");
        assert_eq!(layout.take(1), Ok(true));
        assert_eq!(layout.focused(), Some(2));
        assert_eq!(layout.take(1), Err(LayoutError::NotFound(1)));
        assert_eq!(layout.take(2), Ok(false));
        assert!(layout.is_empty());
    }

    #[test]
    fn panes_come_in_visual_order() {
        let layout = parse("v0.5(h0.5(1,v0.5(2,3)),h0.5(*4,5))");
        assert_eq!(layout.panes(), vec![1, 2, 3, 4, 5]);
        let order: Vec<PaneId> = layout.rects(AREA, 1).iter().map(|r| r.0).collect();
        assert_eq!(order, layout.panes());
    }

    #[test]
    fn geometry_tiles_the_area_with_dividers() {
        let layout = parse("h0.5(1,v0.5(*2,3))");
        assert_eq!(
            layout.rects(AREA, 1),
            vec![
                (1, Rect::new(0, 0, 500, 601)),
                (2, Rect::new(501, 0, 500, 300)),
                (3, Rect::new(501, 301, 500, 300)),
            ]
        );
        let lines: Vec<(Axis, Rect)> = layout
            .dividers(AREA, 1)
            .iter()
            .map(|d| (d.axis, d.rect))
            .collect();
        assert_eq!(
            lines,
            vec![
                (Axis::Horizontal, Rect::new(500, 0, 1, 601)),
                (Axis::Vertical, Rect::new(501, 300, 500, 1)),
            ]
        );
    }

    #[test]
    fn small_and_degenerate_areas_still_tile() {
        let layout = parse("h0.5(1,*2)");
        // Two pixels: the first pane gets the rounding, the divider the rest.
        assert_eq!(
            layout.rects(Rect::new(0, 0, 2, 5), 1),
            vec![(1, Rect::new(0, 0, 1, 5)), (2, Rect::new(2, 0, 0, 5))]
        );
        // A divider wider than the area takes all of it.
        assert_eq!(
            layout.rects(Rect::new(0, 0, 1, 5), 4),
            vec![(1, Rect::new(0, 0, 0, 5)), (2, Rect::new(1, 0, 0, 5))]
        );
        assert_eq!(
            layout.dividers(Rect::new(0, 0, 1, 5), 4)[0].rect,
            Rect::new(0, 0, 1, 5)
        );
        // Negative sizes count as zero, and a negative divider as none.
        assert_eq!(
            layout.rects(Rect::new(3, 4, -10, -2), 1),
            vec![(1, Rect::new(3, 4, 0, 0)), (2, Rect::new(3, 4, 0, 0))]
        );
        assert_eq!(
            layout.rects(Rect::new(0, 0, 10, 1), -3),
            vec![(1, Rect::new(0, 0, 5, 1)), (2, Rect::new(5, 0, 5, 1))]
        );
    }

    #[test]
    fn divider_handles_are_stable() {
        let mut layout = parse("h0.5(1,v0.5(*2,3))");
        let first = handles(&layout);
        assert_ne!(first[0], first[1]);
        assert!(layout.drag_divider(first[1], AREA, 1, Point { x: 700, y: 100 }, 0));
        layout.split(1, Direction::Down, 4).unwrap();
        let now = handles(&layout);
        assert_eq!(now.len(), 3);
        assert_eq!((now[0], now[2]), (first[0], first[1]));
        // Closing pane 3 removes the split it shared with pane 2, and the handle with it.
        layout.close(3).unwrap();
        assert!(!handles(&layout).contains(&first[1]));
        assert!(!layout.drag_divider(first[1], AREA, 1, Point { x: 700, y: 200 }, 0));
        assert!(layout.drag_divider(first[0], AREA, 1, Point { x: 300, y: 200 }, 0));
    }

    #[test]
    fn pane_at_finds_panes_but_not_dividers() {
        let layout = parse("h0.5(1,v0.5(*2,3))");
        let at = |x, y| layout.pane_at(AREA, 1, Point { x, y }).map(|(p, _)| p);
        assert_eq!(at(0, 0), Some(1));
        assert_eq!(at(499, 600), Some(1));
        assert_eq!(at(500, 10), None);
        assert_eq!(at(501, 299), Some(2));
        assert_eq!(at(501, 300), None);
        assert_eq!(at(1000, 301), Some(3));
        assert_eq!(at(-1, 0), None);
        assert_eq!(at(1001, 0), None);
        assert_eq!(at(0, 601), None);
        assert_eq!(
            layout.pane_at(AREA, 1, Point { x: 600, y: 400 }),
            Some((3, Rect::new(501, 301, 500, 300)))
        );
    }

    #[test]
    fn divider_hit_targets_are_widened_by_the_slop() {
        let layout = parse("h0.5(1,v0.5(*2,3))");
        let dividers = layout.dividers(AREA, 1);
        let (outer, inner) = (dividers[0], dividers[1]);
        let at = |x, y| layout.divider_at(AREA, 1, Point { x, y }, 3);
        assert_eq!(at(500, 10), Some(outer));
        assert_eq!(at(497, 10), Some(outer));
        assert_eq!(at(503, 10), Some(outer));
        assert_eq!(at(496, 10), None);
        assert_eq!(at(504, 10), None);
        assert_eq!(at(700, 297), Some(inner));
        assert_eq!(at(700, 303), Some(inner));
        assert_eq!(at(700, 304), None);
        // Where the targets meet, the nearer line wins.
        assert_eq!(at(502, 299), Some(inner));
        assert_eq!(at(501, 250), Some(outer));
        assert_eq!(at(500, 300), Some(outer));
        // Without slop only the drawn line counts.
        assert_eq!(layout.divider_at(AREA, 1, Point { x: 501, y: 10 }, 0), None);
        assert_eq!(
            layout.divider_at(AREA, 1, Point { x: 500, y: 10 }, 0),
            Some(outer)
        );
        // A zero-width divider is still a target.
        let hit = layout.divider_at(AREA, 0, Point { x: 499, y: 10 }, 2);
        assert_eq!(hit.map(|d| d.handle), Some(outer.handle));
    }

    #[test]
    fn divider_hit_testing_survives_points_far_outside() {
        let layout = parse("h0.5(1,v0.5(*2,3))");
        let far = [
            Point { x: i32::MIN, y: 10 },
            Point { x: i32::MAX, y: 10 },
            Point {
                x: 700,
                y: i32::MIN,
            },
            Point {
                x: 700,
                y: i32::MAX,
            },
        ];
        for point in far {
            assert_eq!(layout.divider_at(AREA, 1, point, 3), None, "{point:?}");
            assert_eq!(layout.pane_at(AREA, 1, point), None, "{point:?}");
        }
        // Lines left of and above the origin, with the point beyond the far side.
        let area = Rect::new(-3000, -3000, 1001, 601);
        let beyond = [
            Point {
                x: i32::MAX,
                y: -2990,
            },
            Point {
                x: -2300,
                y: i32::MAX,
            },
        ];
        for point in beyond {
            assert_eq!(layout.divider_at(area, 1, point, 3), None, "{point:?}");
        }
        // The widest slop reaches every pixel of the area, but not a point
        // further away than any i32 slop.
        let outer = layout.dividers(AREA, 1)[0];
        assert_eq!(
            layout.divider_at(AREA, 1, Point { x: 0, y: 10 }, i32::MAX),
            Some(outer)
        );
        assert_eq!(
            layout.divider_at(AREA, 1, Point { x: i32::MIN, y: 10 }, i32::MAX),
            None
        );
    }

    #[test]
    fn dragging_centers_the_divider_on_the_pointer() {
        let mut layout = parse("h0.5(*1,2)");
        let handle = handles(&layout)[0];
        assert!(layout.drag_divider(handle, AREA, 1, Point { x: 300, y: 50 }, 0));
        assert_eq!(
            layout.rects(AREA, 1),
            vec![
                (1, Rect::new(0, 0, 300, 601)),
                (2, Rect::new(301, 0, 700, 601))
            ]
        );
        assert!(!layout.drag_divider(handle, AREA, 1, Point { x: 300, y: 400 }, 0));

        // A four-pixel divider straddles the pointer.
        let area = Rect::new(100, 0, 1004, 50);
        assert!(layout.drag_divider(handle, area, 4, Point { x: 400, y: 0 }, 0));
        assert_eq!(layout.dividers(area, 4)[0].rect, Rect::new(398, 0, 4, 50));

        let mut stacked = parse("v0.5(*1,2)");
        let handle = handles(&stacked)[0];
        assert!(stacked.drag_divider(handle, AREA, 1, Point { x: 5, y: 200 }, 0));
        assert_eq!(heights(&stacked, AREA, 1), vec![200, 400]);
    }

    #[test]
    fn dragging_stops_at_the_minimum_pane_size() {
        let mut layout = parse("h0.5(*1,2)");
        let handle = handles(&layout)[0];
        assert!(layout.drag_divider(handle, AREA, 1, Point { x: 50, y: 0 }, 100));
        assert_eq!(widths(&layout, AREA, 1), vec![100, 900]);
        assert!(layout.drag_divider(handle, AREA, 1, Point { x: 990, y: 0 }, 100));
        assert_eq!(widths(&layout, AREA, 1), vec![900, 100]);
        // Past either end of the area.
        assert!(layout.drag_divider(handle, AREA, 1, Point { x: -5000, y: 0 }, 0));
        assert_eq!(widths(&layout, AREA, 1), vec![0, 1000]);
        assert!(layout.drag_divider(handle, AREA, 1, Point { x: i32::MAX, y: 0 }, 0));
        assert_eq!(widths(&layout, AREA, 1), vec![1000, 0]);
    }

    #[test]
    fn dragging_keeps_inner_ratios_and_stops_when_their_smallest_pane_reaches_the_minimum() {
        let mut layout = parse("h0.5(*1,h0.2(2,3))");
        let handle = handles(&layout)[0];
        assert!(layout.drag_divider(handle, AREA, 1, Point { x: 990, y: 0 }, 50));
        // Pane 2 keeps a fifth of its side, so it reaches 50 px first.
        assert_eq!(widths(&layout, AREA, 1), vec![751, 50, 198]);
    }

    #[test]
    fn a_side_below_the_minimum_can_grow_but_not_shrink() {
        let mut layout = parse("h0.1(*1,2)");
        let handle = handles(&layout)[0];
        assert_eq!(widths(&layout, AREA, 1), vec![100, 900]);
        assert!(!layout.drag_divider(handle, AREA, 1, Point { x: 50, y: 0 }, 200));
        assert!(layout.drag_divider(handle, AREA, 1, Point { x: 150, y: 0 }, 200));
        assert_eq!(widths(&layout, AREA, 1), vec![150, 850]);
        // Too narrow for two panes of the minimum: nothing moves either way.
        let narrow = Rect::new(0, 0, 101, 10);
        layout.equalize(narrow, 1);
        assert_eq!(widths(&layout, narrow, 1), vec![50, 50]);
        assert!(!layout.drag_divider(handle, narrow, 1, Point { x: 10, y: 0 }, 100));
        assert!(!layout.drag_divider(handle, narrow, 1, Point { x: 90, y: 0 }, 100));
        assert!(layout.drag_divider(handle, narrow, 1, Point { x: 90, y: 0 }, 0));
    }

    #[test]
    fn dragging_while_zoomed_or_with_a_stale_handle_does_nothing() {
        let mut layout = parse("h0.5(*1,v0.5(2,3))");
        let first = handles(&layout);
        layout.toggle_zoom();
        let before = layout.clone();
        assert!(!layout.drag_divider(first[0], AREA, 1, Point { x: 100, y: 0 }, 0));
        assert!(!layout.resize(1, Direction::Right, 10, AREA, 1, 0));
        assert_eq!(layout, before);
        layout.toggle_zoom();
        layout.close(3).unwrap();
        assert!(!layout.drag_divider(first[1], AREA, 1, Point { x: 100, y: 100 }, 0));
    }

    #[test]
    fn resize_moves_the_divider_on_that_side() {
        let mut layout = parse("h0.5(*1,2)");
        assert!(layout.resize(1, Direction::Right, 10, AREA, 1, 0));
        assert_eq!(widths(&layout, AREA, 1), vec![510, 490]);
        assert!(layout.resize(2, Direction::Left, 30, AREA, 1, 0));
        assert_eq!(widths(&layout, AREA, 1), vec![480, 520]);
        assert!(layout.resize(2, Direction::Left, -30, AREA, 1, 0));
        assert_eq!(widths(&layout, AREA, 1), vec![510, 490]);
        // No divider on that side: the one on the other side moves that way.
        assert!(layout.resize(1, Direction::Left, 10, AREA, 1, 0));
        assert_eq!(widths(&layout, AREA, 1), vec![500, 500]);
        assert!(layout.resize(2, Direction::Right, 20, AREA, 1, 0));
        assert_eq!(widths(&layout, AREA, 1), vec![520, 480]);
        // No divider along that axis at all, or no such pane.
        assert!(!layout.resize(1, Direction::Down, 10, AREA, 1, 0));
        assert!(!layout.resize(9, Direction::Right, 10, AREA, 1, 0));
        assert!(!layout.resize(1, Direction::Right, 0, AREA, 1, 0));
    }

    #[test]
    fn resize_picks_the_nearest_divider_on_that_side() {
        let mut layout = parse("h0.5(h0.5(1,*2),3)");
        assert_eq!(widths(&layout, AREA, 1), vec![250, 249, 500]);
        assert!(layout.resize(2, Direction::Right, 20, AREA, 1, 0));
        assert_eq!(widths(&layout, AREA, 1), vec![260, 259, 480]);
        assert!(layout.resize(2, Direction::Left, 20, AREA, 1, 0));
        assert_eq!(widths(&layout, AREA, 1), vec![240, 279, 480]);

        let mut layout = parse("h0.5(1,v0.5(*2,3))");
        assert!(layout.resize(2, Direction::Down, 15, AREA, 1, 0));
        assert_eq!(heights(&layout, AREA, 1), vec![601, 315, 285]);
        assert!(layout.resize(3, Direction::Up, 15, AREA, 1, 0));
        assert_eq!(heights(&layout, AREA, 1), vec![601, 300, 300]);
        assert!(layout.resize(2, Direction::Left, 15, AREA, 1, 0));
        assert_eq!(widths(&layout, AREA, 1), vec![485, 515, 515]);
    }

    #[test]
    fn resize_respects_the_minimum_and_survives_huge_amounts() {
        let mut layout = parse("h0.5(*1,2)");
        assert!(layout.resize(1, Direction::Right, i32::MAX, AREA, 1, 100));
        assert_eq!(widths(&layout, AREA, 1), vec![900, 100]);
        assert!(!layout.resize(1, Direction::Right, 1, AREA, 1, 100));
        assert!(layout.resize(1, Direction::Right, i32::MIN, AREA, 1, 100));
        assert_eq!(widths(&layout, AREA, 1), vec![100, 900]);
        assert!(layout.resize(2, Direction::Left, i32::MIN, AREA, 1, 100));
        assert_eq!(widths(&layout, AREA, 1), vec![900, 100]);
    }

    #[test]
    fn equalize_makes_runs_even() {
        let area = Rect::new(0, 0, 1000, 600);
        for text in ["h0.5(1,h0.5(*2,3))", "h0.9(h0.1(1,*2),3)"] {
            let mut layout = parse(text);
            layout.equalize(area, 1);
            assert_eq!(widths(&layout, area, 1), vec![333, 333, 332], "{text}");
            layout.equalize(area, 0);
            assert_eq!(widths(&layout, area, 0), vec![334, 333, 333], "{text}");
        }

        let mut layout = parse("v0.2(h0.7(1,2),h0.5(3,h0.1(4,*5)))");
        let area = Rect::new(0, 0, 1201, 801);
        layout.equalize(area, 1);
        let sizes: Vec<(i32, i32)> = layout
            .rects(area, 1)
            .iter()
            .map(|(_, r)| (r.w, r.h))
            .collect();
        assert_eq!(
            sizes,
            vec![(600, 400), (600, 400), (400, 400), (400, 400), (399, 400)]
        );
    }

    #[test]
    fn equalize_in_a_tiny_area_counts_units() {
        let mut layout = parse("h0.5(1,h0.9(*2,3))");
        layout.equalize(Rect::new(0, 0, 3, 3), 2);
        assert_eq!(layout.to_string(), format!("h{}(1,h0.5(*2,3))", 1.0 / 3.0));
    }

    #[test]
    fn focus_moves_to_the_adjacent_pane() {
        let mut layout = parse("v0.5(h0.5(*1,2),h0.5(3,4))");
        let mut go = |direction| layout.focus_direction(direction, AREA, 1);
        assert_eq!(go(Direction::Left), None);
        assert_eq!(go(Direction::Up), None);
        assert_eq!(go(Direction::Right), Some(2));
        assert_eq!(go(Direction::Right), None);
        assert_eq!(go(Direction::Down), Some(4));
        assert_eq!(go(Direction::Left), Some(3));
        assert_eq!(go(Direction::Up), Some(1));
    }

    #[test]
    fn focus_prefers_the_longest_shared_edge() {
        let mut layout = parse("h0.5(v0.7(1,2),*3)");
        assert_eq!(layout.focus_direction(Direction::Left, AREA, 1), Some(1));
        let mut layout = parse("h0.5(v0.3(1,2),*3)");
        assert_eq!(layout.focus_direction(Direction::Left, AREA, 1), Some(2));
    }

    #[test]
    fn focus_ties_go_to_the_center_line_then_the_most_recent() {
        // With no divider, pane 3's center line runs through pane 2.
        let mut layout = parse("h0.5(v0.5(1,2),*3)");
        assert_eq!(
            layout.focus_direction(Direction::Left, Rect::new(0, 0, 1000, 600), 0),
            Some(2)
        );
        // With one, it runs along the divider: the most recently focused
        // wins, and without history the first.
        let mut layout = parse("h0.5(v0.5(1,2),*3)");
        assert_eq!(
            layout.clone().focus_direction(Direction::Left, AREA, 1),
            Some(1)
        );
        layout.set_focus(2);
        layout.set_focus(3);
        assert_eq!(layout.focus_direction(Direction::Left, AREA, 1), Some(2));
    }

    #[test]
    fn focus_order_wraps_around() {
        let mut layout = parse("h0.5(*1,v0.5(2,3))");
        assert_eq!(layout.focus_next(), Some(2));
        assert_eq!(layout.focus_next(), Some(3));
        assert_eq!(layout.focus_next(), Some(1));
        assert_eq!(layout.focus_previous(), Some(3));
        assert_eq!(layout.focused(), Some(3));
        assert_eq!(Layout::new(5).focus_next(), Some(5));
    }

    #[test]
    fn zoom_shows_only_the_focused_pane() {
        let mut layout = Layout::new(1);
        assert!(!layout.toggle_zoom());
        layout.split(1, Direction::Right, 2).unwrap();
        assert!(layout.toggle_zoom());
        assert!(layout.is_zoomed());
        assert_eq!(layout.rects(AREA, 1), vec![(2, AREA)]);
        assert!(layout.dividers(AREA, 1).is_empty());
        assert_eq!(
            layout.pane_at(AREA, 1, Point { x: 500, y: 5 }),
            Some((2, AREA))
        );
        assert_eq!(layout.divider_at(AREA, 1, Point { x: 500, y: 5 }, 3), None);
        // The zoom follows focus.
        assert!(layout.set_focus(1));
        assert_eq!(layout.rects(AREA, 1), vec![(1, AREA)]);
        assert_eq!(layout.focus_direction(Direction::Right, AREA, 1), Some(2));
        assert_eq!(layout.rects(AREA, 1), vec![(2, AREA)]);
        assert!(layout.is_zoomed());
        assert!(!layout.toggle_zoom());
        assert_eq!(layout.rects(AREA, 1).len(), 2);
    }

    #[test]
    fn changing_the_panes_ends_zoom() {
        type Op = fn(&mut Layout);
        let ops: [Op; 6] = [
            |l| l.split(1, Direction::Down, 9).unwrap(),
            |l| assert_eq!(l.close(3), Ok(Closed::Focus(1))),
            |l| assert_eq!(l.take(3), Ok(true)),
            |l| l.insert(1, Zone::Top, 9).unwrap(),
            |l| l.move_pane(3, 1, Zone::Center).unwrap(),
            |l| l.move_pane(3, 1, Zone::Left).unwrap(),
        ];
        for op in ops {
            let mut layout = parse("h0.5(!1,v0.5(2,3))");
            op(&mut layout);
            assert!(!layout.is_zoomed(), "{layout}");
        }
        // Renaming a pane keeps the zoom, and so does a drop on itself.
        let mut layout = parse("h0.5(!1,v0.5(2,3))");
        layout.replace(1, 9).unwrap();
        layout.move_pane(2, 2, Zone::Left).unwrap();
        assert_eq!(layout.to_string(), "h0.5(!9,v0.5(2,3))");
    }

    #[test]
    fn dropping_a_pane_on_a_center_swaps_them() {
        let mut layout = parse("h0.3(*1,v0.5(2,3))");
        layout.move_pane(3, 1, Zone::Center).unwrap();
        assert_eq!(layout.to_string(), "h0.3(*3,v0.5(2,1))");
    }

    #[test]
    fn dropping_a_pane_on_an_edge_moves_it_there() {
        let mut layout = parse("h0.5(1,v0.5(*2,3))");
        layout.move_pane(1, 3, Zone::Bottom).unwrap();
        assert_eq!(layout.to_string(), "v0.5(2,v0.5(3,*1))");

        let mut layout = parse("h0.5(*1,2)");
        layout.move_pane(1, 2, Zone::Right).unwrap();
        assert_eq!(layout.to_string(), "h0.5(2,*1)");
        layout.move_pane(1, 2, Zone::Top).unwrap();
        assert_eq!(layout.to_string(), "v0.5(*1,2)");

        // The source leaves its old place before it lands.
        let mut layout = parse("h0.5(1,h0.5(2,*3))");
        layout.move_pane(3, 1, Zone::Left).unwrap();
        assert_eq!(layout.to_string(), "h0.5(h0.5(*3,1),2)");
    }

    #[test]
    fn dropping_a_pane_on_itself_changes_nothing() {
        let mut layout = parse("h0.3(1,v0.5(*2,3))");
        layout.set_focus(3);
        layout.set_focus(2);
        let before = layout.clone();
        let dividers = layout.dividers(AREA, 1);
        for zone in [
            Zone::Left,
            Zone::Right,
            Zone::Top,
            Zone::Bottom,
            Zone::Center,
        ] {
            layout.move_pane(3, 3, zone).unwrap();
        }
        assert_eq!(layout, before);
        assert_eq!(layout.dividers(AREA, 1), dividers);
        assert_eq!(layout.history, before.history);
    }

    #[test]
    fn moves_with_unknown_panes_are_refused() {
        let mut layout = parse("h0.5(*1,2)");
        assert_eq!(
            layout.move_pane(9, 1, Zone::Left),
            Err(LayoutError::NotFound(9))
        );
        assert_eq!(
            layout.move_pane(1, 9, Zone::Left),
            Err(LayoutError::NotFound(9))
        );
        assert_eq!(
            layout.move_pane(9, 9, Zone::Center),
            Err(LayoutError::NotFound(9))
        );
    }

    #[test]
    fn every_operation_works_at_the_deepest_nesting() {
        // Alternate sides and axes so the chain spirals inward.
        let sides = [
            Direction::Right,
            Direction::Down,
            Direction::Left,
            Direction::Up,
        ];
        let mut layout = Layout::new(0);
        for i in 1..=MAX_DEPTH as PaneId {
            layout.split(i - 1, sides[i as usize % 4], i).unwrap();
        }
        let deepest = MAX_DEPTH as PaneId;
        let area = Rect::new(0, 0, 1 << 20, 1 << 20);
        let rects = layout.rects(area, 1);
        assert_eq!(rects.len(), MAX_DEPTH + 1);
        let dividers = layout.dividers(area, 1);
        assert_eq!(dividers.len(), MAX_DEPTH);
        let covered: i64 = rects
            .iter()
            .map(|r| r.1)
            .chain(dividers.iter().map(|d| d.rect))
            .map(|r| i64::from(r.w) * i64::from(r.h))
            .sum();
        assert_eq!(covered, 1 << 40);
        // The innermost lines have shrunk to nothing; take the deepest that has not.
        let last = *dividers
            .iter()
            .rev()
            .find(|d| !d.rect.is_empty())
            .expect("a visible divider");
        let middle = Point {
            x: last.rect.x + last.rect.w / 2,
            y: last.rect.y + last.rect.h / 2,
        };
        assert_eq!(layout.divider_at(area, 1, middle, 0), Some(last));
        // Lines this deep have no room to move, but asking must be harmless.
        assert!(!layout.drag_divider(last.handle, area, 1, Point { x: 0, y: 0 }, 0));
        layout.resize(deepest, Direction::Up, 5, area, 1, 1);
        assert!(layout.drag_divider(dividers[0].handle, area, 1, Point { x: 9, y: 0 }, 2));
        assert!(layout.resize(1, Direction::Up, 5, area, 1, 0));
        layout.focus_direction(Direction::Left, area, 1);
        layout.equalize(area, 1);
        assert!(layout.toggle_zoom());
        assert_eq!(layout.rects(area, 1).len(), 1);
        let text = layout.to_string();
        assert_eq!(text.parse::<Layout>(), Ok(layout.clone()));
        layout.move_pane(deepest, 0, Zone::Center).unwrap();
        layout.move_pane(1, deepest - 1, Zone::Left).unwrap();
        assert!(matches!(layout.close(1), Ok(Closed::Focus(_))));
        assert_eq!(layout.len(), MAX_DEPTH);
    }

    #[test]
    fn a_move_that_would_nest_too_deep_changes_nothing() {
        let mut layout = Layout::new(0);
        for i in 1..=MAX_DEPTH as PaneId {
            layout.split(i - 1, Direction::Right, i).unwrap();
        }
        layout.split(0, Direction::Down, 1000).unwrap();
        let before = layout.clone();
        assert_eq!(
            layout.move_pane(1000, MAX_DEPTH as PaneId, Zone::Left),
            Err(LayoutError::TooDeep)
        );
        assert_eq!(layout, before);
        assert_eq!(layout.history, before.history);
    }

    #[test]
    fn panes_move_between_layouts() {
        let mut source = parse("h0.5(1,*2)");
        let mut dest = parse("*3");
        assert_eq!(
            dest.insert(3, Zone::Center, 2),
            Err(LayoutError::CenterZone)
        );
        assert_eq!(source.take(2), Ok(true));
        assert_eq!(source.focused(), Some(1));
        dest.insert(3, Zone::Right, 2).unwrap();
        assert_eq!(dest.to_string(), "h0.5(3,*2)");
        assert_eq!(
            dest.insert(3, Zone::Left, 2),
            Err(LayoutError::AlreadyPresent(2))
        );
        assert_eq!(dest.insert(7, Zone::Left, 8), Err(LayoutError::NotFound(7)));
        let mut empty = Layout::new(1);
        empty.take(1).unwrap();
        assert_eq!(
            empty.insert(1, Zone::Left, 2),
            Err(LayoutError::NotFound(1))
        );
    }

    #[test]
    fn a_whole_layout_moves_in_beside_a_pane() {
        // A tab of three panes dropped on the right half of pane 2.
        let tab = parse("v0.25(7,h0.75(*8,9))");
        let mut dest = parse("h0.3(*1,2)");
        dest.insert_layout(2, Zone::Right, &tab).unwrap();
        assert_eq!(dest.to_string(), "h0.3(1,h0.5(2,v0.25(7,h0.75(*8,9))))");
        assert_eq!(dest.focused(), Some(8));
        assert_eq!(dest.history, vec![1, 8]);
        // Every divider still has a handle of its own.
        let mut seen = handles(&dest);
        seen.sort_by_key(|h| h.0);
        seen.dedup();
        assert_eq!(seen.len(), 4);
        // On the leading sides the newcomers come first.
        for (zone, text) in [
            (Zone::Left, "h0.5(v0.5(5,*6),1)"),
            (Zone::Top, "v0.5(v0.5(5,*6),1)"),
            (Zone::Bottom, "v0.5(1,v0.5(5,*6))"),
        ] {
            let mut dest = Layout::new(1);
            dest.insert_layout(1, zone, &parse("v0.5(5,*6)")).unwrap();
            assert_eq!(dest.to_string(), text, "{zone:?}");
        }
        // Neither side's zoom outlives the move.
        let mut dest = parse("h0.5(1,!2)");
        dest.insert_layout(1, Zone::Left, &parse("v0.5(3,!4)"))
            .unwrap();
        assert_eq!(dest.to_string(), "h0.5(h0.5(v0.5(3,*4),1),2)");
        // Closing the newcomers leaves what was there.
        dest.close(3).unwrap();
        dest.close(4).unwrap();
        assert_eq!(dest.to_string(), "h0.5(*1,2)");
    }

    #[test]
    fn a_layout_that_cannot_move_in_changes_nothing() {
        let mut dest = parse("h0.5(*1,2)");
        let before = dest.clone();
        let tab = parse("v0.5(*3,4)");
        assert_eq!(
            dest.insert_layout(1, Zone::Center, &tab),
            Err(LayoutError::CenterZone)
        );
        assert_eq!(
            dest.insert_layout(9, Zone::Left, &tab),
            Err(LayoutError::NotFound(9))
        );
        assert_eq!(
            dest.insert_layout(1, Zone::Left, &parse("v0.5(*3,2)")),
            Err(LayoutError::AlreadyPresent(2))
        );
        let mut empty = Layout::new(5);
        empty.close(5).unwrap();
        assert_eq!(dest.insert_layout(1, Zone::Left, &empty), Ok(()));
        assert_eq!(
            empty.insert_layout(1, Zone::Left, &tab),
            Err(LayoutError::NotFound(1))
        );
        assert_eq!(dest, before);
        assert_eq!(dest.history, before.history);
        assert_eq!(handles(&dest), handles(&before));

        // Pane 255 sits under 255 splits: one more pane fits beside it, a
        // split of two does not.
        let mut deep = Layout::new(0);
        for i in 1..MAX_DEPTH as PaneId {
            deep.split(i - 1, Direction::Right, i).unwrap();
        }
        let last = MAX_DEPTH as PaneId - 1;
        let before = deep.clone();
        assert_eq!(
            deep.insert_layout(last, Zone::Left, &parse("h0.5(*1000,1001)")),
            Err(LayoutError::TooDeep)
        );
        assert_eq!(deep, before);
        deep.insert_layout(last, Zone::Left, &Layout::new(1000))
            .unwrap();
        assert_eq!(deep.len(), MAX_DEPTH + 1);
    }

    #[test]
    fn replace_swaps_panes_between_layouts() {
        let mut a = parse("h0.5(*1,2)");
        let mut b = parse("v0.25(3,*4)");
        a.replace(2, 3).unwrap();
        b.replace(3, 2).unwrap();
        assert_eq!(a.to_string(), "h0.5(*1,3)");
        assert_eq!(b.to_string(), "v0.25(2,*4)");
        a.replace(1, 9).unwrap();
        assert_eq!(a.to_string(), "h0.5(*9,3)");
        assert_eq!(a.history, vec![9]);
        assert_eq!(a.replace(5, 6), Err(LayoutError::NotFound(5)));
        assert_eq!(a.replace(9, 3), Err(LayoutError::AlreadyPresent(3)));
        assert_eq!(a.replace(9, 9), Ok(()));
    }

    #[test]
    fn equality_ignores_handles_and_history() {
        let mut built = Layout::new(1);
        built.split(1, Direction::Right, 2).unwrap();
        built.set_focus(1);
        let parsed = parse("h0.5(*1,2)");
        assert_eq!(built, parsed);
        assert_ne!(built.history, parsed.history);
        for other in [
            "h0.5(1,*2)",
            "h0.5(!1,2)",
            "v0.5(*1,2)",
            "h0.25(*1,2)",
            "h0.5(*2,1)",
        ] {
            assert_ne!(parse(other), parsed, "{other}");
        }
    }

    #[test]
    fn layouts_can_cross_threads() {
        fn shareable<T: Send + Sync>() {}
        shareable::<Layout>();
        shareable::<Divider>();
        shareable::<LayoutError>();
    }

    #[test]
    fn errors_describe_themselves() {
        assert_eq!(
            LayoutError::NotFound(4).to_string(),
            "pane 4 is not in this layout"
        );
        assert_eq!(
            LayoutError::TooDeep.to_string(),
            "a pane cannot sit under more than 256 splits"
        );
    }
}
