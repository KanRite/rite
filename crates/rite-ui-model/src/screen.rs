//! What the window shows, as the page tells rite-ui (ticket 186). The same
//! shape as `rite_lib::screen::View`, which rite-ui writes for an AI
//! assistant's `screen` tool; this crate can't depend on rite-lib.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenView {
    /// `home`, `board`, `documents`, `ideas` or `roadmaps`.
    pub screen: String,
    pub ticket: Option<ScreenPlace>,
    pub document: Option<ScreenPlace>,
    pub idea: Option<ScreenPlace>,
    pub epic: Option<ScreenPlace>,
    pub search: String,
    pub about: Option<String>,
    pub assignee: String,
}

/// Something in a repository: a ticket's file, a document's or idea's
/// path, an epic's name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenPlace {
    pub repo: String,
    pub path: String,
}

impl ScreenPlace {
    pub fn new(repo: &str, path: &str) -> ScreenPlace {
        ScreenPlace {
            repo: repo.to_string(),
            path: path.to_string(),
        }
    }
}
