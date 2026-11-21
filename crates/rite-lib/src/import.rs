//! Importing tickets from an existing tasks-directory tree into the current
//! board: status (column) preserved, ids renumbered against the
//! destination's own counter, `parent`/`related` and `[[wikilinks]]` in the
//! text remapped within the imported batch (ticket 404). A ticket whose title is already on the board is left
//! out and reported (ticket 173: an import never adds a second copy);
//! same-titled tickets within the batch are both imported, flagged
//! `DUPLICATE` for a human to sort out.
//!
//! The source tree is read-only throughout — nothing here ever writes to
//! it, so the same source can be imported more than once, or into more
//! than one board.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::board::{Board, Column};
use crate::task::{self, Task};

const DUPLICATE_TAG: &str = "DUPLICATE";

/// One ticket as it landed on the destination board.
#[derive(Debug, Clone)]
pub struct ImportedTicket {
    pub old_id: String,
    pub new_id: String,
    pub title: String,
    pub column: Column,
    pub duplicate: bool,
    /// Its repository and branch type don't go together (ticket 066): made
    /// a draft in the backlog; past it, only reported (`rite check`).
    pub mismatched: bool,
}

#[derive(Debug, Default)]
pub struct ImportSummary {
    pub imported: Vec<ImportedTicket>,
    /// Source files that looked like tickets by filename but couldn't be
    /// imported — path and why. Real trees accumulate cruft (a
    /// consolidation that lost its frontmatter, an unreadable file); one
    /// bad ticket shouldn't sink the rest of the batch.
    pub skipped: Vec<(PathBuf, String)>,
    /// Column folders the source didn't have, taken as empty (ticket 251).
    pub missing_columns: Vec<String>,
    /// Tickets lying loose in the source's tasks folder, outside any column,
    /// imported into the backlog (ticket 251).
    pub loose: usize,
}

impl ImportSummary {
    pub fn duplicate_count(&self) -> usize {
        self.imported.iter().filter(|t| t.duplicate).count()
    }

    /// Imported tickets whose repository and branch type don't go together
    /// (ticket 066).
    pub fn mismatched_count(&self) -> usize {
        self.imported.iter().filter(|t| t.mismatched).count()
    }
}

/// A ticket read from the source, resolved and ready to write — or, on a
/// dry run, ready to report without ever touching disk.
struct Pending {
    old_id: String,
    new_id: String,
    title: String,
    column: Column,
    dest_path: PathBuf,
    text: String,
}

/// Import every ticket in `source_root` (a directory shaped like a board:
/// `backlog/`/`todo/`/`doing/`/`done/` — `archive/` is deliberately not a
/// destination, it isn't a real status) into `dest`. `dry_run` computes and
/// returns exactly what a real import would do — including the id mapping
/// and which tickets would be flagged `DUPLICATE` — without writing
/// anything or spending any ids.
pub fn import(dest: &Board, source_root: &Path, dry_run: bool) -> Result<ImportSummary> {
    let source = Board::at(source_root.to_path_buf());

    let existing_titles: HashSet<String> = dest
        .tasks()?
        .iter()
        .map(|task| normalize_title(&task.title()))
        .collect();

    let mut id_map: HashMap<String, String> = HashMap::new();
    // Old `NNN-slug` to new, for the `[[NNN-slug]]` links in ticket text.
    let mut stem_map: HashMap<String, String> = HashMap::new();
    let mut pending: Vec<Pending> = Vec::new();
    let mut skipped: Vec<(PathBuf, String)> = Vec::new();
    let mut next = dest.next_id()?;

    // What the source has (ticket 251): a missing column folder is just
    // empty, and tickets loose in the folder itself go to the backlog.
    let mut missing_columns = Vec::new();
    let mut found: Vec<(Task, Column)> = Vec::new();
    for column in source.workflow().columns() {
        if !source.dir(column).is_dir() {
            missing_columns.push(column.to_string());
            continue;
        }
        found.extend(source.tasks_in(column)?.into_iter().map(|t| (t, column)));
    }
    let loose = loose_tickets(source_root);
    let loose_count = loose.len();
    found.extend(loose.into_iter().map(|t| (t, Column::Backlog)));

    {
        for (task, column) in found {
            let Some(old_number) = task.id else {
                continue;
            };
            let text = match crate::eol::read(&task.path) {
                Ok(text) => text,
                Err(err) => {
                    skipped.push((task.path.clone(), err.to_string()));
                    continue;
                }
            };
            if !text.starts_with("---") {
                skipped.push((task.path.clone(), "no frontmatter".to_string()));
                continue;
            }

            let old_id = format!("{old_number:03}");
            let title = task.title();
            // Never a second copy of what's already here (ticket 173): a
            // ticket whose title is on this board is left out, and said so.
            if existing_titles.contains(&normalize_title(&title)) {
                skipped.push((
                    task.path.clone(),
                    format!("\"{title}\" is already on this board"),
                ));
                continue;
            }

            let new_id = format!("{next:03}");
            let text = match task::set_meta(&text, "id", &format!("{new_id:?}")) {
                Ok(text) => text,
                Err(err) => {
                    skipped.push((task.path.clone(), err.to_string()));
                    continue;
                }
            };
            // Only spend the id once the rewrite has actually succeeded —
            // a skipped ticket leaves no gap.
            next += 1;
            id_map.insert(old_id.clone(), new_id.clone());

            let slug = task
                .stem
                .split_once('-')
                .map(|(_, slug)| slug)
                .unwrap_or(&task.stem);
            let new_stem = format!("{new_id}-{slug}");
            stem_map.insert(task.stem.clone(), new_stem.clone());
            let dest_path = dest.dir(column).join(format!("{new_stem}.md"));

            pending.push(Pending {
                old_id,
                new_id,
                title,
                column,
                dest_path,
                text,
            });
        }
    }

    // Remap links within the batch before anything is written, so a dry
    // run reports exactly what a real run would produce.
    for item in &mut pending {
        item.text = remap_links(&item.text, &id_map)?;
        item.text = remap_wikilinks(&item.text, &stem_map, &id_map);
    }

    // Duplicate detection: within the batch and against the destination
    // board, on the *original* titles (irrespective of content or status).
    let mut title_counts: HashMap<String, u32> = HashMap::new();
    for item in &pending {
        *title_counts
            .entry(normalize_title(&item.title))
            .or_insert(0) += 1;
    }

    let mut imported = Vec::with_capacity(pending.len());
    for item in pending {
        let normalized = normalize_title(&item.title);
        let duplicate = title_counts.get(&normalized).copied().unwrap_or(0) > 1;
        let text = if duplicate {
            add_tag(&item.text, DUPLICATE_TAG)?
        } else {
            item.text
        };
        let (text, mismatched) =
            match crate::pairing::check_text(dest, &item.new_id, item.column, &text) {
                Ok(None) => (text, false),
                Ok(Some(_)) => (crate::pairing::with_draft_tag(&text)?, true),
                Err(_) => (text, true),
            };

        if !dry_run {
            if let Some(parent) = item.dest_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            fs::write(&item.dest_path, &text)
                .with_context(|| format!("writing {}", item.dest_path.display()))?;
        }

        imported.push(ImportedTicket {
            old_id: item.old_id,
            new_id: item.new_id,
            title: item.title,
            column: item.column,
            duplicate,
            mismatched,
        });
    }

    // Spent only once, for real, at the end — a partial failure still can't
    // collide, because next_id()'s own column-scan safety net catches an
    // unspent counter the same way it already does for any lost one.
    if !dry_run {
        dest.record_id(next.saturating_sub(1))?;
    }

    Ok(ImportSummary {
        imported,
        skipped,
        missing_columns,
        loose: loose_count,
    })
}

/// Tickets (`NNN-title.md`) lying directly in a source's tasks folder,
/// outside any column folder (ticket 251), by number.
pub fn loose_tickets(source_root: &Path) -> Vec<Task> {
    let Ok(entries) = fs::read_dir(source_root) else {
        return Vec::new();
    };
    let mut tasks: Vec<Task> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter_map(|p| Task::from_path(&p, Column::Backlog))
        .filter(|t| t.id.is_some())
        .collect();
    tasks.sort_by(|a, b| a.id.cmp(&b.id).then_with(|| a.stem.cmp(&b.stem)));
    tasks
}

/// Whether `dir` has anything to import: at least one of the standard
/// column folders, or tickets loose in it (ticket 251).
pub fn looks_importable(dir: &Path) -> bool {
    Column::STANDARD
        .iter()
        .any(|c| dir.join(c.to_string()).is_dir())
        || !loose_tickets(dir).is_empty()
}

fn normalize_title(title: &str) -> String {
    title.trim().to_ascii_lowercase()
}

/// Rewrites `parent` and each entry of `related` to its mapped id, when the
/// id they name is part of `id_map` — a reference to something outside the
/// imported batch is left exactly as it was, not invented or dropped.
fn remap_links(text: &str, id_map: &HashMap<String, String>) -> Result<String> {
    let mut text = text.to_string();

    let parent_remap = task::meta_in(&text, "parent")
        .map(task::unquote)
        .map(str::trim)
        .and_then(|old| id_map.get(old))
        .cloned();
    if let Some(new_id) = parent_remap {
        text = task::set_meta(&text, "parent", &format!("{new_id:?}"))?;
    }

    let related_remap = task::meta_in(&text, "related").map(|raw| {
        let ids = task::parse_list(raw);
        let changed = ids.iter().any(|id| id_map.contains_key(id));
        (changed, ids)
    });
    if let Some((true, ids)) = related_remap {
        let remapped: Vec<String> = ids
            .into_iter()
            .map(|id| id_map.get(&id).cloned().unwrap_or(id))
            .collect();
        text = task::set_meta(&text, "related", &format!("[{}]", remapped.join(",")))?;
    }

    let clone_remap = task::meta_in(&text, "clone-of")
        .map(task::unquote)
        .map(str::trim)
        .and_then(|old| id_map.get(old))
        .cloned();
    if let Some(new_id) = clone_remap {
        text = task::set_meta(&text, "clone-of", &format!("{new_id:?}"))?;
    }

    let clones_remap = task::meta_in(&text, "clones").map(|raw| {
        let ids = task::parse_list(raw);
        let changed = ids.iter().any(|id| id_map.contains_key(id));
        (changed, ids)
    });
    if let Some((true, ids)) = clones_remap {
        let remapped: Vec<String> = ids
            .into_iter()
            .map(|id| id_map.get(&id).cloned().unwrap_or(id))
            .collect();
        text = task::set_meta(&text, "clones", &format!("[{}]", remapped.join(",")))?;
    }

    let links_remap = task::meta_in(&text, "links").map(|raw| {
        let entries: Vec<crate::links::Link> = task::parse_list(raw)
            .iter()
            .filter_map(|entry| crate::links::Link::parse(entry))
            .collect();
        let changed = entries.iter().any(|link| id_map.contains_key(&link.id));
        (changed, entries)
    });
    if let Some((true, entries)) = links_remap {
        let remapped: Vec<String> = entries
            .into_iter()
            .map(|mut link| {
                if let Some(new_id) = id_map.get(&link.id) {
                    link.id = new_id.clone();
                }
                link.entry()
            })
            .collect();
        text = task::set_meta(&text, "links", &format!("[{}]", remapped.join(",")))?;
    }

    Ok(text)
}

/// Rewrites `[[NNN-slug]]` and bare `[[NNN]]` links in ticket text to the new
/// numbers (ticket 404), for tickets in the batch only. In one pass, so a
/// ticket renumbered to another's old number is not rewritten twice. A link
/// to a ticket outside the batch is left as it was.
fn remap_wikilinks(
    text: &str,
    stem_map: &HashMap<String, String>,
    id_map: &HashMap<String, String>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("[[") {
        let (before, from) = rest.split_at(start);
        out.push_str(before);
        let after = &from[2..];
        let Some(end) = after.find("]]") else {
            out.push_str(from);
            return out;
        };
        let inner = &after[..end];
        let key = inner.trim();
        let replacement = if inner.contains(['\n', '[']) {
            None
        } else if let Some(new_stem) = stem_map.get(key) {
            Some(new_stem)
        } else if key.chars().all(|c| c.is_ascii_digit()) && !key.is_empty() {
            id_map
                .get(&format!("{:03}", key.parse::<u32>().unwrap_or(u32::MAX)))
                .or_else(|| id_map.get(key))
        } else {
            None
        };
        match replacement {
            Some(new) => {
                out.push_str("[[");
                out.push_str(new);
                out.push_str("]]");
            }
            None => {
                out.push_str("[[");
                out.push_str(inner);
                out.push_str("]]");
            }
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

fn add_tag(text: &str, tag: &str) -> Result<String> {
    let mut tags = task::meta_in(text, "tags")
        .map(task::parse_list)
        .unwrap_or_default();
    if tags.iter().any(|existing| existing == tag) {
        return Ok(text.to_string());
    }
    tags.push(tag.to_string());
    task::set_meta(text, "tags", &format!("[{}]", tags.join(",")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board_at(dir: &Path) -> Board {
        let board = Board::at(dir.join("tasks"));
        board.create().expect("board");
        board
    }

    /// A minimal but real ticket file — same shape `set_meta`/`meta_in`
    /// already assume elsewhere in this crate.
    fn write_ticket(
        board: &Board,
        column: Column,
        id: &str,
        slug: &str,
        title: &str,
        parent: &str,
        related: &str,
    ) -> PathBuf {
        let path = board.dir(column).join(format!("{id}-{slug}.md"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            format!(
                "---\nid: \"{id}\"\ntitle: \"{title}\"\nparent: \"{parent}\"\nrelated: {related}\ntags: []\n---\n\nbody\n"
            ),
        )
        .unwrap();
        path
    }

    #[test]
    fn a_missing_column_is_empty_and_loose_tickets_go_to_the_backlog() {
        let src_dir = tempfile::tempdir().unwrap();
        let src = src_dir.path().join("tasks");
        fs::create_dir_all(src.join("done")).unwrap();
        fs::write(
            src.join("124-loose.md"),
            "---\nid: \"124\"\ntitle: \"Loose\"\n---\n\nbody\n",
        )
        .unwrap();
        fs::write(src.join("notes.md"), "not a ticket\n").unwrap();
        assert!(looks_importable(&src));
        assert!(!looks_importable(src_dir.path()), "nothing there");
        let dst_dir = tempfile::tempdir().unwrap();
        let dest = board_at(dst_dir.path());
        let summary = import(&dest, &src, false).unwrap();
        assert_eq!(summary.loose, 1);
        assert_eq!(summary.missing_columns, ["backlog", "todo", "doing"]);
        assert_eq!(summary.imported.len(), 1);
        assert_eq!(summary.imported[0].column, Column::Backlog);
        assert!(dest.dir(Column::Backlog).join("001-loose.md").exists());
    }

    #[test]
    fn a_plain_import_lands_tickets_in_the_right_columns_with_fresh_ids() {
        let src_dir = tempfile::tempdir().unwrap();
        let source = board_at(src_dir.path());
        write_ticket(
            &source,
            Column::Backlog,
            "005",
            "old-backlog",
            "Old backlog ticket",
            "",
            "[]",
        );
        write_ticket(
            &source,
            Column::Done,
            "002",
            "old-done",
            "Old done ticket",
            "",
            "[]",
        );

        let dst_dir = tempfile::tempdir().unwrap();
        let dest = board_at(dst_dir.path());

        let summary = import(&dest, &source.root, false).unwrap();
        assert_eq!(summary.imported.len(), 2);

        // Column order (Column::ALL) is backlog, todo, doing, done, so the
        // backlog ticket is allocated first regardless of its old id.
        let backlog = &summary.imported[0];
        assert_eq!(backlog.old_id, "005");
        assert_eq!(backlog.new_id, "001");
        assert_eq!(backlog.column, Column::Backlog);
        assert!(
            dest.dir(Column::Backlog)
                .join("001-old-backlog.md")
                .exists()
        );

        let done = &summary.imported[1];
        assert_eq!(done.old_id, "002");
        assert_eq!(done.new_id, "002");
        assert_eq!(done.column, Column::Done);
        assert!(dest.dir(Column::Done).join("002-old-done.md").exists());

        // The destination's own counter has moved on past what was spent.
        assert_eq!(dest.next_id().unwrap(), 3);
    }

    #[test]
    fn parent_and_related_are_remapped_within_the_batch() {
        let src_dir = tempfile::tempdir().unwrap();
        let source = board_at(src_dir.path());
        write_ticket(
            &source,
            Column::Backlog,
            "001",
            "parent",
            "Parent",
            "",
            "[]",
        );
        write_ticket(
            &source,
            Column::Backlog,
            "002",
            "child",
            "Child",
            "001",
            "[001]",
        );

        let dst_dir = tempfile::tempdir().unwrap();
        let dest = board_at(dst_dir.path());
        import(&dest, &source.root, false).unwrap();

        let child_text = crate::eol::read(dest.dir(Column::Backlog).join("002-child.md")).unwrap();
        assert!(child_text.contains("parent: \"001\""), "{child_text}");
        assert!(child_text.contains("related: [001]"), "{child_text}");
    }

    #[test]
    fn wikilinks_in_the_text_follow_the_new_numbers_in_one_pass() {
        let src_dir = tempfile::tempdir().unwrap();
        let source = board_at(src_dir.path());
        write_ticket(&source, Column::Backlog, "005", "alpha", "Alpha", "", "[]");
        write_ticket(&source, Column::Backlog, "006", "beta", "Beta", "", "[]");
        let beta = source.dir(Column::Backlog).join("006-beta.md");
        let mut text = fs::read_to_string(&beta).unwrap();
        text.push_str("see [[005-alpha]], bare [[005]], elsewhere [[099-gone]] and [[099]]\n");
        fs::write(&beta, text).unwrap();

        let dst_dir = tempfile::tempdir().unwrap();
        let dest = board_at(dst_dir.path());
        write_ticket(&dest, Column::Done, "001", "x", "Existing", "", "[]");
        write_ticket(&dest, Column::Done, "002", "y", "Existing 2", "", "[]");
        import(&dest, &source.root, false).unwrap();

        // alpha -> 003, beta -> 004.
        let text = crate::eol::read(dest.dir(Column::Backlog).join("004-beta.md")).unwrap();
        assert!(text.contains("see [[003-alpha]], bare [[003]],"), "{text}");
        assert!(text.contains("[[099-gone]] and [[099]]"), "{text}");
    }

    #[test]
    fn a_reference_outside_the_batch_is_left_untouched() {
        let src_dir = tempfile::tempdir().unwrap();
        let source = board_at(src_dir.path());
        write_ticket(
            &source,
            Column::Backlog,
            "001",
            "orphan",
            "Orphan",
            "099",
            "[099]",
        );

        let dst_dir = tempfile::tempdir().unwrap();
        let dest = board_at(dst_dir.path());
        import(&dest, &source.root, false).unwrap();

        let text = crate::eol::read(dest.dir(Column::Backlog).join("001-orphan.md")).unwrap();
        assert!(text.contains("parent: \"099\""), "{text}");
        assert!(text.contains("related: [099]"), "{text}");
    }

    #[test]
    fn same_titled_tickets_are_flagged_duplicate_within_the_batch() {
        let src_dir = tempfile::tempdir().unwrap();
        let source = board_at(src_dir.path());
        write_ticket(
            &source,
            Column::Backlog,
            "001",
            "one",
            "Same title",
            "",
            "[]",
        );
        write_ticket(
            &source,
            Column::Backlog,
            "002",
            "two",
            "same title",
            "",
            "[]",
        );
        write_ticket(
            &source,
            Column::Backlog,
            "003",
            "three",
            "Different",
            "",
            "[]",
        );

        let dst_dir = tempfile::tempdir().unwrap();
        let dest = board_at(dst_dir.path());
        let summary = import(&dest, &source.root, false).unwrap();

        assert_eq!(summary.duplicate_count(), 2, "{:?}", summary.imported);
        assert!(summary.imported[0].duplicate);
        assert!(summary.imported[1].duplicate);
        assert!(!summary.imported[2].duplicate);
        let text = crate::eol::read(dest.dir(Column::Backlog).join("001-one.md")).unwrap();
        assert!(text.contains("tags: [DUPLICATE]"), "{text}");
    }

    #[test]
    fn a_title_already_on_the_destination_board_is_left_out() {
        let src_dir = tempfile::tempdir().unwrap();
        let source = board_at(src_dir.path());
        write_ticket(
            &source,
            Column::Backlog,
            "001",
            "imported",
            "Shared title",
            "",
            "[]",
        );

        let dst_dir = tempfile::tempdir().unwrap();
        let dest = board_at(dst_dir.path());
        write_ticket(
            &dest,
            Column::Backlog,
            "001",
            "existing",
            "Shared title",
            "",
            "[]",
        );
        // The board's own counter must reflect that 001 is already spent.
        dest.record_id(1).unwrap();

        let summary = import(&dest, &source.root, false).unwrap();
        // Never a second copy (ticket 173): skipped, and said why.
        assert!(summary.imported.is_empty());
        assert_eq!(summary.skipped.len(), 1);
        assert!(
            summary.skipped[0].1.contains("already on this board"),
            "{:?}",
            summary.skipped
        );
        assert_eq!(dest.next_id().unwrap(), 2, "no number spent on it");
        // The pre-existing destination ticket must not have been touched.
        let existing_text =
            crate::eol::read(dest.dir(Column::Backlog).join("001-existing.md")).unwrap();
        assert!(!existing_text.contains("DUPLICATE"), "{existing_text}");
    }

    #[test]
    fn dry_run_writes_nothing_and_does_not_advance_the_counter() {
        let src_dir = tempfile::tempdir().unwrap();
        let source = board_at(src_dir.path());
        write_ticket(&source, Column::Backlog, "001", "one", "One", "", "[]");

        let dst_dir = tempfile::tempdir().unwrap();
        let dest = board_at(dst_dir.path());
        let before = dest.next_id().unwrap();

        let summary = import(&dest, &source.root, true).unwrap();
        assert_eq!(summary.imported.len(), 1);
        assert_eq!(summary.imported[0].new_id, "001");
        assert!(!dest.dir(Column::Backlog).join("001-one.md").exists());
        assert_eq!(dest.next_id().unwrap(), before);
    }

    #[test]
    fn the_source_tree_is_untouched_after_a_real_import() {
        let src_dir = tempfile::tempdir().unwrap();
        let source = board_at(src_dir.path());
        let source_path = write_ticket(&source, Column::Backlog, "001", "one", "One", "", "[]");
        let before = crate::eol::read(&source_path).unwrap();

        let dst_dir = tempfile::tempdir().unwrap();
        let dest = board_at(dst_dir.path());
        import(&dest, &source.root, false).unwrap();

        let after = crate::eol::read(&source_path).unwrap();
        assert_eq!(
            before, after,
            "the source ticket must be byte-for-byte unchanged"
        );
    }

    #[test]
    fn a_ticket_with_no_frontmatter_is_skipped_not_a_hard_failure() {
        // Real trees accumulate cruft — this mirrors an actual file found in
        // rite's own history: a consolidation that lost its frontmatter,
        // leaving only a body under a heading.
        let src_dir = tempfile::tempdir().unwrap();
        let source = board_at(src_dir.path());
        let broken = source.dir(Column::Done).join("017-broken.md");
        fs::write(&broken, "## Notes\n\nno frontmatter here at all\n").unwrap();
        write_ticket(
            &source,
            Column::Backlog,
            "001",
            "fine",
            "A fine ticket",
            "",
            "[]",
        );

        let dst_dir = tempfile::tempdir().unwrap();
        let dest = board_at(dst_dir.path());
        let summary = import(&dest, &source.root, false).unwrap();

        assert_eq!(summary.imported.len(), 1, "{:?}", summary.imported);
        assert_eq!(summary.imported[0].title, "A fine ticket");
        assert_eq!(summary.skipped.len(), 1);
        assert_eq!(summary.skipped[0].0, broken);
        assert!(summary.skipped[0].1.contains("frontmatter"));
    }

    #[test]
    fn clone_links_are_remapped_within_the_batch() {
        let map: HashMap<String, String> = [("001".to_string(), "011".to_string())].into();
        let text = "---\nid: \"002\"\nclone-of: \"001\"\nclones: [001,009]\n---\n\nbody\n";
        let out = remap_links(text, &map).unwrap();
        assert!(out.contains("clone-of: \"011\""), "{out}");
        assert!(out.contains("clones: [011,009]"), "{out}");
    }

    #[test]
    fn typed_links_are_remapped_within_the_batch() {
        let map: HashMap<String, String> = [("001".to_string(), "011".to_string())].into();
        let text = "---\nid: \"002\"\nlinks: [requires:001,supersedes:009]\n---\n\nbody\n";
        let out = remap_links(text, &map).unwrap();
        assert!(
            out.contains("links: [requires:011,supersedes:009]"),
            "{out}"
        );
    }
}
