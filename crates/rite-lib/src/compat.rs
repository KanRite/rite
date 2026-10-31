//! Compatibility between KanRite versions (ticket 193).
//!
//! **A breaking change is always the user's choice.** Every release reads
//! what earlier ones wrote, an older build keeps working on data a newer
//! one has touched, and nothing is migrated or rewritten by opening rite or
//! riteui, or by updating (ticket 191). So an update is always safe, and
//! optional (the update bar's Later).
//!
//! A *required* update exists only as a tool the team chooses: a project
//! may record the oldest KanRite allowed to change it, `min-version` in
//! the tickets `.settings`, travelling through git like any setting. Only a
//! person sets it (`rite require-version`, confirmed); no release or code
//! path does. An older build is then **read-only** there
//! ([`ensure_version`], called by `archive::ensure_writable`, which every
//! change already goes through): it still shows the board, refuses changes
//! with a plain message naming `rite update`, and never tries to rewrite
//! what it doesn't understand. Builds from before ticket 193 don't know the
//! setting and ignore it, which is the price of adding it.

use anyhow::{Result, bail};

use crate::board::Board;
use crate::board_settings;
use crate::update::Version;

/// This build's version.
pub fn running() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Version(0, 0, 0))
}

/// The version the project's team requires, when it's newer than this
/// build: then this build may only read the project.
pub fn needed(board: &Board) -> Option<Version> {
    let min = Version::parse(&board_settings::load(board).min_version()?)?;
    (min > running()).then_some(min)
}

/// Refuses a change when the team requires a newer KanRite.
pub fn ensure_version(board: &Board) -> Result<()> {
    if let Some(min) = needed(board) {
        bail!(
            "this project needs KanRite {min} or newer (its team set that), and this is {}: \
             it's read-only here until you update (`rite update`)",
            running()
        );
    }
    Ok(())
}

/// What `rite require-version <v>` would set: `v` checked. Refused when
/// it's newer than this build, which would lock its own user out.
pub fn check_required(version: &str) -> Result<Version> {
    let Some(version) = Version::parse(version) else {
        bail!("'{version}' isn't a version: write it as 4.6.0");
    };
    if version > running() {
        bail!(
            "you're on {}, older than {version}: update first (`rite update`), or you'd make the project read-only for yourself",
            running()
        );
    }
    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board() -> (tempfile::TempDir, Board) {
        let dir = tempfile::tempdir().unwrap();
        let board = Board::at(dir.path().join("tasks"));
        board.create().unwrap();
        (dir, board)
    }

    fn require(board: &Board, v: &str) {
        let mut s = board_settings::load(board);
        s.set_min_version(Some(v));
        s.save(board).unwrap();
    }

    #[test]
    fn a_newer_required_version_makes_the_project_read_only_here() {
        let (_dir, board) = board();
        assert_eq!(needed(&board), None);
        assert!(ensure_version(&board).is_ok());
        require(&board, "999.0.0");
        assert_eq!(needed(&board), Some(Version(999, 0, 0)));
        let err = crate::archive::ensure_writable(&board)
            .unwrap_err()
            .to_string();
        assert!(err.contains("needs KanRite 999.0.0 or newer"), "{err}");
        assert!(err.contains("rite update"), "{err}");
        // This version or older: fine.
        require(&board, &running().to_string());
        assert!(ensure_version(&board).is_ok());
        require(&board, "1.0.0");
        assert!(ensure_version(&board).is_ok());
    }

    /// Fields a later release might add, which this one must keep exactly.
    const FUTURE: [&str; 3] = [
        "future-field: \"kept as written\"",
        "future-list: [a,b]",
        "x-team-note: \"anything\"",
    ];

    #[test]
    fn fields_this_build_doesnt_know_survive_every_kind_of_write() {
        use crate::board::Column;
        use crate::task::Task;
        let (dir, board) = board();
        std::fs::write(dir.path().join(".settings"), "commit = off\nsync = off\n").unwrap();
        let path = board.dir(Column::Backlog).join("001-t.md");
        std::fs::write(
            &path,
            format!(
                "---\nid: \"001\"\ntitle: \"T\"\n{}\npriority: \"low\"\ntags: []\n---\n\n## Notes\n\nbody\n",
                FUTURE.join("\n")
            ),
        )
        .unwrap();
        let kept = |board: &Board| {
            let task = board.find("1").unwrap();
            let text = crate::eol::read(&task.path).unwrap();
            for line in FUTURE {
                assert!(text.lines().any(|l| l == line), "lost {line:?}:\n{text}");
            }
            task
        };
        let task = kept(&board);
        task.set_priority(task.hash().unwrap(), "high").unwrap();
        let task = kept(&board);
        task.set_due(task.hash().unwrap(), "2030-01-01").unwrap();
        let task = kept(&board);
        task.set_points(task.hash().unwrap(), "3").unwrap();
        let task = kept(&board);
        task.set_draft(task.hash().unwrap(), true).unwrap();
        let task = kept(&board);
        task.set_draft(task.hash().unwrap(), false).unwrap();
        let task = kept(&board);
        crate::transition::edit_ticket(
            &board,
            &task,
            task.hash().unwrap(),
            &crate::transition::TicketFields {
                title: "T renamed".into(),
                body: "## Notes\n\nedited\n".into(),
                tags: vec!["CLI".into()],
                ..Default::default()
            },
        )
        .unwrap();
        let task = kept(&board);
        let text = crate::eol::read(&task.path).unwrap();
        crate::transition::save_text(&board, &task, task.hash().unwrap(), &text).unwrap();
        let task = kept(&board);
        crate::transition::move_to(&board, &task, Column::Todo, false).unwrap();
        let task: Task = kept(&board);
        crate::transition::move_to(&board, &task, Column::Doing, false).unwrap();
        kept(&board);
    }

    #[test]
    fn a_required_version_is_checked_and_never_above_this_build() {
        assert!(check_required("4.x").is_err());
        assert!(
            check_required("999.0.0")
                .unwrap_err()
                .to_string()
                .contains("update first")
        );
        assert_eq!(check_required("1.0.0").unwrap(), Version(1, 0, 0));
    }
}
