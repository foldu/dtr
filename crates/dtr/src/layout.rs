//! The pane layout: a binary tree of splits, with leaves that say nothing about
//! what they contain.
//!
//! Nothing here knows what a terminal is. A leaf is an id; what that id maps to
//! — a terminal session today, a file explorer or a log viewer later — is the
//! caller's business. That separation is the whole point: adding a widget pane
//! should be a new variant in the caller's map, not surgery on the geometry.

/// Identifier for one pane. Monotonic; never reused, so a stale id in the tree
/// is a bug rather than a silently different pane.
pub type LeafId = u64;

/// A rectangle in physical pixels, origin top-left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}

/// Which way a split divides its space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Two panes side by side, divided by a vertical line.
    Horizontal,
    /// Two panes stacked, divided by a horizontal line.
    Vertical,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Layout {
    Leaf(LeafId),
    Split {
        axis: Axis,
        /// Fraction of the space given to `first`, clamped to 0.1..=0.9.
        ratio: f32,
        first: Box<Layout>,
        second: Box<Layout>,
    },
}

impl Layout {
    pub fn leaf(id: LeafId) -> Self {
        Self::Leaf(id)
    }

    /// Every leaf with the rectangle it occupies, in tree order (left/top
    /// first). The gap is split between the two halves so the panes do not
    /// touch.
    pub fn rects(&self, area: Rect, gap: f32) -> Vec<(LeafId, Rect)> {
        let mut out = Vec::new();
        self.collect(area, gap, &mut out);
        out
    }

    fn collect(&self, area: Rect, gap: f32, out: &mut Vec<(LeafId, Rect)>) {
        match self {
            Layout::Leaf(id) => out.push((*id, area)),
            Layout::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let ratio = ratio.clamp(0.1, 0.9);
                let (a, b) = match axis {
                    Axis::Horizontal => {
                        let first_width = (area.width - gap) * ratio;
                        (
                            Rect::new(area.x, area.y, first_width, area.height),
                            Rect::new(
                                area.x + first_width + gap,
                                area.y,
                                area.width - first_width - gap,
                                area.height,
                            ),
                        )
                    }
                    Axis::Vertical => {
                        let first_height = (area.height - gap) * ratio;
                        (
                            Rect::new(area.x, area.y, area.width, first_height),
                            Rect::new(
                                area.x,
                                area.y + first_height + gap,
                                area.width,
                                area.height - first_height - gap,
                            ),
                        )
                    }
                };
                first.collect(a, gap, out);
                second.collect(b, gap, out);
            }
        }
    }

    /// Replace `target` with a split holding it and `new`, on the given axis.
    /// Returns false when the leaf is not in this tree.
    pub fn split(&mut self, target: LeafId, axis: Axis, new: LeafId, new_first: bool) -> bool {
        if let Layout::Leaf(id) = self {
            if *id == target {
                let existing = Layout::Leaf(target);
                let added = Layout::Leaf(new);
                *self = if new_first {
                    Layout::Split {
                        axis,
                        ratio: 0.5,
                        first: Box::new(added),
                        second: Box::new(existing),
                    }
                } else {
                    Layout::Split {
                        axis,
                        ratio: 0.5,
                        first: Box::new(existing),
                        second: Box::new(added),
                    }
                };
                return true;
            }
            return false;
        }

        match self {
            Layout::Split { first, second, .. } => {
                first.split(target, axis, new, new_first)
                    || second.split(target, axis, new, new_first)
            }
            Layout::Leaf(_) => false,
        }
    }

    /// Remove a leaf, collapsing its parent so the sibling takes the space.
    ///
    /// Returns whether anything is left: removing the last leaf cannot collapse
    /// the tree, so the caller is told and decides what an empty tab means.
    pub fn remove(&mut self, target: LeafId) -> bool {
        if let Layout::Leaf(id) = self {
            // The only leaf: nothing to collapse into.
            return *id != target;
        }
        self.remove_inner(target);
        true
    }

    /// Find and unlink `target`. A split whose child *is* the target is
    /// replaced by the other child; deeper targets are found recursively, and
    /// the split that directly contains them collapses itself.
    fn remove_inner(&mut self, target: LeafId) -> bool {
        let Layout::Split { first, second, .. } = self else {
            return false;
        };
        if matches!(**first, Layout::Leaf(id) if id == target) {
            let sibling = (**second).clone();
            *self = sibling;
            return true;
        }
        if matches!(**second, Layout::Leaf(id) if id == target) {
            let sibling = (**first).clone();
            *self = sibling;
            return true;
        }
        first.remove_inner(target) || second.remove_inner(target)
    }

    pub fn leaves(&self) -> Vec<LeafId> {
        self.rects(Rect::new(0.0, 0.0, 1.0, 1.0), 0.0)
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    }

    #[allow(dead_code)] // kept for callers that only have an id
    pub fn contains(&self, target: LeafId) -> bool {
        match self {
            Layout::Leaf(id) => *id == target,
            Layout::Split { first, second, .. } => first.contains(target) || second.contains(target),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Axis, Layout, Rect};

    fn area() -> Rect {
        Rect::new(0.0, 0.0, 100.0, 80.0)
    }

    #[test]
    fn a_single_leaf_takes_the_whole_area() {
        let layout = Layout::leaf(1);
        assert_eq!(layout.rects(area(), 0.0), vec![(1, area())]);
    }

    #[test]
    fn a_horizontal_split_is_side_by_side() {
        let mut layout = Layout::leaf(1);
        assert!(layout.split(1, Axis::Horizontal, 2, false));
        let rects = layout.rects(area(), 0.0);
        assert_eq!(rects[0], (1, Rect::new(0.0, 0.0, 50.0, 80.0)));
        assert_eq!(rects[1], (2, Rect::new(50.0, 0.0, 50.0, 80.0)));
    }

    #[test]
    fn a_vertical_split_stacks_and_the_gap_is_taken_from_both() {
        let mut layout = Layout::leaf(1);
        layout.split(1, Axis::Vertical, 2, false);
        let rects = layout.rects(area(), 4.0);
        // 80 px minus the 4 px gap, halved; the second pane starts after the gap.
        assert_eq!(rects[0].1.height, 38.0);
        assert_eq!(rects[1].1.y, 42.0);
        assert_eq!(rects[1].1.height, 38.0);
    }

    #[test]
    fn splitting_a_nested_leaf_only_touches_that_pane() {
        let mut layout = Layout::leaf(1);
        layout.split(1, Axis::Horizontal, 2, false);
        layout.split(2, Axis::Vertical, 3, false);
        let rects = layout.rects(area(), 0.0);
        let ids: Vec<_> = rects.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![1, 2, 3]);
        // The left pane is untouched by the split on the right.
        assert_eq!(rects[0].1, Rect::new(0.0, 0.0, 50.0, 80.0));
    }

    #[test]
    fn removing_a_leaf_gives_its_space_to_the_sibling() {
        let mut layout = Layout::leaf(1);
        layout.split(1, Axis::Horizontal, 2, false);
        assert!(layout.remove(2));
        assert_eq!(layout.rects(area(), 0.0), vec![(1, area())]);
    }

    #[test]
    fn removing_a_nested_leaf_collapses_only_its_parent() {
        let mut layout = Layout::leaf(1);
        layout.split(1, Axis::Horizontal, 2, false);
        layout.split(2, Axis::Vertical, 3, false);
        assert!(layout.remove(3));
        assert_eq!(layout.leaves(), vec![1, 2]);
    }

    #[test]
    fn removing_the_only_leaf_reports_that_nothing_is_left() {
        // There is no sibling to collapse into, so the caller closes the tab.
        let mut layout = Layout::leaf(1);
        assert!(!layout.remove(1));
        assert_eq!(layout.leaves(), vec![1]);
    }

    #[test]
    fn removing_a_leaf_that_is_not_there_changes_nothing() {
        let mut layout = Layout::leaf(1);
        assert!(layout.split(1, Axis::Horizontal, 2, false));
        assert!(layout.remove(9));
        assert_eq!(layout.leaves(), vec![1, 2]);
    }

    #[test]
    fn splitting_an_unknown_leaf_does_nothing() {
        let mut layout = Layout::leaf(1);
        assert!(!layout.split(9, Axis::Horizontal, 2, false));
        assert_eq!(layout.leaves(), vec![1]);
    }

    #[test]
    fn a_ratio_outside_the_sane_range_is_clamped() {
        let layout = Layout::Split {
            axis: Axis::Horizontal,
            ratio: 0.99,
            first: Box::new(Layout::leaf(1)),
            second: Box::new(Layout::leaf(2)),
        };
        let rects = layout.rects(area(), 0.0);
        assert_eq!(rects[0].1.width, 90.0);
    }

    #[test]
    fn contains_finds_nested_leaves() {
        let mut layout = Layout::leaf(1);
        layout.split(1, Axis::Horizontal, 2, false);
        assert!(layout.contains(1));
        assert!(layout.contains(2));
        assert!(!layout.contains(3));
    }
}
