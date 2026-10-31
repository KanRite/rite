//! Cloning a project's declared repositories that aren't on this machine
//! yet (ticket 108). A repository you haven't got can't be targeted
//! (tickets 101/107), so `rite clone` and riteui's "clone…" fetch it for you:
//! beside the tickets repository, from wherever the tickets repository
//! itself came from, then registered.

use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::board::Board;
use crate::registry::{self, Registry};
use crate::{board_settings, git};

/// What [`clone_repo`] did.
#[derive(Debug)]
pub struct Cloned {
    pub path: PathBuf,
    pub url: String,
    /// Other registered clones of the same remote (ticket 095), to warn about.
    pub also_at: Vec<PathBuf>,
    /// The clone was there already, beside the tickets repository, and only
    /// got registered (ticket 342); nothing was fetched.
    pub already_there: bool,
}

/// The repositories the project declares that aren't cloned for it here.
pub fn missing(board: &Board) -> Vec<String> {
    board_settings::load(board)
        .repos()
        .into_iter()
        .filter(|name| registry::clone_of(&board.root, name).is_none())
        .collect()
}

/// Clones `name`, a repository the project declares, into the project
/// directory beside the tickets repository, and registers it. The URL is
/// the tickets repository's own `origin` with the name swapped in
/// ([`sibling_url`]), so it comes the same way — the same SSH host alias,
/// the same forge, or the same local directory. Refuses an undeclared name,
/// one already cloned for this project, and a directory already in the way.
pub fn clone_repo(board: &Board, name: &str) -> Result<Cloned> {
    let name = crate::task::repo_name(name);
    let declared = board_settings::load(board).repos();
    if !declared.contains(&name) {
        bail!("{name} isn't declared for this project — `rite register` it from a clone first");
    }
    if let Some(path) = registry::registered_clone_of(&board.root, &name) {
        bail!("{name} is already cloned here, at {}", path.display());
    }
    // Cloned beside the tickets repository but not registered: a project
    // cloned again or copied. Register it, as the clone would have been
    // (ticket 342), rather than say a directory is in the way.
    if let Some(path) = registry::beside(&board.root, &name) {
        let mut registry = Registry::load();
        registry.add(&path)?;
        registry.save()?;
        let also_at = registry.same_remote(&path);
        return Ok(Cloned {
            url: git::remote_url(&path).unwrap_or_default(),
            path,
            also_at,
            already_there: true,
        });
    }
    let tickets = board.repo_root();
    let target = registry::project_dir_of(tickets).join(&name);
    // symlink_metadata, not exists(): a dangling symlink is still in the way.
    if target.symlink_metadata().is_ok() {
        bail!(
            "{} already exists — move it, or `rite register` it if it's the clone",
            target.display()
        );
    }
    let Some(origin) = git::remote_url(tickets) else {
        bail!(
            "{} has no origin to clone {name} alongside — clone it by hand, then `rite register` it",
            tickets.display()
        );
    };
    let mut url = sibling_url(&origin, &name);
    if git::is_local_path(&url) {
        url = tickets.join(&url).display().to_string();
    }
    if let Err(err) = git::clone(&url, &target) {
        // Nothing half-made left behind to trip over next time.
        let _ = std::fs::remove_dir_all(&target);
        return Err(err);
    }
    let mut registry = Registry::load();
    registry.add(&target)?;
    registry.save()?;
    let also_at = registry.same_remote(&target);
    Ok(Cloned {
        path: target,
        url,
        also_at,
        already_there: false,
    })
}

/// `origin` with its last path part — the tickets repository's name —
/// replaced by `name`, keeping a `.git` suffix if it had one.
/// `git@github.com-me:org/p-tickets.git` → `git@github.com-me:org/code.git`.
/// On Windows a local origin's folders are split by `\` too (ticket 279),
/// or `C:\…\p-tickets.git` would be cut at the drive's colon.
pub fn sibling_url(origin: &str, name: &str) -> String {
    let (trailing, separators): (&[char], &[char]) = if cfg!(windows) {
        (&['/', '\\'], &['/', ':', '\\'])
    } else {
        (&['/'], &['/', ':'])
    };
    let origin = origin.trim().trim_end_matches(trailing);
    let cut = origin.rfind(separators).map_or(0, |at| at + 1);
    let (base, last) = origin.split_at(cut);
    let suffix = if last.ends_with(".git") { ".git" } else { "" };
    format!("{base}{name}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_url_is_the_tickets_origin_with_the_name_swapped_in() {
        for (origin, want) in [
            (
                "git@github.com-oldippy:acme/acme-tickets.git",
                "git@github.com-oldippy:acme/amd.git",
            ),
            (
                "ssh://git@github.com/acme/acme-tickets",
                "ssh://git@github.com/acme/amd",
            ),
            (
                "https://github.com/acme/acme-tickets.git/",
                "https://github.com/acme/amd.git",
            ),
            ("/srv/git/acme-tickets.git", "/srv/git/amd.git"),
            ("../acme-tickets", "../amd"),
            ("host:acme-tickets.git", "host:amd.git"),
        ] {
            assert_eq!(sibling_url(origin, "amd"), want, "{origin}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_origin_keeps_its_folder_and_drive() {
        assert_eq!(
            sibling_url(r"C:\Users\me\origins\p-tickets.git", "amd"),
            r"C:\Users\me\origins\amd.git"
        );
    }

    /// A tickets repository with a board declaring `code`, and no origin.
    /// Only the refusals are tested here: they never reach the registry
    /// file. The clone itself is in tests/test.sh, against bare repos.
    fn project() -> (tempfile::TempDir, Board) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("p-tickets");
        std::fs::create_dir_all(&root).unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["init", "-q"])
            .status()
            .unwrap();
        let board = Board::in_repo(&root);
        board.create().unwrap();
        let mut settings = board_settings::load(&board);
        settings.declare_repo("code").unwrap();
        settings.save(&board).unwrap();
        (dir, board)
    }

    #[test]
    fn an_undeclared_repository_is_refused() {
        let (_dir, board) = project();
        let err = clone_repo(&board, "other").unwrap_err().to_string();
        assert!(err.contains("isn't declared for this project"), "{err}");
    }

    #[test]
    fn a_clone_beside_the_tickets_repository_is_found_whether_or_not_it_is_registered() {
        let (dir, board) = project();
        // Nothing there, a plain folder, and a `.git` with nothing in it.
        assert_eq!(registry::beside(&board.root, "code"), None);
        std::fs::create_dir_all(dir.path().join("code")).unwrap();
        assert_eq!(registry::beside(&board.root, "code"), None);
        std::fs::create_dir_all(dir.path().join("code").join(".git")).unwrap();
        assert_eq!(registry::beside(&board.root, "code"), None);
        // A repository is one (ticket 342).
        git::fake_repo(&dir.path().join("code"));
        assert_eq!(
            registry::beside(&board.root, "code").map(|p| dunce::canonicalize(p).unwrap()),
            Some(dunce::canonicalize(dir.path().join("code")).unwrap())
        );
    }

    #[test]
    fn a_directory_in_the_way_is_refused() {
        let (dir, board) = project();
        std::fs::create_dir_all(dir.path().join("code")).unwrap();
        let err = clone_repo(&board, "code").unwrap_err().to_string();
        assert!(err.contains("already exists"), "{err}");
    }

    #[test]
    fn no_origin_to_go_by_is_refused() {
        let (dir, board) = project();
        let err = clone_repo(&board, "code").unwrap_err().to_string();
        assert!(err.contains("has no origin"), "{err}");
        assert!(!dir.path().join("code").exists());
    }
}
