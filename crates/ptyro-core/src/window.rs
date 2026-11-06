//! A SHELLS window's panes (SPEC §23.17): one number in SHELLS can hold up to [`MAX_PANES`]
//! sessions, tmux's window and its panes. The layout is a split tree whose leaves are pane
//! numbers (`%3.2` is pane 2 of shell 3), kept by the daemon so every client shows the same one.
//! Shares are thousandths, not floats, so a layout compares exactly.

use serde::{Deserialize, Serialize};

/// The most panes one window holds (the TUI's split limit).
pub const MAX_PANES: usize = 4;
/// A side's smallest share, in thousandths.
const MIN_SHARE: u16 = 100;

/// Which way a pane splits: `right` puts the new pane beside it, `down` below it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDir {
    #[default]
    Right,
    Down,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum PaneTree {
    Pane {
        pane: u32,
    },
    Split {
        dir: SplitDir,
        /// `a`'s share of the space, in thousandths (500: halves).
        share: u16,
        a: Box<PaneTree>,
        b: Box<PaneTree>,
    },
}

/// How a window's panes are arranged, which one has focus, and whether it fills the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowLayout {
    pub root: PaneTree,
    #[serde(default)]
    pub focus: u32,
    #[serde(default)]
    pub zoom: bool,
}

impl PaneTree {
    fn panes(&self, out: &mut Vec<u32>) {
        match self {
            Self::Pane { pane } => out.push(*pane),
            Self::Split { a, b, .. } => {
                a.panes(out);
                b.panes(out);
            }
        }
    }

    /// Split the leaf `at` so `new` sits beside or below it; `false` when `at` isn't here.
    fn split(&mut self, at: u32, new: u32, dir: SplitDir) -> bool {
        match self {
            Self::Pane { pane } if *pane == at => {
                let old = std::mem::replace(self, Self::Pane { pane: 0 });
                *self = Self::Split {
                    dir,
                    share: 500,
                    a: Box::new(old),
                    b: Box::new(Self::Pane { pane: new }),
                };
                true
            }
            Self::Pane { .. } => false,
            Self::Split { a, b, .. } => a.split(at, new, dir) || b.split(at, new, dir),
        }
    }

    /// Without the leaves `keep` says no to (their siblings take the space); `None` when
    /// nothing is left.
    fn prune(self, keep: &mut impl FnMut(u32) -> bool) -> Option<Self> {
        match self {
            Self::Pane { pane } => keep(pane).then_some(Self::Pane { pane }),
            Self::Split { dir, share, a, b } => match (a.prune(keep), b.prune(keep)) {
                (Some(a), Some(b)) => Some(Self::Split {
                    dir,
                    share: share.clamp(MIN_SHARE, 1000 - MIN_SHARE),
                    a: Box::new(a),
                    b: Box::new(b),
                }),
                (one, other) => one.or(other),
            },
        }
    }
}

impl WindowLayout {
    /// One pane, filling the window.
    pub fn single(pane: u32) -> Self {
        Self { root: PaneTree::Pane { pane }, focus: pane, zoom: false }
    }

    /// Its panes, left to right and top to bottom.
    pub fn panes(&self) -> Vec<u32> {
        let mut v = Vec::new();
        self.root.panes(&mut v);
        v
    }

    /// Put `new` beside (`right`) or below (`down`) pane `at` (the last pane when `at` isn't
    /// there), and focus it.
    pub fn insert(&mut self, at: u32, new: u32, dir: SplitDir) {
        let at = if self.panes().contains(&at) { at } else { *self.panes().last().unwrap_or(&at) };
        if !self.root.split(at, new, dir) {
            *self = Self::single(new);
        }
        self.focus = new;
        self.zoom = false;
    }

    /// Make it hold exactly `panes`: leaves no longer there go (their siblings take the space),
    /// a pane listed twice keeps its first place, and panes it lacks are added beside the last.
    pub fn fit(&mut self, panes: &[u32]) {
        if panes.is_empty() {
            return;
        }
        let mut seen = Vec::new();
        let root = std::mem::replace(&mut self.root, PaneTree::Pane { pane: 0 });
        let kept = root.prune(&mut |p| {
            let ok = panes.contains(&p) && !seen.contains(&p);
            seen.push(p);
            ok
        });
        let focus = self.focus;
        match kept {
            Some(r) => self.root = r,
            None => *self = Self::single(panes[0]),
        }
        for &p in panes {
            if !self.panes().contains(&p) {
                let last = *self.panes().last().unwrap_or(&p);
                self.root.split(last, p, SplitDir::Right);
            }
        }
        self.focus = if self.panes().contains(&focus) { focus } else { self.panes()[0] };
    }
}

/// `3`, `3.2`: a shell's number and, when given, its pane (SPEC §23.5, §23.17).
pub fn parse_address(text: &str) -> Option<(u32, Option<u32>)> {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match text.split_once('.') {
        Some((n, p)) if digits(n) && digits(p) => Some((n.parse().ok()?, Some(p.parse().ok()?))),
        None if digits(text) => Some((text.parse().ok()?, None)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_puts_the_new_pane_beside_and_focuses_it() {
        let mut l = WindowLayout::single(1);
        l.insert(1, 2, SplitDir::Right);
        l.insert(2, 3, SplitDir::Down);
        assert_eq!(l.panes(), vec![1, 2, 3]);
        assert_eq!(l.focus, 3);
        let PaneTree::Split { dir, b, .. } = &l.root else { panic!() };
        assert_eq!(*dir, SplitDir::Right);
        assert!(matches!(**b, PaneTree::Split { dir: SplitDir::Down, .. }));
        // An unknown pane: beside the last one.
        l.insert(9, 4, SplitDir::Right);
        assert_eq!(l.panes(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn fit_drops_gone_panes_and_adds_missing_ones() {
        let mut l = WindowLayout::single(1);
        l.insert(1, 2, SplitDir::Right);
        l.insert(2, 3, SplitDir::Down);
        l.focus = 2;
        l.fit(&[1, 3]);
        assert_eq!(l.panes(), vec![1, 3]);
        assert_eq!(l.focus, 1, "the focused pane went: the first has focus");
        l.fit(&[1, 3, 5]);
        assert_eq!(l.panes(), vec![1, 3, 5]);
        l.fit(&[]);
        assert_eq!(l.panes(), vec![1, 3, 5], "nothing to fit to: left alone");
        l.fit(&[7]);
        assert_eq!(l, WindowLayout::single(7));
    }

    #[test]
    fn fit_keeps_one_leaf_per_pane_and_sane_shares() {
        let l = WindowLayout {
            root: PaneTree::Split {
                dir: SplitDir::Down,
                share: 2,
                a: Box::new(PaneTree::Pane { pane: 1 }),
                b: Box::new(PaneTree::Split {
                    dir: SplitDir::Right,
                    share: 500,
                    a: Box::new(PaneTree::Pane { pane: 1 }),
                    b: Box::new(PaneTree::Pane { pane: 2 }),
                }),
            },
            focus: 2,
            zoom: false,
        };
        let mut fitted = l.clone();
        fitted.fit(&[1, 2]);
        assert_eq!(fitted.panes(), vec![1, 2]);
        let PaneTree::Split { share, .. } = fitted.root else { panic!() };
        assert_eq!(share, MIN_SHARE);
    }

    #[test]
    fn addresses() {
        assert_eq!(parse_address("3"), Some((3, None)));
        assert_eq!(parse_address("3.2"), Some((3, Some(2))));
        for bad in ["", "3.", ".2", "3.2.1", "x", "3.x"] {
            assert_eq!(parse_address(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn round_trips_through_json() {
        let mut l = WindowLayout::single(1);
        l.insert(1, 2, SplitDir::Down);
        let j = serde_json::to_string(&l).unwrap();
        assert!(j.contains(r#""dir":"down""#), "{j}");
        assert_eq!(serde_json::from_str::<WindowLayout>(&j).unwrap(), l);
    }
}
