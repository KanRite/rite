//! A project's documents (ticket 129): the `{project}-documents` repository
//! `rite init` makes beside the tickets one (ADRs, RFCs, docs), and one
//! documents repository shared by every project, named in the project's
//! `.settings` (`shared-documents`). Documents are pushed straight to main
//! with git as their history (kanrite-documents' ADR 0007): [`history`]
//! lists a document's versions, [`read_version`] reads one, and
//! [`restore`] writes it back as a new commit and pushes it (ticket 169).
//!
//! Every read is confined to its repository: a path is relative, made of
//! plain names (no `..`, no root), and must still be inside the repository
//! once symbolic links are followed.

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::board::Board;
use crate::board_settings;
use crate::current_project;
use crate::git;
use crate::registry::{self, Registry};

/// The largest file rite reads for the reader: documents and the images
/// they show. Anything bigger is refused rather than loaded.
pub const MAX_FILE: u64 = 8 * 1024 * 1024;

/// Which of a project's documents repositories.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The project's own `{project}-documents`.
    Own,
    /// The one every project shares (`shared-documents`).
    Shared,
}

/// A documents repository, found on this machine or not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub kind: Kind,
    /// The repository's name.
    pub name: String,
    /// Its clone here; `None` when it isn't cloned on this machine.
    pub root: Option<PathBuf>,
}

/// One document in a repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    /// Relative to the repository, with `/` between parts.
    pub path: String,
    /// The first `# ` heading, else the file name without `.md`.
    pub title: String,
    /// Marked `status: archived` (ticket 176).
    pub archived: bool,
}

/// The documents repositories of the project whose board is `board`: its
/// own first, then the shared one when the project names one (and it isn't
/// the project's own).
pub fn sources(board: &Board) -> Vec<Source> {
    let root = board.repo_root();
    let project = current_project::project_name(root);
    let dir = registry::project_dir_of(root);
    let registry = Registry::load();
    let own = format!("{project}-documents");
    let mut out = vec![Source {
        kind: Kind::Own,
        root: locate(&registry, &own, &dir),
        name: own.clone(),
    }];
    if let Some(shared) = board_settings::load(board).shared_documents()
        && shared != own
    {
        out.push(Source {
            kind: Kind::Shared,
            root: locate(&registry, &shared, &dir),
            name: shared,
        });
    }
    out
}

/// Where `name` is cloned for the project in `dir`: beside the project's
/// other repositories, else registered on this machine (`find_near`).
fn locate(registry: &Registry, name: &str, dir: &Path) -> Option<PathBuf> {
    let sibling = dir.join(name);
    if crate::git::is_repo_dir(&sibling) {
        return Some(sibling);
    }
    registry
        .find_near(name, dir)
        .map(|entry| entry.root.clone())
        .filter(|root| root.is_dir())
}

/// Every markdown document in `root`, sorted by path. Hidden files and
/// folders (`.git`, `.github`, `.settings`) are left out.
pub fn list(root: &Path) -> Result<Vec<Document>> {
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<Document>) -> Result<()> {
    let entries = fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        // Not followed: a link could lead out of the repository, or round
        // in a circle.
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            walk(root, &path, out)?;
        } else if kind.is_file() && is_markdown(&name) {
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let relative = slashed(relative);
            let text = crate::eol::read(&path).unwrap_or_default();
            out.push(Document {
                title: title_in(&text).unwrap_or_else(|| stem(&name)),
                archived: crate::documents_edit::is_archived(&text),
                path: relative,
            });
        }
    }
    Ok(())
}

fn is_markdown(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".md") || lower.ends_with(".markdown")
}

fn stem(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(name)
        .to_string()
}

fn slashed(path: &Path) -> String {
    path.components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// A document's title: its first `# ` heading, skipping frontmatter.
pub fn title_of(text: &str) -> Option<String> {
    title_in(text)
}

/// A document's title: its first `# ` heading, skipping frontmatter.
fn title_in(text: &str) -> Option<String> {
    let mut lines = text.lines();
    let mut in_frontmatter = false;
    let mut first = true;
    for line in lines.by_ref() {
        let trimmed = line.trim();
        if first && trimmed == "---" {
            in_frontmatter = true;
            first = false;
            continue;
        }
        first = false;
        if in_frontmatter {
            if trimmed == "---" {
                in_frontmatter = false;
            }
            continue;
        }
        if let Some(title) = trimmed.strip_prefix("# ") {
            let title = title.trim().trim_end_matches('#').trim();
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
    }
    None
}

/// `relative` inside `root`, refused when it could reach outside it: an
/// absolute path, a `..`, or a symbolic link that leads elsewhere.
pub fn resolve(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = Path::new(relative);
    if relative.is_empty()
        || !path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        bail!("{relative:?} isn't a path inside the documents repository");
    }
    let root = dunce::canonicalize(root).with_context(|| format!("reading {}", root.display()))?;
    let full = dunce::canonicalize(root.join(path))
        .with_context(|| format!("{relative} isn't in {}", root.display()))?;
    if !full.starts_with(&root) {
        bail!("{relative} leads outside the documents repository");
    }
    if full
        .strip_prefix(&root)
        .is_ok_and(|inside| inside.components().any(|part| part.as_os_str() == ".git"))
    {
        bail!("{relative} is inside .git");
    }
    Ok(full)
}

/// The bytes of `relative` in `root` (a document or an image it shows),
/// confined as [`resolve`] says and no bigger than [`MAX_FILE`].
pub fn read_bytes(root: &Path, relative: &str) -> Result<Vec<u8>> {
    let full = resolve(root, relative)?;
    let size = fs::metadata(&full)
        .with_context(|| format!("reading {relative}"))?
        .len();
    if size > MAX_FILE {
        bail!("{relative} is too big to show ({size} bytes)");
    }
    fs::read(&full).with_context(|| format!("reading {relative}"))
}

/// The text of the document `relative` in `root`.
pub fn read(root: &Path, relative: &str) -> Result<String> {
    if !is_markdown(relative) {
        bail!("{relative} isn't a markdown document");
    }
    let bytes = read_bytes(root, relative)?;
    String::from_utf8(bytes)
        .map(crate::eol::lf)
        .with_context(|| format!("{relative} isn't UTF-8 text"))
}

/// Where a link in the document `from` points, inside the repository:
/// `../adr/0001.md` from `rfc/0002.md` is `adr/0001.md`. `None` for a link
/// that leaves the repository, or isn't a relative path at all (a web
/// address, `mailto:`, `#a-heading`).
pub fn join_link(from: &str, link: &str) -> Option<String> {
    let link = link.split(['#', '?']).next().unwrap_or_default();
    if link.is_empty() || link.contains(':') || link.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = from.split('/').collect();
    parts.pop();
    for part in link.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// One version of a document, from git (ticket 169).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub commit: String,
    pub short: String,
    pub author: String,
    pub email: String,
    /// `YYYY-MM-DD HH:MM`, the commit's date.
    pub date: String,
    pub subject: String,
    /// The document's path at that commit: it may since have been renamed.
    pub path: String,
}

/// What [`restore`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Restored {
    /// The commit message.
    pub message: String,
    /// Nothing left to push: pushed, or no remote to push to (as a board
    /// with no remote is never said to be behind, ticket 142).
    pub pushed: bool,
    /// Why it wasn't pushed, when it should have been.
    pub warning: Option<String>,
}

/// The document `relative`'s versions, newest first, at most `limit`:
/// `git log --follow`, so its history carries on across a rename (such as
/// `adr/` becoming `adr/adr/`).
pub fn history(root: &Path, relative: &str, limit: usize) -> Result<Vec<Version>> {
    resolve(root, relative)?;
    // Capped at git's own largest count: `usize::MAX` means every version.
    let limit = format!("-n{}", limit.clamp(1, i32::MAX as usize));
    let out = git::checked(
        root,
        &[
            "log",
            "--follow",
            &limit,
            "--date=format:%Y-%m-%d %H:%M",
            "--format=%x1e%H%x1f%h%x1f%an%x1f%ae%x1f%ad%x1f%s",
            "--name-only",
            "--",
            relative,
        ],
    )?;
    Ok(parse_history(&out))
}

/// Reads `git log`'s output as [`history`] asks for it: each commit starts
/// with a record separator, its fields split by unit separators, then the
/// name of the file at that commit.
fn parse_history(out: &str) -> Vec<Version> {
    out.split('\u{1e}')
        .filter_map(|record| {
            let mut lines = record.lines().filter(|l| !l.trim().is_empty());
            let fields: Vec<&str> = lines.next()?.split('\u{1f}').collect();
            let path = lines.next()?.trim().to_string();
            let [commit, short, author, email, date, subject] = fields[..] else {
                return None;
            };
            Some(Version {
                commit: commit.to_string(),
                short: short.to_string(),
                author: author.to_string(),
                email: email.to_string(),
                date: date.to_string(),
                subject: subject.to_string(),
                path,
            })
        })
        .collect()
}

/// The text of `path` as it was at `commit`.
pub fn read_version(root: &Path, path: &str, commit: &str) -> Result<String> {
    if commit.len() < 4 || !commit.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("{commit:?} isn't a commit");
    }
    if !is_markdown(path)
        || !Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        bail!("{path:?} isn't a document in the repository");
    }
    let size = git::checked(root, &["cat-file", "-s", &format!("{commit}:{path}")])?;
    if size.trim().parse::<u64>().unwrap_or(u64::MAX) > MAX_FILE {
        bail!("{path} at {commit} is too big to show");
    }
    // As stored, CRLF read as LF like any other read (`eol`): `git::checked`
    // trims, which would lose trailing newlines and make an unchanged
    // document look changed.
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["show", &format!("{commit}:{path}")])
        .stdin(std::process::Stdio::null())
        .output()
        .context("running git show")?;
    if !out.status.success() {
        bail!(
            "reading {path} at {commit}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    String::from_utf8(out.stdout)
        .map(crate::eol::lf)
        .with_context(|| format!("{path} at {commit} isn't UTF-8 text"))
}

/// Puts the document `relative` back as it was at `commit`: its text then,
/// written to its path now, committed as a new commit and pushed (so a
/// restore can be undone the same way). Refused when the document has
/// uncommitted changes, when the clone has diverged from origin, and when
/// there's nothing to restore. A push that fails after the commit keeps the
/// commit and says why, as auto-commit does.
///
/// A small sync of its own rather than `sync::catch_up`, which is about
/// boards: tickets, duplicate ids, board settings.
pub fn restore(root: &Path, relative: &str, commit: &str) -> Result<Restored> {
    let full = resolve(root, relative)?;
    let dirty = git::checked(root, &["status", "--porcelain", "--", relative])?;
    if !dirty.trim().is_empty() {
        bail!("{relative} has changes that aren't committed; commit or discard them first");
    }
    let version = history(root, relative, usize::MAX)?
        .into_iter()
        .find(|v| v.commit.starts_with(commit) || commit.starts_with(&v.short))
        .with_context(|| format!("{commit} isn't a version of {relative}"))?;
    let old = read_version(root, &version.path, &version.commit)?;
    let now = crate::eol::read(&full).unwrap_or_default();
    if old == now {
        bail!("{relative} is already as it was at {}", version.short);
    }
    let message = format!(
        "restore: {relative} to {} ({}, {})",
        version.short, version.date, version.author
    );
    commit_change(root, &message, || {
        fs::write(&full, &old).with_context(|| format!("writing {relative}"))?;
        Ok(vec![PathBuf::from(relative)])
    })
}

/// The one way a change is written to a documents repository (tickets 169,
/// 176; ADR 0007): catch up with origin, let `write` make the change and
/// name the paths it touched, commit just those, and push (replaying once
/// if origin moved). Offline, the change is committed locally with a
/// warning; a push that still fails keeps the commit and says why.
pub fn commit_change(
    root: &Path,
    message: &str,
    write: impl FnOnce() -> Result<Vec<PathBuf>>,
) -> Result<Restored> {
    let remote = git::remote_url(root).is_some();
    let offline = catch_up(root)?;
    let paths: std::collections::BTreeSet<PathBuf> = write()?.into_iter().collect();
    git::commit_paths_locally(root, message, &paths)?;
    let message = message.to_string();
    if !remote {
        return Ok(Restored {
            message,
            pushed: true,
            warning: None,
        });
    }
    if let Some(w) = offline {
        return Ok(Restored {
            message,
            pushed: false,
            warning: Some(format!("{w}; committed locally")),
        });
    }
    Ok(match push_or_replay(root) {
        Ok(()) => Restored {
            message,
            pushed: true,
            warning: None,
        },
        Err(e) => Restored {
            message,
            pushed: false,
            warning: Some(format!("committed, but couldn't push: {e:#}")),
        },
    })
}

/// [`commit_change`] for a change that can simply be made again (ticket 131:
/// a vote, a note). `change` writes it and returns the commit message and
/// the paths. If the push is rejected because origin moved, that commit is
/// dropped, the clone fast-forwarded, and `change` run again on the new
/// files, up to three times: two people voting on one idea edit the same
/// line, which a rebase replay would only turn into a conflict.
pub fn commit_reapplied(
    root: &Path,
    mut change: impl FnMut() -> Result<(String, Vec<PathBuf>)>,
) -> Result<Restored> {
    let remote = git::remote_url(root).is_some();
    let offline = catch_up(root)?;
    let mut tries = 0;
    loop {
        let base = git::checked(root, &["rev-parse", "HEAD"]).ok();
        let (message, paths) = change()?;
        let paths: std::collections::BTreeSet<PathBuf> = paths.into_iter().collect();
        git::commit_paths_locally(root, &message, &paths)?;
        if !remote {
            return Ok(Restored {
                message,
                pushed: true,
                warning: None,
            });
        }
        if let Some(w) = &offline {
            return Ok(Restored {
                message,
                pushed: false,
                warning: Some(format!("{w}; committed locally")),
            });
        }
        let Err(e) = git::push(root) else {
            return Ok(Restored {
                message,
                pushed: true,
                warning: None,
            });
        };
        tries += 1;
        let Some(base) = base.filter(|_| tries < 3 && git::fetch(root).is_ok()) else {
            return Ok(Restored {
                message,
                pushed: false,
                warning: Some(format!("committed, but couldn't push: {e:#}")),
            });
        };
        // Only this commit is dropped, never earlier unpushed work.
        git::reset_keep(root, base.trim())?;
        git::merge_ff_only(root).context(
            "origin moved and this clone has commits of its own that aren't pushed; pull and sort that out first",
        )?;
    }
}

/// Refuses when any of `paths` (relative to `root`) has changes that aren't
/// committed, so a write never sweeps someone's edits into its commit.
pub fn ensure_clean(root: &Path, paths: &[&str]) -> Result<()> {
    let mut args = vec!["status", "--porcelain", "--untracked-files=no", "--"];
    args.extend(paths);
    let dirty = git::checked(root, &args)?;
    if !dirty.trim().is_empty() {
        bail!(
            "{} has changes that aren't committed; commit or discard them first",
            paths.join(", ")
        );
    }
    Ok(())
}

/// Brings a documents repository in step with origin before a change is
/// written to it (tickets 169, 170): fetch, then fast-forward when only
/// behind; refused when it has diverged. `Ok(Some(warning))` when origin
/// can't be reached (the change can still be made locally); nothing to do
/// without a remote. The documents side of `sync::catch_up`, which is about
/// boards.
pub fn catch_up(root: &Path) -> Result<Option<String>> {
    if git::remote_url(root).is_none() {
        return Ok(None);
    }
    if git::fetch(root).is_err() {
        return Ok(Some("couldn't reach origin".to_string()));
    }
    match git::ahead_behind(root) {
        Some((0, behind)) if behind > 0 => git::merge_ff_only(root)?,
        Some((ahead, behind)) if ahead > 0 && behind > 0 => bail!(
            "{} has diverged from origin; pull and sort that out first",
            root.display()
        ),
        _ => {}
    }
    Ok(None)
}

/// Pushes a documents repository's new commit; if origin moved since the
/// fetch, replays it on top once (`rebase @{u}`) and pushes again. A
/// conflict is backed out and the first error returned, the commit kept.
pub fn push_or_replay(root: &Path) -> Result<()> {
    git::push(root).or_else(|first| {
        if git::fetch(root).is_ok() && git::rebase_upstream(root).unwrap_or(false) {
            git::push(root)
        } else {
            let _ = git::rebase_abort(root);
            Err(first)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_skip_frontmatter() {
        assert_eq!(
            title_in("---\ntitle: x\n---\n\n# Real title #\n").as_deref(),
            Some("Real title")
        );
        assert_eq!(title_in("no heading\n## Sub\n"), None);
    }

    #[test]
    fn a_crlf_document_reads_as_lf_and_keeps_its_title() {
        // Ticket 277: frontmatter and headings found as in any other file.
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("note.md"),
            "---\r\nstatus: archived\r\n---\r\n\r\n# Windows note\r\n\r\nText.\r\n",
        )
        .unwrap();
        let text = read(dir.path(), "note.md").unwrap();
        assert_eq!(
            text,
            "---\nstatus: archived\n---\n\n# Windows note\n\nText.\n"
        );
        assert_eq!(title_in(&text).as_deref(), Some("Windows note"));
    }

    #[test]
    fn links_resolve_inside_the_repository() {
        assert_eq!(
            join_link("rfc/0002.md", "../adr/0001.md#why").as_deref(),
            Some("adr/0001.md")
        );
        assert_eq!(
            join_link("README.md", "./docs/a.md").as_deref(),
            Some("docs/a.md")
        );
        assert_eq!(join_link("README.md", "../elsewhere.md"), None);
        assert_eq!(join_link("README.md", "https://example.com/a.md"), None);
        assert_eq!(join_link("README.md", "#top"), None);
        assert_eq!(join_link("README.md", "/etc/passwd"), None);
    }

    #[test]
    fn lists_markdown_and_skips_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("adr")).unwrap();
        fs::create_dir_all(root.join(".github")).unwrap();
        fs::write(root.join("README.md"), "# Read me\n").unwrap();
        fs::write(root.join("adr/0001-x.md"), "text\n").unwrap();
        fs::write(root.join("adr/notes.txt"), "not markdown\n").unwrap();
        fs::write(root.join(".github/x.md"), "# hidden\n").unwrap();
        let docs = list(root).unwrap();
        let paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, ["README.md", "adr/0001-x.md"]);
        assert_eq!(docs[0].title, "Read me");
        assert_eq!(docs[1].title, "0001-x");
    }

    #[test]
    fn reads_stay_inside() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("docs");
        crate::git::fake_repo(&root);
        fs::write(root.join("a.md"), "# A\n").unwrap();
        fs::write(root.join(".git/config"), "secret\n").unwrap();
        fs::write(outer.path().join("secret.md"), "no\n").unwrap();
        assert_eq!(read(&root, "a.md").unwrap(), "# A\n");
        assert!(read(&root, "../secret.md").is_err());
        assert!(read(&root, "/etc/passwd").is_err());
        assert!(read_bytes(&root, ".git/config").is_err());
        assert!(read(&root, "missing.md").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outer.path().join("secret.md"), root.join("link.md"))
                .unwrap();
            assert!(read(&root, "link.md").is_err());
        }
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("git")
            .success();
        assert!(ok, "git {args:?}");
    }

    fn identify(dir: &Path) {
        git_in(dir, &["config", "user.email", "t@t.co"]);
        git_in(dir, &["config", "user.name", "Tim"]);
        git_in(dir, &["config", "commit.gpgsign", "false"]);
    }

    /// A documents repository whose `adr/0001-x.md` went through three
    /// versions, the last after a move to `adr/adr/`.
    fn documents() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("p-documents");
        fs::create_dir_all(root.join("adr")).unwrap();
        git_in(&root, &["init", "-q", "-b", "main"]);
        identify(&root);
        fs::write(root.join("adr/0001-x.md"), "# X\n\nfirst\n").unwrap();
        git_in(&root, &["add", "-A"]);
        git_in(&root, &["commit", "-qm", "one"]);
        fs::write(root.join("adr/0001-x.md"), "# X\n\nsecond\n").unwrap();
        git_in(&root, &["commit", "-qam", "two"]);
        fs::create_dir_all(root.join("adr/adr")).unwrap();
        git_in(&root, &["mv", "adr/0001-x.md", "adr/adr/0001-x.md"]);
        git_in(&root, &["commit", "-qm", "move"]);
        fs::write(root.join("adr/adr/0001-x.md"), "# X\n\nthird\n").unwrap();
        git_in(&root, &["commit", "-qam", "three"]);
        (dir, root)
    }

    #[test]
    fn history_is_newest_first_and_follows_a_move() {
        let (_dir, root) = documents();
        let versions = history(&root, "adr/adr/0001-x.md", 20).unwrap();
        let subjects: Vec<&str> = versions.iter().map(|v| v.subject.as_str()).collect();
        assert_eq!(subjects, ["three", "move", "two", "one"]);
        assert_eq!(versions[0].path, "adr/adr/0001-x.md");
        assert_eq!(versions[3].path, "adr/0001-x.md", "its name then");
        assert_eq!(versions[0].author, "Tim");
        assert_eq!(history(&root, "adr/adr/0001-x.md", 2).unwrap().len(), 2);
    }

    #[test]
    fn an_old_version_reads_back_exactly() {
        let (_dir, root) = documents();
        let one = &history(&root, "adr/adr/0001-x.md", 20).unwrap()[3];
        assert_eq!(
            read_version(&root, &one.path, &one.commit).unwrap(),
            "# X\n\nfirst\n"
        );
        assert!(read_version(&root, &one.path, "HEAD; rm -rf /").is_err());
        assert!(read_version(&root, "../x.md", &one.commit).is_err());
    }

    #[test]
    fn restore_writes_the_old_text_as_a_new_commit() {
        let (_dir, root) = documents();
        let one = history(&root, "adr/adr/0001-x.md", 20).unwrap()[3].clone();
        let done = restore(&root, "adr/adr/0001-x.md", &one.short).unwrap();
        assert!(
            done.message.starts_with("restore: adr/adr/0001-x.md to "),
            "{}",
            done.message
        );
        assert!(done.pushed, "no remote: nothing left to push");
        assert_eq!(done.warning, None);
        assert_eq!(
            crate::eol::read(root.join("adr/adr/0001-x.md")).unwrap(),
            "# X\n\nfirst\n"
        );
        let after = history(&root, "adr/adr/0001-x.md", 20).unwrap();
        assert_eq!(after.len(), 5);
        assert!(after[0].subject.starts_with("restore:"));
        // Nothing to restore now, and local edits are never overwritten.
        assert!(restore(&root, "adr/adr/0001-x.md", &one.short).is_err());
        fs::write(root.join("adr/adr/0001-x.md"), "edited\n").unwrap();
        let err = restore(&root, "adr/adr/0001-x.md", &after[1].short).unwrap_err();
        assert!(err.to_string().contains("aren't committed"), "{err}");
    }

    #[test]
    fn restore_pushes_and_replays_onto_a_moved_origin() {
        let (dir, root) = documents();
        let origin = dir.path().join("origin.git");
        git_in(
            dir.path(),
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                origin.to_str().unwrap(),
            ],
        );
        git_in(
            &root,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git_in(&root, &["push", "-q", "-u", "origin", "main"]);
        // Someone else changes another document after this clone fetched.
        let other = dir.path().join("other");
        git_in(
            dir.path(),
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
        identify(&other);
        fs::write(other.join("README.md"), "# P\n").unwrap();
        git_in(&other, &["add", "-A"]);
        git_in(&other, &["commit", "-qm", "readme"]);
        git_in(&other, &["push", "-q"]);

        let two = history(&root, "adr/adr/0001-x.md", 20).unwrap()[2].clone();
        let done = restore(&root, "adr/adr/0001-x.md", &two.commit).unwrap();
        assert!(done.pushed, "{:?}", done.warning);
        git_in(&other, &["pull", "-q"]);
        assert_eq!(
            crate::eol::read(other.join("adr/adr/0001-x.md")).unwrap(),
            "# X\n\nsecond\n"
        );
        assert!(root.join("README.md").exists(), "caught up first");
    }
}
