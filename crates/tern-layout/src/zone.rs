//! Drop zones for a pane dragged over another pane.

use tern_types::{Point, Rect};

use crate::Zone;

/// Which zone of the pane at `pane` the point falls in. Each edge zone is
/// the outer quarter of the pane on that side; where two overlap (near a
/// corner) the edge closest to the point wins, and the middle is
/// [`Zone::Center`]. A point outside the pane counts as nearest the edge it
/// is beyond.
pub fn drop_zone(pane: Rect, point: Point) -> Zone {
    // Distances run from the pixel's center and are doubled to stay in
    // integers, so the zones are mirror images of each other.
    let px = 2 * i64::from(point.x) + 1;
    let py = 2 * i64::from(point.y) + 1;
    let (x, y) = (2 * i64::from(pane.x), 2 * i64::from(pane.y));
    let (w, h) = (i64::from(pane.w.max(0)), i64::from(pane.h.max(0)));
    let edges = [
        (Zone::Left, px - x, w),
        (Zone::Right, x + 2 * w - px, w),
        (Zone::Top, py - y, h),
        (Zone::Bottom, y + 2 * h - py, h),
    ];
    edges
        .into_iter()
        // Within a quarter of the length: distance / 2 < length / 4.
        .filter(|&(_, distance, length)| 2 * distance < length)
        .min_by_key(|&(_, distance, _)| distance)
        .map_or(Zone::Center, |(zone, _, _)| zone)
}

/// The part of the pane at `pane` to highlight for a zone: the half on that
/// side, or the whole pane for [`Zone::Center`]. The two halves of a side
/// tile the pane.
pub fn zone_rect(pane: Rect, zone: Zone) -> Rect {
    let (w, h) = (pane.w.max(0), pane.h.max(0));
    match zone {
        Zone::Left => Rect::new(pane.x, pane.y, w / 2, h),
        Zone::Right => Rect::new(pane.x + w / 2, pane.y, w - w / 2, h),
        Zone::Top => Rect::new(pane.x, pane.y, w, h / 2),
        Zone::Bottom => Rect::new(pane.x, pane.y + h / 2, w, h - h / 2),
        Zone::Center => Rect::new(pane.x, pane.y, w, h),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zone(x: i32, y: i32) -> Zone {
        drop_zone(
            Rect::new(100, 200, 100, 80),
            Point {
                x: 100 + x,
                y: 200 + y,
            },
        )
    }

    #[test]
    fn edges_are_the_outer_quarters() {
        assert_eq!(zone(50, 40), Zone::Center);
        assert_eq!(zone(10, 40), Zone::Left);
        assert_eq!(zone(90, 40), Zone::Right);
        assert_eq!(zone(50, 5), Zone::Top);
        assert_eq!(zone(50, 75), Zone::Bottom);
        // A quarter of 100 is 25: pixels 0..=24 and 75..=99 are edge pixels.
        assert_eq!(zone(24, 40), Zone::Left);
        assert_eq!(zone(25, 40), Zone::Center);
        assert_eq!(zone(74, 40), Zone::Center);
        assert_eq!(zone(75, 40), Zone::Right);
        // A quarter of 80 is 20.
        assert_eq!(zone(50, 19), Zone::Top);
        assert_eq!(zone(50, 20), Zone::Center);
        assert_eq!(zone(50, 59), Zone::Center);
        assert_eq!(zone(50, 60), Zone::Bottom);
    }

    #[test]
    fn the_closest_edge_wins_in_corners() {
        assert_eq!(zone(5, 2), Zone::Top);
        assert_eq!(zone(2, 5), Zone::Left);
        assert_eq!(zone(97, 70), Zone::Right);
        assert_eq!(zone(80, 78), Zone::Bottom);
        // Equally close: the first of left, right, top, bottom.
        assert_eq!(zone(3, 3), Zone::Left);
    }

    #[test]
    fn points_outside_go_to_the_edge_they_are_beyond() {
        assert_eq!(zone(-5, 40), Zone::Left);
        assert_eq!(zone(150, 40), Zone::Right);
        assert_eq!(zone(50, -1), Zone::Top);
        assert_eq!(zone(50, 80), Zone::Bottom);
    }

    #[test]
    fn degenerate_panes_do_not_panic() {
        for rect in [
            Rect::new(0, 0, 0, 0),
            Rect::new(0, 0, -4, 3),
            Rect::new(i32::MAX - 1, i32::MIN, 1, 1),
        ] {
            for point in [
                Point { x: 0, y: 0 },
                Point {
                    x: i32::MAX,
                    y: i32::MIN,
                },
                Point {
                    x: i32::MIN,
                    y: i32::MAX,
                },
            ] {
                drop_zone(rect, point);
            }
        }
        // A zero-size pane has no inside: the pixel lies beyond its right and
        // bottom edges at once, and right comes first.
        assert_eq!(
            drop_zone(Rect::new(0, 0, 0, 0), Point { x: 0, y: 0 }),
            Zone::Right
        );
        assert_eq!(
            drop_zone(Rect::new(0, 0, 1, 1), Point { x: 0, y: 0 }),
            Zone::Center
        );
    }

    #[test]
    fn zones_mirror_and_lie_in_their_highlight() {
        let flip_x = |z| match z {
            Zone::Left => Zone::Right,
            Zone::Right => Zone::Left,
            z => z,
        };
        let flip_y = |z| match z {
            Zone::Top => Zone::Bottom,
            Zone::Bottom => Zone::Top,
            z => z,
        };
        for w in 0..30 {
            for h in 0..30 {
                let pane = Rect::new(-7, 11, w, h);
                for x in -2..w + 2 {
                    for y in -2..h + 2 {
                        let at = |x, y| {
                            drop_zone(
                                pane,
                                Point {
                                    x: -7 + x,
                                    y: 11 + y,
                                },
                            )
                        };
                        let zone = at(x, y);
                        assert_eq!(flip_x(at(w - 1 - x, y)), zone, "{w}x{h} at {x},{y}");
                        assert_eq!(flip_y(at(x, h - 1 - y)), zone, "{w}x{h} at {x},{y}");
                        let point = Point {
                            x: -7 + x,
                            y: 11 + y,
                        };
                        if pane.contains(point) {
                            assert!(zone_rect(pane, zone).contains(point), "{w}x{h} at {x},{y}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn zone_rects_are_halves() {
        let pane = Rect::new(10, 20, 101, 81);
        assert_eq!(zone_rect(pane, Zone::Left), Rect::new(10, 20, 50, 81));
        assert_eq!(zone_rect(pane, Zone::Right), Rect::new(60, 20, 51, 81));
        assert_eq!(zone_rect(pane, Zone::Top), Rect::new(10, 20, 101, 40));
        assert_eq!(zone_rect(pane, Zone::Bottom), Rect::new(10, 60, 101, 41));
        assert_eq!(zone_rect(pane, Zone::Center), pane);
        assert_eq!(
            zone_rect(Rect::new(0, 0, -3, 5), Zone::Center),
            Rect::new(0, 0, 0, 5)
        );
    }
}
