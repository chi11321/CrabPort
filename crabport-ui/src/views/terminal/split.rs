//! Terminal split-pane layout.
//!
//! Each terminal tab owns a [`SplitTree`] describing how its panes are
//! arranged. A pane is a single [`TerminalView`] entity. The tree is a
//! binary tree of horizontal/vertical splits; each split stores a divider
//! ratio (the fraction of space the *first* child gets, in `[0.05, 0.95]`).
//!
//! ```text
//! ┌──────────┬──────────┐
//! │  pane A  │  pane B  │   SplitDir::Vertical, ratio ~0.5
//! │          │          │   (divider runs vertically; children are side-by-side)
//! └──────────┴──────────┘
//!
//! ┌─────────────────────┐
//! │      pane A         │   SplitDir::Horizontal, ratio ~0.5
//! ├─────────────────────┤   (divider runs horizontally; children are stacked)
//! │      pane B         │
//! └─────────────────────┘
//! ```
//!
//! Naming follows tmux: `SplitDir::Vertical` splits the pane *vertically*
//! (into left/right), `SplitDir::Horizontal` splits it *horizontally* (into
//! top/bottom).
//!
//! The active pane (the one that last received focus) is tracked by id so
//! the toolbar / right-hand panel / keybindings keep operating on the pane
//! the user is interacting with.

/// A binary split direction.
///
/// `Vertical` → children laid out left/right (a vertical divider line).
/// `Horizontal` → children laid out top/bottom (a horizontal divider line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitDir {
    Vertical,
    Horizontal,
}

impl SplitDir {
    /// The perpendicular direction — used when nesting splits so a
    /// "split right" inside a vertical split becomes a horizontal split.
    pub fn perpendicular(self) -> Self {
        match self {
            SplitDir::Vertical => SplitDir::Horizontal,
            SplitDir::Horizontal => SplitDir::Vertical,
        }
    }
}

/// A node in the split tree. A leaf holds a single terminal pane; a split
/// holds two children plus a divider ratio.
#[derive(Clone, Debug)]
pub enum SplitNode {
    /// A terminal pane. The `u64` is a stable pane id (unique within the
    /// tab), used to key focus + the pane registry.
    Pane(u64),
    /// An interior split.
    Split {
        dir: SplitDir,
        /// Fraction `[0.05, 0.95]` of the total extent assigned to the
        /// first child. The second child gets the remainder.
        ratio: f32,
        a: Box<SplitNode>,
        b: Box<SplitNode>,
    },
}

impl SplitNode {
    /// Recursively collect every pane id under this node.
    pub fn pane_ids(&self) -> Vec<u64> {
        match self {
            SplitNode::Pane(id) => vec![*id],
            SplitNode::Split { a, b, .. } => {
                let mut v = a.pane_ids();
                v.extend(b.pane_ids());
                v
            }
        }
    }

    /// Find the pane id whose [`TerminalView`] matches `target`, if any.
    pub fn find_pane(&self, target: u64) -> bool {
        match self {
            SplitNode::Pane(id) => *id == target,
            SplitNode::Split { a, b, .. } => a.find_pane(target) || b.find_pane(target),
        }
    }

    /// Remove the pane with `id` from the tree, returning the replacement
    /// node. If the pane was one half of a split, the *other* half takes its
    /// place (the split collapses). Returns `None` only when the tree was a
    /// single pane matching `id` (i.e. the tab is now empty).
    pub fn remove_pane(self, id: u64) -> Option<SplitNode> {
        match self {
            SplitNode::Pane(cur) => {
                if cur == id {
                    None
                } else {
                    Some(SplitNode::Pane(cur))
                }
            }
            SplitNode::Split { dir, ratio, a, b } => {
                if a.find_pane(id) {
                    let a = a.remove_pane(id);
                    match a {
                        Some(new_a) => Some(SplitNode::Split {
                            dir,
                            ratio,
                            a: Box::new(new_a),
                            b,
                        }),
                        None => Some(*b),
                    }
                } else if b.find_pane(id) {
                    let b = b.remove_pane(id);
                    match b {
                        Some(new_b) => Some(SplitNode::Split {
                            dir,
                            ratio,
                            a,
                            b: Box::new(new_b),
                        }),
                        None => Some(*a),
                    }
                } else {
                    Some(SplitNode::Split { dir, ratio, a, b })
                }
            }
        }
    }

    /// Replace `target` pane id with `replacement` in place (used when
    /// swapping the active pane after a close).
    pub fn replace_pane(&mut self, target: u64, replacement: u64) {
        match self {
            SplitNode::Pane(id) => {
                if *id == target {
                    *id = replacement;
                }
            }
            SplitNode::Split { a, b, .. } => {
                a.replace_pane(target, replacement);
                b.replace_pane(target, replacement);
            }
        }
    }
}

/// The per-tab split state: the layout tree + which pane is active.
#[derive(Clone, Debug)]
pub struct SplitTree {
    pub root: SplitNode,
    /// Pane id of the pane that should receive keyboard focus / drive the
    /// toolbar. Updated whenever a pane is clicked or created.
    pub active_pane: u64,
}

impl SplitTree {
    /// A single-pane tree for an existing pane id.
    pub fn single(pane_id: u64) -> Self {
        Self {
            root: SplitNode::Pane(pane_id),
            active_pane: pane_id,
        }
    }

    /// All pane ids in this tree.
    pub fn pane_ids(&self) -> Vec<u64> {
        self.root.pane_ids()
    }

    /// Split the active pane: insert a new pane as the second child of a
    /// fresh split in `dir`. Returns the new pane id so the caller can
    /// create + register its [`TerminalView`].
    ///
    /// The new pane becomes the active pane.
    pub fn split_active(&mut self, dir: SplitDir, new_pane_id: u64) {
        let active = self.active_pane;
        let old_leaf = std::mem::replace(&mut self.root, SplitNode::Pane(new_pane_id));
        // old_leaf is the entire previous tree; we need to splice the split
        // in at the active pane's position. Rebuild by walking: replace the
        // active Pane node with a Split { a: old active, b: new }.
        self.root = splice_pane(old_leaf, active, dir, new_pane_id);
        self.active_pane = new_pane_id;
    }

    /// Remove a pane. If the tree becomes empty, returns `None` (caller
    /// closes the tab). Otherwise updates `active_pane` if the removed pane
    /// was active (falls back to the first remaining pane).
    pub fn remove_pane(mut self, id: u64) -> Option<SplitTree> {
        let remaining = self
            .root
            .clone()
            .pane_ids()
            .into_iter()
            .filter(|p| *p != id)
            .collect::<Vec<_>>();
        if remaining.is_empty() {
            return None;
        }
        self.root = self
            .root
            .remove_pane(id)
            .unwrap_or(SplitNode::Pane(remaining[0]));
        if self.active_pane == id {
            self.active_pane = remaining[0];
        }
        Some(self)
    }

    /// Set the ratio on the split that contains `pane_id` as one of its
    /// *immediate* children — used while dragging a divider. Walks the tree
    /// to find the parent split of `pane_id` and updates its ratio.
    pub fn set_ratio_for_child(&mut self, pane_id: u64, ratio: f32) {
        let ratio = ratio.clamp(0.05, 0.95);
        set_ratio_for_child(&mut self.root, pane_id, ratio);
    }
}

/// Replace the `target` Pane node inside `tree` with a `Split` whose `a` is
/// the old pane and `b` is `new_pane`. Returns the rebuilt tree.
fn splice_pane(tree: SplitNode, target: u64, dir: SplitDir, new_pane: u64) -> SplitNode {
    match tree {
        SplitNode::Pane(id) => {
            if id == target {
                SplitNode::Split {
                    dir,
                    ratio: 0.5,
                    a: Box::new(SplitNode::Pane(target)),
                    b: Box::new(SplitNode::Pane(new_pane)),
                }
            } else {
                SplitNode::Pane(id)
            }
        }
        SplitNode::Split {
            dir: d,
            ratio,
            a,
            b,
        } => SplitNode::Split {
            dir: d,
            ratio,
            a: Box::new(splice_pane(*a, target, dir, new_pane)),
            b: Box::new(splice_pane(*b, target, dir, new_pane)),
        },
    }
}

fn set_ratio_for_child(node: &mut SplitNode, pane_id: u64, ratio: f32) {
    match node {
        SplitNode::Pane(_) => {}
        SplitNode::Split { a, b, ratio: r, .. } => {
            let a_is_leaf_pane = matches!(a.as_ref(), SplitNode::Pane(_));
            let b_is_leaf_pane = matches!(b.as_ref(), SplitNode::Pane(_));
            // If either immediate child is the target pane, this is the
            // split whose divider the user is dragging.
            if a_is_leaf_pane && a.find_pane(pane_id) {
                *r = ratio;
                return;
            }
            if b_is_leaf_pane && b.find_pane(pane_id) {
                // `ratio` is the first child's share; the divider position is
                // the same conceptually, so store it directly.
                *r = ratio;
                return;
            }
            set_ratio_for_child(a, pane_id, ratio);
            set_ratio_for_child(b, pane_id, ratio);
        }
    }
}

/// Divider drag hit-test half-width in pixels (the grabbable band around the
/// divider line).
pub const DIVIDER_HIT: f32 = 4.0;
/// Minimum ratio a child can be squeezed to (keeps panes usable).
pub const MIN_RATIO: f32 = 0.05;

/// State held while the user is dragging a split divider.
#[derive(Clone, Debug)]
pub struct SplitDrag {
    /// Tab the dragged divider belongs to.
    pub tab_id: u64,
    /// Pane id of the first child of the split being resized (identifies
    /// which split in the tree this drag controls).
    pub pane_id: u64,
    /// Split direction (determines which axis the cursor maps to).
    pub dir: SplitDir,
    /// Pixel origin + extent of the split container, captured at drag start
    /// so we can convert the cursor position into a `[0,1]` ratio.
    pub origin: f32,
    pub extent: f32,
}
