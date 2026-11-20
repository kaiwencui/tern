//! Random sequences of operations on random areas, checked after every step
//! against the promises every layout keeps. The generator is seeded, so a
//! failure replays exactly.

use std::collections::HashSet;
use std::panic::{catch_unwind, AssertUnwindSafe};

use tern_types::{PaneId, Point, Rect};

use crate::tree::{split_lengths, split_rect, visit, Node, Visit};
use crate::{
    normalized, Axis, Closed, Direction, DividerHandle, Layout, LayoutError, Zone, MAX_DEPTH,
};

/// SplitMix64: small, fast and good enough to shuffle operations.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// A number in lo..=hi.
    fn between(&mut self, lo: i32, hi: i32) -> i32 {
        let span = (i64::from(hi) - i64::from(lo) + 1) as u64;
        (i64::from(lo) + (self.next() % span) as i64) as i32
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }
}

const DIRECTIONS: [Direction; 4] = [
    Direction::Left,
    Direction::Right,
    Direction::Up,
    Direction::Down,
];
const ZONES: [Zone; 5] = [
    Zone::Left,
    Zone::Right,
    Zone::Top,
    Zone::Bottom,
    Zone::Center,
];
const MINIMUMS: [i32; 7] = [0, 0, 1, 5, 20, 80, 300];

/// Mostly ordinary window sizes, often tiny ones, sometimes zero or negative.
fn random_length(rng: &mut Rng, tiny: bool) -> i32 {
    match rng.below(10) {
        0 => rng.between(-3, 0),
        _ if tiny => rng.between(0, 6),
        1 | 2 => rng.between(1, 12),
        _ => rng.between(13, 3000),
    }
}

fn random_area(rng: &mut Rng, tiny: bool) -> Rect {
    let (x, y) = (rng.between(-500, 500), rng.between(-500, 500));
    Rect::new(x, y, random_length(rng, tiny), random_length(rng, tiny))
}

/// A point in or just around the area, now and then one as far off as an
/// i32 reaches.
fn random_point(rng: &mut Rng, area: Rect) -> Point {
    if rng.below(40) == 0 {
        let far = [i32::MIN, i32::MAX];
        return Point {
            x: rng.pick(&far),
            y: area.y + rng.between(0, area.h.max(0)),
        };
    }
    Point {
        x: area.x + rng.between(-20, area.w.max(0) + 20),
        y: area.y + rng.between(-20, area.h.max(0) + 20),
    }
}

#[test]
fn random_operations_keep_every_promise() {
    run_seeds(0..400, false);
}

#[test]
fn random_operations_in_tiny_areas_keep_every_promise() {
    run_seeds(1000..1200, true);
}

/// Valid layout text with a few random edits never panics the parser: it
/// either reads as a layout that keeps every promise, or fails with an offset
/// inside the text.
#[test]
fn mangled_text_is_rejected_or_read_whole() {
    let valid = [
        "*7",
        "h0.5(*1,2)",
        "v0.3333333333333333(1,!2)",
        "h0(1,v1(2,*3))",
        "h0.1(v0.5(1,2),h0.7(3,v0.75(*18446744073709551615,0)))",
    ];
    let alphabet: Vec<char> = "hv0123456789.,()*!eE+- é".chars().collect();
    let mut rng = Rng(42);
    let mut read = 0;
    for _ in 0..20_000 {
        let mut text: Vec<char> = rng.pick(&valid).chars().collect();
        for _ in 0..=rng.below(3) {
            let at = rng.below(text.len() + 1);
            match rng.below(4) {
                0 => text.insert(at, rng.pick(&alphabet)),
                1 if at < text.len() => text[at] = rng.pick(&alphabet),
                2 if at < text.len() => {
                    text.remove(at);
                }
                3 if !text.is_empty() => {
                    // Repeat a stretch, which nests and duplicates panes.
                    let from = rng.below(text.len());
                    let to = from + rng.below(text.len() - from);
                    let stretch: Vec<char> = text[from..=to].to_vec();
                    text.splice(at..at, stretch);
                }
                _ => {}
            }
        }
        let text: String = text.into_iter().collect();
        match text.parse::<Layout>() {
            Ok(layout) => {
                read += 1;
                let panes = layout.panes();
                let set: HashSet<PaneId> = panes.iter().copied().collect();
                assert_eq!(set.len(), panes.len(), "{text}");
                assert!(layout.focused().is_some_and(|f| set.contains(&f)) || panes.is_empty());
                assert!(!layout.is_zoomed() || panes.len() >= 2, "{text}");
                let again: Layout = layout.to_string().parse().expect("written text reads back");
                assert_eq!(again, layout, "{text}");
            }
            Err(e) => assert!(text.is_char_boundary(e.offset()), "{text}: {e}"),
        }
    }
    // Some edits keep the text valid, so both outcomes are exercised.
    assert!(read > 100 && read < 19_000, "{read} of 20000 read");
}

fn run_seeds(seeds: std::ops::Range<u64>, tiny: bool) {
    for seed in seeds {
        let mut trace = Vec::new();
        if catch_unwind(AssertUnwindSafe(|| run(seed, tiny, &mut trace))).is_err() {
            let recent = &trace[trace.len().saturating_sub(6)..];
            panic!(
                "seed {seed} failed; the last steps were:\n{}",
                recent.join("\n")
            );
        }
    }
}

/// What one run of operations works with.
struct World {
    rng: Rng,
    layout: Layout,
    area: Rect,
    divider: i32,
    next_id: PaneId,
    /// Panes taken out, waiting to be inserted again.
    outside: Vec<PaneId>,
    /// Every divider handle this layout has shown, current or not.
    seen: Vec<DividerHandle>,
}

fn run(seed: u64, tiny: bool, trace: &mut Vec<String>) {
    let mut rng = Rng(seed);
    let area = random_area(&mut rng, tiny);
    let divider = rng.between(0, 6);
    let mut w = World {
        rng,
        layout: Layout::new(0),
        area,
        divider,
        next_id: 1,
        outside: Vec::new(),
        seen: Vec::new(),
    };
    for _ in 0..200 {
        if w.rng.below(12) == 0 {
            w.area = random_area(&mut w.rng, tiny);
            w.divider = w.rng.between(0, 6);
        }
        let what = if w.layout.is_empty() {
            w.layout = Layout::new(w.next_id);
            w.next_id += 1;
            w.seen.clear();
            "start over".to_string()
        } else {
            step(&mut w)
        };
        trace.push(format!(
            "{what} in {:?} with divider {}: {}",
            w.area, w.divider, w.layout
        ));
        check(&mut w);
    }
}

/// Applies one random operation, checks what that operation promises, and
/// says what it was.
fn step(w: &mut World) -> String {
    let panes = w.layout.panes();
    let pane = w.rng.pick(&panes);
    let before = w.layout.clone();
    let (area, divider) = (w.area, w.divider);
    match w.rng.below(18) {
        0..=3 if panes.len() < 24 => {
            let direction = w.rng.pick(&DIRECTIONS);
            let new = fresh(w);
            match w.layout.split(pane, direction, new) {
                Ok(()) => {
                    assert_eq!(w.layout.focused(), Some(new));
                    assert!(!w.layout.is_zoomed());
                    assert_eq!(w.layout.len(), panes.len() + 1);
                    assert_placed(&w.layout, new, pane, direction);
                }
                Err(e) => {
                    assert_eq!(e, LayoutError::TooDeep);
                    assert_eq!(w.layout, before);
                }
            }
            format!("split {pane} {direction:?} {new}")
        }
        0..=4 => {
            // Laid out large, so only ratios of 0 or 1 leave panes empty.
            let old = unzoomed(&before).rects(Rect::new(0, 0, 1 << 20, 1 << 20), 1);
            match w.layout.close(pane) {
                Ok(Closed::Focus(focus)) => {
                    assert_eq!(w.layout.focused(), Some(focus));
                    if before.focused() == Some(pane) {
                        assert_bordering(&old, pane, focus);
                    } else {
                        assert_eq!(before.focused(), Some(focus));
                    }
                }
                Ok(Closed::Empty) => assert_eq!(panes.len(), 1),
                Err(e) => panic!("{e}"),
            }
            assert!(!w.layout.contains(pane));
            assert!(!w.layout.is_zoomed());
            format!("close {pane}")
        }
        5 => {
            let target = w.rng.pick(&panes);
            let zone = w.rng.pick(&ZONES);
            match w.layout.move_pane(pane, target, zone) {
                Ok(()) if pane == target => assert_eq!(w.layout, before),
                Ok(()) => {
                    assert_eq!(sorted(w.layout.panes()), sorted(panes.clone()));
                    assert_eq!(w.layout.focused(), Some(pane));
                    assert!(!w.layout.is_zoomed());
                    match zone.side() {
                        Some(side) => assert_placed(&w.layout, pane, target, side),
                        None => {
                            // The two panes trade rectangles; nothing else changes.
                            let swap = |p: PaneId| match p {
                                p if p == pane => target,
                                p if p == target => pane,
                                p => p,
                            };
                            let old: Vec<_> = unzoomed(&before).rects(area, divider);
                            let new: Vec<_> = w.layout.rects(area, divider);
                            let expected: Vec<_> = old.iter().map(|&(p, r)| (swap(p), r)).collect();
                            assert_eq!(new, expected);
                        }
                    }
                }
                Err(e) => {
                    assert_eq!(e, LayoutError::TooDeep);
                    assert_eq!(w.layout, before);
                }
            }
            format!("move {pane} to {zone:?} of {target}")
        }
        6 => {
            let dividers = w.layout.dividers(area, divider);
            if w.rng.below(4) == 0 && !w.seen.is_empty() {
                let handle = w.rng.pick(&w.seen);
                if !dividers.iter().any(|d| d.handle == handle) {
                    let pointer = random_point(&mut w.rng, area);
                    assert!(!w.layout.drag_divider(handle, area, divider, pointer, 0));
                    assert_eq!(w.layout, before);
                    return format!("drag stale {handle:?}");
                }
            }
            if dividers.is_empty() {
                return "drag nothing".to_string();
            }
            let line = w.rng.pick(&dividers);
            let pointer = random_point(&mut w.rng, area);
            let min = w.rng.pick(&MINIMUMS);
            let old = w.layout.rects(area, divider);
            let moved = w
                .layout
                .drag_divider(line.handle, area, divider, pointer, min);
            assert_moved_within_limits(w, &before, &old, moved, min);
            if !before.is_zoomed() {
                assert_drag_went_as_far_as_allowed(w, &before, line.handle, pointer, min);
            }
            format!("drag {:?} to {pointer:?} keeping {min}", line.handle)
        }
        7 => {
            let direction = w.rng.pick(&DIRECTIONS);
            let amount = w.rng.between(-300, 300);
            let min = w.rng.pick(&MINIMUMS);
            let old = w.layout.rects(area, divider);
            let moved = w.layout.resize(pane, direction, amount, area, divider, min);
            assert_moved_within_limits(w, &before, &old, moved, min);
            if min == 0 && !before.is_zoomed() {
                assert_edge_moved(w, &old, pane, direction, amount);
            }
            format!("resize {pane} {direction:?} by {amount} keeping {min}")
        }
        8 => {
            let zoomed = w.layout.toggle_zoom();
            assert_eq!(zoomed, !before.is_zoomed() && panes.len() > 1);
            assert_eq!(w.layout.is_zoomed(), zoomed);
            format!("toggle zoom to {zoomed}")
        }
        9 => {
            w.layout.equalize(area, divider);
            assert_eq!(w.layout.panes(), panes);
            assert_eq!(w.layout.focused(), before.focused());
            assert_eq!(w.layout.is_zoomed(), before.is_zoomed());
            if let Some(root) = &w.layout.root {
                assert_runs_even(root, normalized(area), divider);
            }
            "equalize".to_string()
        }
        10 => {
            assert!(w.layout.set_focus(pane));
            assert_eq!(w.layout.focused(), Some(pane));
            assert!(!w.layout.set_focus(w.next_id));
            assert_eq!(w.layout.is_zoomed(), before.is_zoomed());
            format!("focus {pane}")
        }
        11 => {
            let direction = w.rng.pick(&DIRECTIONS);
            let rects = unzoomed(&before).rects(area, divider);
            let from = rects
                .iter()
                .find(|r| Some(r.0) == before.focused())
                .expect("focus laid out")
                .1;
            // Every pane only a divider away on that side, with the length of
            // edge it shares.
            let neighbors: Vec<(PaneId, i32)> = rects
                .iter()
                .filter(|r| Some(r.0) != before.focused())
                .filter_map(|&(p, to)| {
                    let (gap, overlap) = gap_and_overlap(from, to, direction);
                    ((0..=divider.max(0)).contains(&gap) && overlap > 0).then_some((p, overlap))
                })
                .collect();
            match w.layout.focus_direction(direction, area, divider) {
                Some(next) => {
                    assert_eq!(w.layout.focused(), Some(next));
                    let longest = neighbors.iter().map(|n| n.1).max();
                    let shared = neighbors.iter().find(|n| n.0 == next).map(|n| n.1);
                    assert!(
                        shared.is_some() && shared == longest,
                        "{next} is not the best neighbor {direction:?} in {neighbors:?}"
                    );
                }
                None => {
                    assert!(neighbors.is_empty(), "missed {neighbors:?}");
                    assert_eq!(w.layout, before);
                }
            }
            assert_eq!(w.layout.is_zoomed(), before.is_zoomed());
            format!("focus {direction:?}")
        }
        12 => {
            let forward = w.rng.below(2) == 0;
            let next = if forward {
                w.layout.focus_next()
            } else {
                w.layout.focus_previous()
            };
            let at = panes
                .iter()
                .position(|&p| Some(p) == before.focused())
                .unwrap_or(0);
            let n = panes.len();
            let expected = panes[if forward {
                (at + 1) % n
            } else {
                (at + n - 1) % n
            }];
            assert_eq!(next, Some(expected));
            format!("focus next {forward}")
        }
        13 => {
            match w.layout.take(pane) {
                Ok(remaining) => assert_eq!(remaining, panes.len() > 1),
                Err(e) => panic!("{e}"),
            }
            w.outside.push(pane);
            format!("take {pane}")
        }
        14 => {
            let incoming = match w.outside.pop() {
                Some(p) => p,
                None => fresh(w),
            };
            let zone = w.rng.pick(&ZONES);
            match w.layout.insert(pane, zone, incoming) {
                Ok(()) => {
                    assert_eq!(w.layout.focused(), Some(incoming));
                    let side = zone.side().expect("an edge zone");
                    assert_placed(&w.layout, incoming, pane, side);
                }
                Err(e) => {
                    let expected = if zone == Zone::Center {
                        LayoutError::CenterZone
                    } else {
                        LayoutError::TooDeep
                    };
                    assert_eq!(e, expected);
                    assert_eq!(w.layout, before);
                    w.outside.push(incoming);
                }
            }
            format!("insert {incoming} at {zone:?} of {pane}")
        }
        15 => {
            let other = w.rng.pick(&panes);
            if other != pane {
                assert_eq!(
                    w.layout.replace(pane, other),
                    Err(LayoutError::AlreadyPresent(other))
                );
            }
            let new = fresh(w);
            w.layout.replace(pane, new).expect("pane is present");
            assert!(!w.layout.contains(pane));
            let rename = |p: PaneId| if p == pane { new } else { p };
            assert_eq!(w.layout.focused(), before.focused().map(rename));
            assert_eq!(w.layout.is_zoomed(), before.is_zoomed());
            let expected: Vec<_> = before
                .rects(area, divider)
                .iter()
                .map(|&(p, r)| (rename(p), r))
                .collect();
            assert_eq!(w.layout.rects(area, divider), expected);
            format!("replace {pane} with {new}")
        }
        16 => {
            // A tab of a few panes dropped on this one.
            let mut incoming = Layout::new(fresh(w));
            for _ in 0..w.rng.below(4) {
                let at = w.rng.pick(&incoming.panes());
                let direction = w.rng.pick(&DIRECTIONS);
                let new = fresh(w);
                incoming.split(at, direction, new).expect("a small layout");
            }
            if w.rng.below(3) == 0 {
                incoming.toggle_zoom();
            }
            let zone = w.rng.pick(&ZONES);
            match w.layout.insert_layout(pane, zone, &incoming) {
                Ok(()) => {
                    assert_eq!(w.layout.focused(), incoming.focused());
                    assert!(!w.layout.is_zoomed());
                    assert_eq!(w.layout.len(), panes.len() + incoming.len());
                    // The newcomers sit beside the pane, arranged as they were.
                    let side = zone.side().expect("an edge zone");
                    let root = w.layout.root.as_ref().expect("a layout with panes");
                    let (parent, pane_first) = root.parent_of(pane).expect("pane under a split");
                    let beside = if pane_first {
                        &parent.second
                    } else {
                        &parent.first
                    };
                    assert_eq!(Some(beside), incoming.root.as_ref());
                    assert_eq!(parent.axis, side.axis());
                    assert_eq!(pane_first, side.is_forward());
                }
                Err(e) => {
                    let expected = if zone == Zone::Center {
                        LayoutError::CenterZone
                    } else {
                        LayoutError::TooDeep
                    };
                    assert_eq!(e, expected);
                    assert_eq!(w.layout, before);
                }
            }
            format!("insert layout {incoming} at {zone:?} of {pane}")
        }
        _ => {
            // Carry on with a layout read back from text: its handles are new.
            w.layout = w.layout.to_string().parse().expect("round trip");
            w.seen.clear();
            "reload".to_string()
        }
    }
}

fn fresh(w: &mut World) -> PaneId {
    w.next_id += 1;
    w.next_id - 1
}

fn sorted(mut panes: Vec<PaneId>) -> Vec<PaneId> {
    panes.sort_unstable();
    panes
}

fn unzoomed(layout: &Layout) -> Layout {
    let mut copy = layout.clone();
    copy.zoomed = false;
    copy
}

/// The pane sits right beside `target` on `side`, the two alone in one split.
fn assert_placed(layout: &Layout, pane: PaneId, target: PaneId, side: Direction) {
    let root = layout.root.as_ref().expect("a layout with panes");
    let (parent, pane_first) = root.parent_of(pane).expect("pane under a split");
    let other = if pane_first {
        &parent.second
    } else {
        &parent.first
    };
    assert!(
        other.is_pane(target),
        "{pane} is not beside {target} in {layout}"
    );
    assert_eq!(parent.axis, side.axis());
    assert_eq!(pane_first, !side.is_forward());
}

/// A divider move reported honestly and squeezed no pane below the minimum:
/// any pane that got smaller is still at least `min` on that side.
fn assert_moved_within_limits(
    w: &World,
    before: &Layout,
    old: &[(PaneId, Rect)],
    moved: bool,
    min: i32,
) {
    let new = w.layout.rects(w.area, w.divider);
    if !moved {
        assert_eq!(&w.layout, before);
        return;
    }
    assert_ne!(new, old, "a reported move changed no rectangle");
    for (pane, a) in &new {
        let b = old.iter().find(|(p, _)| p == pane).expect("same panes").1;
        assert!(
            a.w >= b.w || a.w >= min,
            "pane {pane} squeezed from {b:?} to {a:?}"
        );
        assert!(
            a.h >= b.h || a.h >= min,
            "pane {pane} squeezed from {b:?} to {a:?}"
        );
    }
}

/// How far `to` lies from `from` in that direction, and how long the stretch
/// of edge they share is.
fn gap_and_overlap(from: Rect, to: Rect, direction: Direction) -> (i32, i32) {
    match direction {
        Direction::Left => (
            from.x - to.right(),
            from.bottom().min(to.bottom()) - from.y.max(to.y),
        ),
        Direction::Right => (
            to.x - from.right(),
            from.bottom().min(to.bottom()) - from.y.max(to.y),
        ),
        Direction::Up => (
            from.y - to.bottom(),
            from.right().min(to.right()) - from.x.max(to.x),
        ),
        Direction::Down => (
            to.y - from.bottom(),
            from.right().min(to.right()) - from.x.max(to.x),
        ),
    }
}

/// The pane that took focus from a closed pane bordered it, across a
/// one-pixel divider, unless one of the two had no area.
fn assert_bordering(rects: &[(PaneId, Rect)], closed: PaneId, focus: PaneId) {
    let find = |p: PaneId| {
        rects
            .iter()
            .find(|r| r.0 == p)
            .map(|r| r.1)
            .expect("pane laid out")
    };
    let (a, b) = (find(closed), find(focus));
    if a.is_empty() || b.is_empty() {
        return;
    }
    let bordering = DIRECTIONS.iter().any(|&d| {
        let (gap, overlap) = gap_and_overlap(a, b, d);
        (0..=1).contains(&gap) && overlap > 0
    });
    assert!(bordering, "{b:?} does not border the closed {a:?}");
}

/// After equalize, the units of every run that had room for its dividers
/// differ in length by at most a pixel.
fn assert_runs_even(node: &Node, rect: Rect, divider: i32) {
    let Node::Split(s) = node else {
        return;
    };
    let mut units = Vec::new();
    collect_units(node, rect, s.axis, divider, &mut units);
    let lengths: Vec<i32> = units.iter().map(|(_, r)| s.axis.extent(*r)).collect();
    let room =
        i64::from(s.axis.extent(rect)) - (units.len() as i64 - 1) * i64::from(divider.max(0));
    if room >= 0 {
        let (lo, hi) = (lengths.iter().min(), lengths.iter().max());
        assert!(
            hi.zip(lo).is_some_and(|(hi, lo)| hi - lo <= 1),
            "uneven run {lengths:?}"
        );
    }
    for (unit, r) in units {
        assert_runs_even(unit, r, divider);
    }
}

fn collect_units<'a>(
    node: &'a Node,
    rect: Rect,
    axis: Axis,
    divider: i32,
    out: &mut Vec<(&'a Node, Rect)>,
) {
    match node {
        Node::Split(s) if s.axis == axis => {
            let (a, _, b) = split_rect(rect, axis, s.ratio, divider);
            collect_units(&s.first, a, axis, divider, out);
            collect_units(&s.second, b, axis, divider, out);
        }
        _ => out.push((node, rect)),
    }
}

/// A split as laid out: its axis, where its first child starts along that
/// axis, the first child's length, the line's width, the length both children
/// share, the panes on each side, and its id.
struct Placed {
    axis: Axis,
    start: i32,
    first: i32,
    line: i32,
    avail: i32,
    sides: [Vec<PaneId>; 2],
    id: u64,
}

/// The split behind a divider handle; dividers come in the order splits are
/// visited.
fn placed(layout: &Layout, handle: DividerHandle, area: Rect, divider: i32) -> Option<Placed> {
    let index = unzoomed(layout)
        .dividers(area, divider)
        .iter()
        .position(|d| d.handle == handle)?;
    let mut found = None;
    let mut seen = 0;
    visit(layout.root.as_ref()?, normalized(area), divider, &mut |v| {
        if let Visit::Split { split, rect, .. } = v {
            if seen == index {
                let (first, line, second) =
                    split_lengths(split.axis.extent(rect), split.ratio, divider);
                let (mut a, mut b) = (Vec::new(), Vec::new());
                split.first.panes(&mut a);
                split.second.panes(&mut b);
                found = Some(Placed {
                    axis: split.axis,
                    start: split.axis.start(rect),
                    first,
                    line,
                    avail: first + second,
                    sides: [a, b],
                    id: split.id,
                });
            }
            seen += 1;
        }
    });
    found
}

/// A drag lands the line centered on the pointer, unless that would take a
/// side below the minimum. When it stops short, one more pixel toward the
/// pointer would squeeze a pane on the shrinking side below the minimum, or
/// that side already had a pane below it.
fn assert_drag_went_as_far_as_allowed(
    w: &World,
    before: &Layout,
    handle: DividerHandle,
    pointer: Point,
    min: i32,
) {
    let (area, divider) = (w.area, w.divider);
    let (Some(old), Some(new)) = (
        placed(before, handle, area, divider),
        placed(&w.layout, handle, area, divider),
    ) else {
        panic!("the dragged split went away");
    };
    let wanted =
        (i64::from(old.axis.coord(pointer)) - i64::from(old.start) - i64::from(old.line / 2))
            .clamp(0, i64::from(old.avail)) as i32;
    if new.first == wanted {
        return;
    }
    assert!(
        min > 0,
        "without a minimum the line stopped at {} short of {wanted}",
        new.first
    );
    let toward = if wanted > new.first { 1 } else { -1 };
    let shrinking = &new.sides[usize::from(toward > 0)];
    let length = |r: Rect| new.axis.extent(r);
    let now = w.layout.rects(area, divider);
    let below_already = before
        .rects(area, divider)
        .iter()
        .any(|(p, r)| shrinking.contains(p) && length(*r) < min);
    let mut further = w.layout.clone();
    if let Some(split) = further.root.as_mut().and_then(|r| r.split_mut(new.id)) {
        split.ratio = f64::from(new.first + toward) / f64::from(new.avail);
    }
    let squeezes = further
        .rects(area, divider)
        .iter()
        .zip(&now)
        .any(|((p, a), (_, b))| {
            shrinking.contains(p) && length(*a) < length(*b) && length(*a) < min
        });
    assert!(
        below_already || squeezes,
        "the line stopped at {} short of {wanted} keeping {min}",
        new.first
    );
}

/// With no minimum, resizing moves the pane's edge on that side by the amount,
/// or less when a pane on one side of the moving line has shrunk to nothing.
fn assert_edge_moved(
    w: &World,
    old: &[(PaneId, Rect)],
    pane: PaneId,
    direction: Direction,
    amount: i32,
) {
    let area = normalized(w.area);
    let new = w.layout.rects(w.area, w.divider);
    let find =
        |rects: &[(PaneId, Rect)]| rects.iter().find(|r| r.0 == pane).expect("pane laid out").1;
    let (a, b) = (find(old), find(&new));
    let edge = |r: Rect| match direction {
        Direction::Left => r.x,
        Direction::Right => r.right(),
        Direction::Up => r.y,
        Direction::Down => r.bottom(),
    };
    if edge(a) == edge(area) {
        // No divider on that side: the other one moved instead.
        return;
    }
    let wanted = if direction.is_forward() {
        amount
    } else {
        -amount
    };
    let moved = edge(b) - edge(a);
    if moved != wanted {
        let collapsed = new.iter().any(|(_, r)| direction.axis().extent(*r) == 0);
        assert!(
            moved.signum() != -wanted.signum() && moved.abs() < wanted.abs() && collapsed,
            "the edge moved {moved} instead of {wanted}"
        );
    }
}

/// Everything that holds for any layout in any area.
fn check(w: &mut World) {
    let layout = &w.layout;
    let (area, divider) = (w.area, w.divider);
    let panes = layout.panes();
    let set: HashSet<PaneId> = panes.iter().copied().collect();
    assert_eq!(set.len(), panes.len(), "a pane appears twice in {layout}");
    assert_eq!(layout.len(), panes.len());
    assert_eq!(layout.is_empty(), panes.is_empty());
    match layout.focused() {
        Some(focus) => assert!(set.contains(&focus), "focus {focus} is not in {layout}"),
        None => assert!(panes.is_empty(), "{layout} has no focus"),
    }
    assert!(
        !layout.is_zoomed() || panes.len() >= 2,
        "{layout} zooms a lone pane"
    );
    for p in &w.outside {
        assert!(!set.contains(p));
    }

    let history: HashSet<PaneId> = layout.history.iter().copied().collect();
    assert_eq!(history.len(), layout.history.len());
    assert!(history.is_subset(&set));
    assert_eq!(layout.history.last().copied(), layout.focused());

    let mut ids = HashSet::new();
    if let Some(root) = &layout.root {
        check_tree(root, 0, &mut ids);
    }

    let full = normalized(area);
    let rects = layout.rects(area, divider);
    let dividers = layout.dividers(area, divider);
    match layout.focused() {
        Some(focus) if layout.is_zoomed() => {
            assert_eq!(rects, vec![(focus, full)]);
            assert!(dividers.is_empty());
        }
        _ => {
            let order: Vec<PaneId> = rects.iter().map(|r| r.0).collect();
            assert_eq!(order, panes);
            assert_eq!(dividers.len(), panes.len().saturating_sub(1));
            let handles: HashSet<DividerHandle> = dividers.iter().map(|d| d.handle).collect();
            assert_eq!(handles.len(), dividers.len());
            for h in handles {
                if !w.seen.contains(&h) {
                    w.seen.push(h);
                }
            }
        }
    }
    if !panes.is_empty() {
        let pieces: Vec<Rect> = rects
            .iter()
            .map(|r| r.1)
            .chain(dividers.iter().map(|d| d.rect))
            .collect();
        assert_tiles(full, &pieces, layout);
        for _ in 0..4 {
            let p = random_point(&mut w.rng, area);
            let pane = layout.pane_at(area, divider, p);
            let on_divider = dividers.iter().any(|d| d.rect.contains(p));
            if let Some(hit) = pane {
                assert!(hit.1.contains(p) && rects.contains(&hit));
            }
            assert!(!(pane.is_some() && on_divider));
            assert_eq!(
                pane.is_some() || on_divider,
                full.contains(p),
                "{p:?} in {layout}"
            );
            assert_eq!(layout.divider_at(area, divider, p, 0).is_some(), on_divider);
        }
    }

    let text = layout.to_string();
    let back: Layout = text.parse().unwrap_or_else(|e| panic!("{text}: {e}"));
    assert_eq!(&back, layout, "{text}");
    assert_eq!(back.to_string(), text);
}

fn check_tree(node: &Node, depth: usize, ids: &mut HashSet<u64>) {
    assert!(depth <= MAX_DEPTH);
    if let Node::Split(s) = node {
        assert!((0.0..=1.0).contains(&s.ratio), "ratio {}", s.ratio);
        assert!(ids.insert(s.id), "split id {} repeats", s.id);
        check_tree(&s.first, depth + 1, ids);
        check_tree(&s.second, depth + 1, ids);
    }
}

/// The pieces lie inside the area, overlap nowhere, and cover all of it.
fn assert_tiles(area: Rect, pieces: &[Rect], layout: &Layout) {
    let mut covered = 0i64;
    for (i, a) in pieces.iter().enumerate() {
        assert!(a.w >= 0 && a.h >= 0, "{a:?} in {layout}");
        let inside = a.x >= area.x
            && a.y >= area.y
            && a.right() <= area.right()
            && a.bottom() <= area.bottom();
        assert!(inside, "{a:?} leaves {area:?} in {layout}");
        covered += i64::from(a.w) * i64::from(a.h);
        for b in &pieces[i + 1..] {
            let overlap = a.x.max(b.x) < a.right().min(b.right())
                && a.y.max(b.y) < a.bottom().min(b.bottom());
            assert!(!overlap, "{a:?} overlaps {b:?} in {layout}");
        }
    }
    assert_eq!(
        covered,
        i64::from(area.w) * i64::from(area.h),
        "gaps in {area:?} for {layout}"
    );
}
