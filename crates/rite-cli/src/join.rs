//! `rite join <project>` — join a project someone else already created with
//! `rite init`: clone its tickets and documents repositories as siblings
//! here, register them, and add yourself to the project's `users` (ticket
//! 088).
//!
//! Deliberately plain `git`, never `gh`: joining needs nothing more than
//! read/write access to two existing repositories, so the user's own SSH key
//! is the only credential — no API token, nothing created, no branch
//! protection touched. That is the difference from `init` (which creates)
//! and from `register` (which records a repository already on this machine).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use rite_lib::board::Board;
use rite_lib::board_settings::{self, User};
use rite_lib::git;

use crate::init::{owner_for, register};

/// Where the repositories are cloned from unless `RITE_GIT_BASE` says
/// otherwise — GitHub over SSH. The override is what lets the test suite
/// join from local bare repositories, and a team on another forge use it.
const DEFAULT_GIT_BASE: &str = "git@github.com:";

/// How many times the `users` change is re-applied on top of a remote that
/// moved under it before giving up. Several people joining in the same few
/// seconds each lose a round to whoever pushed first, so this has to cover
/// more than two-way contention.
const PUSH_ATTEMPTS: u32 = 5;

pub fn run(project: String, org: Option<String>) -> Result<()> {
    board_settings::validate_project_name(&project)?;
    if let Some(repo_root) = git::repo_root() {
        bail!(
            "rite join <project> must not be run inside a git repository (you're inside {}) — \
             cd to the plain directory that should hold the project first",
            repo_root.display()
        );
    }
    let base = std::env::current_dir().context("reading the current directory")?;
    let tickets = base.join(format!("{project}-tickets"));
    let documents = base.join(format!("{project}-documents"));
    for target in [&tickets, &documents] {
        // A folder of that name in another case is in the way too, whatever
        // the disk says about case (ticket 338).
        let folder = target
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some(other) = rite_lib::names::folder_named(&base, &folder)? {
            bail!(
                "{} already exists — rite join only clones into a directory that doesn't                  have the project yet",
                other.display()
            );
        }
        // symlink_metadata, not exists(): a dangling symlink is still in the way.
        if target.symlink_metadata().is_ok() {
            bail!(
                "{} already exists — rite join only clones into a directory that doesn't \
                 have the project yet",
                target.display()
            );
        }
    }

    let git_base = rite_lib::renamed::var("RITE_GIT_BASE")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_GIT_BASE.to_string());
    let owner = owner_for(&project, org.as_deref());

    // Everything up to and including the push either completes or leaves
    // nothing behind, so running `rite join` again starts from a clean slate
    // instead of tripping over its own half-finished clones.
    let mut created = Vec::new();
    let joined = clone_and_join(
        &git_base,
        owner,
        &project,
        &tickets,
        &documents,
        &mut created,
    );
    let (user, added) = match joined {
        Ok(joined) => joined,
        Err(err) => {
            for dir in &created {
                let _ = std::fs::remove_dir_all(dir);
            }
            return Err(err);
        }
    };

    register(&tickets)?;
    register(&documents)?;

    println!("Joined project '{project}' as {user}:");
    println!("  tickets:   {}", tickets.display());
    println!("  documents: {}", documents.display());
    if !added {
        println!("(you were already one of its users)");
    }
    crate::project::keep_existing(&base, &project)
}

/// The fallible middle of `run`: both clones, the check that this really is
/// an rite project, and adding the user. Every directory it creates is pushed
/// onto `created` the moment it exists, so the caller can clean up.
fn clone_and_join(
    git_base: &str,
    owner: &str,
    project: &str,
    tickets: &Path,
    documents: &Path,
    created: &mut Vec<PathBuf>,
) -> Result<(User, bool)> {
    for (target, kind) in [(tickets, "tickets"), (documents, "documents")] {
        let url = repo_url(git_base, owner, project, kind);
        let result = git::clone(&url, target);
        if target.exists() {
            created.push(target.to_path_buf());
        }
        result.with_context(|| format!("cloning {project}-{kind}"))?;
    }

    let board = Board::in_repo(tickets);
    if !board.root.is_dir() {
        bail!(
            "{project}-tickets has no board — it isn't an rite project yet; whoever owns it \
             should run `rite init {project}` first"
        );
    }
    if let Some(declared) = board_settings::load(&board).project()
        && declared != project
    {
        bail!("{project}-tickets declares project '{declared}', not '{project}'");
    }

    let user = identity(tickets)?;
    let added = add_user_and_push(&board, &user)?;
    Ok((user, added))
}

/// `{base}{owner}/{project}-{kind}.git` — `git@github.com:` + `grimartz/grimartz-tickets.git`.
fn repo_url(base: &str, owner: &str, project: &str, kind: &str) -> String {
    format!("{base}{owner}/{project}-{kind}.git")
}

/// Who's joining: git's own identity, the same `user.name`/`user.email` that
/// signs their commits and that leases record.
fn identity(repo: &Path) -> Result<User> {
    User::from_git(repo).ok_or_else(|| {
        anyhow::anyhow!("git has no user.email — set it (git config --global user.email ...) first")
    })
}

/// Adds `user` to the board's `users`, commits and pushes. A rejected push
/// means someone else (another joiner, a ticket move) got there first: the
/// clone is seconds old and its only local commit is this one, so it is
/// reset onto the remote and the change re-applied, rather than rebased
/// through a conflict on the same line. Returns whether the user was new.
fn add_user_and_push(board: &Board, user: &User) -> Result<bool> {
    let repo = board.repo_root();
    let settings_file: BTreeSet<PathBuf> = [PathBuf::from(".settings")].into();
    let branch = git::current_branch(repo).unwrap_or_else(|| "main".to_string());
    let mut attempt = 1;
    loop {
        let mut settings = board_settings::load(board);
        if !settings.add_user(user)? {
            return Ok(false);
        }
        settings.save(board)?;
        let result = git::commit_paths(repo, &format!("join: {user}"), &settings_file);
        match result {
            Ok(()) => return Ok(true),
            Err(err) if attempt >= PUSH_ATTEMPTS => {
                return Err(err).context(format!(
                    "could not add you to the project's users after {PUSH_ATTEMPTS} attempts"
                ));
            }
            Err(_) => {
                backoff(attempt);
                git::fetch(repo)?;
                git::reset_hard(repo, &format!("origin/{branch}"))?;
                attempt += 1;
            }
        }
    }
}

/// A short, jittered pause before retrying, growing with each attempt.
/// Without the jitter, joiners that lost the same round retry in lockstep
/// and collide again. The clock's nanoseconds mixed with the process id are
/// random enough for spreading out a handful of pushes; no RNG dependency
/// for this one use.
fn backoff(attempt: u32) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or_default();
    let jitter = u64::from(nanos ^ std::process::id().wrapping_mul(2_654_435_761)) % 250;
    std::thread::sleep(std::time::Duration::from_millis(
        100 * u64::from(attempt) + jitter,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_url_joins_base_owner_project_and_kind() {
        assert_eq!(
            repo_url(DEFAULT_GIT_BASE, "grimartz", "grimartz", "tickets"),
            "git@github.com:grimartz/grimartz-tickets.git"
        );
        assert_eq!(
            repo_url("/srv/git/", "acme", "widgets", "documents"),
            "/srv/git/acme/widgets-documents.git"
        );
    }
}
