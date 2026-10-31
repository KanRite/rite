//! The locally registered repositories: the **per-machine** half of
//! registration.
//!
//! Deliberately a list of paths and nothing more. The tickets themselves are
//! never copied into a store: they're markdown in each repo, they change when
//! you check out a branch or pull someone else's work, and an index of them
//! would be wrong the moment it was written. Reading a board is a directory
//! listing and a few small files, which is fast enough that caching it would
//! only buy staleness.
//!
//! Stored one path per line at `$XDG_CONFIG_HOME/kanrite/repos`, so it can be
//! read, edited and version-controlled by hand.
//!
//! **This is not the whole of registration any more.** The *other* half —
//! which repository names a project actually declares as its own — is
//! `board_settings::repos()`, shared and git-tracked in the tickets repo's
//! own `.settings`. This file only ever answers "is a given declared name
//! cloned on *this* machine, and where" (`find`); it no longer decides *which*
//! names to look for; nothing here reaches across unrelated projects any more,
//! and nothing writes to it silently in the background — `rite register`
//! (`rite-cli`) is the only writer now, `registry::remember`'s
//! run-on-every-command auto-fill is gone along with it.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::board::Board;
use crate::git;

/// A repository on the list, and the board inside it.
#[derive(Clone, Debug)]
pub struct Entry {
    /// The repository's working-tree root.
    pub root: PathBuf,
    /// What to call it: the directory's own name.
    pub name: String,
}

impl Entry {
    pub fn new(root: PathBuf) -> Entry {
        let name = root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());
        Entry { root, name }
    }

    pub fn board(&self) -> Board {
        Board::in_repo(&self.root)
    }

    /// Has this repository got a board to show?
    pub fn has_board(&self) -> bool {
        self.board().root.is_dir()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Registry {
    pub entries: Vec<Entry>,
}

impl Registry {
    pub fn load() -> Registry {
        match path() {
            Some(path) => Registry::load_from(&path),
            None => Registry::default(),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = path().ok_or_else(|| anyhow::anyhow!("no config directory"))?;
        self.save_to(&path)
    }

    /// The half that takes a path, so tests need no environment fiddling.
    pub fn load_from(path: &Path) -> Registry {
        let Ok(text) = crate::eol::read(path) else {
            return Registry::default();
        };
        let entries = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(PathBuf::from)
            // A clone that's gone (a temp directory after a reboot, a
            // deleted or renamed repo) is nothing to keep (ticket 095): left
            // out here, so the next save drops it too.
            .filter(|root| root.is_dir())
            .map(Entry::new)
            .collect();
        Registry { entries }
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut text = String::from("# Repositories KanRite knows about, one path per line.\n");
        for entry in &self.entries {
            text.push_str(&entry.root.display().to_string());
            text.push('\n');
        }
        fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// Register a repository. Returns false if it was already on the list.
    pub fn add(&mut self, root: &Path) -> Result<bool> {
        let root = dunce::canonicalize(root)
            .with_context(|| format!("{} doesn't exist", root.display()))?;
        if !crate::git::is_repo_dir(&root) {
            bail!("{}", crate::git::not_a_repo(&root));
        }
        if self.entries.iter().any(|entry| entry.root == root) {
            return Ok(false);
        }
        // Another clone in the same folder whose name differs only in case
        // (possible on a case-sensitive disk) would be taken for this one:
        // refused, not quietly mixed up (ticket 338).
        let new = Entry::new(root);
        if let Some(twin) = self.entries.iter().find(|entry| {
            entry.root.parent() == new.root.parent() && crate::names::same(&entry.name, &new.name)
        }) {
            bail!(
                "{} and {} differ only in case: keep one, so a name can't mean either",
                twin.root.display(),
                new.root.display()
            );
        }
        self.entries.push(new);
        self.entries
            .sort_by_key(|entry| (crate::names::key(&entry.name), entry.name.clone()));
        Ok(true)
    }

    /// Unregister a repository by path or by name.
    pub fn remove(&mut self, reference: &str) -> bool {
        let canonical = dunce::canonicalize(Path::new(reference)).ok();
        let before = self.entries.len();
        self.entries.retain(|entry| {
            let by_path = canonical.as_ref().is_some_and(|path| entry.root == *path);
            let by_name = crate::names::same(&entry.name, reference);
            !(by_path || by_name)
        });
        self.entries.len() != before
    }

    /// Is `name` registered on this machine, and if so, its `Entry`?
    ///
    /// This is the only question this file answers now — *which* names are
    /// worth asking about comes from the current project's own declared list
    /// (`board_settings::repos()`), never from scanning this file's own
    /// entries wholesale. A declared name with no matching entry here (or one
    /// whose board no longer exists — `has_board()`) is what `riteui` shows
    /// greyed out, not an error and not silently omitted.
    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|entry| crate::names::same(&entry.name, name))
    }

    /// The clone of `name` that belongs with the project in `project_dir`
    /// (ticket 095): the one in that directory, else the only one there is
    /// anywhere (a shared repository kept elsewhere). Several elsewhere and
    /// none here is ambiguous — the same project name can live in two
    /// project directories — so it's `None` rather than a guess.
    pub fn find_near(&self, name: &str, project_dir: &Path) -> Option<&Entry> {
        let named: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|e| crate::names::same(&e.name, name))
            .collect();
        let project_dir =
            dunce::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
        let here = named
            .iter()
            .find(|entry| entry.root.parent() == Some(project_dir.as_path()));
        match (here, named.as_slice()) {
            (Some(entry), _) => Some(entry),
            (None, [only]) => Some(only),
            (None, _) => None,
        }
    }

    /// Other registered clones of the same repository as `root` (ticket
    /// 095): the same `origin` once normalised, or — when `root` has no
    /// `origin` — the same first commit as another clone without one (forks
    /// share first commits, so that's no use when there's a remote).
    pub fn same_remote(&self, root: &Path) -> Vec<PathBuf> {
        let root = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let others = self.entries.iter().filter(|entry| entry.root != root);
        match git::remote_identity(&root) {
            Some(remote) => others
                .filter(|entry| git::remote_identity(&entry.root).as_ref() == Some(&remote))
                .map(|entry| entry.root.clone())
                .collect(),
            None => {
                let Some(first) = git::root_commit(&root) else {
                    return Vec::new();
                };
                others
                    .filter(|entry| git::remote_url(&entry.root).is_none())
                    .filter(|entry| git::root_commit(&entry.root).as_ref() == Some(&first))
                    .map(|entry| entry.root.clone())
                    .collect()
            }
        }
    }
}

/// Where `name` is cloned for the project that `from` (any path inside its
/// tickets repository) belongs to — see [`Registry::find_near`] — or
/// `None`. A repository you haven't got can't be targeted (tickets
/// 101/107): no ticket can be pointed at it, and no branch made for it,
/// until it's cloned.
pub fn clone_of(from: &Path, name: &str) -> Option<PathBuf> {
    registered_clone_of(from, name).or_else(|| beside(from, name))
}

/// [`clone_of`], only a clone this machine's registry knows.
pub fn registered_clone_of(from: &Path, name: &str) -> Option<PathBuf> {
    Registry::load()
        .find_near(name, &project_dir_of(from))
        .map(|entry| entry.root.clone())
}

/// A clone of `name` in the project's own folder, beside its tickets
/// repository, whether or not it's registered (ticket 342): a project
/// cloned again, or copied, has them there before anyone runs `rite
/// register`, and they're cloned all the same.
pub fn beside(from: &Path, name: &str) -> Option<PathBuf> {
    let clone = crate::names::find_folder(&project_dir_of(from), name)?;
    git::is_repo_dir(&clone).then_some(clone)
}

/// The project directory `from` is in: the parent of the repository
/// holding it, found by walking up to the `.git` — no git call. `from`
/// itself when it isn't in a repository.
pub fn project_dir_of(from: &Path) -> PathBuf {
    let from = dunce::canonicalize(from).unwrap_or_else(|_| from.to_path_buf());
    from.ancestors()
        .find(|dir| crate::git::is_repo_dir(dir))
        .and_then(Path::parent)
        .unwrap_or(&from)
        .to_path_buf()
}

/// `repos` in the config folder ([`crate::places::config`]).
fn path() -> Option<PathBuf> {
    crate::places::config().map(|dir| dir.join("repos"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn repo(parent: &Path, name: &str) -> PathBuf {
        let root = parent.join(name);
        fs::create_dir_all(&root).unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["init", "-q"])
            .status()
            .unwrap();
        root
    }

    #[test]
    fn repositories_round_trip_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let alpha = repo(dir.path(), "alpha");
        let beta = repo(dir.path(), "beta");

        let mut registry = Registry::default();
        assert!(registry.add(&beta).unwrap());
        assert!(registry.add(&alpha).unwrap());
        // Sorted by name, so the list reads the same however it was built.
        assert_eq!(
            registry.entries.iter().map(|e| &e.name).collect::<Vec<_>>(),
            ["alpha", "beta"]
        );

        let path = dir.path().join("config/repos");
        registry.save_to(&path).unwrap();
        let loaded = Registry::load_from(&path);
        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(loaded.entries[0].name, "alpha");
    }

    #[test]
    fn adding_the_same_repository_twice_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let alpha = repo(dir.path(), "alpha");
        let mut registry = Registry::default();
        assert!(registry.add(&alpha).unwrap());
        assert!(!registry.add(&alpha).unwrap());
        assert_eq!(registry.entries.len(), 1);
    }

    #[test]
    fn only_git_repositories_can_be_registered() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain");
        fs::create_dir_all(&plain).unwrap();
        let mut registry = Registry::default();
        let err = registry.add(&plain).unwrap_err().to_string();
        assert!(err.contains("not a git repository"), "{err}");
        // A `.git` folder with nothing in it is no clone either (ticket 341).
        let hollow = dir.path().join("hollow");
        fs::create_dir_all(hollow.join(".git")).unwrap();
        let err = registry.add(&hollow).unwrap_err().to_string();
        assert!(err.contains("nothing in it"), "{err}");
        assert!(registry.entries.is_empty());
        let err = registry
            .add(&dir.path().join("nowhere"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("doesn't exist"), "{err}");
    }

    #[test]
    fn a_repository_is_found_whatever_case_its_name_is_typed_in() {
        // Ticket 338.
        let dir = tempfile::tempdir().unwrap();
        let kanrite = repo(dir.path(), "KanRite-tickets");
        let mut registry = Registry::default();
        registry.add(&kanrite).unwrap();
        for typed in ["KanRite-tickets", "kanrite-tickets", "KANRITE-TICKETS"] {
            assert_eq!(
                registry.find(typed).map(|e| e.root.clone()).as_ref(),
                Some(&registry.entries[0].root),
                "{typed}"
            );
            assert!(registry.find_near(typed, dir.path()).is_some(), "{typed}");
        }
        assert!(registry.find("other").is_none());
        assert!(registry.remove("kanrite-TICKETS"));
        assert!(registry.entries.is_empty());
    }

    #[test]
    fn two_clones_that_differ_only_in_case_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let lower = repo(dir.path(), "acme-tickets");
        let upper = dir.path().join("Acme-Tickets");
        // A disk that ignores case has only the one folder.
        if fs::create_dir(&upper).is_err() {
            return;
        }
        Command::new("git")
            .arg("-C")
            .arg(&upper)
            .args(["init", "-q"])
            .status()
            .unwrap();
        let mut registry = Registry::default();
        registry.add(&lower).unwrap();
        let err = registry.add(&upper).unwrap_err().to_string();
        assert!(err.contains("differ only in case"), "{err}");
        assert_eq!(registry.entries.len(), 1);
    }

    #[test]
    fn removal_works_by_name_or_by_path() {
        let dir = tempfile::tempdir().unwrap();
        let alpha = repo(dir.path(), "alpha");
        let beta = repo(dir.path(), "beta");
        let mut registry = Registry::default();
        registry.add(&alpha).unwrap();
        registry.add(&beta).unwrap();

        assert!(registry.remove("alpha"));
        assert!(!registry.remove("alpha"));
        assert!(registry.remove(beta.to_str().unwrap()));
        assert!(registry.entries.is_empty());
    }

    #[test]
    fn find_locates_a_registered_repo_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let alpha = repo(dir.path(), "alpha");
        let mut registry = Registry::default();
        registry.add(&alpha).unwrap();

        // Stored canonical: on Windows the temp folder's short name
        // (RUNNER~1) comes back long, so compare with the canonical form.
        assert_eq!(
            registry.find("alpha").unwrap().root,
            dunce::canonicalize(&alpha).unwrap()
        );
        assert!(registry.find("nope").is_none());
    }

    #[test]
    fn a_missing_file_is_an_empty_registry() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            Registry::load_from(&dir.path().join("nothing"))
                .entries
                .is_empty()
        );
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("repos");
        let one = repo(dir.path(), "one");
        let two = repo(dir.path(), "two");
        fs::write(
            &path,
            format!("# a comment\n\n{}\n  {}  \n", one.display(), two.display()),
        )
        .unwrap();
        let registry = Registry::load_from(&path);
        assert_eq!(registry.entries.len(), 2);
        assert_eq!(registry.entries[1].name, "two");
    }

    #[test]
    fn clones_that_are_gone_are_dropped_on_load_and_save() {
        let dir = tempfile::tempdir().unwrap();
        let here = repo(dir.path(), "here");
        let path = dir.path().join("repos");
        let gone = dir.path().join("gone");
        fs::write(&path, format!("{}\n{}\n", here.display(), gone.display())).unwrap();
        let registry = Registry::load_from(&path);
        assert_eq!(registry.entries.len(), 1);
        registry.save_to(&path).unwrap();
        assert!(!crate::eol::read(&path).unwrap().contains("gone"));
    }

    #[test]
    fn a_name_resolves_to_the_clone_in_the_project_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let (a, b, c) = (root.join("a"), root.join("b"), root.join("c"));
        let code_a = repo(&a, "p-code");
        let code_b = repo(&b, "p-code");
        let shared = repo(&c, "shared");
        let mut registry = Registry::default();
        for clone in [&code_a, &code_b, &shared] {
            registry.add(clone).unwrap();
        }
        assert_eq!(registry.find_near("p-code", &a).unwrap().root, code_a);
        assert_eq!(registry.find_near("p-code", &b).unwrap().root, code_b);
        // The only clone of a name is used from anywhere.
        assert_eq!(registry.find_near("shared", &a).unwrap().root, shared);
        // Two elsewhere and none here: no guess.
        assert!(registry.find_near("p-code", &root).is_none());
        assert_eq!(project_dir_of(&code_a.join("tasks")), a);
    }

    fn git(root: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["-c", "user.email=t@t.co", "-c", "user.name=t"])
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn clones_of_one_remote_are_found_however_its_url_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let one = repo(&root, "one");
        let two = repo(&root.join("elsewhere"), "one");
        let other = repo(&root, "other");
        git(
            &one,
            &["remote", "add", "origin", "git@github.com:Acme/board.git"],
        );
        git(
            &two,
            &["remote", "add", "origin", "https://github.com/acme/board"],
        );
        git(
            &other,
            &["remote", "add", "origin", "https://github.com/acme/other"],
        );
        let mut registry = Registry::default();
        for clone in [&one, &two, &other] {
            registry.add(clone).unwrap();
        }
        assert_eq!(registry.same_remote(&one), [two]);
        assert!(registry.same_remote(&other).is_empty());
    }

    #[test]
    fn without_an_origin_clones_are_matched_by_their_first_commit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let first = repo(&root, "first");
        git(&first, &["commit", "-q", "--allow-empty", "-m", "seed"]);
        let copy = root.join("copy");
        git(&root, &["clone", "-q", "first", "copy"]);
        git(&copy, &["remote", "remove", "origin"]);
        let unrelated = repo(&root, "unrelated");
        // A different message, or two empty commits made in the same second
        // are the same commit.
        git(
            &unrelated,
            &["commit", "-q", "--allow-empty", "-m", "another"],
        );
        let mut registry = Registry::default();
        for clone in [&first, &copy, &unrelated] {
            registry.add(clone).unwrap();
        }
        assert_eq!(registry.same_remote(&first), [copy]);
        assert!(registry.same_remote(&unrelated).is_empty());
    }
}
