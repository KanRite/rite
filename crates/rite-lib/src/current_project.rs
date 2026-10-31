//! Several projects in one folder (ticket 093): which one `rite` means.
//!
//! `rite init`/`rite join` put a project's repositories side by side in a
//! plain folder — the *project directory* — and nothing stops a second
//! project landing next to the first (`kanrite-tickets` beside
//! `multi-user-test-tickets`). `Board::discover()` used to find both and
//! either refuse (from the folder itself) or silently give up (from a code
//! repository next to them). `.current-project`, a one-line file in that
//! folder, says which one is meant; `rite project use <name>` writes it.
//!
//! The file belongs to this machine and this folder, never to a repository:
//! which project you're working on right now is not something to commit.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::board::{Board, project_siblings};
use crate::{board_settings, git};

/// The file in the project directory naming the current project.
pub const FILE: &str = ".current-project";

/// One project found in a project directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectEntry {
    pub name: String,
    /// The repository holding the project's board.
    pub tickets: PathBuf,
    /// `{name}-documents` beside it, when there is one.
    pub documents: Option<PathBuf>,
}

/// The folder holding a project's repositories: the parent of the
/// repository you're standing in, or — outside any repository — the
/// current directory itself.
pub fn project_dir() -> Result<PathBuf> {
    if let Some(root) = git::repo_root()
        && let Some(parent) = root.parent()
    {
        return Ok(parent.to_path_buf());
    }
    std::env::current_dir().context("reading the current directory")
}

/// What `dir/.current-project` names, if it exists and says anything.
pub fn read(dir: &Path) -> Option<String> {
    let text = crate::eol::read(dir.join(FILE)).ok()?;
    let name = text.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Makes `name` the current project in `dir`.
pub fn write(dir: &Path, name: &str) -> Result<()> {
    board_settings::validate_project_name(name)?;
    let path = dir.join(FILE);
    fs::write(&path, format!("{name}\n")).with_context(|| format!("writing {}", path.display()))
}

/// The project a board repository belongs to: `kanrite-tickets` is
/// `kanrite` by the layout `init` creates; otherwise whatever the board
/// declares; otherwise the directory's own name.
pub fn project_name(tickets: &Path) -> String {
    let dir_name = tickets
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some(name) = dir_name.strip_suffix("-tickets")
        && !name.is_empty()
    {
        return name.to_string();
    }
    board_settings::load(&Board::in_repo(tickets))
        .project()
        .map(str::to_string)
        .unwrap_or(dir_name)
}

/// The name to show for the project whose board is in `tickets` (ticket
/// 326): its `display-name`, else [`project_name`]. For people to read
/// only; anything that finds or matches a project uses `project_name`.
pub fn shown_name(tickets: &Path) -> String {
    board_settings::load(&Board::in_repo(tickets))
        .display_name()
        .unwrap_or_else(|| project_name(tickets))
}

/// Every project in `dir`, sorted by name.
pub fn projects(dir: &Path) -> Vec<ProjectEntry> {
    let mut found: Vec<ProjectEntry> = project_siblings(dir)
        .into_iter()
        .map(|tickets| {
            let name = project_name(&tickets);
            // Found by name without case (ticket 338): on a case-sensitive
            // disk `KanRite-tickets` may sit beside `kanrite-documents`.
            let documents = crate::names::find_folder(dir, &format!("{name}-documents"));
            ProjectEntry {
                documents: documents.filter(|documents| documents.is_dir()),
                name,
                tickets,
            }
        })
        .collect();
    found.sort_by_key(|entry| (crate::names::key(&entry.name), entry.name.clone()));
    found
}

/// The project `rite` uses in `dir`: the one `.current-project` names, or
/// the only one there is. `None` when there are none, or several and no
/// choice has been made.
pub fn current(dir: &Path) -> Option<String> {
    if let Some(name) = read(dir) {
        return Some(name);
    }
    match projects(dir).as_slice() {
        [only] => Some(only.name.clone()),
        _ => None,
    }
}

/// After `init`/`join` has added `added` to `dir`: if that made the folder
/// ambiguous — exactly one project was here before, none is chosen yet —
/// record the one that was already here as current, so adding a project
/// never silently changes which board `rite` shows. Returns the name it
/// kept, for the caller to say so; `None` when nothing needed doing (an
/// existing choice is never touched).
pub fn keep_existing(dir: &Path, added: &str) -> Result<Option<String>> {
    if read(dir).is_some() {
        return Ok(None);
    }
    let others: Vec<ProjectEntry> = projects(dir)
        .into_iter()
        .filter(|project| !crate::names::same(&project.name, added))
        .collect();
    match others.as_slice() {
        [existing] => {
            write(dir, &existing.name)?;
            Ok(Some(existing.name.clone()))
        }
        _ => Ok(None),
    }
}

/// Discovery's question: which board repository in `dir` is meant?
///
/// `.current-project` decides when it's set (and naming a project that
/// isn't there is an error, not a fallback). Without it, the only board is
/// the answer, and no board is `Ok(None)`. Several is an error when
/// `strict` (standing in the folder itself — there's nothing else to fall
/// back to), or when at least two of them are `*-tickets` repositories,
/// the layout `init`/`join` create: that's a folder of projects, and a
/// silent "no board" there hides the problem. Anything else — several
/// unrelated boards that happen to share a parent, like a temp directory
/// full of scratch repositories — stays `Ok(None)`, the caller's own
/// fallback, as before ticket 093.
pub(crate) fn pick(dir: &Path, strict: bool) -> Result<Option<PathBuf>> {
    let candidates = project_siblings(dir);
    if let Some(name) = read(dir) {
        // The name is the same however it's cased (ticket 338). Two clones
        // that differ only in case can't both be it: refused, not picked.
        let named: Vec<PathBuf> = candidates
            .into_iter()
            .filter(|tickets| crate::names::same(&project_name(tickets), &name))
            .collect();
        return match named.as_slice() {
            [only] => Ok(Some(only.clone())),
            [] => Err(anyhow!(
                "current project '{name}' (from {}) isn't here — run `rite project use <name>` \
                 to pick another (`rite project` lists them)",
                dir.join(FILE).display()
            )),
            many => Err(anyhow!(case_twins(many))),
        };
    }
    match candidates.as_slice() {
        [] => Ok(None),
        [only] => Ok(Some(only.clone())),
        many => {
            let tickets_repos = many
                .iter()
                .filter(|path| {
                    path.file_name()
                        .is_some_and(|name| name.to_string_lossy().ends_with("-tickets"))
                })
                .count();
            if strict || tickets_repos >= 2 {
                bail!(ambiguous(many));
            }
            Ok(None)
        }
    }
}

/// Two or more project boards in one folder whose names differ only in
/// case, so no name chooses between them (ticket 338).
fn case_twins(paths: &[PathBuf]) -> String {
    let names: Vec<String> = paths
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    format!(
        "{} differ only in case: keep one of them, so a project name can't mean either",
        names.join(" and ")
    )
}

/// Pairs of projects in `dir` whose names differ only in case, for a listing
/// to warn about (ticket 338). Only possible on a case-sensitive disk.
pub fn twins(dir: &Path) -> Vec<(PathBuf, PathBuf)> {
    let found = projects(dir);
    let mut out = Vec::new();
    for (i, a) in found.iter().enumerate() {
        for b in &found[i + 1..] {
            if a.name != b.name && crate::names::same(&a.name, &b.name) {
                out.push((a.tickets.clone(), b.tickets.clone()));
            }
        }
    }
    out
}

fn ambiguous(paths: &[PathBuf]) -> String {
    let names: Vec<String> = paths
        .iter()
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    format!(
        "more than one project board here ({}) — run `rite project use <name>` to pick one \
         (`rite project` lists them)",
        names.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_with_board(parent: &Path, name: &str) -> PathBuf {
        let repo = parent.join(name);
        crate::git::fake_repo(&repo);
        fs::create_dir_all(repo.join("tasks/backlog")).unwrap();
        repo
    }

    #[test]
    fn the_file_round_trips_and_is_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path()), None);
        write(dir.path(), "acme").unwrap();
        assert_eq!(read(dir.path()), Some("acme".to_string()));
        fs::write(dir.path().join(FILE), "  \n").unwrap();
        assert_eq!(read(dir.path()), None);
    }

    #[test]
    fn an_invalid_name_is_never_written() {
        let dir = tempfile::tempdir().unwrap();
        assert!(write(dir.path(), "no/slashes").is_err());
        assert!(!dir.path().join(FILE).exists());
    }

    #[test]
    fn a_project_is_named_after_its_tickets_repository() {
        let dir = tempfile::tempdir().unwrap();
        let tickets = repo_with_board(dir.path(), "acme-tickets");
        assert_eq!(project_name(&tickets), "acme");
        let other = repo_with_board(dir.path(), "board");
        assert_eq!(project_name(&other), "board");
    }

    #[test]
    fn projects_lists_each_with_its_documents_sibling() {
        let dir = tempfile::tempdir().unwrap();
        repo_with_board(dir.path(), "beta-tickets");
        repo_with_board(dir.path(), "acme-tickets");
        fs::create_dir_all(dir.path().join("acme-documents")).unwrap();
        let found = projects(dir.path());
        assert_eq!(
            found.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["acme", "beta"]
        );
        assert_eq!(found[0].documents, Some(dir.path().join("acme-documents")));
        assert_eq!(found[1].documents, None);
    }

    #[test]
    fn a_project_is_found_whatever_case_its_name_is_written_in() {
        // Ticket 338: the name typed, the folder on disk and the file say
        // `KanRite`, `kanrite` and `KANRITE`, and are one project.
        let dir = tempfile::tempdir().unwrap();
        let tickets = repo_with_board(dir.path(), "KanRite-tickets");
        // Its documents, in another case (a case-sensitive disk has both).
        fs::create_dir_all(dir.path().join("kanrite-documents")).unwrap();
        write(dir.path(), "KANRITE").unwrap();
        assert_eq!(pick(dir.path(), true).unwrap(), Some(tickets.clone()));
        write(dir.path(), "kanrite").unwrap();
        assert_eq!(pick(dir.path(), true).unwrap(), Some(tickets.clone()));
        let found = projects(dir.path());
        assert_eq!(found[0].name, "KanRite");
        assert_eq!(
            found[0]
                .documents
                .as_deref()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().to_lowercase()),
            Some("kanrite-documents".to_string())
        );
        // A name that isn't there is still refused.
        write(dir.path(), "other").unwrap();
        assert!(pick(dir.path(), true).is_err());
    }

    #[test]
    fn keeping_the_existing_project_ignores_case() {
        let dir = tempfile::tempdir().unwrap();
        repo_with_board(dir.path(), "KanRite-tickets");
        repo_with_board(dir.path(), "acme-tickets");
        // `kanrite` was just added, as typed in lower case: not "another".
        assert_eq!(
            keep_existing(dir.path(), "kanrite").unwrap(),
            Some("acme".to_string())
        );
    }

    #[test]
    fn two_projects_that_differ_only_in_case_are_refused_not_picked() {
        let dir = tempfile::tempdir().unwrap();
        repo_with_board(dir.path(), "acme-tickets");
        fs::create_dir_all(dir.path().join("Acme-tickets")).unwrap();
        if !dir.path().join("Acme-tickets").join(".git").exists()
            && fs::read_dir(dir.path()).unwrap().count() == 2
        {
            repo_with_board(dir.path(), "Acme-tickets");
        } else {
            return; // a disk that ignores case: only one folder can exist
        }
        write(dir.path(), "acme").unwrap();
        let err = pick(dir.path(), true).unwrap_err().to_string();
        assert!(err.contains("differ only in case"), "{err}");
        assert_eq!(twins(dir.path()).len(), 1);
    }

    #[test]
    fn one_project_is_current_without_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let acme = repo_with_board(dir.path(), "acme-tickets");
        assert_eq!(current(dir.path()), Some("acme".to_string()));
        assert_eq!(pick(dir.path(), true).unwrap(), Some(acme));
    }

    #[test]
    fn the_file_picks_between_projects() {
        let dir = tempfile::tempdir().unwrap();
        repo_with_board(dir.path(), "acme-tickets");
        let beta = repo_with_board(dir.path(), "beta-tickets");
        write(dir.path(), "beta").unwrap();
        assert_eq!(pick(dir.path(), true).unwrap(), Some(beta.clone()));
        assert_eq!(pick(dir.path(), false).unwrap(), Some(beta));
    }

    #[test]
    fn naming_a_project_that_isnt_there_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        repo_with_board(dir.path(), "acme-tickets");
        write(dir.path(), "gone").unwrap();
        let err = pick(dir.path(), false).unwrap_err().to_string();
        assert!(
            err.contains("'gone'") && err.contains("rite project use"),
            "{err}"
        );
    }

    #[test]
    fn several_projects_and_no_choice_is_an_error_naming_the_fix() {
        let dir = tempfile::tempdir().unwrap();
        repo_with_board(dir.path(), "acme-tickets");
        repo_with_board(dir.path(), "beta-tickets");
        for strict in [true, false] {
            let err = pick(dir.path(), strict).unwrap_err().to_string();
            assert!(err.contains("more than one project board"), "{err}");
            assert!(err.contains("rite project use"), "{err}");
        }
    }

    #[test]
    fn adding_a_second_project_keeps_the_first_current() {
        let dir = tempfile::tempdir().unwrap();
        repo_with_board(dir.path(), "acme-tickets");
        repo_with_board(dir.path(), "beta-tickets");
        assert_eq!(
            keep_existing(dir.path(), "beta").unwrap(),
            Some("acme".to_string())
        );
        assert_eq!(read(dir.path()), Some("acme".to_string()));
    }

    #[test]
    fn keep_existing_leaves_a_choice_or_a_lone_project_alone() {
        let dir = tempfile::tempdir().unwrap();
        repo_with_board(dir.path(), "acme-tickets");
        assert_eq!(keep_existing(dir.path(), "acme").unwrap(), None);
        assert_eq!(read(dir.path()), None);

        repo_with_board(dir.path(), "beta-tickets");
        write(dir.path(), "beta").unwrap();
        assert_eq!(keep_existing(dir.path(), "gamma").unwrap(), None);
        assert_eq!(read(dir.path()), Some("beta".to_string()));
    }

    #[test]
    fn unrelated_scratch_boards_only_fail_when_strict() {
        let dir = tempfile::tempdir().unwrap();
        repo_with_board(dir.path(), "tmp.one");
        repo_with_board(dir.path(), "tmp.two");
        assert_eq!(pick(dir.path(), false).unwrap(), None);
        assert!(pick(dir.path(), true).is_err());
    }
}
