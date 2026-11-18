//! `rite doctor` (ticket 351): look over this machine's KanRite setup, say
//! what's wrong and the exact command that puts it right, and change nothing
//! unless asked.
//!
//! Each check gives one or more [`Finding`]s: `ok` when it found nothing,
//! else a `warning` (clutter that does no harm, or something worth knowing)
//! or a `problem` (something broken, or a project you can't see). The CLI
//! exits non-zero while a problem is left.
//!
//! [`fix`] changes **only rite's own per-machine files** in the config
//! folder: the registry (`repos`), `ui.hidden-projects` and
//! `ui.folded-graphs` in `config`, and `project-trust`. Never a repository
//! (no commit, no push, no `.settings`), never another tool's config, never
//! a folder. Registering a project here is local only, unlike `rite
//! register`, which also declares the repository for the whole team.
//!
//! Everything takes a [`Machine`] (the config folder and the temporary
//! folder), so tests need no environment fiddling.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;

use crate::archive;
use crate::board::Board;
use crate::board_settings;
use crate::current_project;
use crate::git;
use crate::names;
use crate::registry::{self, Registry};
use crate::settings::Config;

/// Where this machine keeps rite's files, and its temporary folder (boards
/// under it are scratch, which the landing page skips too).
#[derive(Clone, Debug)]
pub struct Machine {
    pub config: PathBuf,
    pub temp: PathBuf,
}

impl Machine {
    /// This machine's: [`crate::places::config`] and the system's temporary
    /// folder. `None` when there's no config folder at all.
    pub fn here() -> Option<Machine> {
        Some(Machine {
            config: crate::places::config()?,
            temp: std::env::temp_dir(),
        })
    }

    fn repos(&self) -> PathBuf {
        self.config.join("repos")
    }

    fn settings(&self) -> PathBuf {
        self.config.join("config")
    }

    fn trust(&self) -> PathBuf {
        self.config.join("project-trust")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warning,
    Problem,
}

impl Status {
    pub fn word(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warning => "warning",
            Status::Problem => "problem",
        }
    }
}

/// How to put a finding right.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Fix {
    /// The command to run.
    pub command: String,
    /// Whether `rite doctor --fix` does it.
    pub automatic: bool,
}

/// One thing a check found.
#[derive(Clone, Debug, Serialize)]
pub struct Finding {
    /// Which check: stable, for scripts and riteui.
    pub check: &'static str,
    pub status: Status,
    pub title: String,
    /// More to say, a line each.
    pub detail: Vec<String>,
    pub fix: Option<Fix>,
}

impl Finding {
    fn ok(check: &'static str, title: &str) -> Finding {
        Finding {
            check,
            status: Status::Ok,
            title: title.to_string(),
            detail: Vec::new(),
            fix: None,
        }
    }

    fn found(check: &'static str, status: Status, title: String) -> Finding {
        Finding {
            check,
            status,
            title,
            detail: Vec::new(),
            fix: None,
        }
    }

    fn because(mut self, line: impl Into<String>) -> Finding {
        self.detail.push(line.into());
        self
    }

    fn automatic(mut self) -> Finding {
        self.fix = Some(Fix {
            command: "rite doctor --fix".to_string(),
            automatic: true,
        });
        self
    }

    pub fn by_hand(mut self, command: impl Into<String>) -> Finding {
        self.fix = Some(Fix {
            command: command.into(),
            automatic: false,
        });
        self
    }
}

/// The check ids, in the order they run.
pub const REGISTRY_MISSING: &str = "registry-missing";
pub const REGISTRY_NOT_A_REPO: &str = "registry-not-a-repo";
pub const UNREGISTERED_PROJECT: &str = "unregistered-project";
pub const DECLARED_NOT_REGISTERED: &str = "declared-not-registered";
pub const STALE_HIDDEN: &str = "stale-hidden";
pub const STALE_FOLDED: &str = "stale-folded";
pub const STALE_TRUST: &str = "stale-trust";
/// The install, found on the PATH (ticket 385).
pub const NO_RITE_ON_PATH: &str = "no-rite-on-path";
pub const INSTALL_UPDATABLE: &str = "install-updatable";
pub const INSTALL_OLD_MARKER: &str = "install-old-marker";
pub const INSTALL_COPIES: &str = "install-copies";
/// An older version's per-machine folder still in place.
pub const FOLDER_STRANDED: &str = "folder-stranded";

/// `my_work`'s keys, in the same shape (comma-separated folders).
const HIDDEN: &str = crate::my_work::HIDDEN;
const FOLDED: &str = crate::my_work::FOLDED;

/// Every check this library knows, run against `machine`. Reads only.
pub fn check(machine: &Machine) -> Vec<Finding> {
    let state = State::read(machine);
    let mut out = Vec::new();
    out.extend(or_ok(
        registry_missing(&state),
        REGISTRY_MISSING,
        "every registered folder is there",
    ));
    out.extend(or_ok(
        registry_not_a_repo(&state),
        REGISTRY_NOT_A_REPO,
        "every registered folder is a git repository",
    ));
    out.extend(or_ok(
        unregistered_projects(&state),
        UNREGISTERED_PROJECT,
        "every project found on this machine is registered",
    ));
    out.extend(or_ok(
        declared_not_registered(&state),
        DECLARED_NOT_REGISTERED,
        "every declared repository cloned beside its project is registered",
    ));
    out.extend(or_ok(
        stale_paths(&state, HIDDEN, STALE_HIDDEN, "hidden project"),
        STALE_HIDDEN,
        "no hidden project is missing",
    ));
    out.extend(or_ok(
        stale_paths(&state, FOLDED, STALE_FOLDED, "folded sprint graph"),
        STALE_FOLDED,
        "no folded sprint graph is for a missing project",
    ));
    out.extend(or_ok(
        stale_trust(&state),
        STALE_TRUST,
        "every project-trust entry is for a board that's there",
    ));
    out
}

fn or_ok(found: Vec<Finding>, check: &'static str, title: &str) -> Vec<Finding> {
    if found.is_empty() {
        vec![Finding::ok(check, title)]
    } else {
        found
    }
}

/// What the checks read, read once.
struct State {
    machine: Machine,
    /// The registry's lines as written, missing folders and all.
    raw_registry: Vec<PathBuf>,
    /// The registry as rite loads it (missing folders left out).
    registry: Registry,
    settings: Config,
    trust: Config,
}

impl State {
    fn read(machine: &Machine) -> State {
        let raw_registry = crate::eol::read(machine.repos())
            .map(|text| {
                text.lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty() && !line.starts_with('#'))
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default();
        State {
            machine: machine.clone(),
            raw_registry,
            registry: Registry::load_from(&machine.repos()),
            settings: Config::load_from(&machine.settings()),
            trust: Config::load_from(&machine.trust()),
        }
    }

    fn paths(&self, key: &str) -> Vec<PathBuf> {
        list(&self.settings, key)
    }

    fn registered(&self, root: &Path) -> bool {
        let root = canonical(root);
        self.registry
            .entries
            .iter()
            .any(|entry| canonical(&entry.root) == root)
    }

    fn is_scratch(&self, root: &Path) -> bool {
        canonical(root).starts_with(canonical(&self.machine.temp))
    }
}

fn canonical(path: &Path) -> PathBuf {
    dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// A comma-separated list of folders, as `my_work` keeps them.
fn list(config: &Config, key: &str) -> Vec<PathBuf> {
    config
        .get(key)
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

fn registry_missing(state: &State) -> Vec<Finding> {
    state
        .raw_registry
        .iter()
        .filter(|root| !root.is_dir())
        .map(|root| {
            Finding::found(
                REGISTRY_MISSING,
                Status::Warning,
                format!("registered folder {} is gone", root.display()),
            )
            .because("rite already skips it; --fix takes it off this machine's list")
            .automatic()
        })
        .collect()
}

fn registry_not_a_repo(state: &State) -> Vec<Finding> {
    state
        .registry
        .entries
        .iter()
        .filter(|entry| !git::is_repo_dir(&entry.root))
        .map(|entry| {
            Finding::found(
                REGISTRY_NOT_A_REPO,
                Status::Problem,
                format!("{} is registered but isn't a git repository", entry.name),
            )
            .because(git::not_a_repo(&entry.root))
            .by_hand(format!(
                "clone {} again into {}",
                entry.name,
                entry.root.display()
            ))
        })
        .collect()
}

/// A repository with a board that rite would show as a project, but which
/// isn't registered, so the landing page (and its Hidden list) can't show
/// it. Looked for where this machine has pointed at projects before: the
/// hidden and folded lists, project trust, and the project folders beside
/// each registered repository's own (two levels, never the whole disk).
fn unregistered_candidates(state: &State) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let mut consider = |root: PathBuf| {
        let root = canonical(&root);
        if !seen.insert(root.clone()) {
            return;
        }
        if git::is_repo_dir(&root)
            && Board::in_repo(&root).root.is_dir()
            && !state.registered(&root)
            && !state.is_scratch(&root)
            && !archive::is_archived(&Board::in_repo(&root))
        {
            out.push(root);
        }
    };
    for root in state.paths(HIDDEN).into_iter().chain(state.paths(FOLDED)) {
        consider(root);
    }
    for board in state.trust.keys() {
        if let Some(repo) = Path::new(board).parent() {
            consider(repo.to_path_buf());
        }
    }
    let mut parents = BTreeSet::new();
    for entry in &state.registry.entries {
        if let Some(parent) = registry::project_dir_of(&entry.root).parent() {
            parents.insert(parent.to_path_buf());
        }
    }
    for parent in parents {
        for project_dir in subfolders(&parent) {
            for repo in subfolders(&project_dir) {
                consider(repo);
            }
        }
    }
    out
}

/// The folders in `dir`, hidden ones left out, sorted.
fn subfolders(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = read
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

fn unregistered_projects(state: &State) -> Vec<Finding> {
    let hidden: Vec<PathBuf> = state.paths(HIDDEN).iter().map(|p| canonical(p)).collect();
    unregistered_candidates(state)
        .into_iter()
        .map(|root| {
            let name = current_project::project_name(&root);
            let mut finding = Finding::found(
                UNREGISTERED_PROJECT,
                Status::Problem,
                format!("project {name} isn't registered on this machine"),
            )
            .because(format!(
                "{} has a board, but riteui's landing page and its Hidden list show only registered projects",
                root.display()
            ))
            .because(
                "--fix registers it and the repositories beside it on this machine only: nothing is committed",
            );
            if hidden.contains(&root) {
                finding = finding.because(format!(
                    "it's hidden: once registered it's in the Hidden list, or `rite mine --unhide {name}` shows it"
                ));
            }
            finding.automatic()
        })
        .collect()
}

/// The repositories to register for the project whose board is in `root`:
/// itself, its `{project}-documents` beside it, and each repository it
/// declares that's cloned beside it.
fn project_repos(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf()];
    let dir = registry::project_dir_of(root);
    if let Some(base) = root
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix("-tickets"))
        && let Some(documents) = names::find_folder(&dir, &format!("{base}-documents"))
        && git::is_repo_dir(&documents)
    {
        out.push(documents);
    }
    for name in board_settings::load(&Board::in_repo(root)).repos() {
        if let Some(clone) = registry::beside(root, &name)
            && !out.contains(&clone)
        {
            out.push(clone);
        }
    }
    out
}

fn declared_not_registered(state: &State) -> Vec<Finding> {
    let mut out = Vec::new();
    for entry in &state.registry.entries {
        if !entry.has_board() {
            continue;
        }
        for name in board_settings::load(&entry.board()).repos() {
            let Some(clone) = registry::beside(&entry.root, &name) else {
                continue;
            };
            if state.registered(&clone) {
                continue;
            }
            out.push(
                Finding::found(
                    DECLARED_NOT_REGISTERED,
                    Status::Warning,
                    format!(
                        "{name}, declared by {}, is cloned but not registered",
                        current_project::project_name(&entry.root)
                    ),
                )
                .because(format!("{} isn't on this machine's list", clone.display()))
                .automatic(),
            );
        }
    }
    out
}

fn stale_paths(state: &State, key: &str, check: &'static str, what: &str) -> Vec<Finding> {
    state
        .paths(key)
        .into_iter()
        .filter(|path| !path.is_dir())
        .map(|path| {
            Finding::found(
                check,
                Status::Warning,
                format!("{what} {} is gone", path.display()),
            )
            .because(format!(
                "{key} in {} names it; it does nothing",
                state.machine.settings().display()
            ))
            .automatic()
        })
        .collect()
}

fn stale_trust(state: &State) -> Vec<Finding> {
    let gone: Vec<&str> = state
        .trust
        .keys()
        .filter(|board| !Path::new(board).is_dir())
        .collect();
    if gone.is_empty() {
        return Vec::new();
    }
    let mut finding = Finding::found(
        STALE_TRUST,
        Status::Warning,
        if gone.len() == 1 {
            "1 project-trust entry is for a board that's gone".to_string()
        } else {
            format!(
                "{} project-trust entries are for boards that are gone",
                gone.len()
            )
        },
    );
    const SHOWN: usize = 5;
    for board in gone.iter().take(SHOWN) {
        finding = finding.because(board.to_string());
    }
    if gone.len() > SHOWN {
        finding = finding.because(format!("and {} more", gone.len() - SHOWN));
    }
    vec![finding.automatic()]
}

// ---- The install, and the per-machine folders (ticket 385). Read only: ----
// ---- rite never replaces its own binary or moves a folder from here.     ----

use std::ffi::OsStr;

use crate::places;
use crate::update_apply::{self, Install};

/// The `rite` files on a PATH, first to last, one per distinct file.
pub fn rites_on_path(path_var: Option<&OsStr>) -> Vec<PathBuf> {
    let name = update_apply::file_name("rite");
    let mut found: Vec<PathBuf> = Vec::new();
    for dir in path_var.map(std::env::split_paths).into_iter().flatten() {
        let file = dir.join(&name);
        if !file.is_file() {
            continue;
        }
        let file = dunce::canonicalize(&file).unwrap_or(file);
        if !found.contains(&file) {
            found.push(file);
        }
    }
    found
}

/// The install the `rite` first on `path_var` belongs to: can `rite update`
/// replace it, is its marker current, and is there only one `rite` on the
/// PATH. `rite` on the PATH is what AI tools and scripts run, so that's the
/// one judged, not the binary that happens to be running this check.
pub fn install(path_var: Option<&OsStr>) -> Vec<Finding> {
    let found = rites_on_path(path_var);
    let Some(first) = found.first() else {
        return vec![
            Finding::found(
                NO_RITE_ON_PATH,
                Status::Warning,
                "no `rite` is on the PATH".to_string(),
            )
            .because("AI tools start `rite mcp` by that name, and so do scripts")
            .by_hand(update_apply::INSTALL_COMMAND),
        ];
    };
    let mut out = vec![install_updatable(first)];
    if let Some(dir) = first.parent()
        && update_apply::old_marker(dir)
    {
        out.push(
            Finding::found(
                INSTALL_OLD_MARKER,
                Status::Warning,
                format!(
                    "the install in {} has a marker from before the rename",
                    dir.display()
                ),
            )
            .because("`rite update` still reads it, but the installer writes a new one")
            .by_hand(update_apply::INSTALL_COMMAND),
        );
    } else {
        out.push(Finding::ok(
            INSTALL_OLD_MARKER,
            "the install's marker is current",
        ));
    }
    out.extend(or_ok(
        copies(&found),
        INSTALL_COPIES,
        "one `rite` on the PATH",
    ));
    out
}

fn install_updatable(rite: &Path) -> Finding {
    match update_apply::install_dir(rite) {
        Install::Managed(dir) => Finding::ok(
            INSTALL_UPDATABLE,
            &format!(
                "rite in {} is a release install `rite update` manages",
                dir.display()
            ),
        ),
        // A source or cargo build is updated from its clone: not a fault.
        Install::Unmanaged {
            why,
            reinstall: false,
        } => Finding::ok(INSTALL_UPDATABLE, "rite is a source or cargo build").because(why),
        Install::Unmanaged { why, .. } => Finding::found(
            INSTALL_UPDATABLE,
            Status::Warning,
            "`rite update` can't replace the `rite` on the PATH".to_string(),
        )
        .because(why)
        .by_hand(update_apply::INSTALL_COMMAND),
    }
}

fn copies(found: &[PathBuf]) -> Vec<Finding> {
    if found.len() < 2 {
        return Vec::new();
    }
    let mut finding = Finding::found(
        INSTALL_COPIES,
        Status::Warning,
        format!("{} different `rite` files are on the PATH", found.len()),
    );
    for (n, file) in found.iter().enumerate() {
        let note = if n == 0 { " (the one that runs)" } else { "" };
        finding = finding.because(format!("{}{note}", file.display()));
    }
    vec![finding.because(
        "delete the ones you don't want (`cargo uninstall` for one cargo put there), so \
         `rite update` and the tools all mean the same one",
    )]
}

/// Folders an older version kept (`agile-md`, or 5.0.0's under the home
/// folder on Windows) that are still there. rite moves them at its next
/// start, so one that's left is a move that failed or a second copy that
/// is now ignored.
pub fn folders() -> Vec<Finding> {
    folders_of(&places::stranded())
}

fn folders_of(stranded: &[places::Stranded]) -> Vec<Finding> {
    let found: Vec<Finding> = stranded
        .iter()
        .map(|s| {
            let what = |p: &Path| p.display().to_string();
            if s.left_behind {
                let mut finding = Finding::found(
                    FOLDER_STRANDED,
                    Status::Warning,
                    format!(
                        "{} is ignored: {} is used instead",
                        what(&s.old),
                        what(&s.new)
                    ),
                )
                .because("both folders exist, so rite didn't move one over the other");
                if s.old.join("project-trust").is_file() {
                    finding = finding.because(
                        "its project-trust file is stale: trust is read from the new folder",
                    );
                }
                finding.by_hand(format!(
                    "check nothing in {} is needed, then delete it",
                    what(&s.old)
                ))
            } else {
                Finding::found(
                    FOLDER_STRANDED,
                    Status::Warning,
                    format!("{} hasn't been moved to {}", what(&s.old), what(&s.new)),
                )
                .because("rite moves it the next time it starts; this is used meanwhile")
                .by_hand("rite --version")
            }
        })
        .collect();
    or_ok(
        found,
        FOLDER_STRANDED,
        "rite's per-machine folders are where they belong",
    )
}

/// Puts right what [`check`] can, in rite's own files only, and says what
/// it changed, a line each. Run [`check`] again afterwards for what's left.
pub fn fix(machine: &Machine) -> Result<Vec<String>> {
    let state = State::read(machine);
    let mut done = Vec::new();

    // The registry: gone folders out, unregistered projects and declared
    // clones in. Only written when something changes.
    let mut registry = state.registry.clone();
    let mut changed = false;
    for root in state.raw_registry.iter().filter(|root| !root.is_dir()) {
        done.push(format!("took {} off the registry (gone)", root.display()));
        changed = true;
    }
    let mut add = |registry: &mut Registry, repo: &Path, why: &str| match registry.add(repo) {
        Ok(true) => {
            done.push(format!("registered {} ({why})", repo.display()));
            true
        }
        Ok(false) => false,
        Err(err) => {
            done.push(format!("couldn't register {}: {err:#}", repo.display()));
            false
        }
    };
    for root in unregistered_candidates(&state) {
        let name = current_project::project_name(&root);
        for repo in project_repos(&root) {
            changed |= add(&mut registry, &repo, &format!("project {name}"));
        }
    }
    for entry in state.registry.entries.iter().filter(|e| e.has_board()) {
        for name in board_settings::load(&entry.board()).repos() {
            if let Some(clone) = registry::beside(&entry.root, &name) {
                changed |= add(&mut registry, &clone, "declared, cloned beside it");
            }
        }
    }
    if changed {
        registry.save_to(&machine.repos())?;
    }

    // Hidden and folded lists: entries for folders that are gone.
    let mut settings = state.settings.clone();
    let mut settings_changed = false;
    for key in [HIDDEN, FOLDED] {
        let all = list(&settings, key);
        let (kept, gone): (Vec<PathBuf>, Vec<PathBuf>) = all.into_iter().partition(|p| p.is_dir());
        if gone.is_empty() {
            continue;
        }
        for path in &gone {
            done.push(format!("took {} off {key} (gone)", path.display()));
        }
        let joined: Vec<String> = kept.iter().map(|p| p.display().to_string()).collect();
        settings.set(key, &joined.join(","));
        settings_changed = true;
    }
    if settings_changed {
        settings.save_to(&machine.settings())?;
    }

    // Project trust: entries for boards that are gone.
    let mut trust = state.trust.clone();
    let gone: Vec<String> = trust
        .keys()
        .filter(|board| !Path::new(board).is_dir())
        .map(str::to_string)
        .collect();
    if !gone.is_empty() {
        for board in &gone {
            trust.remove(board);
        }
        trust.save_to(&machine.trust())?;
        done.push(format!(
            "took {} project-trust {} for boards that are gone",
            gone.len(),
            if gone.len() == 1 { "entry" } else { "entries" }
        ));
    }
    Ok(done)
}

/// How many findings are problems, and how many warnings.
pub fn counts(findings: &[Finding]) -> (usize, usize) {
    let problems = findings
        .iter()
        .filter(|f| f.status == Status::Problem)
        .count();
    let warnings = findings
        .iter()
        .filter(|f| f.status == Status::Warning)
        .count();
    (problems, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    struct Fixture {
        _dir: tempfile::TempDir,
        machine: Machine,
        /// Where projects live (not under `machine.temp`).
        work: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = canonical(dir.path());
        let machine = Machine {
            config: root.join("config"),
            temp: root.join("tmp"),
        };
        fs::create_dir_all(&machine.config).unwrap();
        fs::create_dir_all(&machine.temp).unwrap();
        let work = root.join("work");
        fs::create_dir_all(&work).unwrap();
        Fixture {
            _dir: dir,
            machine,
            work,
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    /// A git repository at `parent/name`, with a board when `board`.
    fn repo(parent: &Path, name: &str, board: bool) -> PathBuf {
        let root = parent.join(name);
        fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q"]);
        if board {
            fs::create_dir_all(root.join("tasks/backlog")).unwrap();
        }
        root
    }

    fn register(machine: &Machine, roots: &[&Path]) {
        let mut registry = Registry::load_from(&machine.repos());
        for root in roots {
            registry.add(root).unwrap();
        }
        registry.save_to(&machine.repos()).unwrap();
    }

    fn set(machine: &Machine, key: &str, value: &str) {
        let mut config = Config::load_from(&machine.settings());
        config.set(key, value);
        config.save_to(&machine.settings()).unwrap();
    }

    fn not_ok(findings: &[Finding]) -> Vec<(&str, Status)> {
        findings
            .iter()
            .filter(|f| f.status != Status::Ok)
            .map(|f| (f.check, f.status))
            .collect()
    }

    /// A project the way `rite init` lays it out, registered.
    fn healthy(fx: &Fixture) -> (PathBuf, PathBuf) {
        let dir = fx.work.join("acme");
        let tickets = repo(&dir, "acme-tickets", true);
        let documents = repo(&dir, "acme-documents", false);
        register(&fx.machine, &[&tickets, &documents]);
        (tickets, documents)
    }

    #[test]
    fn a_machine_with_nothing_wrong_is_all_ok() {
        let fx = fixture();
        let (tickets, _) = healthy(&fx);
        set(&fx.machine, HIDDEN, &tickets.display().to_string());
        let findings = check(&fx.machine);
        assert!(not_ok(&findings).is_empty(), "{findings:#?}");
        // One ok line a check.
        assert_eq!(findings.len(), 7);
        assert_eq!(counts(&findings), (0, 0));
        assert!(fix(&fx.machine).unwrap().is_empty());
    }

    #[test]
    fn an_empty_machine_is_all_ok() {
        let fx = fixture();
        assert!(not_ok(&check(&fx.machine)).is_empty());
    }

    #[test]
    fn a_hidden_project_that_isnt_registered_is_found_and_registered_locally() {
        // The KanRite-hosted case (2026-11-10).
        let fx = fixture();
        let dir = fx.work.join("hosted");
        let tickets = repo(&dir, "hosted-tickets", true);
        let documents = repo(&dir, "hosted-documents", false);
        let api = repo(&dir, "api", false);
        fs::write(tickets.join(".settings"), "project = hosted\nrepos = api\n").unwrap();
        git(&tickets, &["add", "."]);
        git(
            &tickets,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "start",
            ],
        );
        set(&fx.machine, HIDDEN, &tickets.display().to_string());

        let findings = check(&fx.machine);
        assert_eq!(not_ok(&findings), [(UNREGISTERED_PROJECT, Status::Problem)]);
        let found = &findings
            .iter()
            .find(|f| f.check == UNREGISTERED_PROJECT)
            .unwrap();
        assert!(found.title.contains("hosted"), "{found:?}");
        assert!(
            found
                .detail
                .iter()
                .any(|l| l.contains("rite mine --unhide hosted")),
            "{found:?}"
        );
        assert_eq!(found.fix.as_ref().unwrap().command, "rite doctor --fix");

        let settings_before = fs::read_to_string(tickets.join(".settings")).unwrap();
        let done = fix(&fx.machine).unwrap();
        assert!(
            done.iter().any(|l| l.contains("hosted-tickets")),
            "{done:?}"
        );
        let registry = Registry::load_from(&fx.machine.repos());
        for repo in [&tickets, &documents, &api] {
            assert!(
                registry.entries.iter().any(|e| &e.root == repo),
                "{} not registered",
                repo.display()
            );
        }
        // Local only: nothing in the repository changed.
        assert_eq!(
            fs::read_to_string(tickets.join(".settings")).unwrap(),
            settings_before
        );
        let count = Command::new("git")
            .arg("-C")
            .arg(&tickets)
            .args(["rev-list", "--count", "HEAD"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "1");
        // Still hidden: the user hid it.
        assert!(
            Config::load_from(&fx.machine.settings())
                .get(HIDDEN)
                .unwrap()
                .contains("hosted-tickets")
        );
        assert!(not_ok(&check(&fx.machine)).is_empty());
    }

    #[test]
    fn a_project_beside_a_registered_one_is_found() {
        let fx = fixture();
        healthy(&fx);
        let other = repo(&fx.work.join("other"), "other-tickets", true);
        let findings = check(&fx.machine);
        assert_eq!(not_ok(&findings), [(UNREGISTERED_PROJECT, Status::Problem)]);
        fix(&fx.machine).unwrap();
        assert!(
            Registry::load_from(&fx.machine.repos())
                .entries
                .iter()
                .any(|e| e.root == other)
        );
    }

    #[test]
    fn scratch_archived_and_boardless_repositories_are_not_projects() {
        let fx = fixture();
        healthy(&fx);
        repo(&fx.work.join("plain"), "code", false);
        let old = repo(&fx.work.join("old"), "old-tickets", true);
        fs::write(old.join(".settings"), "archived = on\n").unwrap();
        let scratch = repo(&fx.machine.temp, "scratch", true);
        set(&fx.machine, HIDDEN, &scratch.display().to_string());
        assert!(not_ok(&check(&fx.machine)).is_empty());
    }

    #[test]
    fn a_registered_folder_that_is_gone_is_a_warning_fix_drops() {
        let fx = fixture();
        healthy(&fx);
        let gone = fx.work.join("gone/gone-tickets");
        let mut text = fs::read_to_string(fx.machine.repos()).unwrap();
        text.push_str(&format!("{}\n", gone.display()));
        fs::write(fx.machine.repos(), text).unwrap();
        assert_eq!(
            not_ok(&check(&fx.machine)),
            [(REGISTRY_MISSING, Status::Warning)]
        );
        fix(&fx.machine).unwrap();
        assert!(
            !fs::read_to_string(fx.machine.repos())
                .unwrap()
                .contains("gone-tickets")
        );
        assert!(not_ok(&check(&fx.machine)).is_empty());
    }

    #[test]
    fn a_registered_folder_that_isnt_a_repository_is_a_problem_left_alone() {
        let fx = fixture();
        healthy(&fx);
        let hollow = fx.work.join("acme/hollow");
        fs::create_dir_all(hollow.join(".git")).unwrap();
        let mut text = fs::read_to_string(fx.machine.repos()).unwrap();
        text.push_str(&format!("{}\n", hollow.display()));
        fs::write(fx.machine.repos(), text).unwrap();
        let findings = check(&fx.machine);
        assert_eq!(not_ok(&findings), [(REGISTRY_NOT_A_REPO, Status::Problem)]);
        let found = findings
            .iter()
            .find(|f| f.check == REGISTRY_NOT_A_REPO)
            .unwrap();
        assert!(!found.fix.as_ref().unwrap().automatic);
        fix(&fx.machine).unwrap();
        assert_eq!(
            not_ok(&check(&fx.machine)),
            [(REGISTRY_NOT_A_REPO, Status::Problem)]
        );
    }

    #[test]
    fn a_declared_clone_beside_the_tickets_that_isnt_registered_is_a_warning() {
        let fx = fixture();
        let (tickets, _) = healthy(&fx);
        fs::write(tickets.join(".settings"), "repos = web\n").unwrap();
        let web = repo(&fx.work.join("acme"), "web", false);
        assert_eq!(
            not_ok(&check(&fx.machine)),
            [(DECLARED_NOT_REGISTERED, Status::Warning)]
        );
        fix(&fx.machine).unwrap();
        assert!(
            Registry::load_from(&fx.machine.repos())
                .entries
                .iter()
                .any(|e| e.root == web)
        );
        assert!(not_ok(&check(&fx.machine)).is_empty());
    }

    #[test]
    fn hidden_folded_and_trust_entries_for_gone_folders_are_warnings_fix_drops() {
        let fx = fixture();
        let (tickets, _) = healthy(&fx);
        let gone = fx.work.join("gone/gone-tickets");
        set(
            &fx.machine,
            HIDDEN,
            &format!("{},{}", gone.display(), tickets.display()),
        );
        set(&fx.machine, FOLDED, &gone.display().to_string());
        set(&fx.machine, "theme", "dark");
        let mut trust = Config::default();
        trust.set(&tickets.join("tasks").display().to_string(), "acme");
        trust.set(&gone.join("tasks").display().to_string(), "gone");
        trust.save_to(&fx.machine.trust()).unwrap();

        assert_eq!(
            not_ok(&check(&fx.machine)),
            [
                (STALE_HIDDEN, Status::Warning),
                (STALE_FOLDED, Status::Warning),
                (STALE_TRUST, Status::Warning)
            ]
        );
        let done = fix(&fx.machine).unwrap();
        assert_eq!(done.len(), 3, "{done:?}");
        let settings = Config::load_from(&fx.machine.settings());
        assert_eq!(
            settings.get(HIDDEN),
            Some(tickets.display().to_string().as_str())
        );
        assert_eq!(settings.get(FOLDED), Some(""));
        assert_eq!(settings.get("theme"), Some("dark"), "other keys kept");
        let trust = Config::load_from(&fx.machine.trust());
        assert_eq!(trust.keys().count(), 1);
        assert!(not_ok(&check(&fx.machine)).is_empty());
    }

    /// A folder with a `rite` in it, and the installer's marker when given.
    fn bin(parent: &Path, name: &str, marker: Option<&str>) -> PathBuf {
        let dir = parent.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(update_apply::file_name("rite")), "#!/bin/sh\n").unwrap();
        if let Some(marker) = marker {
            fs::write(
                dir.join(update_apply::MARKER),
                format!("#!/bin/sh\n{marker}\n"),
            )
            .unwrap();
        }
        dir
    }

    fn path_of(dirs: &[&Path]) -> std::ffi::OsString {
        std::env::join_paths(dirs).unwrap()
    }

    #[test]
    fn a_managed_install_with_one_rite_on_the_path_is_all_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = bin(tmp.path(), "bin", Some(update_apply::RELEASE_INSTALL));
        let found = install(Some(path_of(&[&dir]).as_os_str()));
        assert_eq!(not_ok(&found), []);
        assert_eq!(found.len(), 3);
    }

    #[test]
    fn no_rite_on_the_path_is_a_warning_naming_the_installer() {
        let tmp = tempfile::tempdir().unwrap();
        let empty = tmp.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        for path in [Some(path_of(&[&empty])), None] {
            let found = install(path.as_deref());
            assert_eq!(not_ok(&found), [(NO_RITE_ON_PATH, Status::Warning)]);
            assert_eq!(
                found[0].fix.as_ref().unwrap().command,
                update_apply::INSTALL_COMMAND
            );
            assert!(!found[0].fix.as_ref().unwrap().automatic);
        }
    }

    #[test]
    fn an_install_without_a_marker_cant_be_updated_by_rite() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = bin(tmp.path(), "bin", None);
        let found = install(Some(path_of(&[&dir]).as_os_str()));
        assert_eq!(not_ok(&found), [(INSTALL_UPDATABLE, Status::Warning)]);
    }

    #[test]
    fn a_source_build_is_not_a_fault() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = bin(tmp.path(), "bin", Some("# kanrite install: source"));
        let found = install(Some(path_of(&[&dir]).as_os_str()));
        assert_eq!(not_ok(&found), []);
        assert!(found[0].title.contains("source or cargo build"));
    }

    #[test]
    fn a_marker_from_before_the_rename_is_a_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = bin(tmp.path(), "bin", Some(update_apply::OLD_RELEASE_INSTALL));
        let found = install(Some(path_of(&[&dir]).as_os_str()));
        assert_eq!(not_ok(&found), [(INSTALL_OLD_MARKER, Status::Warning)]);
    }

    #[test]
    fn two_different_rites_on_the_path_are_a_warning_listing_both_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let first = bin(tmp.path(), "cargo", None);
        let second = bin(tmp.path(), "local", Some(update_apply::RELEASE_INSTALL));
        let found = install(Some(path_of(&[&first, &second, &first]).as_os_str()));
        let copies: Vec<_> = found.iter().filter(|f| f.check == INSTALL_COPIES).collect();
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].status, Status::Warning);
        assert!(
            copies[0].title.starts_with("2 different"),
            "{:?}",
            copies[0].title
        );
        assert!(copies[0].detail[0].ends_with("(the one that runs)"));
        assert!(copies[0].detail[1].contains("local"));
    }

    #[test]
    fn folders_an_older_version_left_are_warnings_and_none_is_ok() {
        assert_eq!(not_ok(&folders_of(&[])), []);
        let old = PathBuf::from("/h/.config/kanrite");
        let new = PathBuf::from("/h/AppData/Roaming/kanrite");
        let unmoved = places::Stranded {
            old: old.clone(),
            new: new.clone(),
            left_behind: false,
        };
        let found = folders_of(&[unmoved]);
        assert_eq!(not_ok(&found), [(FOLDER_STRANDED, Status::Warning)]);
        assert!(found[0].title.contains("hasn't been moved"));
        let behind = places::Stranded {
            old,
            new,
            left_behind: true,
        };
        let found = folders_of(&[behind]);
        assert!(found[0].title.contains("is ignored"));
    }

    #[test]
    fn a_stale_project_trust_file_in_a_left_behind_folder_is_named() {
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join(".config/kanrite");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("project-trust"), "x = y\n").unwrap();
        let found = folders_of(&[places::Stranded {
            old,
            new: tmp.path().join("Roaming/kanrite"),
            left_behind: true,
        }]);
        assert!(found[0].detail.iter().any(|d| d.contains("project-trust")));
    }

    #[test]
    fn findings_serialise_for_riteui_and_scripts() {
        let finding = Finding::found(STALE_TRUST, Status::Warning, "x".into()).automatic();
        let json = serde_json::to_value(&finding).unwrap();
        assert_eq!(json["status"], "warning");
        assert_eq!(json["check"], "stale-trust");
        assert_eq!(json["fix"]["automatic"], true);
    }
}
