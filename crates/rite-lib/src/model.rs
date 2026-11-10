//! The board's data model, free of any front end: tickets as cards in lanes,
//! the repositories a project spans, and the filters and ordering over them.
//!
//! Lifted out of the desktop board (`gui/`) so any front end reads and
//! orders tickets exactly as it does. Nothing here draws or prints; every
//! write still goes through `Board`/`Task`, never a file.

use std::collections::BTreeSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::Result;

use crate::board::{Board, Column};
use crate::group::{Group, Kind};
use crate::registry::Registry;
use crate::task::Task;

pub struct Card {
    pub task: Task,
    pub title: String,
    pub id: String,
    pub assignee: Option<String>,
    pub points: Option<String>,
    /// Sort key: the hand-set order when there is one, otherwise the id, so a
    /// board nobody has dragged still reads in id order.
    pub rank: f64,
}

pub struct RepoBoard {
    pub name: String,
    pub board: Board,
    pub lanes: Vec<Vec<Card>>,
    /// Backlog tickets grouped by folder. `None` is the loose backlog —
    /// everything not filed under an epic or sprint yet.
    pub groups: Vec<(Option<Group>, Vec<Card>)>,
    pub visible: bool,
    /// Which list the epic/sprint dropdown shows. Per-repo, like `groups`
    /// itself — each registered repository has its own independent set.
    pub selected_kind: Kind,
    /// The chosen group, if any. `None` means no filter: the loose backlog
    /// and every column, unfiltered, exactly as before this feature existed.
    /// Switching `selected_kind` clears this — a name from the other kind's
    /// list doesn't carry over into the new one.
    pub selected_group: Option<String>,
}

/// How many tickets carry each tag across `boards` — a project's boards,
/// the tickets repository's and every registered repository's (ticket 122).
/// Uppercase, sorted; the archive doesn't count.
pub fn tag_counts(boards: &[&Board]) -> anyhow::Result<std::collections::BTreeMap<String, usize>> {
    let mut counts = std::collections::BTreeMap::new();
    for board in boards {
        for (tag, n) in board.tag_counts()? {
            *counts.entry(tag).or_insert(0) += n;
        }
    }
    Ok(counts)
}

/// Takes `tag` off the project's list, kept in `project`'s `.settings`, but
/// only if no ticket on any of `boards` carries it: removing a tag never
/// changes a ticket (ticket 122). Saves the settings; committing is the
/// caller's (the auto-commit wraps this).
pub fn remove_unused_tag(project: &Board, boards: &[&Board], tag: &str) -> anyhow::Result<String> {
    let tag = crate::task::normalize_tag(tag);
    if crate::task::is_reserved_tag(&tag) {
        anyhow::bail!("{tag} is built in and can't be removed");
    }
    let used = tag_counts(boards)?.get(&tag).copied().unwrap_or(0);
    if used > 0 {
        anyhow::bail!("{tag} is still on {used} ticket(s); take it off them first");
    }
    let mut settings = crate::board_settings::load(project);
    if !settings.remove_tag(&tag) {
        anyhow::bail!("{tag} isn't on the project's tag list");
    }
    settings.save(project)?;
    Ok(tag)
}

/// Whether `card` should be drawn under the current tag filter. An empty
/// filter matches everything, same as no filter at all. Case is ignored
/// (ticket 122).
pub fn card_visible(card: &Card, filter: &BTreeSet<String>) -> bool {
    filter.is_empty() || filter.iter().any(|tag| card.task.has_tag(tag))
}

/// Whether `card`'s task currently belongs to the group named `name`. Inside
/// `backlog/` the folder decides (`task.epic`, set from where the file was
/// found); outside it there's no group directory, so the frontmatter key is
/// the only carrier (`epic_meta()`, a fresh read) — same split
/// `Board::tasks_in_group` already makes.
pub fn card_in_group(card: &Card, name: &str) -> bool {
    if card.task.column == Column::Backlog {
        card.task.epic.as_deref() == Some(name)
    } else {
        card.task.epic_meta().as_deref() == Some(name)
    }
}

/// The rank a card dropped at `index` in a lane takes: the midpoint of its
/// neighbours, so one drop rewrites one file. `lane` is every other card's
/// rank in order — the card being moved must already be left out, so
/// dragging within a lane doesn't measure against its old position.
pub fn rank_between(lane: &[f64], index: usize) -> f64 {
    let index = index.min(lane.len());
    match (
        index.checked_sub(1).and_then(|i| lane.get(i)),
        lane.get(index),
    ) {
        (Some(before), Some(after)) => (before + after) / 2.0,
        (Some(before), None) => before + 1.0,
        (None, Some(after)) => after - 1.0,
        (None, None) => 0.0,
    }
}

/// The backlog, split into the loose tickets and one group per epic folder.
/// The loose group comes first and is always present, so there is somewhere to
/// drop a ticket you want to take *out* of an epic.
pub fn read_groups(board: &Board) -> Result<Vec<(Option<Group>, Vec<Card>)>> {
    let all = board.tasks_in(Column::Backlog)?;
    let mut groups: Vec<(Option<Group>, Vec<Card>)> = vec![(None, Vec::new())];
    for group in board.groups()? {
        groups.push((Some(group), Vec::new()));
    }
    for task in all {
        let slot = groups
            .iter_mut()
            .find(|(group, _)| group.as_ref().map(|g| g.name.as_str()) == task.epic.as_deref());
        if let Some((_, cards)) = slot {
            cards.push(card_from(task));
        }
    }
    for (_, cards) in &mut groups {
        sort_cards(cards);
    }
    Ok(groups)
}

pub fn card_from(task: Task) -> Card {
    let rank = task
        .order()
        .unwrap_or_else(|| task.id.map(f64::from).unwrap_or(f64::MAX));
    Card {
        title: task.title(),
        id: task.id_display(),
        assignee: task.assignee(),
        points: task.points(),
        rank,
        task,
    }
}

pub fn sort_cards(cards: &mut [Card]) {
    cards.sort_by(|a, b| {
        a.rank
            .partial_cmp(&b.rank)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// Put a typed body into a rendered ticket, keeping its frontmatter.
///
/// An empty body leaves the template's own, so clearing the box gets you the
/// default ticket rather than an empty file that `write` would refuse.
pub fn splice_body(rendered: &str, body: &str) -> String {
    if body.trim().is_empty() {
        return rendered.to_string();
    }
    match crate::task::split_body(rendered) {
        Some((head, _)) => format!("{head}{body}"),
        // No frontmatter to keep: the template is not one we understand, so
        // don't pretend — write what it rendered.
        None => rendered.to_string(),
    }
}

pub fn read_lanes(board: &Board) -> Result<Vec<Vec<Card>>> {
    let mut lanes = Vec::new();
    for column in board.workflow().columns() {
        let mut cards: Vec<Card> = board.tasks_in(column)?.into_iter().map(card_from).collect();
        sort_cards(&mut cards);
        lanes.push(cards);
    }
    Ok(lanes)
}

/// Builds a `RepoBoard` from a repository root and the `Board` inside it.
/// The directory's own basename names it, not `Board::name()` — a board's
/// root is `<repo>/tasks`, so the latter would label every repository
/// "tasks".
pub fn open_repo_board(root: PathBuf, board: Board) -> Result<RepoBoard> {
    let name = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());
    Ok(RepoBoard {
        name,
        lanes: read_lanes(&board)?,
        groups: read_groups(&board)?,
        board,
        visible: true,
        selected_kind: Kind::Epic,
        selected_group: None,
    })
}

/// Everything the repositories of one project resolve to on this machine.
pub struct Repos {
    /// The current project's own board first, then each declared repository
    /// that is registered here and has a board.
    pub repos: Vec<RepoBoard>,
    /// Declared names with no clone here, for a front end to show greyed out
    /// rather than silently omit.
    pub unregistered: Vec<String>,
    /// The git identity (`user.email`) leases are compared against.
    pub identity: Option<String>,
}

/// Open the current project's board and every declared, registered
/// repository beside it.
pub fn load_repos() -> Result<Repos> {
    let found = find_repos()?;
    let repos = found
        .repos
        .into_iter()
        .map(|(_, root, board)| open_repo_board(root, board))
        .collect::<Result<Vec<_>>>()?;
    Ok(Repos {
        repos,
        unregistered: found.unregistered,
        identity: found.identity,
    })
}

/// The project's boards, as [`load_repos`] finds them, **without reading a
/// single ticket** (ticket 184): `(name, repository root, board)` for each.
/// riteui opens the project for every command, including the once-a-second
/// fingerprint, and reading every ticket into cards each time only to drop
/// them was most of what it did.
pub fn find_repos() -> Result<FoundRepos> {
    // A hard error now, not a `.ok()` folded away — `riteui` operates on
    // exactly one project (the one launched from), never a personal
    // cross-project stack the way the old machine-wide registry allowed.
    find_repos_from(Board::discover()?)
}

/// [`find_repos`] for the project whose own board is `current`, wherever
/// it is: what the MCP server opens another project by (ticket 381).
pub fn find_repos_from(current: Board) -> Result<FoundRepos> {
    let current_root = current.repo_root().to_path_buf();
    let declared = crate::board_settings::load(&current).repos();
    let registry = Registry::load();
    let project_dir = crate::registry::project_dir_of(&current_root);

    let mut repos = vec![(repo_name(&current_root), current_root.clone(), current)];
    // A name this project declares with no clone here — registered, or
    // beside the tickets repository (ticket 342) — is rendered greyed out
    // in the repo-visibility row, not silently omitted, so a teammate who's
    // added a repository sees it exists before they've cloned it. A clone
    // with no board of its own (a code repository whose tickets are on the
    // project's board) is cloned, and has no board to add.
    let mut unregistered = Vec::new();
    for name in &declared {
        let clone = registry
            .find_near(name, &project_dir)
            .map(|entry| entry.root.clone())
            .or_else(|| crate::registry::beside(&current_root, name));
        let Some(root) = clone else {
            unregistered.push(name.clone());
            continue;
        };
        if root == current_root {
            continue; // already added, above
        }
        let board = Board::in_repo(&root);
        if board.root.is_dir() {
            repos.push((repo_name(&root), root, board));
        }
    }
    // `user.email` is ordinarily global, so any registered repo's
    // context resolves the same identity — the first one is as good as
    // any other.
    let identity = crate::git::config(&repos[0].2.root, "user.email");
    Ok(FoundRepos {
        repos,
        unregistered,
        identity,
    })
}

/// What [`find_repos`] finds.
pub struct FoundRepos {
    /// `(name, repository root, board)`: the current project's own board
    /// first, then each declared repository registered here with a board.
    pub repos: Vec<(String, PathBuf, Board)>,
    pub unregistered: Vec<String>,
    pub identity: Option<String>,
}

fn repo_name(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string())
}

/// A cheap summary of what's on disk for `board` (ticket 091): every file
/// under the board directory (columns, epic and sprint folders) and the
/// repository's `.settings`, by relative path, size and modification time.
/// Equal fingerprints mean nothing a front end shows has changed, so a
/// window can ask for this every second and reload only when it differs —
/// whatever changed the files: `rite` in a terminal, an editor, a pull or a
/// checkout. `archive/` is left out: nothing on the board shows it.
///
/// Only meaningful within one process (it uses the standard hasher), which
/// is all a poll needs.
pub fn fingerprint(board: &Board) -> u64 {
    let mut entries = Vec::new();
    collect(&board.root, &board.root, &board.archive_dir(), &mut entries);
    let settings = crate::board_settings::path(board);
    entries.push(entry(Path::new(".settings"), &settings));
    entries.sort();
    let mut hasher = DefaultHasher::new();
    entries.hash(&mut hasher);
    hasher.finish()
}

type Entry = (PathBuf, u64, u128);

fn collect(root: &Path, dir: &Path, skip: &Path, out: &mut Vec<Entry>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for item in read.flatten() {
        let path = item.path();
        if path == skip {
            continue;
        }
        if path.is_dir() {
            collect(root, &path, skip, out);
        } else {
            out.push(entry(path.strip_prefix(root).unwrap_or(&path), &path));
        }
    }
}

/// `(relative path, size, mtime in ns)`; a file that can't be read
/// fingerprints as size and time zero, so its disappearing still counts.
fn entry(relative: &Path, path: &Path) -> Entry {
    let meta = std::fs::metadata(path).ok();
    let size = meta.as_ref().map_or(0, std::fs::Metadata::len);
    let modified = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    (relative.to_path_buf(), size, modified)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_board() -> (tempfile::TempDir, Board) {
        let dir = tempfile::tempdir().unwrap();
        let board = Board::at(dir.path().join("tasks"));
        for column in ["backlog", "todo", "doing", "done", "archive"] {
            std::fs::create_dir_all(board.root.join(column)).unwrap();
        }
        std::fs::write(board.root.join("backlog/001-a.md"), "a").unwrap();
        (dir, board)
    }

    /// Ticket 122: usage counts across boards ignore case, and a tag any
    /// ticket carries can't be taken off the list.
    #[test]
    fn a_tag_in_use_on_any_board_is_not_removed() {
        let (_one, first) = scratch_board();
        let (_two, second) = scratch_board();
        std::fs::write(
            first.root.join("backlog/002-b.md"),
            "---\nid: \"002\"\ntitle: \"B\"\ntags: [future]\n---\n",
        )
        .unwrap();
        std::fs::write(
            second.root.join("todo/003-c.md"),
            "---\nid: \"003\"\ntitle: \"C\"\ntags: [FUTURE,cli]\n---\n",
        )
        .unwrap();
        // The archive doesn't count.
        std::fs::write(
            second.root.join("archive/004-d.md"),
            "---\nid: \"004\"\ntitle: \"D\"\ntags: [OLD]\n---\n",
        )
        .unwrap();
        let boards = [&first, &second];
        let counts = tag_counts(&boards).unwrap();
        assert_eq!(counts.get("FUTURE"), Some(&2));
        assert_eq!(counts.get("CLI"), Some(&1));
        assert_eq!(counts.get("OLD"), None);

        let mut settings = crate::board_settings::load(&first);
        for tag in ["FUTURE", "OLD"] {
            settings.add_tag(tag).unwrap();
        }
        settings.save(&first).unwrap();
        let refused = remove_unused_tag(&first, &boards, "future")
            .unwrap_err()
            .to_string();
        assert!(refused.contains("still on 2 ticket(s)"), "{refused}");
        assert_eq!(remove_unused_tag(&first, &boards, "old").unwrap(), "OLD");
        assert_eq!(crate::board_settings::load(&first).tags(), ["FUTURE"]);
        assert!(remove_unused_tag(&first, &boards, "nope").is_err());
        let refused = remove_unused_tag(&first, &boards, "draft")
            .unwrap_err()
            .to_string();
        assert!(refused.contains("built in"), "{refused}");
    }

    #[test]
    fn the_fingerprint_is_stable_when_nothing_changes() {
        let (_dir, board) = scratch_board();
        assert_eq!(fingerprint(&board), fingerprint(&board));
    }

    #[test]
    fn the_fingerprint_changes_with_the_board() {
        let (dir, board) = scratch_board();
        let mut seen = vec![fingerprint(&board)];
        let mut changed = |f: &dyn Fn()| {
            f();
            let now = fingerprint(&board);
            assert!(!seen.contains(&now), "fingerprint didn't change");
            seen.push(now);
        };
        changed(&|| std::fs::write(board.root.join("backlog/002-b.md"), "b").unwrap());
        changed(&|| std::fs::write(board.root.join("backlog/001-a.md"), "edited").unwrap());
        changed(&|| {
            std::fs::rename(
                board.root.join("backlog/001-a.md"),
                board.root.join("todo/001-a.md"),
            )
            .unwrap()
        });
        // An empty folder isn't a group (no `_group.md`), and only files count.
        std::fs::create_dir_all(board.root.join("backlog/epic")).unwrap();
        changed(&|| std::fs::write(board.root.join("backlog/epic/_group.md"), "g").unwrap());
        changed(&|| std::fs::write(dir.path().join(".settings"), "wip-limit = 3\n").unwrap());
    }

    #[test]
    fn the_archive_doesnt_count() {
        let (_dir, board) = scratch_board();
        let before = fingerprint(&board);
        std::fs::write(board.root.join("archive/009-gone.md"), "x").unwrap();
        assert_eq!(fingerprint(&board), before);
    }

    #[test]
    fn rank_between_takes_the_midpoint_of_its_neighbours() {
        assert_eq!(rank_between(&[1.0, 3.0], 1), 2.0);
    }

    #[test]
    fn rank_between_runs_off_either_end_by_one() {
        assert_eq!(rank_between(&[1.0, 3.0], 0), 0.0);
        assert_eq!(rank_between(&[1.0, 3.0], 2), 4.0);
        assert_eq!(rank_between(&[1.0, 3.0], 9), 4.0, "past the end clamps");
    }

    #[test]
    fn rank_between_in_an_empty_lane_is_zero() {
        assert_eq!(rank_between(&[], 0), 0.0);
    }

    #[test]
    fn lanes_come_back_in_rank_order_with_the_hand_set_order_winning() {
        let dir = tempfile::tempdir().unwrap();
        let board = Board::at(dir.path().join("tasks"));
        board.create().unwrap();
        let write = |name: &str, extra: &str| {
            let id = &name[..3];
            let body = format!("---\nid: \"{id}\"\ntitle: \"T{id}\"\n{extra}---\n\nbody\n");
            std::fs::write(board.dir(Column::Todo).join(name), body).unwrap();
        };
        write("001-a.md", "order: \"5\"\n");
        write("002-b.md", "");
        let lanes = read_lanes(&board).unwrap();
        let todo: Vec<&str> = lanes[1].iter().map(|card| card.id.as_str()).collect();
        assert_eq!(todo, ["002", "001"]);
    }
}
