//! Squarified treemap layout (Bruls, Huizing, van Wijk).
//!
//! Deliberately free of any UI dependency: callers get plain rectangles and
//! convert them to whatever their renderer uses. That keeps the maths testable
//! under `cargo test` instead of only by looking at a window.

/// An axis-aligned rectangle in screen units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Box4 {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Box4 {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    pub fn area(&self) -> f64 {
        self.w.max(0.0) * self.h.max(0.0)
    }
}

/// Lay `areas` out inside `rect`, keeping tiles as square as possible.
///
/// The returned slice is in the **same order as the input**; internal sorting is
/// an implementation detail so callers can zip the result with their own items.
/// Zero or negative areas come back as empty boxes in the rect's top-left corner.
pub fn squarify(areas: &[f64], rect: Box4) -> Vec<Box4> {
    let mut out = vec![Box4::new(rect.x, rect.y, 0.0, 0.0); areas.len()];
    if areas.is_empty() || rect.area() <= 0.0 {
        return out;
    }

    let total: f64 = areas.iter().filter(|area| **area > 0.0).sum();
    if total <= 0.0 {
        return out;
    }

    // Convert byte-like weights into the rect's area units.
    let scale = rect.area() / total;
    let mut order: Vec<usize> = (0..areas.len()).filter(|&i| areas[i] > 0.0).collect();
    order.sort_by(|&a, &b| {
        areas[b]
            .partial_cmp(&areas[a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut remaining = rect;
    let mut i = 0;

    while i < order.len() {
        let side = remaining.w.min(remaining.h);
        if side <= f64::EPSILON {
            break;
        }

        // Grow the row while adding the next tile improves the worst aspect ratio.
        let mut row_area = 0.0;
        let mut end = i;
        let mut best_worst = f64::INFINITY;
        let mut best_area = 0.0;

        for k in i..order.len() {
            let trial = row_area + areas[order[k]] * scale;
            let thickness = trial / side;
            if thickness <= 0.0 {
                break;
            }

            let mut worst: f64 = 0.0;
            for j in i..=k {
                let length = areas[order[j]] * scale / thickness;
                if length <= 0.0 {
                    continue;
                }
                worst = worst.max((thickness / length).max(length / thickness));
            }

            if worst <= best_worst {
                best_worst = worst;
                best_area = trial;
                end = k;
                row_area = trial;
            } else {
                break;
            }
        }

        if best_area <= 0.0 {
            break;
        }

        // Rows run along the shorter side, which is what keeps tiles squarish.
        let thickness = (best_area / side).min(remaining.w.max(remaining.h));
        let mut offset = 0.0;
        let vertical_strip = remaining.w >= remaining.h;

        for j in i..=end {
            let area = areas[order[j]] * scale;
            let length = area / thickness;
            out[order[j]] = if vertical_strip {
                Box4::new(remaining.x, remaining.y + offset, thickness, length)
            } else {
                Box4::new(remaining.x + offset, remaining.y, length, thickness)
            };
            offset += length;
        }

        remaining = if vertical_strip {
            Box4::new(
                remaining.x + thickness,
                remaining.y,
                (remaining.w - thickness).max(0.0),
                remaining.h,
            )
        } else {
            Box4::new(
                remaining.x,
                remaining.y + thickness,
                remaining.w,
                (remaining.h - thickness).max(0.0),
            )
        };

        i = end + 1;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> Box4 {
        Box4::new(0.0, 0.0, 100.0, 60.0)
    }

    fn overlap(a: &Box4, b: &Box4) -> f64 {
        let x = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
        let y = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
        if x <= 0.0 || y <= 0.0 {
            0.0
        } else {
            x * y
        }
    }

    #[test]
    fn tiles_fill_the_rect_without_overlapping() {
        let areas = [40.0, 25.0, 15.0, 10.0, 6.0, 4.0];
        let boxes = squarify(&areas, rect());

        let covered: f64 = boxes.iter().map(|b| b.area()).sum();
        assert!(
            (covered - rect().area()).abs() < 1.0,
            "covered {covered} of {}",
            rect().area()
        );

        for (i, a) in boxes.iter().enumerate() {
            assert!(a.w > 0.0 && a.h > 0.0, "tile {i} collapsed: {a:?}");
            for b in boxes.iter().skip(i + 1) {
                assert!(overlap(a, b) < 1e-6, "tiles overlap: {a:?} {b:?}");
            }
        }
    }

    #[test]
    fn result_order_matches_the_input_order() {
        // The first item is the smallest; it must still come back first.
        let areas = [1.0, 100.0, 10.0];
        let boxes = squarify(&areas, rect());

        assert_eq!(boxes.len(), 3);
        assert!(
            boxes[1].area() > boxes[2].area() && boxes[2].area() > boxes[0].area(),
            "areas did not track the input: {:?}",
            boxes.iter().map(|b| b.area()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_single_tile_fills_the_whole_rect() {
        let boxes = squarify(&[7.0], rect());
        assert!((boxes[0].area() - rect().area()).abs() < 1e-6);
    }

    #[test]
    fn empty_and_zero_input_are_not_panics() {
        assert!(squarify(&[], rect()).is_empty());

        let boxes = squarify(&[0.0, 0.0], rect());
        assert_eq!(boxes.len(), 2);
        assert!(boxes.iter().all(|b| b.area() == 0.0));

        let degenerate = squarify(&[1.0, 2.0], Box4::new(0.0, 0.0, 0.0, 0.0));
        assert!(degenerate.iter().all(|b| b.area() == 0.0));
    }

    #[test]
    fn a_dominant_tile_keeps_a_sane_aspect_ratio() {
        // 95% of the area must still be a rectangle, not a sliver.
        let boxes = squarify(&[95.0, 3.0, 2.0], Box4::new(0.0, 0.0, 100.0, 100.0));
        let ratio = boxes[0].w / boxes[0].h;
        assert!(
            ratio > 0.2 && ratio < 5.0,
            "dominant tile aspect ratio {ratio}"
        );
    }
}
