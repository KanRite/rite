//! Changing documents (ticket 176): editing one, making a new one, marking
//! one archived, moving one, and making or removing folders. Every change
//! goes through [`documents::commit_change`](crate::documents::commit_change):
//! committed on its own and pushed straight to main (ADR 0007), so History
//! and git can undo it.
//!
//! Paths are relative to the repository, `/` between parts, and are checked
//! with [`documents::resolve`](crate::documents::resolve) (or, for one that
//! doesn't exist yet, [`plain`]) so nothing outside the repository, or in
//! `.git`, can be named.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::documents::{Restored, commit_change, ensure_clean, join_link, resolve};
use crate::git;

/// The frontmatter key and value that mark a document archived.
const STATUS: &str = "status";
const ARCHIVED: &str = "archived";

/// A fingerprint of a document's text, as 16 hex digits (a page can't hold a
/// `u64`): handed out when it's opened for editing and back on save, which
/// is refused if the file changed in between.
pub fn version(text: &str) -> String {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// `(frontmatter with its fences and trailing newline, body)`, or `None` for
/// the frontmatter when the document has none.
pub fn split_frontmatter(text: &str) -> (Option<&str>, &str) {
    let Some(rest) = text.strip_prefix("---\n") else {
        return (None, text);
    };
    let mut offset = 4;
    for line in rest.split_inclusive('\n') {
        offset += line.len();
        if line.trim_end() == "---" {
            return (Some(&text[..offset]), &text[offset..]);
        }
    }
    (None, text)
}

/// Whether the document is marked `status: archived`.
pub fn is_archived(text: &str) -> bool {
    let (Some(front), _) = split_frontmatter(text) else {
        return false;
    };
    front.lines().any(|line| {
        line.split_once(':').is_some_and(|(key, value)| {
            key.trim() == STATUS
                && value
                    .trim()
                    .trim_matches(['"', '\''])
                    .eq_ignore_ascii_case(ARCHIVED)
        })
    })
}

/// `text` marked archived, or not: the `status` line added, or taken away
/// (and an emptied frontmatter block with it).
pub fn with_archived(text: &str, on: bool) -> String {
    let (front, body) = split_frontmatter(text);
    let mut lines: Vec<String> = front
        .map(|f| {
            f.lines()
                .filter(|l| l.trim_end() != "---")
                .filter(|l| !l.split_once(':').is_some_and(|(k, _)| k.trim() == STATUS))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if on {
        lines.push(format!("{STATUS}: {ARCHIVED}"));
    }
    if lines.is_empty() {
        return body.to_string();
    }
    format!("---\n{}\n---\n{body}", lines.join("\n"))
}

/// Marks a document archived, or not.
pub fn set_archived(root: &Path, path: &str, on: bool) -> Result<Restored> {
    let full = resolve(root, path)?;
    ensure_clean(root, &[path])?;
    let text = crate::eol::read(&full).with_context(|| format!("reading {path}"))?;
    if is_archived(&text) == on {
        bail!("{path} is {}archived already", if on { "" } else { "not " });
    }
    let verb = if on { "archive" } else { "unarchive" };
    commit_change(root, &format!("{verb}: {path}"), || {
        fs::write(&full, with_archived(&text, on)).with_context(|| format!("writing {path}"))?;
        Ok(vec![PathBuf::from(path)])
    })
}

/// Saves a document's new text, unless it changed since it was opened
/// (`opened` is [`version`] of the text then).
pub fn save(root: &Path, path: &str, text: &str, opened: &str) -> Result<Restored> {
    let full = resolve(root, path)?;
    ensure_clean(root, &[path])?;
    let now = crate::eol::read(&full).with_context(|| format!("reading {path}"))?;
    if version(&now) != opened {
        bail!("{path} changed since you opened it; reload it and make your change again");
    }
    if now == text {
        bail!("nothing to save: {path} is unchanged");
    }
    commit_change(root, &format!("edit: {path}"), || {
        fs::write(&full, text).with_context(|| format!("writing {path}"))?;
        Ok(vec![PathBuf::from(path)])
    })
}

/// `relative` as a path inside the repository when it's made only of plain,
/// non-hidden names: for a folder or file that may not exist yet. Empty is
/// the repository root.
pub fn plain(relative: &str) -> Result<String> {
    let relative = relative.trim().trim_matches('/');
    if relative.is_empty() {
        return Ok(String::new());
    }
    let ok = Path::new(relative).components().all(
        |part| matches!(part, Component::Normal(name) if !name.to_string_lossy().starts_with('.')),
    );
    if !ok {
        bail!("{relative:?} isn't a plain path inside the documents repository");
    }
    Ok(relative.replace('\\', "/"))
}

fn join(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        name.to_string()
    } else {
        format!("{folder}/{name}")
    }
}

/// Every folder in the repository that isn't hidden, empty ones included,
/// sorted; `/` between parts.
pub fn folders(root: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let path = entry.path();
            if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
            walk(root, &path, out);
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// Makes the folder `name` inside `parent` (empty for the root), kept with
/// a `.gitkeep` since git doesn't keep empty folders.
pub fn create_folder(root: &Path, parent: &str, name: &str) -> Result<Restored> {
    let parent = plain(parent)?;
    let name = name.trim();
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        bail!("a folder name is one name, like superseded");
    }
    let folder = plain(&join(&parent, name))?;
    let full = root.join(&folder);
    if full.exists() {
        bail!("{folder} is already there");
    }
    let keep = format!("{folder}/.gitkeep");
    commit_change(root, &format!("folder: {folder}"), || {
        fs::create_dir_all(&full).with_context(|| format!("making {folder}"))?;
        fs::write(full.join(".gitkeep"), "").with_context(|| format!("making {keep}"))?;
        Ok(vec![PathBuf::from(&keep)])
    })
}

/// Removes an empty folder: nothing in it but `.gitkeep` files and empty
/// folders. Never the repository itself.
pub fn delete_folder(root: &Path, folder: &str) -> Result<Restored> {
    let folder = plain(folder)?;
    if folder.is_empty() {
        bail!("that's the repository itself");
    }
    let full = resolve(root, &folder)?;
    if !full.is_dir() {
        bail!("{folder} isn't a folder");
    }
    let mut keeps = Vec::new();
    if !only_keeps(&full, &mut keeps) {
        bail!("{folder} isn't empty: move or delete what's in it first");
    }
    let tracked: Vec<PathBuf> = keeps
        .iter()
        .filter_map(|k| {
            k.strip_prefix(dunce::canonicalize(root).ok()?)
                .ok()
                .map(Path::to_path_buf)
        })
        .filter(|k| git::is_tracked(root, &root.join(k)))
        .collect();
    commit_change(root, &format!("folder: remove {folder}"), || {
        for keep in &tracked {
            git::checked(root, &["rm", "-q", "--", &keep.to_string_lossy()])?;
        }
        // `git rm` takes the folder away with its last file; anything left
        // (empty sub-folders, an untracked .gitkeep) goes here.
        if full.exists() {
            fs::remove_dir_all(&full).with_context(|| format!("removing {folder}"))?;
        }
        Ok(tracked.clone())
    })
}

/// Whether `dir` holds only `.gitkeep` files and folders that do too;
/// collects the `.gitkeep`s.
fn only_keeps(dir: &Path, keeps: &mut Vec<PathBuf>) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => {
                if !only_keeps(&path, keeps) {
                    return false;
                }
            }
            Ok(_) if entry.file_name() == ".gitkeep" => keeps.push(path),
            _ => return false,
        }
    }
    true
}

/// `to` (a path in the repository) as a link from a document in `from_dir`.
pub fn relative_link(from_dir: &str, to: &str) -> String {
    let from: Vec<&str> = from_dir.split('/').filter(|p| !p.is_empty()).collect();
    let to_parts: Vec<&str> = to.split('/').filter(|p| !p.is_empty()).collect();
    let common = from
        .iter()
        .zip(&to_parts)
        .take_while(|(a, b)| a == b)
        .count();
    let mut out: Vec<&str> = vec![".."; from.len() - common];
    out.extend(&to_parts[common..]);
    out.join("/")
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// `text` with each link target `f` maps rewritten, in `[text](target)` and
/// `[label]: target` form. `f` gets the target without any `#anchor`, and
/// the anchor is kept.
pub fn rewrite_links(text: &str, f: impl Fn(&str) -> Option<String>) -> String {
    let rewrite = |target: &str| -> Option<String> {
        let (bare, anchor) = match target.find('#') {
            Some(at) => target.split_at(at),
            None => (target, ""),
        };
        if bare.is_empty() {
            return None;
        }
        let decoded = bare.replace("%20", " ");
        f(&decoded).map(|new| {
            let new = if bare.contains("%20") {
                new.replace(' ', "%20")
            } else {
                new
            };
            format!("{new}{anchor}")
        })
    };
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        // A reference definition: `[label]: target "title"`.
        if let Some(after) = trimmed.strip_prefix('[')
            && let Some((label, rest)) = after.split_once("]:")
            && !label.is_empty()
            && let Some(target) = rest.split_whitespace().next()
        {
            let bare = target.trim_start_matches('<').trim_end_matches('>');
            if let Some(new) = rewrite(bare) {
                out.push_str(&line.replacen(bare, &new, 1));
                continue;
            }
        }
        // Inline links and images: `](target)`.
        let mut rest = line;
        while let Some(at) = rest.find("](") {
            out.push_str(&rest[..at + 2]);
            rest = &rest[at + 2..];
            let end = rest
                .find(|c: char| c == ')' || c.is_whitespace())
                .unwrap_or(rest.len());
            let raw = &rest[..end];
            let bare = raw.trim_start_matches('<').trim_end_matches('>');
            match rewrite(bare) {
                Some(new) => out.push_str(&raw.replacen(bare, &new, 1)),
                None => out.push_str(raw),
            }
            rest = &rest[end..];
        }
        out.push_str(rest);
    }
    out
}

/// Moves the document `from` into `to_folder` (empty for the root): `git
/// mv`, so its history follows, with links rewritten so they keep working
/// — other documents' links to it, and its own relative links, images
/// included. The rename is a commit of its own, so git always sees it as
/// one (rewriting links in the same commit can push a short document below
/// git's rename threshold, and History would lose it); the link changes
/// follow in a second. Both are pushed together.
pub fn move_document(root: &Path, from: &str, to_folder: &str) -> Result<(String, Restored)> {
    resolve(root, from)?;
    let to_folder = plain(to_folder)?;
    if !to_folder.is_empty() && !root.join(&to_folder).is_dir() {
        bail!("{to_folder} isn't a folder");
    }
    let name = from.rsplit('/').next().unwrap_or(from);
    let to = join(&to_folder, name);
    if to == from {
        bail!(
            "{from} is already in {}",
            if to_folder.is_empty() {
                "the top folder"
            } else {
                &to_folder
            }
        );
    }
    if root.join(&to).exists() {
        bail!("{to} is already there");
    }
    // Every document whose links will change, the moved one included.
    let documents: Vec<String> = crate::documents::list(root)?
        .into_iter()
        .map(|d| d.path)
        .collect();
    let mut changes: Vec<(String, String)> = Vec::new();
    for doc in &documents {
        let text = crate::eol::read(root.join(doc)).unwrap_or_default();
        let new = if doc == from {
            // Its own links: resolve from where it was, point from where
            // it goes.
            rewrite_links(&text, |target| {
                let resolved = join_link(from, target)?;
                if resolved == from {
                    return None;
                }
                Some(relative_link(&to_folder, &resolved))
            })
        } else {
            rewrite_links(&text, |target| {
                (join_link(doc, target)? == from).then(|| relative_link(dir_of(doc), &to))
            })
        };
        if new != text {
            changes.push((doc.clone(), new));
        }
    }
    let mut touched: Vec<&str> = changes.iter().map(|(d, _)| d.as_str()).collect();
    touched.push(from);
    // The start page goes with it (ticket 179).
    let repin = pinned(root).as_deref() == Some(from);
    if repin {
        touched.push(SETTINGS);
    }
    // Everyone's own pins of it go with it too (ticket 232).
    let source = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let pins = crate::doc_pins::moved(root, &source, from, &to);
    touched.extend(pins.iter().map(|(f, _)| f.as_str()));
    ensure_clean(root, &touched)?;
    let links = format!("move: links to {to}");
    let done = commit_change(root, &links, || {
        git::checked(root, &["mv", "--", from, &to])?;
        let renamed = std::collections::BTreeSet::from([PathBuf::from(from), PathBuf::from(&to)]);
        git::commit_paths_locally(root, &format!("move: {from} → {to}"), &renamed)?;
        let mut paths = Vec::new();
        for (doc, text) in &changes {
            let at = if doc == from {
                to.as_str()
            } else {
                doc.as_str()
            };
            fs::write(root.join(at), text).with_context(|| format!("writing {at}"))?;
            paths.push(PathBuf::from(at));
        }
        if repin {
            set_pin(root, Some(&to))?;
            paths.push(PathBuf::from(SETTINGS));
        }
        for (file, text) in &pins {
            fs::write(root.join(file), text).with_context(|| format!("writing {file}"))?;
            paths.push(PathBuf::from(file));
        }
        Ok(paths)
    })?;
    let done = Restored {
        message: format!("move: {from} → {to}"),
        ..done
    };
    Ok((to, done))
}

/// The documents repository's own settings file (ticket 179), committed in
/// it like any document: `start-page = docs/index.md`.
pub const SETTINGS: &str = ".agile-md";
const START_PAGE: &str = "start-page";

/// The document pinned as the start page, if one is and it's still there.
pub fn pinned(root: &Path) -> Option<String> {
    let config = crate::settings::Config::load_from(&root.join(SETTINGS));
    let path = config.get(START_PAGE)?.trim().to_string();
    resolve(root, &path)
        .ok()
        .filter(|p| p.is_file())
        .map(|_| path)
}

/// The page Documents opens on (ticket 179): the pinned one, else the
/// README at the top of the repository.
pub fn start_page(root: &Path) -> Option<String> {
    pinned(root).or_else(|| {
        ["README.md", "readme.md", "Readme.md"]
            .into_iter()
            .find(|name| root.join(name).is_file())
            .map(str::to_string)
    })
}

/// Pins `path` as the start page, or, with `None`, goes back to the README.
pub fn pin_start_page(root: &Path, path: Option<&str>) -> Result<Restored> {
    if let Some(path) = path {
        if !resolve(root, path)?.is_file() {
            bail!("{path} isn't a document");
        }
        if pinned(root).as_deref() == Some(path) {
            bail!("{path} is the start page already");
        }
    } else if pinned(root).is_none() {
        bail!("no start page is pinned; the README is the start page");
    }
    ensure_clean(root, &[SETTINGS])?;
    let message = match path {
        Some(path) => format!("pin: start page {path}"),
        None => "unpin: start page".to_string(),
    };
    commit_change(root, &message, || {
        set_pin(root, path)?;
        Ok(vec![PathBuf::from(SETTINGS)])
    })
}

fn set_pin(root: &Path, path: Option<&str>) -> Result<()> {
    let file = root.join(SETTINGS);
    let mut config = crate::settings::Config::load_from(&file);
    match path {
        Some(path) => config.set(START_PAGE, path),
        None => {
            config.remove(START_PAGE);
        }
    }
    config.save_to(&file)
}

/// One page of the link tree under the start page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    /// 0 for the start page, 1 for what it links to, and so on.
    pub depth: usize,
    pub path: String,
    pub title: String,
}

/// The documents linked from `start`, then from those, depth first and each
/// page once (ticket 179): the site map the start page defines. Only
/// markdown documents inside the repository that exist count.
pub fn link_tree(root: &Path, start: &str) -> Result<Vec<Page>> {
    fn visit(root: &Path, path: &str, depth: usize, seen: &mut Vec<String>, out: &mut Vec<Page>) {
        if seen.iter().any(|p| p == path) {
            return;
        }
        seen.push(path.to_string());
        let Ok(text) = crate::eol::read(root.join(path)) else {
            return;
        };
        let title = crate::documents::title_of(&text)
            .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(path).to_string());
        out.push(Page {
            depth,
            path: path.to_string(),
            title,
        });
        let links = std::cell::RefCell::new(Vec::new());
        rewrite_links(&text, |target| {
            if let Some(found) = join_link(path, target) {
                links.borrow_mut().push(found);
            }
            None
        });
        for link in links.into_inner() {
            let lower = link.to_ascii_lowercase();
            let markdown = lower.ends_with(".md") || lower.ends_with(".markdown");
            if markdown && resolve(root, &link).is_ok_and(|p| p.is_file()) {
                visit(root, &link, depth + 1, seen, out);
            }
        }
    }
    resolve(root, start)?;
    let mut out = Vec::new();
    visit(root, start, 0, &mut Vec::new(), &mut out);
    Ok(out)
}

/// A line that matched a search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub path: String,
    pub title: String,
    /// 1-based; 0 when only the title matched.
    pub line: usize,
    pub text: String,
}

/// Every document in `root` whose title or text holds `query`, ignoring
/// case: its first few matching lines each, frontmatter left out. Titles
/// matching come first.
pub fn search(root: &Path, query: &str, per_document: usize) -> Result<Vec<Hit>> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    let mut titled = Vec::new();
    let mut rest = Vec::new();
    for doc in crate::documents::list(root)? {
        let text = crate::eol::read(root.join(&doc.path)).unwrap_or_default();
        let (front, body) = split_frontmatter(&text);
        let skip = front.map_or(0, |f| f.lines().count());
        let mut hits: Vec<Hit> = body
            .lines()
            .enumerate()
            .filter(|(_, l)| l.to_lowercase().contains(&needle))
            .take(per_document.max(1))
            .map(|(i, l)| Hit {
                path: doc.path.clone(),
                title: doc.title.clone(),
                line: skip + i + 1,
                text: l.trim().chars().take(200).collect(),
            })
            .collect();
        let title_hit = doc.title.to_lowercase().contains(&needle);
        if hits.is_empty() && title_hit {
            hits.push(Hit {
                path: doc.path.clone(),
                title: doc.title.clone(),
                line: 0,
                text: String::new(),
            });
        }
        if title_hit {
            titled.extend(hits);
        } else {
            rest.extend(hits);
        }
    }
    titled.extend(rest);
    Ok(titled)
}

/// The numbers of a folder's documents named `NNNN-…`; with `deep`, its
/// sub-folders' too (ticket 177: an ADR moved into `adr/adr/superseded/`
/// keeps its number, so a new one mustn't take it again).
fn numbers(root: &Path, folder: &str, deep: bool) -> Vec<u32> {
    fn walk(dir: &Path, deep: bool, out: &mut Vec<u32>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if deep {
                    walk(&entry.path(), deep, out);
                }
            } else if let Some((number, _)) = name.split_once('-')
                && number.len() == 4
                && name.ends_with(".md")
                && let Ok(n) = number.parse()
            {
                out.push(n);
            }
        }
    }
    let mut out = Vec::new();
    walk(&root.join(folder), deep, &mut out);
    out
}

/// The documents repository `repo` names, else the project's own (ticket
/// 380), cloned here: `(name, root)`. Refused for an archived project's
/// own documents, as riteui's editor refuses them; a shared repository
/// belongs to no one project, so it never is.
pub fn writable_source(
    board: &crate::board::Board,
    repo: Option<&str>,
) -> Result<(String, PathBuf)> {
    let sources = crate::documents::sources(board);
    let wanted = repo.map(str::trim).filter(|r| !r.is_empty());
    let source = match wanted {
        Some(repo) => sources
            .iter()
            .find(|s| crate::names::same(&s.name, repo))
            .with_context(|| {
                format!("{repo} isn't one of this project's documents repositories")
            })?,
        None => sources
            .iter()
            .find(|s| s.kind == crate::documents::Kind::Own)
            .context("this project has no documents repository")?,
    };
    let root = source
        .root
        .clone()
        .with_context(|| format!("{} isn't cloned on this machine", source.name))?;
    if source.kind == crate::documents::Kind::Own {
        crate::archive::ensure_writable(board)?;
    }
    Ok((source.name.clone(), root))
}

/// `path` as given, without its repository's name in front (`name/…`, as
/// search results show it).
pub fn strip_source<'a>(name: &str, path: &'a str) -> &'a str {
    path.strip_prefix(&format!("{name}/")).unwrap_or(path)
}

/// Replaces a document's text (ticket 380): [`save`], checked against
/// `opened` when given, else against the file as it is now, as a ticket's
/// edit takes its current version when none is given.
pub fn replace(root: &Path, path: &str, text: &str, opened: Option<&str>) -> Result<Restored> {
    let full = resolve(root, path)?;
    let current = match opened.map(str::trim).filter(|v| !v.is_empty()) {
        Some(version) => version.to_string(),
        None => version(&crate::eol::read(&full).with_context(|| format!("reading {path}"))?),
    };
    save(root, path, &crate::eol::lf(text.to_string()), &current)
}

/// `# heading`, then `body` (LF line endings, ending in one newline). A
/// `# ` line at the top of `body` gives way to `heading`.
fn with_heading(heading: &str, body: &str) -> String {
    let body = crate::eol::lf(body.to_string());
    let body = body.trim_start_matches('\n');
    let body = match body.split_once('\n') {
        Some((first, rest)) if first.starts_with("# ") => rest.trim_start_matches('\n'),
        None if body.starts_with("# ") => "",
        _ => body,
    };
    let body = body.trim_end();
    if body.is_empty() {
        format!("# {heading}\n\n")
    } else {
        format!("# {heading}\n\n{body}\n")
    }
}

/// Makes a new document titled `title` in `folder`: `<slug>.md` beginning
/// `# <title>`. In a numbered folder (one whose documents are `NNNN-…`, or
/// an empty `adr/adr` or `adr/rfc`) it takes the next number, and in those
/// two its heading says `ADR NNNN:` or `RFC NNNN:`. Returns its path.
pub fn create_document(root: &Path, folder: &str, title: &str) -> Result<(String, Restored)> {
    create_document_with(root, folder, title, None)
}

/// [`create_document`] with the document's text below the heading, in the
/// same commit (ticket 380: the CLI's `rite docs new` and the MCP server's
/// `create_document`). A body whose first line is already a `# ` heading has
/// that line replaced by the one worked out here, so an ADR's number is
/// always right and the heading never appears twice.
pub fn create_document_with(
    root: &Path,
    folder: &str,
    title: &str,
    body: Option<&str>,
) -> Result<(String, Restored)> {
    let folder = plain(folder)?;
    if !folder.is_empty() && !root.join(&folder).is_dir() {
        bail!("{folder} isn't a folder");
    }
    let title = title.trim();
    let slug = crate::task::slugify(title);
    if slug.is_empty() {
        bail!("a document needs a title with letters or numbers in it");
    }
    let kind = match folder.as_str() {
        "adr/adr" => Some("ADR"),
        "adr/rfc" => Some("RFC"),
        _ => None,
    };
    // Numbered when its own documents are; the next number counts those
    // moved into its sub-folders too.
    let numbered = kind.is_some() || !numbers(root, &folder, false).is_empty();
    let taken = if numbered {
        numbers(root, &folder, true)
    } else {
        Vec::new()
    };
    let (name, heading) = if numbered {
        let next = taken.iter().max().copied().unwrap_or(0) + 1;
        let heading = match kind {
            Some(kind) => format!("{kind} {next:04}: {title}"),
            None => title.to_string(),
        };
        (format!("{next:04}-{slug}.md"), heading)
    } else {
        (format!("{slug}.md"), title.to_string())
    };
    let path = join(&folder, &name);
    let full = root.join(&path);
    if full.exists() {
        bail!("{path} is already there");
    }
    let text = with_heading(&heading, body.unwrap_or_default());
    let done = commit_change(root, &format!("new: {path}"), || {
        fs::write(&full, &text).with_context(|| format!("writing {path}"))?;
        Ok(vec![PathBuf::from(&path)])
    })?;
    Ok((path, done))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A documents repository with a bare origin: `adr/adr/0001-x.md`,
    /// linked from the README, linking to `img/p.png`.
    fn repo() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin.git");
        let root = dir.path().join("p-documents");
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
        fs::create_dir_all(root.join("adr/adr")).unwrap();
        fs::create_dir_all(root.join("adr/img")).unwrap();
        git_in(&root, &["init", "-q", "-b", "main"]);
        for (k, v) in [
            ("user.email", "t@t.co"),
            ("user.name", "Tim"),
            ("commit.gpgsign", "false"),
        ] {
            git_in(&root, &["config", k, v]);
        }
        fs::write(
            root.join("README.md"),
            "# P\n\nSee [ADR 1](adr/adr/0001-x.md#why).\n",
        )
        .unwrap();
        fs::write(
            root.join("adr/adr/0001-x.md"),
            "# ADR 0001: X\n\n![p](../img/p.png) and [the readme](../../README.md)\n",
        )
        .unwrap();
        fs::write(root.join("adr/img/p.png"), "png").unwrap();
        git_in(&root, &["add", "-A"]);
        git_in(&root, &["commit", "-q", "-m", "seed"]);
        git_in(
            &root,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git_in(&root, &["push", "-q", "-u", "origin", "main"]);
        (dir, root)
    }

    fn origin_has(dir: &Path, rev_path: &str) -> String {
        let out = std::process::Command::new("git")
            .arg("--git-dir")
            .arg(dir.join("origin.git"))
            .args(["show", rev_path])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn the_start_page_is_the_readme_until_one_is_pinned_and_follows_a_move() {
        let (dir, root) = repo();
        assert_eq!(start_page(&root).as_deref(), Some("README.md"));
        assert!(pin_start_page(&root, None).is_err(), "nothing pinned yet");
        assert!(pin_start_page(&root, Some("nope.md")).is_err());
        let done = pin_start_page(&root, Some("adr/adr/0001-x.md")).unwrap();
        assert!(done.pushed);
        assert_eq!(start_page(&root).as_deref(), Some("adr/adr/0001-x.md"));
        assert!(
            origin_has(dir.path(), "main:.agile-md").contains("start-page = adr/adr/0001-x.md")
        );
        assert!(
            pin_start_page(&root, Some("adr/adr/0001-x.md")).is_err(),
            "already"
        );
        fs::create_dir_all(root.join("adr/adr/old")).unwrap();
        let (to, _) = move_document(&root, "adr/adr/0001-x.md", "adr/adr/old").unwrap();
        assert_eq!(
            pinned(&root).as_deref(),
            Some(to.as_str()),
            "the pin moved with it"
        );
        pin_start_page(&root, None).unwrap();
        assert_eq!(start_page(&root).as_deref(), Some("README.md"));
    }

    #[test]
    fn the_link_tree_follows_links_from_the_start_page_once_each() {
        let (_dir, root) = repo();
        // README → ADR 1, which links back to the README (shown once) and
        // to a picture (not a document).
        fs::write(
            root.join("docs.md"),
            "# Docs\n\n[readme](README.md) [gone](nope.md) [web](https://x.io/a.md)\n",
        )
        .unwrap();
        fs::write(
            root.join("README.md"),
            "# P\n\nSee [ADR 1](adr/adr/0001-x.md#why) and [docs](docs.md).\n",
        )
        .unwrap();
        let tree = link_tree(&root, "README.md").unwrap();
        let got: Vec<(usize, &str)> = tree.iter().map(|p| (p.depth, p.path.as_str())).collect();
        assert_eq!(
            got,
            [(0, "README.md"), (1, "adr/adr/0001-x.md"), (1, "docs.md")]
        );
        assert_eq!(tree[1].title, "ADR 0001: X");
        assert!(link_tree(&root, "../outside.md").is_err());
    }

    #[test]
    fn search_finds_titles_and_lines_ignoring_case() {
        let (_dir, root) = repo();
        fs::write(
            root.join("notes.md"),
            "---\ntitle: n\n---\n# Notes\n\nThe readme says hi.\nREADME again.\n",
        )
        .unwrap();
        let hits = search(&root, "readme", 3).unwrap();
        // ADR 0001 links to the readme; notes.md says it twice; README.md's
        // own text doesn't hold the word, but its file name isn't searched.
        let notes: Vec<&Hit> = hits.iter().filter(|h| h.path == "notes.md").collect();
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].line, 6, "counted from the top of the file");
        assert_eq!(notes[0].text, "The readme says hi.");
        assert!(hits.iter().any(|h| h.path == "adr/adr/0001-x.md"));
        let titled = search(&root, "adr 0001", 3).unwrap();
        assert_eq!(
            titled[0].path, "adr/adr/0001-x.md",
            "a title match comes first"
        );
        assert!(search(&root, "  ", 3).unwrap().is_empty());
        assert!(search(&root, "zzz-nothing", 3).unwrap().is_empty());
    }

    #[test]
    fn archiving_adds_and_removes_the_status_line() {
        assert!(!is_archived("# X\n"));
        let on = with_archived("# X\n", true);
        assert_eq!(on, "---\nstatus: archived\n---\n# X\n");
        assert!(is_archived(&on));
        assert_eq!(
            with_archived(&on, false),
            "# X\n",
            "an emptied block goes too"
        );
        let kept = with_archived("---\ntitle: X\n---\n# X\n", true);
        assert_eq!(kept, "---\ntitle: X\nstatus: archived\n---\n# X\n");
        assert_eq!(with_archived(&kept, false), "---\ntitle: X\n---\n# X\n");
    }

    #[test]
    fn set_archived_commits_pushes_and_shows_in_the_list() {
        let (dir, root) = repo();
        let done = set_archived(&root, "adr/adr/0001-x.md", true).unwrap();
        assert!(done.pushed, "{:?}", done.warning);
        assert!(origin_has(dir.path(), "main:adr/adr/0001-x.md").contains("status: archived"));
        let listed = crate::documents::list(&root).unwrap();
        assert!(
            listed
                .iter()
                .find(|d| d.path == "adr/adr/0001-x.md")
                .unwrap()
                .archived
        );
        assert!(
            set_archived(&root, "adr/adr/0001-x.md", true).is_err(),
            "already"
        );
        fs::write(root.join("adr/adr/0001-x.md"), "edited\n").unwrap();
        assert!(
            set_archived(&root, "adr/adr/0001-x.md", false).is_err(),
            "uncommitted"
        );
    }

    #[test]
    fn folders_are_made_listed_and_removed_only_when_empty() {
        let (_dir, root) = repo();
        create_folder(&root, "adr/adr", "superseded").unwrap();
        assert!(root.join("adr/adr/superseded/.gitkeep").exists());
        assert!(folders(&root).contains(&"adr/adr/superseded".to_string()));
        assert!(
            create_folder(&root, "adr/adr", "superseded").is_err(),
            "already there"
        );
        assert!(create_folder(&root, "", ".hidden").is_err());
        assert!(create_folder(&root, "../x", "y").is_err());
        assert!(delete_folder(&root, "adr/adr").is_err(), "holds a document");
        delete_folder(&root, "adr/adr/superseded").unwrap();
        assert!(!root.join("adr/adr/superseded").exists());
        let status = git::checked(&root, &["status", "--porcelain"]).unwrap();
        assert!(status.is_empty(), "{status}");
    }

    #[test]
    fn a_link_is_written_relative_to_where_it_is() {
        assert_eq!(relative_link("adr/adr", "adr/img/p.png"), "../img/p.png");
        assert_eq!(relative_link("", "adr/adr/0001-x.md"), "adr/adr/0001-x.md");
        assert_eq!(
            relative_link("adr/adr/old", "README.md"),
            "../../../README.md"
        );
        assert_eq!(
            rewrite_links("[a](x.md#h) [b](y.md)\n[r]: x.md \"t\"\n", |t| (t
                == "x.md")
                .then(|| "z/x.md".into())),
            "[a](z/x.md#h) [b](y.md)\n[r]: z/x.md \"t\"\n"
        );
    }

    #[test]
    fn moving_keeps_every_link_working_and_the_history() {
        let (dir, root) = repo();
        create_folder(&root, "adr/adr", "old").unwrap();
        let (to, done) = move_document(&root, "adr/adr/0001-x.md", "adr/adr/old").unwrap();
        assert_eq!(to, "adr/adr/old/0001-x.md");
        assert!(done.pushed);
        let readme = crate::eol::read(root.join("README.md")).unwrap();
        assert!(readme.contains("(adr/adr/old/0001-x.md#why)"), "{readme}");
        let moved = crate::eol::read(root.join(&to)).unwrap();
        assert!(moved.contains("(../../img/p.png)"), "{moved}");
        assert!(moved.contains("(../../../README.md)"), "{moved}");
        let subjects: Vec<String> = crate::documents::history(&root, &to, 20)
            .unwrap()
            .into_iter()
            .map(|v| v.subject)
            .collect();
        assert_eq!(
            subjects,
            [
                "move: links to adr/adr/old/0001-x.md",
                "move: adr/adr/0001-x.md → adr/adr/old/0001-x.md",
                "seed"
            ],
            "follows the move"
        );
        assert!(origin_has(dir.path(), "main:adr/adr/old/0001-x.md").contains("ADR 0001"));
        assert!(
            move_document(&root, &to, "adr/adr/old").is_err(),
            "same place"
        );
        fs::write(root.join("adr/adr/0001-x.md"), "# clash\n").unwrap();
        assert!(
            move_document(&root, &to, "adr/adr").is_err(),
            "a file is there"
        );
    }

    #[test]
    fn saving_refuses_a_stale_version() {
        let (_dir, root) = repo();
        let text = crate::eol::read(root.join("README.md")).unwrap();
        let opened = version(&text);
        save(&root, "README.md", "# P\n\nNew.\n", &opened).unwrap();
        assert_eq!(
            crate::eol::read(root.join("README.md")).unwrap(),
            "# P\n\nNew.\n"
        );
        let err = save(&root, "README.md", "# P\n\nAgain.\n", &opened).unwrap_err();
        assert!(
            err.to_string().contains("changed since you opened it"),
            "{err}"
        );
    }

    #[test]
    fn a_moved_document_keeps_its_pins() {
        let (_dir, root) = repo();
        crate::doc_pins::set(&root, "t@t.co", "p-documents", "adr/adr/0001-x.md", true).unwrap();
        create_folder(&root, "adr/adr", "old").unwrap();
        let (to, _) = move_document(&root, "adr/adr/0001-x.md", "adr/adr/old").unwrap();
        let pins = crate::doc_pins::pins(&root, "t@t.co");
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].path, to);
        let status = git::checked(&root, &["status", "--porcelain"]).unwrap();
        assert!(status.trim().is_empty(), "committed: {status}");
    }

    #[test]
    fn new_documents_are_named_and_numbered() {
        let (_dir, root) = repo();
        // ADR 0001 moved into a sub-folder keeps its number (ticket 177).
        create_folder(&root, "adr/adr", "superseded").unwrap();
        move_document(&root, "adr/adr/0001-x.md", "adr/adr/superseded").unwrap();
        let (path, _) = create_document(&root, "adr/adr", "Keep the cache").unwrap();
        assert_eq!(path, "adr/adr/0002-keep-the-cache.md");
        assert_eq!(
            crate::eol::read(root.join(&path)).unwrap(),
            "# ADR 0002: Keep the cache\n\n"
        );
        fs::create_dir_all(root.join("adr/rfc")).unwrap();
        let (rfc, _) = create_document(&root, "adr/rfc", "Retry payments").unwrap();
        assert_eq!(rfc, "adr/rfc/0001-retry-payments.md");
        let (doc, _) = create_document(&root, "", "Release notes").unwrap();
        assert_eq!(doc, "release-notes.md");
        assert!(
            create_document(&root, "", "Release notes").is_err(),
            "already there"
        );
        assert!(create_document(&root, "", "🎉").is_err());
    }

    #[test]
    fn a_new_document_can_have_its_text_in_the_same_commit() {
        // Ticket 380.
        let (_dir, root) = repo();
        let commits = || git::count_commits(&root, &["HEAD"]).unwrap();
        let before = commits();
        fs::create_dir_all(root.join("adr/rfc")).unwrap();
        // A body with its own heading: the numbered one replaces it.
        let (path, done) = create_document_with(
            &root,
            "adr/rfc",
            "Hosted sync",
            Some("# My heading\r\n\r\n## Context\r\n\r\nWhy.\r\n"),
        )
        .unwrap();
        assert_eq!(path, "adr/rfc/0001-hosted-sync.md");
        assert_eq!(
            crate::eol::read(root.join(&path)).unwrap(),
            "# RFC 0001: Hosted sync\n\n## Context\n\nWhy.\n"
        );
        assert!(done.pushed, "pushed straight to main");
        assert_eq!(commits(), before + 1, "one commit");
        // No heading of its own: the body goes below the one made here.
        let (doc, _) = create_document_with(&root, "", "Notes", Some("Plain text.")).unwrap();
        assert_eq!(
            crate::eol::read(root.join(&doc)).unwrap(),
            "# Notes\n\nPlain text.\n"
        );
    }

    #[test]
    fn replace_checks_a_version_only_when_given_one() {
        // Ticket 380.
        let (_dir, root) = repo();
        let opened = version(&crate::eol::read(root.join("README.md")).unwrap());
        replace(&root, "README.md", "# P\r\n\r\nOne.\r\n", None).unwrap();
        assert_eq!(
            crate::eol::read(root.join("README.md")).unwrap(),
            "# P\n\nOne.\n",
            "written LF"
        );
        let err = replace(&root, "README.md", "# P\n\nTwo.\n", Some(&opened)).unwrap_err();
        assert!(
            err.to_string().contains("changed since you opened it"),
            "{err}"
        );
        assert!(
            replace(&root, "README.md", "# P\n\nOne.\n", None).is_err(),
            "unchanged"
        );
        assert!(replace(&root, "nope.md", "x", None).is_err(), "missing");
    }
}
