//! `rite init <project>` — create or connect a project's dedicated tickets
//! *and* documents repositories, or (`--local-only`) just scaffold the
//! board here.
//!
//! Two sibling repositories, not one (`adr/rfc/0003-ticket-repository-topology-and-team-collaboration.md`
//! in `kanrite-documents`): `{project}-tickets` behaves exactly as it
//! always has — a board, direct pushes, no PR. `{project}-documents` holds
//! ADRs, RFCs, docs and ideas, pushed straight to main too since
//! kanrite-documents' ADR 0007 (it was PR-reviewed before). (Board
//! settings live in the tickets repository, ticket 111.) Both get the same
//! branch protection: no force-push or deletion of main (ticket 174).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use rite_lib::board::Board;
use rite_lib::board_settings::User;
use rite_lib::sync::{self, Purpose};
use rite_lib::transition::Event;
use rite_lib::{board_settings, git, names, registry::Registry};

use rite_lib::github;

pub fn run(project: String, local_only: bool, org: Option<String>) -> Result<()> {
    board_settings::validate_project_name(&project)?;

    if local_only {
        return init_here(&project);
    }

    // Guard, not a placement trick: an earlier version tried to infer a
    // sensible location (a sibling of whatever repo you happened to be
    // standing in) when run from inside one. That inference was a real
    // source of confusion — better to refuse outright and say so, than to
    // guess where you meant. `rite init <project>` must be run from a plain
    // directory; `--local-only` is the command for "inside a repo".
    if let Some(repo_root) = git::repo_root() {
        bail!(
            "rite init <project> must not be run inside a git repository (you're inside {}) — \
             cd to a plain directory first, or use --local-only to scaffold the board here instead",
            repo_root.display()
        );
    }
    let base = std::env::current_dir().context("reading the current directory")?;

    // Tickets first, documents second. Every board setting, `project`
    // included, lives in the tickets repository (ticket 111).
    let tickets = init_tickets(&base, &project, org.as_deref())?;
    let documents = init_documents(&base, &project, org.as_deref())?;
    // An archived project comes back (ticket 263).
    bring_back(&base, &tickets)?;

    println!("Initialised project '{project}':");
    println!("  tickets:    {}", tickets.display());
    if let Some(url) = git::remote_url(&tickets) {
        println!("              {url}");
    }
    println!("  documents: {}", documents.display());
    if let Some(url) = git::remote_url(&documents) {
        println!("              {url}");
    }
    crate::project::keep_existing(&base, &project)
}

/// Creates or connects `{project}-tickets`, scaffolds the board, registers
/// it, and — for a freshly created repository — protects its main branch
/// (as documents is, ticket 174). Unchanged in behaviour from
/// before this repository had a documents sibling; only pulled out of
/// `run()` so both repositories share the same creation path.
fn init_tickets(base: &Path, project: &str, org: Option<&str>) -> Result<PathBuf> {
    // An existing folder of that name in any case is the one (ticket 338);
    // two that differ only in case are refused.
    let folder = format!("{project}-tickets");
    let target = names::folder_named(base, &folder)?.unwrap_or_else(|| base.join(&folder));
    let full_name = format!("{}/{project}-tickets", owner_for(project, org));

    let created_new = if target.exists() {
        warn_if_already_present(&target)?;
        unarchive_on_github(&target)?;
        false
    } else if clone_or_create(base, &full_name, &target)? {
        // A freshly cloned, still-empty repository has no commits yet, so
        // its branch name comes from the local git's own defaultBranch
        // config rather than anything GitHub set — force it to `main`
        // before the first commit, regardless of that config.
        git::set_head_branch(&target, "main")?;
        true
    } else {
        false
    };

    let board = Board::in_repo(&target);
    // An existing clone may be behind its remote (ticket 094): catch it up
    // before anything is written, or the user lands on stale state and the
    // push conflicts with whatever the remote has since.
    let up_to_date = created_new || ready_to_record(&board, project);
    board.create()?;
    set_project(&board, project)?;
    // A new repository's initial commit below carries the entry; an existing
    // one gets a commit of its own.
    if up_to_date {
        add_first_user(&board, project, !created_new)?;
    }

    if created_new {
        write_gitattributes(&target)?;
        git::commit_all(&target, &format!("Initialise {project} tickets board"))?;
        git::push(&target)?;
        warn_if_protection_unavailable(&full_name, github::protect_main(&full_name)?);
    }

    register(&target)?;
    Ok(target)
}

/// Creates or connects `{project}-documents`: `adr/adr/`, `adr/rfc/`,
/// `docs/`, `ideas/`, `README.md` and `CHANGELOG.md` (ticket 168). No settings: they all live in the tickets
/// repository (ticket 111). Registered, same as tickets.
fn init_documents(base: &Path, project: &str, org: Option<&str>) -> Result<PathBuf> {
    let folder = format!("{project}-documents");
    let target = names::folder_named(base, &folder)?.unwrap_or_else(|| base.join(&folder));
    let full_name = format!("{}/{project}-documents", owner_for(project, org));

    let created_new = if target.exists() {
        warn_if_already_present(&target)?;
        unarchive_on_github(&target)?;
        false
    } else if clone_or_create(base, &full_name, &target)? {
        git::set_head_branch(&target, "main")?;
        true
    } else {
        false
    };

    scaffold_documents(&target, project)?;

    if created_new {
        write_gitattributes(&target)?;
        git::commit_all(&target, &format!("Initialise {project} documents"))?;
        git::push(&target)?;
        warn_if_protection_unavailable(&full_name, github::protect_main(&full_name)?);
    }

    register(&target)?;
    Ok(target)
}

/// A new tickets or documents repository's `.gitattributes` (ticket 277):
/// every text file LF in every checkout. Git for Windows checks text out
/// CRLF by default; rite reads that anyway (`rite_lib::eol`), but with this
/// a Windows clone holds the same bytes as anyone else's, and an editor
/// there keeps to LF. Only for a repository `init` creates: an existing one
/// would need a commit of its own, and nothing changes a repository by
/// itself (ADR 0008).
const GITATTRIBUTES: &str = "# Text files are LF on every system, Windows included (written by rite init).\n\
                             * text=auto eol=lf\n";

/// Writes [`GITATTRIBUTES`] into `target`, leaving one that's there alone.
fn write_gitattributes(target: &Path) -> Result<()> {
    let path = target.join(".gitattributes");
    if !path.exists() {
        std::fs::write(&path, GITATTRIBUTES)
            .with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

/// Registers `target` on this machine, so `rite`/`riteui` can find it.
pub(crate) fn register(target: &Path) -> Result<()> {
    let mut registry = Registry::load();
    registry.add(target)?;
    registry.save()?;
    warn_if_cloned_twice(&registry, target);
    Ok(())
}

/// Two clones of one repository work, but drift apart (ticket 095), so
/// registering a second one warns, naming the other. Never refuses.
pub(crate) fn warn_if_cloned_twice(registry: &Registry, target: &Path) {
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| target.display().to_string());
    for other in registry.same_remote(target) {
        eprintln!(
            "warning: {name} is also cloned at {} — two copies of one repository drift apart; \
             prefer one",
            other.display()
        );
    }
}

fn warn_if_already_present(target: &Path) -> Result<()> {
    if !git::is_repo_dir(target) {
        if git::has_empty_dot_git(target) {
            bail!("{}", git::not_a_repo(target));
        }
        bail!(
            "{} already exists and isn't a git repository — remove it or choose a \
             different project name",
            target.display()
        );
    }
    if git::remote_url(target).is_none() {
        eprintln!(
            "warning: {} has no 'origin' remote — it isn't visible to a team yet",
            target.display()
        );
    }
    Ok(())
}

fn warn_if_protection_unavailable(full_name: &str, outcome: github::ProtectionOutcome) {
    if let github::ProtectionOutcome::Unavailable = outcome {
        eprintln!(
            "warning: {full_name} has no branch protection — GitHub Pro is required for a \
             private repository on this plan. Direct pushes still work; only a force-push \
             or deletion of main would have been blocked."
        );
    }
}

/// `adr/adr/` (decisions), `adr/rfc/` (proposals), `docs/`, `ideas/`
/// (ticket 168), `README.md`, an empty `CHANGELOG.md` header, and
/// a `.settings/` directory carrying just the declared `project` — the
/// three fields ticket 057 also moves here (`repos`/`wip-limit`/
/// `lease-hours`) are left for a team to set later via a PR, not written
/// with defaults up front.
/// A documents repository's folders (ticket 168), each kept with a
/// `.gitkeep` until it has documents.
const DOCUMENTS_FOLDERS: [&str; 4] = ["adr/adr", "adr/rfc", "docs", "ideas"];

fn scaffold_documents(target: &Path, project: &str) -> Result<()> {
    for dir in DOCUMENTS_FOLDERS {
        std::fs::create_dir_all(target.join(dir))
            .with_context(|| format!("creating {}/{dir}", target.display()))?;
        std::fs::write(target.join(dir).join(".gitkeep"), "")?;
    }
    let readme = target.join("README.md");
    if !readme.exists() {
        std::fs::write(
            &readme,
            format!(
                "# {project} documents\n\nADRs, RFCs, docs and ideas for {project}.\n\n\
                 - `adr/adr/`: architecture decision records, numbered.\n\
                 - `adr/rfc/`: requests for comments, the proposals decisions come from.\n\
                 - `docs/`: notes and how-tos.\n\
                 - `ideas/`: ideas not yet worked into a ticket or an RFC.\n"
            ),
        )?;
    }
    let changelog = target.join("CHANGELOG.md");
    if !changelog.exists() {
        std::fs::write(&changelog, "# Changelog\n\n")?;
    }
    // No `.settings/` here any more: every board setting, `project`
    // included, lives in the tickets repository's `.settings` (ticket 111).
    Ok(())
}

/// `--local-only`: today's plain `rite init` behaviour, plus recording the
/// declared project name.
fn init_here(project: &str) -> Result<()> {
    let board = Board::locate()?;
    board.create()?;
    set_project(&board, project)?;
    // `--local-only` never commits; the entry is there for the next one.
    add_first_user(&board, project, false)?;
    // Best-effort, same as the non-local-only path above: registering this
    // machine's own copy of the board it was just created in, so it's
    // findable without a separate `rite register` immediately afterward.
    let mut registry = Registry::load();
    if registry.add(board.repo_root()).is_ok() {
        let _ = registry.save();
    }
    println!("Initialised a kanrite board at {}", board.root.display());
    Ok(())
}

/// Whether an existing tickets clone is fit to record the user on (ticket
/// 094). A clone with uncommitted edits to tracked files is left alone, as
/// is one that can't be brought up to date ([`sync::catch_up`] refuses a
/// clone that has diverged). Otherwise it's caught up exactly as any other
/// change would be: fast-forwarded when only behind, its own earlier
/// commits pushed when only ahead. No remote, or `sync = off`, is as before.
/// Never resets or stashes: this may be someone's real working copy.
fn ready_to_record(board: &Board, project: &str) -> bool {
    let repo = board.repo_root();
    let skipped = |why: &str| {
        eprintln!(
            "warning: {why}, so you weren't added to {project}'s users — run rite init {project} \
             again once it's up to date"
        );
        false
    };
    if git::remote_url(repo).is_none() {
        return true;
    }
    if git::has_local_edits(repo) == Some(true) {
        return skipped(&format!(
            "{} has uncommitted changes — commit or stash them first",
            repo.display()
        ));
    }
    match sync::catch_up(board, Purpose::Write) {
        Ok(events) => {
            for event in events {
                match event {
                    Event::Info(text) => println!("{text}"),
                    Event::Warn(text) => eprintln!("warning: {text}"),
                }
            }
            true
        }
        Err(err) => skipped(&format!("{err:#}")),
    }
}

/// Records whoever is running `init` — git's `user.name <user.email>` — in
/// the project's `users` (ticket 090), so a project never starts with
/// nobody on it. Re-running `init` on an existing project is how one made
/// before this existed gets its first user; already being listed is a
/// no-op. With `commit`, the change is committed on its own and pushed
/// when there's an `origin`.
///
/// Deliberately *not* `join`'s reset-and-retry when the push is rejected:
/// that is safe only in a clone seconds old, and this may be someone's real
/// working copy. The commit stays, with a warning to pull and push.
///
/// No git email is a warning, not a failure: the project itself was made,
/// and failing now would misreport that.
fn add_first_user(board: &Board, project: &str, commit: bool) -> Result<()> {
    let repo = board.repo_root();
    let Some(user) = User::from_git(repo) else {
        eprintln!(
            "warning: git has no user.email, so you weren't added to {project}'s users — set it \
             (git config --global user.email ...) and run rite init {project} again"
        );
        return Ok(());
    };
    let mut settings = board_settings::load(board);
    if !settings.add_user(&user)? {
        return Ok(());
    }
    settings.save(board)?;
    println!("added you ({user}) to {project}'s users");
    if !commit {
        return Ok(());
    }
    let paths: BTreeSet<PathBuf> = [PathBuf::from(".settings")].into();
    let message = format!("init: add {user} to users");
    let result = match git::remote_url(repo) {
        Some(_) => git::commit_paths(repo, &message, &paths),
        None => git::commit_paths_locally(repo, &message, &paths),
    };
    if let Err(err) = result {
        eprintln!(
            "warning: added you to {project}'s users locally, but couldn't commit and push it: \
             {err:#} — pull, then push, to share it"
        );
    }
    Ok(())
}

fn set_project(board: &Board, project: &str) -> Result<()> {
    let mut settings = board_settings::load(board);
    settings.set_project(project)?;
    settings.save(board)
}

/// Which GitHub owner (organisation or personal account) both repositories
/// are created under: `--org`, when given, otherwise the project name
/// itself. On GitHub, an owner is always either an org or a user account,
/// and the project name is expected to name one of those directly — not
/// `gh`'s own default (the CLI's authenticated account), which has no
/// reason to match the project at all.
pub(crate) fn owner_for<'a>(project: &'a str, org: Option<&'a str>) -> &'a str {
    org.unwrap_or(project)
}

/// Clones `full_name` into `target` when it's already on GitHub (ticket
/// 263: never a second creation attempt), unarchiving it there first if
/// it's archived, since an archived repository refuses the pushes `init`
/// makes; otherwise creates it, cloned. Returns whether it was created.
fn clone_or_create(base: &Path, full_name: &str, target: &Path) -> Result<bool> {
    match github_archived(full_name) {
        Some(archived) => {
            if archived {
                rite_lib::archive::gh("unarchive", full_name)?;
                println!("unarchived {full_name} on GitHub");
            }
            let clone = Command::new("gh")
                .current_dir(base)
                .args([
                    "repo",
                    "clone",
                    full_name,
                    target.to_str().unwrap_or(full_name),
                ])
                .status()
                .context("running gh repo clone — is the gh CLI installed and authenticated?")?;
            if !clone.success() {
                bail!("gh repo clone {full_name} failed");
            }
            Ok(false)
        }
        None => {
            create_and_clone(base, full_name, target)?;
            Ok(true)
        }
    }
}

/// Whether `full_name` on GitHub is archived; `None` when it isn't there
/// (or `gh` can't say).
fn github_archived(full_name: &str) -> Option<bool> {
    let out = Command::new("gh")
        .args([
            "repo",
            "view",
            full_name,
            "--json",
            "isArchived",
            "--jq",
            ".isArchived",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    match String::from_utf8_lossy(&out.stdout).trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// An existing clone whose GitHub repository is archived: unarchived there
/// (ticket 263), so what `init` writes can be pushed. A clone that isn't on
/// GitHub, or that `gh` can't see, is left as it is.
fn unarchive_on_github(target: &Path) -> Result<()> {
    let Some(slug) = rite_lib::import_issues::github_slug(target) else {
        return Ok(());
    };
    if github_archived(&slug) == Some(true) {
        rite_lib::archive::gh("unarchive", &slug)?;
        println!("unarchived {slug} on GitHub");
    }
    Ok(())
}

/// Brings an archived project back (ticket 263): its declared repositories
/// unarchived on GitHub, `archived` turned off and pushed, and every
/// declared repository already cloned beside the tickets repository
/// registered again. `rite clone --all` fetches the rest.
fn bring_back(base: &Path, tickets: &Path) -> Result<()> {
    let board = Board::in_repo(tickets);
    if !rite_lib::archive::is_archived(&board) {
        return Ok(());
    }
    let mut keep = |_: &Board| -> Result<Vec<String>> { Ok(Vec::new()) };
    for event in
        rite_lib::archive::set_archived(&board, false, true, &mut rite_lib::archive::gh, &mut keep)?
    {
        match event {
            Event::Info(text) => println!("{text}"),
            Event::Warn(text) => eprintln!("warning: {text}"),
        }
    }
    let mut missing = Vec::new();
    for name in board_settings::load(&board).repos() {
        let clone = base.join(&name);
        if git::is_repo_dir(&clone) {
            register(&clone)?;
        } else {
            missing.push(name);
        }
    }
    if !missing.is_empty() {
        println!(
            "not cloned here: {} — `rite clone --all` fetches them",
            missing.join(", ")
        );
    }
    Ok(())
}

/// Creates `full_name` and clones it as a child of `base` (the current
/// directory — `run()` has already refused to get here from inside an
/// existing repository), at `target`. Recovers from a previous run that
/// died after creation but before cloning by cloning the existing remote
/// instead of failing.
fn create_and_clone(base: &Path, full_name: &str, target: &Path) -> Result<()> {
    let create = Command::new("gh")
        .current_dir(base)
        .args(["repo", "create", full_name, "--private", "--clone"])
        .output()
        .context("running gh repo create — is the gh CLI installed and authenticated?")?;
    if create.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&create.stderr);
    if !stderr.to_lowercase().contains("already exists") {
        bail!("gh repo create {full_name} failed: {}", stderr.trim());
    }
    // Recoverable: a previous run already created the remote repository —
    // clone it instead, the same way `gh repo create --clone` would have.
    let clone = Command::new("gh")
        .current_dir(base)
        .args([
            "repo",
            "clone",
            full_name,
            target.to_str().unwrap_or(full_name),
        ])
        .status()
        .context("running gh repo clone")?;
    if !clone.success() {
        bail!("gh repo clone {full_name} failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_with_an_empty_dot_git_is_refused_before_anything_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("proj-tickets");
        std::fs::create_dir_all(target.join(".git")).unwrap();
        let err = warn_if_already_present(&target).unwrap_err().to_string();
        assert!(err.contains("nothing in it"), "{err}");
        // A folder with no `.git` at all keeps its own message.
        let plain = dir.path().join("proj-documents");
        std::fs::create_dir_all(&plain).unwrap();
        let err = warn_if_already_present(&plain).unwrap_err().to_string();
        assert!(err.contains("isn't a git repository"), "{err}");
    }

    #[test]
    fn the_owner_defaults_to_the_project_name() {
        assert_eq!(owner_for("agile-md", None), "agile-md");
    }

    #[test]
    fn an_explicit_org_overrides_the_project_name() {
        assert_eq!(
            owner_for("agile-md", Some("some-other-org")),
            "some-other-org"
        );
    }

    #[test]
    fn scaffold_documents_creates_the_folders_and_files() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("proj-documents");
        std::fs::create_dir_all(&target).unwrap();
        scaffold_documents(&target, "proj").unwrap();

        for sub in DOCUMENTS_FOLDERS {
            assert!(target.join(sub).is_dir(), "{sub} missing");
        }
        assert!(target.join("README.md").exists());
        assert!(target.join("CHANGELOG.md").exists());
        assert!(
            !target.join(".settings").exists(),
            "settings live in the tickets repository now (ticket 111)"
        );
    }

    #[test]
    fn a_new_repository_gets_lf_line_endings_and_an_existing_file_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        write_gitattributes(dir.path()).unwrap();
        let written = std::fs::read_to_string(dir.path().join(".gitattributes")).unwrap();
        assert!(
            written.lines().any(|line| line == "* text=auto eol=lf"),
            "{written}"
        );

        std::fs::write(dir.path().join(".gitattributes"), "*.md text\n").unwrap();
        write_gitattributes(dir.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".gitattributes")).unwrap(),
            "*.md text\n"
        );
    }

    #[test]
    fn scaffold_documents_never_overwrites_an_existing_readme() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("proj-documents");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("README.md"), "hand-written\n").unwrap();
        scaffold_documents(&target, "proj").unwrap();
        assert_eq!(
            std::fs::read_to_string(target.join("README.md")).unwrap(),
            "hand-written\n"
        );
    }
}
