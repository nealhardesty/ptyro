//! Pane layout (M2.10): a binary split tree of up to [`MAX_PANES`] panes with 1-cell dividers.
//! Pure geometry; the app decides what each pane shows.

use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};

pub type PaneId = u32;
pub const MAX_PANES: usize = 4;
const MIN_RATIO: f32 = 0.1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dir {
    /// Side by side: `a` left, `b` right.
    Right,
    /// Stacked: `a` above, `b` below.
    Down,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Node {
    Pane { id: PaneId },
    Split { dir: Dir, ratio: f32, a: Box<Node>, b: Box<Node> },
}

/// A divider between the two halves of a split: where it is, and how to drag it.
#[derive(Debug, Clone, PartialEq)]
pub struct Divider {
    /// Which split (left/right turns from the root; `false` = `a`).
    pub path: Vec<bool>,
    pub dir: Dir,
    /// The divider's own cells.
    pub rect: Rect,
    /// The whole split's area (for turning a mouse position into a ratio).
    pub area: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toward {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub root: Node,
    next: PaneId,
}

impl Default for Layout {
    fn default() -> Self {
        Self { root: Node::Pane { id: 1 }, next: 2 }
    }
}

fn split_rect(area: Rect, dir: Dir, ratio: f32) -> (Rect, Rect, Rect) {
    match dir {
        Dir::Right => {
            let usable = area.width.saturating_sub(1);
            let a = ((f32::from(usable) * ratio).round() as u16)
                .clamp(1.min(usable), usable.saturating_sub(1).max(1));
            let a_rect = Rect { width: a, ..area };
            let div = Rect { x: area.x + a, width: 1.min(area.width.saturating_sub(a)), ..area };
            let b_rect = Rect {
                x: div.x + div.width,
                width: area.width.saturating_sub(a + div.width),
                ..area
            };
            (a_rect, div, b_rect)
        }
        Dir::Down => {
            let usable = area.height.saturating_sub(1);
            let a = ((f32::from(usable) * ratio).round() as u16)
                .clamp(1.min(usable), usable.saturating_sub(1).max(1));
            let a_rect = Rect { height: a, ..area };
            let div = Rect { y: area.y + a, height: 1.min(area.height.saturating_sub(a)), ..area };
            let b_rect = Rect {
                y: div.y + div.height,
                height: area.height.saturating_sub(a + div.height),
                ..area
            };
            (a_rect, div, b_rect)
        }
    }
}

impl Node {
    fn panes(&self, out: &mut Vec<PaneId>) {
        match self {
            Self::Pane { id } => out.push(*id),
            Self::Split { a, b, .. } => {
                a.panes(out);
                b.panes(out);
            }
        }
    }

    fn rects(
        &self,
        area: Rect,
        out: &mut Vec<(PaneId, Rect)>,
        divs: &mut Vec<Divider>,
        path: &mut Vec<bool>,
    ) {
        match self {
            Self::Pane { id } => out.push((*id, area)),
            Self::Split { dir, ratio, a, b } => {
                let (ra, div, rb) = split_rect(area, *dir, *ratio);
                divs.push(Divider { path: path.clone(), dir: *dir, rect: div, area });
                path.push(false);
                a.rects(ra, out, divs, path);
                path.pop();
                path.push(true);
                b.rects(rb, out, divs, path);
                path.pop();
            }
        }
    }

    fn contains(&self, pane: PaneId) -> bool {
        match self {
            Self::Pane { id } => *id == pane,
            Self::Split { a, b, .. } => a.contains(pane) || b.contains(pane),
        }
    }

    fn at_mut(&mut self, path: &[bool]) -> Option<&mut Node> {
        match (path.split_first(), self) {
            (None, n) => Some(n),
            (Some((side, rest)), Self::Split { a, b, .. }) => {
                if *side { b } else { a }.at_mut(rest)
            }
            _ => None,
        }
    }
}

impl Layout {
    pub fn panes(&self) -> Vec<PaneId> {
        let mut v = Vec::new();
        self.root.panes(&mut v);
        v
    }

    pub fn first(&self) -> PaneId {
        self.panes()[0]
    }

    pub fn contains(&self, pane: PaneId) -> bool {
        self.root.contains(pane)
    }

    pub fn rects(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        self.geometry(area).0
    }

    pub fn dividers(&self, area: Rect) -> Vec<Divider> {
        self.geometry(area).1
    }

    fn geometry(&self, area: Rect) -> (Vec<(PaneId, Rect)>, Vec<Divider>) {
        let (mut rects, mut divs) = (Vec::new(), Vec::new());
        self.root.rects(area, &mut rects, &mut divs, &mut Vec::new());
        (rects, divs)
    }

    /// Split `pane`; the new pane goes right of or below it. `None` at [`MAX_PANES`].
    pub fn split(&mut self, pane: PaneId, dir: Dir) -> Option<PaneId> {
        if self.panes().len() >= MAX_PANES {
            return None;
        }
        let new = self.next;
        fn go(n: &mut Node, pane: PaneId, dir: Dir, new: PaneId) -> bool {
            match n {
                Node::Pane { id } if *id == pane => {
                    let old = std::mem::replace(n, Node::Pane { id: 0 });
                    *n = Node::Split {
                        dir,
                        ratio: 0.5,
                        a: Box::new(old),
                        b: Box::new(Node::Pane { id: new }),
                    };
                    true
                }
                Node::Pane { .. } => false,
                Node::Split { a, b, .. } => go(a, pane, dir, new) || go(b, pane, dir, new),
            }
        }
        if go(&mut self.root, pane, dir, new) {
            self.next += 1;
            Some(new)
        } else {
            None
        }
    }

    /// Remove `pane`; its sibling takes the space. The last pane can't be closed.
    pub fn close(&mut self, pane: PaneId) -> bool {
        fn go(n: &mut Node, pane: PaneId) -> bool {
            let Node::Split { a, b, .. } = n else { return false };
            let keep = if matches!(**a, Node::Pane { id } if id == pane) {
                Some(std::mem::replace(&mut **b, Node::Pane { id: 0 }))
            } else if matches!(**b, Node::Pane { id } if id == pane) {
                Some(std::mem::replace(&mut **a, Node::Pane { id: 0 }))
            } else {
                None
            };
            match keep {
                Some(k) => {
                    *n = k;
                    true
                }
                None => go(a, pane) || go(b, pane),
            }
        }
        go(&mut self.root, pane)
    }

    /// The pane next to `pane` in a direction: overlapping on the other axis, nearest edge first.
    pub fn neighbor(&self, area: Rect, pane: PaneId, toward: Toward) -> Option<PaneId> {
        let rects = self.rects(area);
        let me = rects.iter().find(|(id, _)| *id == pane)?.1;
        let overlap = |a0: u16, a1: u16, b0: u16, b1: u16| a0.max(b0) < a1.min(b1);
        rects
            .iter()
            .filter(|(id, _)| *id != pane)
            .filter_map(|&(id, r)| {
                let (ok, gap) = match toward {
                    Toward::Left => (
                        r.right() <= me.x && overlap(r.y, r.bottom(), me.y, me.bottom()),
                        me.x.saturating_sub(r.right()),
                    ),
                    Toward::Right => (
                        r.x >= me.right() && overlap(r.y, r.bottom(), me.y, me.bottom()),
                        r.x.saturating_sub(me.right()),
                    ),
                    Toward::Up => (
                        r.bottom() <= me.y && overlap(r.x, r.right(), me.x, me.right()),
                        me.y.saturating_sub(r.bottom()),
                    ),
                    Toward::Down => (
                        r.y >= me.bottom() && overlap(r.x, r.right(), me.x, me.right()),
                        r.y.saturating_sub(me.bottom()),
                    ),
                };
                ok.then_some((gap, id))
            })
            .min()
            .map(|(_, id)| id)
    }

    /// Grow (`delta` > 0) or shrink `pane` along `dir` by about `delta` cells, using the nearest
    /// enclosing split of that direction.
    pub fn resize(&mut self, area: Rect, pane: PaneId, dir: Dir, delta: i16) -> bool {
        let divs = self.dividers(area);
        // Deepest split of this direction that contains the pane.
        let mut best: Option<(Vec<bool>, bool, Rect)> = None;
        for d in divs.iter().filter(|d| d.dir == dir) {
            let Some(node) = self.node(&d.path) else { continue };
            let Node::Split { a, b, .. } = node else { continue };
            let side = if a.contains(pane) {
                false
            } else if b.contains(pane) {
                true
            } else {
                continue;
            };
            if best.as_ref().is_none_or(|(p, _, _)| d.path.len() > p.len()) {
                best = Some((d.path.clone(), side, d.area));
            }
        }
        let Some((path, in_b, parent)) = best else { return false };
        let size = f32::from(match dir {
            Dir::Right => parent.width,
            Dir::Down => parent.height,
        })
        .max(1.0);
        let step = f32::from(delta) / size * if in_b { -1.0 } else { 1.0 };
        if let Some(Node::Split { ratio, .. }) = self.root.at_mut(&path) {
            *ratio = (*ratio + step).clamp(MIN_RATIO, 1.0 - MIN_RATIO);
            return true;
        }
        false
    }

    /// Set a split's ratio from a mouse position (divider drag).
    pub fn drag(&mut self, d: &Divider, x: u16, y: u16) {
        let r = match d.dir {
            Dir::Right => f32::from(x.saturating_sub(d.area.x)) / f32::from(d.area.width.max(1)),
            Dir::Down => f32::from(y.saturating_sub(d.area.y)) / f32::from(d.area.height.max(1)),
        };
        if let Some(Node::Split { ratio, .. }) = self.root.at_mut(&d.path) {
            *ratio = r.clamp(MIN_RATIO, 1.0 - MIN_RATIO);
        }
    }

    fn node(&self, path: &[bool]) -> Option<&Node> {
        let mut n = &self.root;
        for side in path {
            let Node::Split { a, b, .. } = n else { return None };
            n = if *side { b } else { a };
        }
        Some(n)
    }

    /// Rebuild `next` after deserializing (it isn't trusted).
    pub fn sanitize(mut self) -> Self {
        let max = self.panes().into_iter().max().unwrap_or(0);
        self.next = self.next.max(max + 1);
        if self.panes().len() > MAX_PANES {
            return Self::default();
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect { x: 0, y: 0, width: 101, height: 41 };

    #[test]
    fn split_close_and_geometry() {
        let mut l = Layout::default();
        let p2 = l.split(1, Dir::Right).unwrap();
        let p3 = l.split(p2, Dir::Down).unwrap();
        assert_eq!(l.panes(), vec![1, p2, p3]);
        let r = l.rects(AREA);
        assert_eq!(r[0].1, Rect::new(0, 0, 50, 41));
        assert_eq!(r[1].1, Rect::new(51, 0, 50, 20));
        assert_eq!(r[2].1, Rect::new(51, 21, 50, 20));
        assert_eq!(l.dividers(AREA).len(), 2);
        assert_eq!(l.neighbor(AREA, 1, Toward::Right), Some(p2));
        assert_eq!(l.neighbor(AREA, p3, Toward::Up), Some(p2));
        assert_eq!(l.neighbor(AREA, p3, Toward::Left), Some(1));
        assert_eq!(l.neighbor(AREA, 1, Toward::Left), None);
        assert!(l.close(p2));
        assert_eq!(l.panes(), vec![1, p3]);
        assert!(l.close(p3));
        assert!(!l.close(1), "the last pane stays");
    }

    #[test]
    fn at_most_four_panes() {
        let mut l = Layout::default();
        let mut last = 1;
        for _ in 0..3 {
            last = l.split(last, Dir::Right).unwrap();
        }
        assert!(l.split(last, Dir::Down).is_none());
    }

    #[test]
    fn resize_and_drag() {
        let mut l = Layout::default();
        let p2 = l.split(1, Dir::Right).unwrap();
        assert!(l.resize(AREA, 1, Dir::Right, 10));
        assert!(l.rects(AREA)[0].1.width > 55);
        assert!(l.resize(AREA, p2, Dir::Right, 20), "growing b moves the divider left");
        assert!(l.rects(AREA)[0].1.width < 50);
        assert!(!l.resize(AREA, 1, Dir::Down, 5), "no stacked split to resize");
        let d = l.dividers(AREA)[0].clone();
        l.drag(&d, 75, 0);
        assert!((l.rects(AREA)[0].1.width as i32 - 75).abs() <= 1);
    }

    #[test]
    fn round_trips_through_json() {
        let mut l = Layout::default();
        l.split(1, Dir::Down);
        let j = serde_json::to_string(&l).unwrap();
        let back: Layout = serde_json::from_str::<Layout>(&j).unwrap().sanitize();
        assert_eq!(back, l);
    }
}
