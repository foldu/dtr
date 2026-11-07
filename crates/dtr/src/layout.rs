//! The pane layout: a binary tree of splits, with leaves that say nothing about
//! what they contain.
//!
//! Nothing here knows what a terminal is. A leaf is an id; what that id maps to
//! — a terminal session today, a file explorer or a log viewer later — is the
//! caller's business. That separation is the whole point: adding a widget pane
//! should be a new variant in the caller's map, not surgery on the geometry.
//!
//! Everything the pointer can do to a split lives here too: which divider is
//! under a point, what moving it does to the ratio, and which pane is next in a
//! direction. Those are geometry questions, and the answers have to come from
//! the same arithmetic that draws the panes — a drag that computed its own
//! rectangles would put the divider where the picture is not.

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

/// The smallest share of a split either side may keep.
///
/// A pane narrower than a tenth of its split is a line of text, not a terminal,
/// and the clamp is the tree's rather than the drag's so a stored ratio and a
/// dragged one cannot disagree. Giving one pane the whole window is what zoom
/// is for.
pub const MIN_RATIO: f32 = 0.1;

/// A direction a keypress can move focus or a divider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    /// Which kind of split this direction crosses. `Left` and `Right` cross a
    /// horizontal split — the one whose panes are side by side.
    fn axis(self) -> Axis {
        match self {
            Self::Left | Self::Right => Axis::Horizontal,
            Self::Up | Self::Down => Axis::Vertical,
        }
    }

    /// The other way, which is what a resize falls back to when the focused pane
    /// has no divider on the side asked for — there is only one border to move,
    /// and moving it the other way is the same key doing something.
    pub fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::Up => Self::Down,
            Self::Down => Self::Up,
        }
    }
}

/// One gap between two panes, and the split it belongs to.
///
/// `path` is where the split sits in the tree — `false` took the first child,
/// `true` the second — which is how a drag addresses the same split on every
/// frame without holding a borrow of the tree. `area` is the whole rectangle the
/// split divides, gap included, because that is what turns a pointer position
/// into a ratio; `strip` is the gap itself, which is what the pointer has to
/// land on.
#[derive(Clone, Debug, PartialEq)]
pub struct Divider {
    pub path: Vec<bool>,
    pub axis: Axis,
    pub area: Rect,
    pub strip: Rect,
}

/// The gap is `gap` units wide, so a press that has to land inside it exactly is
/// a press nobody can make. This is how far either side still counts.
const TOUCH: f32 = 0.5;

/// The two rectangles a split makes of `area`, with the gap taken from between
/// them.
fn halves(axis: Axis, ratio: f32, area: Rect, gap: f32) -> (Rect, Rect) {
    let ratio = ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO);
    match axis {
        Axis::Horizontal => {
            let first = (area.width - gap) * ratio;
            (
                Rect::new(area.x, area.y, first, area.height),
                Rect::new(
                    area.x + first + gap,
                    area.y,
                    area.width - first - gap,
                    area.height,
                ),
            )
        }
        Axis::Vertical => {
            let first = (area.height - gap) * ratio;
            (
                Rect::new(area.x, area.y, area.width, first),
                Rect::new(
                    area.x,
                    area.y + first + gap,
                    area.width,
                    area.height - first - gap,
                ),
            )
        }
    }
}

/// The gap a split leaves between its halves, which is also the divider the
/// pointer grabs. `first` is the rectangle the first child got.
fn gap_of(axis: Axis, area: Rect, first: Rect, gap: f32) -> Rect {
    match axis {
        Axis::Horizontal => Rect::new(first.x + first.width, area.y, gap, area.height),
        Axis::Vertical => Rect::new(area.x, first.y + first.height, area.width, gap),
    }
}

/// Whether two edges are the same edge.
///
/// Rectangles that share an edge are computed from different arithmetic, so
/// `==` on those floats is a coin toss; half a pixel is the same edge.
fn touches(a: f32, b: f32) -> bool {
    (a - b).abs() <= TOUCH
}

/// How much two spans on one axis overlap.
fn overlap(start: f32, length: f32, other: f32, other_length: f32) -> f32 {
    (start + length).min(other + other_length) - start.max(other)
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
                let (a, b) = halves(*axis, *ratio, area, gap);
                first.collect(a, gap, out);
                second.collect(b, gap, out);
            }
        }
    }

    /// Every leaf that is on screen, which is one leaf when the tab is zoomed.
    ///
    /// The rule lives here rather than in the caller because the *pty's* size
    /// and the painted rectangle have to agree: a hidden pane that kept being
    /// resized would come back from a zoom with the wrong grid, and one that is
    /// drawn across the whole window but sized to its old half wraps every line
    /// in the wrong place.
    pub fn visible(&self, area: Rect, gap: f32, zoom: Option<LeafId>) -> Vec<(LeafId, Rect)> {
        match zoom {
            Some(id) if self.contains(id) => vec![(id, area)],
            _ => self.rects(area, gap),
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

    pub fn contains(&self, target: LeafId) -> bool {
        match self {
            Layout::Leaf(id) => *id == target,
            Layout::Split { first, second, .. } => {
                first.contains(target) || second.contains(target)
            }
        }
    }

    /// The divider under a point, if the point is on one.
    ///
    /// `slop` widens every divider along the axis it moves, because a gap a few
    /// points wide is not a target a hand can hit — and the cursor saying
    /// "resizable" while the press does nothing is worse than no cursor at all.
    pub fn divider_at(&self, area: Rect, gap: f32, x: f32, y: f32, slop: f32) -> Option<Divider> {
        let mut path = Vec::new();
        self.find_divider(area, gap, x, y, slop, &mut path)
    }

    fn find_divider(
        &self,
        area: Rect,
        gap: f32,
        x: f32,
        y: f32,
        slop: f32,
        path: &mut Vec<bool>,
    ) -> Option<Divider> {
        let Layout::Split {
            axis,
            ratio,
            first,
            second,
        } = self
        else {
            return None;
        };
        let (a, b) = halves(*axis, *ratio, area, gap);
        let strip = gap_of(*axis, area, a, gap);
        // A divider only moves along its own axis, so the slop belongs there and
        // the other axis is bounded by the split it divides.
        let (along, across) = match axis {
            Axis::Horizontal => (x, y),
            Axis::Vertical => (y, x),
        };
        let (start, end) = match axis {
            Axis::Horizontal => (strip.x, strip.x + strip.width),
            Axis::Vertical => (strip.y, strip.y + strip.height),
        };
        let (low, high) = match axis {
            Axis::Horizontal => (area.y, area.y + area.height),
            Axis::Vertical => (area.x, area.x + area.width),
        };
        if along >= start - slop && along <= end + slop && across >= low && across <= high {
            return Some(Divider {
                path: path.clone(),
                axis: *axis,
                area,
                strip,
            });
        }
        // Not this split's gap: the point is in one of the halves, or nowhere.
        for (second_half, child) in [(false, first), (true, second)] {
            let half = if second_half { b } else { a };
            if !half.contains(x, y) {
                continue;
            }
            path.push(second_half);
            let found = child.find_divider(half, gap, x, y, slop, path);
            path.pop();
            return found;
        }
        None
    }

    /// Every divider in the tree, in tree order.
    pub fn dividers(&self, area: Rect, gap: f32) -> Vec<Divider> {
        let mut out = Vec::new();
        self.collect_dividers(area, gap, &mut Vec::new(), &mut out);
        out
    }

    fn collect_dividers(&self, area: Rect, gap: f32, path: &mut Vec<bool>, out: &mut Vec<Divider>) {
        let Layout::Split {
            axis,
            ratio,
            first,
            second,
        } = self
        else {
            return;
        };
        let (a, b) = halves(*axis, *ratio, area, gap);
        let strip = gap_of(*axis, area, a, gap);
        out.push(Divider {
            path: path.clone(),
            axis: *axis,
            area,
            strip,
        });
        path.push(false);
        first.collect_dividers(a, gap, path, out);
        path.pop();
        path.push(true);
        second.collect_dividers(b, gap, path, out);
        path.pop();
    }

    /// The divider that borders `leaf` on one side, if its edge is one.
    ///
    /// This is what a resize key moves: the nearest split touching that edge of
    /// the focused pane, so the arrow resizes the pane you are looking at rather
    /// than whichever split happens to be highest in the tree.
    pub fn divider_toward(
        &self,
        area: Rect,
        gap: f32,
        leaf: LeafId,
        direction: Direction,
    ) -> Option<Divider> {
        let rect = self
            .rects(area, gap)
            .into_iter()
            .find(|(id, _)| *id == leaf)?
            .1;
        self.dividers(area, gap)
            .into_iter()
            .filter(|divider| divider.axis == direction.axis())
            .filter(|divider| match direction {
                Direction::Left => {
                    touches(divider.strip.x + divider.strip.width, rect.x)
                        && overlap(divider.strip.y, divider.strip.height, rect.y, rect.height) > 0.0
                }
                Direction::Right => {
                    touches(divider.strip.x, rect.x + rect.width)
                        && overlap(divider.strip.y, divider.strip.height, rect.y, rect.height) > 0.0
                }
                Direction::Up => {
                    touches(divider.strip.y + divider.strip.height, rect.y)
                        && overlap(divider.strip.x, divider.strip.width, rect.x, rect.width) > 0.0
                }
                Direction::Down => {
                    touches(divider.strip.y, rect.y + rect.height)
                        && overlap(divider.strip.x, divider.strip.width, rect.x, rect.width) > 0.0
                }
            })
            // Deepest wins: the split that most immediately borders the pane is
            // the one a resize is about.
            .max_by_key(|divider| divider.path.len())
    }

    /// The ratio this path's split is drawn at.
    pub fn ratio(&self, path: &[bool]) -> Option<f32> {
        let mut node = self;
        for second in path {
            let Layout::Split {
                first, second: b, ..
            } = node
            else {
                return None;
            };
            node = if *second { &**b } else { &**first };
        }
        match node {
            Layout::Split { ratio, .. } => Some(*ratio),
            Layout::Leaf(_) => None,
        }
    }

    /// Move one split's divider, clamped to what the tree allows.
    ///
    /// Returns whether the path named a split, which is how a keypress can tell
    /// that a resize changed nothing and skip the repaint.
    pub fn set_ratio(&mut self, path: &[bool], ratio: f32) -> bool {
        let ratio = ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO);
        let mut node = self;
        for second in path {
            let Layout::Split {
                first, second: b, ..
            } = node
            else {
                return false;
            };
            node = if *second { &mut **b } else { &mut **first };
        }
        match node {
            Layout::Split { ratio: current, .. } => {
                *current = ratio;
                true
            }
            Layout::Leaf(_) => false,
        }
    }

    /// The nearest pane in a direction, for keyboard focus movement.
    ///
    /// Nearest rather than tree-adjacent: with three columns the pane one column
    /// over is the answer, not whichever sibling a tree walk happens to reach,
    /// and a pane that only touches the corner is not in any direction at all
    /// unless the two rectangles actually share a span.
    pub fn neighbour(
        &self,
        area: Rect,
        gap: f32,
        leaf: LeafId,
        direction: Direction,
    ) -> Option<LeafId> {
        let rects = self.rects(area, gap);
        let from = rects.iter().find(|(id, _)| *id == leaf)?.1;
        let mut best: Option<(LeafId, f32, f32)> = None;
        for (id, rect) in rects {
            if id == leaf {
                continue;
            }
            let (distance, shared) = match direction {
                Direction::Left => {
                    if rect.x + rect.width > from.x + TOUCH {
                        continue;
                    }
                    (
                        from.x - (rect.x + rect.width),
                        overlap(rect.y, rect.height, from.y, from.height),
                    )
                }
                Direction::Right => {
                    if rect.x < from.x + from.width - TOUCH {
                        continue;
                    }
                    (
                        rect.x - (from.x + from.width),
                        overlap(rect.y, rect.height, from.y, from.height),
                    )
                }
                Direction::Up => {
                    if rect.y + rect.height > from.y + TOUCH {
                        continue;
                    }
                    (
                        from.y - (rect.y + rect.height),
                        overlap(rect.x, rect.width, from.x, from.width),
                    )
                }
                Direction::Down => {
                    if rect.y < from.y + from.height - TOUCH {
                        continue;
                    }
                    (
                        rect.y - (from.y + from.height),
                        overlap(rect.x, rect.width, from.x, from.width),
                    )
                }
            };
            if shared <= 0.0 {
                continue;
            }
            let better = match best {
                None => true,
                Some((_, best_distance, best_shared)) => {
                    distance < best_distance - TOUCH
                        || (distance <= best_distance + TOUCH && shared > best_shared)
                }
            };
            if better {
                best = Some((id, distance, shared));
            }
        }
        best.map(|(id, _, _)| id)
    }
}

#[cfg(test)]
mod tests {
    use super::{Axis, Direction, Layout, MIN_RATIO, Rect};

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

    /// A horizontal split's divider is the gap itself, and it is addressed from
    /// the root.
    #[test]
    fn a_divider_sits_in_the_gap() {
        let mut layout = Layout::leaf(1);
        layout.split(1, Axis::Horizontal, 2, false);
        let found = layout
            .divider_at(area(), 4.0, 50.0, 40.0, 0.0)
            .expect("the gap is the divider");
        assert_eq!(found.axis, Axis::Horizontal);
        assert_eq!(found.path, Vec::<bool>::new());
        assert_eq!(found.area, area());
        assert_eq!(found.strip, Rect::new(48.0, 0.0, 4.0, 80.0));
    }

    /// A pane is not a divider, however close a hand thinks the edge is.
    #[test]
    fn a_pane_is_not_a_divider() {
        let mut layout = Layout::leaf(1);
        layout.split(1, Axis::Horizontal, 2, false);
        assert!(layout.divider_at(area(), 4.0, 10.0, 40.0, 0.0).is_none());
        assert!(layout.divider_at(area(), 4.0, 96.0, 40.0, 0.0).is_none());
    }

    /// A four point gap is not a target, so the press has a few points of
    /// tolerance either side — and no more than that.
    #[test]
    fn a_divider_can_be_grabbed_from_either_side() {
        let mut layout = Layout::leaf(1);
        layout.split(1, Axis::Horizontal, 2, false);
        assert!(layout.divider_at(area(), 4.0, 46.0, 40.0, 2.0).is_some());
        assert!(layout.divider_at(area(), 4.0, 54.0, 40.0, 2.0).is_some());
        assert!(layout.divider_at(area(), 4.0, 45.0, 40.0, 2.0).is_none());
        assert!(layout.divider_at(area(), 4.0, 55.0, 40.0, 2.0).is_none());
    }

    /// A nested split is addressed by the choices that lead to it, so a drag can
    /// name the same split on every frame.
    #[test]
    fn a_nested_divider_is_addressed_by_its_path() {
        let mut layout = Layout::leaf(1);
        layout.split(1, Axis::Horizontal, 2, false);
        layout.split(2, Axis::Vertical, 3, false);
        let found = layout
            .divider_at(area(), 4.0, 70.0, 40.0, 0.0)
            .expect("the inner gap");
        assert_eq!(found.axis, Axis::Vertical);
        assert_eq!(found.path, vec![true]);
        assert_eq!(found.area, Rect::new(52.0, 0.0, 48.0, 80.0));
    }

    #[test]
    fn dragging_a_divider_moves_it() {
        let mut layout = Layout::leaf(1);
        layout.split(1, Axis::Horizontal, 2, false);
        assert!(layout.set_ratio(&[], 0.25));
        assert_eq!(layout.ratio(&[]), Some(0.25));
        let rects = layout.rects(area(), 0.0);
        assert_eq!(rects[0].1.width, 25.0);
        assert_eq!(rects[1].1.x, 25.0);
    }

    /// A stored ratio and a dragged one are clamped in the same place, so no
    /// drag can leave a pane too narrow to be a terminal.
    #[test]
    fn the_ratio_never_leaves_the_middle() {
        let mut layout = Layout::leaf(1);
        layout.split(1, Axis::Horizontal, 2, false);
        assert!(layout.set_ratio(&[], 0.0));
        assert_eq!(layout.ratio(&[]), Some(MIN_RATIO));
        assert_eq!(layout.rects(area(), 0.0)[0].1.width, 10.0);
        assert!(layout.set_ratio(&[], 5.0));
        assert_eq!(layout.ratio(&[]), Some(1.0 - MIN_RATIO));
    }

    #[test]
    fn a_path_that_is_not_a_split_changes_nothing() {
        let mut layout = Layout::leaf(1);
        layout.split(1, Axis::Horizontal, 2, false);
        assert!(!layout.set_ratio(&[true], 0.5));
        assert_eq!(layout.ratio(&[true]), None);
        assert!(!layout.set_ratio(&[false, true], 0.5));
    }

    /// Focus moves to the pane in that direction and refuses to invent one at
    /// the edge of the window.
    #[test]
    fn focus_takes_the_neighbour_in_a_direction() {
        let mut layout = Layout::leaf(1);
        layout.split(1, Axis::Horizontal, 2, false);
        layout.split(2, Axis::Vertical, 3, false);
        assert_eq!(layout.neighbour(area(), 0.0, 1, Direction::Right), Some(2));
        assert_eq!(layout.neighbour(area(), 0.0, 2, Direction::Left), Some(1));
        assert_eq!(layout.neighbour(area(), 0.0, 2, Direction::Down), Some(3));
        assert_eq!(layout.neighbour(area(), 0.0, 3, Direction::Up), Some(2));
        assert_eq!(layout.neighbour(area(), 0.0, 1, Direction::Left), None);
        assert_eq!(layout.neighbour(area(), 0.0, 1, Direction::Up), None);
        assert_eq!(layout.neighbour(area(), 0.0, 9, Direction::Left), None);
    }

    /// A stack of three: pressing down from the top goes to the pane below it,
    /// not to the far end of the column.
    #[test]
    fn focus_takes_the_nearest_pane() {
        let mut layout = Layout::leaf(1);
        layout.split(1, Axis::Vertical, 2, false);
        layout.split(2, Axis::Vertical, 3, false);
        assert_eq!(layout.neighbour(area(), 0.0, 1, Direction::Down), Some(2));
        assert_eq!(layout.neighbour(area(), 0.0, 3, Direction::Up), Some(2));
        assert_eq!(layout.neighbour(area(), 0.0, 1, Direction::Up), None);
    }

    /// A pane that spans the full height has nothing above or below it, even
    /// though other panes start and end beside it.
    #[test]
    fn a_pane_that_spans_the_full_height_has_no_vertical_neighbour() {
        let mut layout = Layout::leaf(1);
        layout.split(1, Axis::Horizontal, 2, false);
        layout.split(1, Axis::Vertical, 3, false);
        // The left column is `1` over `3`; `2` is the whole right half.
        assert_eq!(layout.neighbour(area(), 0.0, 2, Direction::Up), None);
        assert_eq!(layout.neighbour(area(), 0.0, 2, Direction::Down), None);
        // Both halves of the column are equally far away, so the tie goes to the
        // first in tree order rather than to a coin toss.
        assert_eq!(layout.neighbour(area(), 0.0, 2, Direction::Left), Some(1));
    }

    /// A resize moves the divider that borders the pane, and the window edge is
    /// not one.
    #[test]
    fn a_resize_finds_the_divider_at_the_pane_edge() {
        let mut layout = Layout::leaf(1);
        layout.split(1, Axis::Horizontal, 2, false);
        let from_left = layout
            .divider_toward(area(), 0.0, 1, Direction::Right)
            .expect("the gap is on 1's right");
        assert_eq!(from_left.path, Vec::<bool>::new());
        assert_eq!(from_left.axis, Axis::Horizontal);
        assert_eq!(
            layout.divider_toward(area(), 0.0, 1, Direction::Left),
            None,
            "the window edge is not a divider"
        );
        let from_right = layout
            .divider_toward(area(), 0.0, 2, Direction::Left)
            .expect("the gap is on 2's left");
        assert_eq!(from_right.path, Vec::<bool>::new());
        assert_eq!(layout.divider_toward(area(), 0.0, 2, Direction::Up), None);
    }

    /// With two splits around one pane, each arrow picks the one on that side.
    #[test]
    fn a_nested_resize_picks_the_divider_on_that_side() {
        let mut layout = Layout::leaf(1);
        layout.split(1, Axis::Horizontal, 2, false);
        layout.split(2, Axis::Vertical, 3, false);
        let left = layout
            .divider_toward(area(), 0.0, 2, Direction::Left)
            .expect("the root divider");
        assert_eq!(left.path, Vec::<bool>::new());
        assert_eq!(left.axis, Axis::Horizontal);
        let down = layout
            .divider_toward(area(), 0.0, 2, Direction::Down)
            .expect("the inner divider");
        assert_eq!(down.path, vec![true]);
        assert_eq!(down.axis, Axis::Vertical);
        assert_eq!(
            layout.divider_toward(area(), 0.0, 2, Direction::Right),
            None
        );
    }

    /// Zoom is one pane in the whole area, and it is the same answer the pty's
    /// size has to come from.
    #[test]
    fn zoom_shows_one_pane_in_the_whole_area() {
        let mut layout = Layout::leaf(1);
        layout.split(1, Axis::Horizontal, 2, false);
        assert_eq!(layout.visible(area(), 4.0, None).len(), 2);
        assert_eq!(layout.visible(area(), 4.0, Some(2)), vec![(2, area())]);
        // A leaf that is not in this tree is not a zoom, so a stale id shows
        // the layout rather than an empty window.
        assert_eq!(layout.visible(area(), 4.0, Some(9)).len(), 2);
    }
}
