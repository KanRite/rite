//! KanRite — a tiny filesystem Kanban in markdown.
//!
//! Tasks are markdown files that move between `todo/`, `doing/` and `done/` in
//! `<repo-root>/tasks`. Status is the folder; `git mv` is the audit trail; the
//! ticket format is a MiniJinja template you can edit.
//!
//! This is `rite-lib`: everything that touches the board lives here, and
//! nothing that knows about a terminal or a window does. Creating a ticket and
//! moving it live here; how that is asked for and how it is drawn belong to
//! the front ends. See docs/adr/0004-one-library-two-interfaces.md.

pub mod access;
pub mod archive;
pub mod assistant;
pub mod authorship;
pub mod auto_priority;
pub mod board;
pub mod board_settings;
pub mod branch;
pub mod clone;
pub mod columns;
pub mod compat;
pub mod create;
pub mod current_project;
pub mod doc_pins;
pub mod documents;
pub mod documents_edit;
pub mod eol;
pub mod git;
pub mod github;
pub mod group;
pub mod hierarchy;
pub mod home;
pub mod ideas;
pub mod import;
pub mod import_documents;
pub mod import_issues;
pub mod import_log;
pub mod links;
pub mod model;
pub mod my_work;
pub mod names;
pub mod new_repo;
pub mod numbering;
pub mod pairing;
pub mod places;
pub mod project_trust;
pub mod recover;
pub mod recur;
pub mod registry;
pub mod renamed;
pub mod renumber;
pub mod requires;
pub mod richtext;
pub mod roadmap;
pub mod screen;
pub mod settings;
pub mod sprints;
pub mod stats;
pub mod sync;
pub mod task;
pub mod templates;
pub mod transition;
pub mod update;
pub mod update_apply;
pub mod workflow;
