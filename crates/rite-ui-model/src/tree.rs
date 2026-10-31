//! The requires tree as the page receives it (ticket 214): the same shape
//! as `rite_lib::requires::TreeNode`, which rite-ui sends; this crate can't
//! depend on rite-lib, so rite-ui's tests check the two agree.

use serde::{Deserialize, Serialize};

use crate::epic::Place;

/// How a node hangs from its parent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Edge {
    Root,
    /// The parent requires it: drawn solid.
    Requires,
    /// Related to the parent: drawn dashed.
    Related,
    /// The parent's child, in the hierarchy (ticket 261).
    Child,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeNode {
    pub id: String,
    pub title: String,
    pub file: String,
    /// The column's directory name.
    pub column: String,
    pub epic: Option<String>,
    /// Where it is, in words.
    pub place: String,
    pub done: bool,
    pub edge: Edge,
    /// What it requires that isn't done yet.
    pub unmet: Vec<String>,
    /// In a started sprint (its name): not expanded.
    pub stub: Option<String>,
    /// Drawn above already: a pointer only.
    pub seen: bool,
    /// The ticket the tree was opened from.
    pub current: bool,
    pub children: Vec<TreeNode>,
}

impl TreeNode {
    /// Where its card is on `repo`'s board, for going to it: its folder
    /// among the board's columns (ticket 224).
    pub fn place_in(&self, repo: &str, workflow: &crate::Workflow) -> Place {
        Place {
            repo: repo.to_string(),
            column: workflow
                .columns()
                .find(|c| c.dir_name() == self.column)
                .unwrap_or_default(),
            epic: self.epic.clone(),
        }
    }
}
