//! The tree itself and the arithmetic that turns it into rectangles.

use tern_types::{PaneId, Rect};

use crate::{Axis, Direction};

#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Node {
    Leaf(PaneId),
    Split(Box<Split>),
}

#[derive(Clone, Debug)]
pub(crate) struct Split {
    /// Names this split in divider handles. It is runtime identity, not part
    /// of the layout's value, so equality ignores it.
    pub id: u64,
    pub axis: Axis,
    /// The first child's share of the length left beside the divider, from 0
    /// to 1.
    pub ratio: f64,
    pub first: Node,
    pub second: Node,
}

impl PartialEq for Split {
    fn eq(&self, other: &Split) -> bool {
        self.axis == other.axis
            && self.ratio == other.ratio
            && self.first == other.first
            && self.second == other.second
    }
}

impl Split {
    /// An even split holding `existing` and `added`, with `added` on the
    /// `side` of `existing`.
    pub fn beside(id: u64, existing: PaneId, side: Direction, added: PaneId) -> Split {
        let (first, second) = if side.is_forward() {
            (existing, added)
        } else {
            (added, existing)
        };
        Split {
            id,
            axis: side.axis(),
            ratio: 0.5,
            first: Node::Leaf(first),
            second: Node::Leaf(second),
        }
    }
}

impl Node {
    pub fn is_pane(&self, pane: PaneId) -> bool {
        matches!(self, Node::Leaf(p) if *p == pane)
    }

    pub fn contains(&self, pane: PaneId) -> bool {
        match self {
            Node::Leaf(p) => *p == pane,
            Node::Split(s) => s.first.contains(pane) || s.second.contains(pane),
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Node::Leaf(_) => 1,
            Node::Split(s) => s.first.len() + s.second.len(),
        }
    }

    /// Appends the panes in visual order: depth first, first child first.
    pub fn panes(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Leaf(p) => out.push(*p),
            Node::Split(s) => {
                s.first.panes(out);
                s.second.panes(out);
            }
        }
    }

    /// The number of splits above `pane`, if it is here.
    pub fn depth_of(&self, pane: PaneId) -> Option<usize> {
        match self {
            Node::Leaf(p) => (*p == pane).then_some(0),
            Node::Split(s) => s
                .first
                .depth_of(pane)
                .or_else(|| s.second.depth_of(pane))
                .map(|d| d + 1),
        }
    }

    pub fn leaf_mut(&mut self, pane: PaneId) -> Option<&mut Node> {
        if self.is_pane(pane) {
            return Some(self);
        }
        match self {
            Node::Leaf(_) => None,
            Node::Split(s) => {
                let Split { first, second, .. } = &mut **s;
                first.leaf_mut(pane).or_else(|| second.leaf_mut(pane))
            }
        }
    }

    pub fn split_mut(&mut self, id: u64) -> Option<&mut Split> {
        let Node::Split(s) = self else {
            return None;
        };
        if s.id == id {
            return Some(s);
        }
        let Split { first, second, .. } = &mut **s;
        first.split_mut(id).or_else(|| second.split_mut(id))
    }

    /// The most splits above any one pane of this subtree.
    pub fn height(&self) -> usize {
        match self {
            Node::Leaf(_) => 0,
            Node::Split(s) => 1 + s.first.height().max(s.second.height()),
        }
    }

    pub fn for_each_split_mut(&mut self, f: &mut impl FnMut(&mut Split)) {
        if let Node::Split(s) = self {
            f(s);
            s.first.for_each_split_mut(f);
            s.second.for_each_split_mut(f);
        }
    }

    pub fn for_each_pane_mut(&mut self, f: &mut impl FnMut(&mut PaneId)) {
        match self {
            Node::Leaf(p) => f(p),
            Node::Split(s) => {
                s.first.for_each_pane_mut(f);
                s.second.for_each_pane_mut(f);
            }
        }
    }

    /// Removes the leaf `pane` from below this node, and its sibling takes
    /// the place of their parent split. Returns whether the pane was found.
    /// This node itself must not be that leaf.
    pub fn remove(&mut self, pane: PaneId) -> bool {
        let Node::Split(split) = self else {
            return false;
        };
        // The placeholder leaf is dropped together with the parent split.
        let sibling = if split.first.is_pane(pane) {
            std::mem::replace(&mut split.second, Node::Leaf(pane))
        } else if split.second.is_pane(pane) {
            std::mem::replace(&mut split.first, Node::Leaf(pane))
        } else {
            return split.first.remove(pane) || split.second.remove(pane);
        };
        *self = sibling;
        true
    }

    /// The split directly above `pane`, and whether `pane` is its first child.
    pub fn parent_of(&self, pane: PaneId) -> Option<(&Split, bool)> {
        let Node::Split(s) = self else {
            return None;
        };
        if s.first.is_pane(pane) {
            return Some((s, true));
        }
        if s.second.is_pane(pane) {
            return Some((s, false));
        }
        s.first.parent_of(pane).or_else(|| s.second.parent_of(pane))
    }

    /// Fills `path` with the splits from this node down to `pane`, each with
    /// whether `pane` lies in its first child. Returns whether it was found.
    pub fn path_to<'a>(&'a self, pane: PaneId, path: &mut Vec<(&'a Split, bool)>) -> bool {
        match self {
            Node::Leaf(p) => *p == pane,
            Node::Split(s) => {
                for (child, in_first) in [(&s.first, true), (&s.second, false)] {
                    path.push((s, in_first));
                    if child.path_to(pane, path) {
                        return true;
                    }
                    path.pop();
                }
                false
            }
        }
    }

    /// Of the panes along this subtree's leading (left or top) or trailing
    /// edge for `axis`, the one `rank` puts highest; ties go to the first in
    /// visual order.
    pub fn edge_pane(
        &self,
        axis: Axis,
        leading: bool,
        rank: &impl Fn(PaneId) -> Option<usize>,
    ) -> PaneId {
        match self {
            Node::Leaf(p) => *p,
            Node::Split(s) if s.axis == axis => {
                let child = if leading { &s.first } else { &s.second };
                child.edge_pane(axis, leading, rank)
            }
            Node::Split(s) => {
                let a = s.first.edge_pane(axis, leading, rank);
                let b = s.second.edge_pane(axis, leading, rank);
                if rank(b) > rank(a) {
                    b
                } else {
                    a
                }
            }
        }
    }
}

/// How a split `extent` pixels long divides: the first child's length, the
/// divider's and the second child's. They add up to the extent, so children
/// and divider tile the split exactly whatever the ratio.
pub(crate) fn split_lengths(extent: i32, ratio: f64, divider: i32) -> (i32, i32, i32) {
    let extent = extent.max(0);
    // A divider wider than the split takes all of it.
    let line = divider.max(0).min(extent);
    let avail = extent - line;
    let first = first_length(ratio, avail);
    (first, line, avail - first)
}

/// The first child's share of `avail` pixels.
pub(crate) fn first_length(ratio: f64, avail: i32) -> i32 {
    // `as` saturates, and ratios are never NaN.
    ((ratio * f64::from(avail)).round() as i32).clamp(0, avail.max(0))
}

/// The first child, the divider and the second child of a split of `rect`.
pub(crate) fn split_rect(rect: Rect, axis: Axis, ratio: f64, divider: i32) -> (Rect, Rect, Rect) {
    let (a, line, b) = split_lengths(axis.extent(rect), ratio, divider);
    match axis {
        Axis::Horizontal => (
            Rect::new(rect.x, rect.y, a, rect.h),
            Rect::new(rect.x + a, rect.y, line, rect.h),
            Rect::new(rect.x + a + line, rect.y, b, rect.h),
        ),
        Axis::Vertical => (
            Rect::new(rect.x, rect.y, rect.w, a),
            Rect::new(rect.x, rect.y + a, rect.w, line),
            Rect::new(rect.x, rect.y + a + line, rect.w, b),
        ),
    }
}

/// One step of a walk over the tree laid out in an area.
pub(crate) enum Visit<'a> {
    Pane(PaneId, Rect),
    /// A split, the rectangle it divides and its divider line. Splits come
    /// before their children.
    Split {
        split: &'a Split,
        rect: Rect,
        line: Rect,
    },
}

/// Lays `node` out in `rect` (whose sizes must not be negative) and reports
/// every split and pane, in visual order.
pub(crate) fn visit<'a>(node: &'a Node, rect: Rect, divider: i32, f: &mut impl FnMut(Visit<'a>)) {
    match node {
        Node::Leaf(p) => f(Visit::Pane(*p, rect)),
        Node::Split(s) => {
            let (a, line, b) = split_rect(rect, s.axis, s.ratio, divider);
            f(Visit::Split {
                split: s,
                rect,
                line,
            });
            visit(&s.first, a, divider, f);
            visit(&s.second, b, divider, f);
        }
    }
}

/// The length along `axis` of the smallest pane under `node` when the node is
/// `extent` long on that axis and keeps its ratios.
fn smallest_pane(node: &Node, axis: Axis, extent: i32, divider: i32) -> i32 {
    match node {
        Node::Leaf(_) => extent,
        Node::Split(s) if s.axis == axis => {
            let (a, _, b) = split_lengths(extent, s.ratio, divider);
            smallest_pane(&s.first, axis, a, divider)
                .min(smallest_pane(&s.second, axis, b, divider))
        }
        Node::Split(s) => smallest_pane(&s.first, axis, extent, divider)
            .min(smallest_pane(&s.second, axis, extent, divider)),
    }
}

/// The shortest length along `axis`, up to `limit`, at which every pane
/// under `node` is at least `min` long with the node's ratios unchanged; None
/// if even `limit` is too short.
pub(crate) fn min_extent(
    node: &Node,
    axis: Axis,
    min: i32,
    limit: i32,
    divider: i32,
) -> Option<i32> {
    if smallest_pane(node, axis, limit, divider) < min {
        return None;
    }
    // Every pane's length grows with the node's, never shrinks, so the
    // smallest pane does too and a binary search finds the threshold.
    let (mut lo, mut hi) = (0, limit);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if smallest_pane(node, axis, mid, divider) >= min {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    Some(hi)
}

/// How many units a run of same-axis splits holds: the leaves and
/// other-axis subtrees that sit side by side along `axis`.
pub(crate) fn run_units(node: &Node, axis: Axis) -> usize {
    match node {
        Node::Split(s) if s.axis == axis => run_units(&s.first, axis) + run_units(&s.second, axis),
        _ => 1,
    }
}

/// Gives every unit of each run of same-axis splits under `node` an equal
/// length, laid out in `rect`.
pub(crate) fn equalize(node: &mut Node, rect: Rect, divider: i32) {
    let Node::Split(s) = node else {
        return;
    };
    let axis = s.axis;
    let units = run_units(node, axis);
    let line = i64::from(divider.max(0));
    let room = i64::from(axis.extent(rect)) - (units as i64 - 1) * line;
    // Lengths equal to the pixel need every divider at full width; in a
    // smaller area the ratios just count units.
    let lengths: Option<Vec<i32>> = (room >= 0).then(|| {
        let n = units as i64;
        // Each length is at most the room, which fits in an i32.
        (0..n)
            .map(|i| (room / n + i64::from(i < room % n)) as i32)
            .collect()
    });
    share(node, rect, axis, divider, lengths.as_deref(), units);
}

/// Sets the ratios of the run of `axis` splits at `node` so its `units`
/// units get `lengths`, or equal shares by count when lengths is None.
fn share(
    node: &mut Node,
    rect: Rect,
    axis: Axis,
    divider: i32,
    lengths: Option<&[i32]>,
    units: usize,
) {
    match node {
        Node::Split(s) if s.axis == axis => {
            let k = run_units(&s.first, axis);
            let extent = axis.extent(rect);
            let line = divider.max(0).min(extent);
            let avail = extent - line;
            s.ratio = match lengths {
                Some(l) if avail > 0 => {
                    let want: i32 = l[..k].iter().sum::<i32>() + (k as i32 - 1) * line;
                    f64::from(want) / f64::from(avail)
                }
                _ => k as f64 / units as f64,
            };
            let (a, _, b) = split_rect(rect, axis, s.ratio, divider);
            share(&mut s.first, a, axis, divider, lengths.map(|l| &l[..k]), k);
            share(
                &mut s.second,
                b,
                axis,
                divider,
                lengths.map(|l| &l[k..]),
                units - k,
            );
        }
        _ => equalize(node, rect, divider),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_lengths_always_add_up() {
        for extent in 0..60 {
            for divider in 0..6 {
                for ratio in [0.0, 0.1, 0.25, 0.3333, 0.5, 0.7, 0.999, 1.0] {
                    let (a, line, b) = split_lengths(extent, ratio, divider);
                    assert!(a >= 0 && line >= 0 && b >= 0);
                    assert_eq!(a + line + b, extent, "{extent} {divider} {ratio}");
                    assert_eq!(line, divider.min(extent));
                }
            }
        }
        assert_eq!(split_lengths(-5, 0.5, 1), (0, 0, 0));
        assert_eq!(split_lengths(10, 0.5, -3), (5, 0, 5));
    }

    #[test]
    fn ratio_for_a_length_gives_that_length_back() {
        for avail in [1, 2, 3, 7, 99, 1000, 4097, 65_535, 1 << 24, i32::MAX - 1] {
            for want in [0, 1, avail / 3, avail / 2, avail - 1, avail] {
                let ratio = f64::from(want) / f64::from(avail);
                assert_eq!(first_length(ratio, avail), want, "{want} of {avail}");
            }
        }
    }

    #[test]
    fn min_extent_respects_fixed_inner_ratios() {
        // A pane beside a 20/80 split: the 20% pane limits how small it gets.
        let node = Node::Split(Box::new(Split {
            id: 0,
            axis: Axis::Horizontal,
            ratio: 0.2,
            first: Node::Leaf(1),
            second: Node::Leaf(2),
        }));
        let e = min_extent(&node, Axis::Horizontal, 50, 1000, 1).expect("fits");
        assert!(smallest_pane(&node, Axis::Horizontal, e, 1) >= 50);
        assert!(smallest_pane(&node, Axis::Horizontal, e - 1, 1) < 50);
        assert_eq!(e, 249);
        // Along the other axis both panes span the whole length.
        assert_eq!(min_extent(&node, Axis::Vertical, 50, 1000, 1), Some(50));
        assert_eq!(min_extent(&node, Axis::Horizontal, 50, 200, 1), None);
        assert_eq!(min_extent(&node, Axis::Horizontal, 0, 0, 1), Some(0));
    }
}
