//! A lightweight, trust-on-first-use check that a board's declared project
//! identity hasn't changed unexpectedly.
//!
//! `board_settings.rs`'s immutability is enforced by `rite`'s own write path,
//! not a filesystem lock — nothing stops a direct hand-edit of `<repo-root>/.settings`.
//! This is the other half: a per-machine, **not** git-tracked record of the
//! project name `rite` last saw for each board it has visited, at
//! `$XDG_CONFIG_HOME/kanrite/project-trust`. Machine-generated bookkeeping,
//! not a preference, so it gets its own file — the same reasoning
//! `registry.rs` already gives for keeping the repository list out of the
//! settings file.
//!
//! This is deliberately not cryptographic. It catches an unexpected change
//! from what this machine saw before; it does not prove the file wasn't
//! tampered with by someone who also updates this record.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::board::Board;
use crate::board_settings;
use crate::settings::Config;

/// Checks the board's declared project, if it has one, against what this
/// machine last saw for it. The first time a board is seen, its current
/// value is adopted as trusted — that's an ordinary first clone, not drift.
pub fn check(board: &Board) -> Result<()> {
    let Some(store_path) = path() else {
        return Ok(());
    };
    check_at(board, &store_path)
}

/// The half that takes a path, so tests need no environment fiddling.
fn check_at(board: &Board, store_path: &Path) -> Result<()> {
    let Some(project) = board_settings::load(board).project().map(str::to_string) else {
        return Ok(());
    };
    let raw = board.root.display().to_string();
    let key = key_of(&board.root);
    let mut trust = Config::load_from(store_path);
    // An entry written before the key was normalised is still the board's
    // (ticket 338): the same folder was kept under each spelling of its path.
    let seen = trust
        .get(&key)
        .or_else(|| trust.get(&raw))
        .map(str::to_string);
    match seen.as_deref() {
        None => {
            trust.set(&key, &project);
            trust.save_to(store_path)?;
        }
        // The same project whatever the case it's written in (ticket 338).
        Some(seen) if crate::names::same(seen, &project) => {}
        Some(seen) => {
            bail!(
                "project identity for {} changed unexpectedly: last seen '{seen}', now '{project}'. \
                 If this is intentional, remove the entry for this path from {} and re-run.",
                board.root.display(),
                store_path.display(),
            );
        }
    }
    Ok(())
}

/// The board's folder as the record keys it: on Windows with one kind of
/// slash and no case, since `G:/a/B` and `g:` are one folder there and had
/// each their own entry (ticket 338). Elsewhere as it is.
fn key_of(root: &Path) -> String {
    let shown = root.display().to_string();
    if cfg!(windows) {
        shown.replace('/', "\\").to_lowercase()
    } else {
        shown
    }
}

/// `project-trust` in the config folder ([`crate::places::config`]).
fn path() -> Option<PathBuf> {
    crate::places::config().map(|dir| dir.join("project-trust"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn board(dir: &Path) -> Board {
        Board::at(dir.join("tasks"))
    }

    fn set_project(board: &Board, name: &str) {
        fs::create_dir_all(&board.root).unwrap();
        let mut settings = board_settings::load(board);
        settings.set_project(name).unwrap();
        settings.save(board).unwrap();
    }

    #[test]
    fn a_board_with_no_declared_project_is_never_a_problem() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let store = dir.path().join("trust");
        check_at(&board, &store).unwrap();
        assert!(!store.exists(), "nothing to trust, nothing written");
    }

    #[test]
    fn the_first_sighting_is_adopted_silently() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        set_project(&board, "acme-app");
        let store = dir.path().join("trust");

        check_at(&board, &store).unwrap();
        assert_eq!(
            Config::load_from(&store).get(&key_of(&board.root)),
            Some("acme-app")
        );
    }

    #[test]
    fn a_change_of_case_alone_is_not_a_change_of_project() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let store = dir.path().join("trust");
        set_project(&board, "kanrite");
        check_at(&board, &store).unwrap();
        // The setting is edited to `KanRite`: still the same project.
        fs::write(
            board.root.parent().unwrap().join(".settings"),
            "project = KanRite
",
        )
        .unwrap();
        check_at(&board, &store).unwrap();
        // A different name still is a change.
        fs::write(
            board.root.parent().unwrap().join(".settings"),
            "project = other
",
        )
        .unwrap();
        assert!(check_at(&board, &store).is_err());
    }

    #[test]
    fn an_entry_under_the_raw_path_is_still_the_boards() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        set_project(&board, "kanrite");
        let store = dir.path().join("trust");
        // Written before keys were normalised: under the path as displayed.
        let mut trust = Config::load_from(&store);
        trust.set(&board.root.display().to_string(), "agilerite");
        trust.save_to(&store).unwrap();
        let err = check_at(&board, &store).unwrap_err().to_string();
        assert!(err.contains("last seen 'agilerite'"), "{err}");
    }

    #[cfg(windows)]
    #[test]
    fn on_windows_the_key_has_one_kind_of_slash_and_no_case() {
        assert_eq!(
            key_of(Path::new(r"G:/github/KanRite\kanrite-tickets\tasks")),
            r"g:\github\kanrite\kanrite-tickets\tasks"
        );
    }

    #[test]
    fn a_matching_second_sighting_is_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        set_project(&board, "acme-app");
        let store = dir.path().join("trust");

        check_at(&board, &store).unwrap();
        check_at(&board, &store).unwrap();
    }

    #[test]
    fn a_changed_value_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let store = dir.path().join("trust");

        set_project(&board, "acme-app");
        check_at(&board, &store).unwrap();

        // Simulate a hand-edit of the board-local settings file: rewrite it
        // directly rather than going through `set_project`, which would
        // itself refuse the change.
        fs::write(board_settings::path(&board), "project = widgets\n").unwrap();

        let err = check_at(&board, &store).unwrap_err().to_string();
        assert!(err.contains("acme-app"), "{err}");
        assert!(err.contains("widgets"), "{err}");
    }
}
