//! Typed links between tickets (ticket 207): a ticket says how it stands to
//! another — it requires it, supersedes it — and the other
//! ticket says the opposite, so either end shows the link.
//!
//! Where a link is recorded depends on what it is. Four kinds already had a
//! home and keep it, so there is one record of each fact and `rite board`,
//! `rite duplicate` and the editor go on reading the same keys:
//!
//! - `RelatesTo` is `related` on both tickets,
//! - `ChildOf` is the child's `parent`, with `ParentOf` the parent's
//!   `## Children` wikilink,
//! - `CloneOf` is the copy's `clone-of`, with `ClonedBy` the original's
//!   `clones`.
//!
//! The rest are new and live in a `links` flow list, one `kind:id` entry
//! each (`links: [requires:012,supersedes:003]`). Ids rather than paths, like
//! `parent` and `related`, so a link survives a rename or a move between
//! columns.
//!
//! Only tickets on the same board can be linked: a bare id can't name a
//! ticket on another, the same limit `rite duplicate` has.
//!
//! **Done waits** (ticket 213): a ticket can't move to done while a ticket
//! it requires, or one of its children, is still on the board and not
//! done ([`waiting_on`], checked by `Board::move_task`). There is no
//! override: the way past it is to change the link ([`retype`], e.g. to
//! relates-to) or remove it. An abandoned or missing ticket holds nothing
//! back.

use std::fmt;

use anyhow::{Result, bail};

use crate::board::{Board, Column, Kind};
use crate::task::Task;

/// How one ticket stands to another, read from the ticket that records it
/// (`Requires` on ticket 12 means 12 requires the other ticket).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LinkType {
    Requires,
    RequiredBy,
    ParentOf,
    ChildOf,
    RelatesTo,
    CloneOf,
    ClonedBy,
    Supersedes,
    SupersededBy,
}

impl LinkType {
    /// Every kind, in the order a picker lists them.
    pub const ALL: [LinkType; 9] = [
        LinkType::RelatesTo,
        LinkType::Requires,
        LinkType::RequiredBy,
        LinkType::ParentOf,
        LinkType::ChildOf,
        LinkType::CloneOf,
        LinkType::ClonedBy,
        LinkType::Supersedes,
        LinkType::SupersededBy,
    ];

    /// The kebab-case name used on the command line and in `links`.
    pub fn slug(self) -> &'static str {
        match self {
            LinkType::Requires => "requires",
            LinkType::RequiredBy => "required-by",
            LinkType::ParentOf => "parent-of",
            LinkType::ChildOf => "child-of",
            LinkType::RelatesTo => "relates-to",
            LinkType::CloneOf => "clone-of",
            LinkType::ClonedBy => "cloned-by",
            LinkType::Supersedes => "supersedes",
            LinkType::SupersededBy => "superseded-by",
        }
    }

    /// The kind as the other end records it.
    pub fn inverse(self) -> LinkType {
        match self {
            LinkType::Requires => LinkType::RequiredBy,
            LinkType::RequiredBy => LinkType::Requires,
            LinkType::ParentOf => LinkType::ChildOf,
            LinkType::ChildOf => LinkType::ParentOf,
            LinkType::RelatesTo => LinkType::RelatesTo,
            LinkType::CloneOf => LinkType::ClonedBy,
            LinkType::ClonedBy => LinkType::CloneOf,
            LinkType::Supersedes => LinkType::SupersededBy,
            LinkType::SupersededBy => LinkType::Supersedes,
        }
    }

    /// A phrase that reads after a ticket's number: `012 requires 013`.
    pub fn phrase(self) -> &'static str {
        match self {
            LinkType::Requires => "requires",
            LinkType::RequiredBy => "is required by",
            LinkType::ParentOf => "is the parent of",
            LinkType::ChildOf => "is a child of",
            LinkType::RelatesTo => "relates to",
            LinkType::CloneOf => "is a clone of",
            LinkType::ClonedBy => "is cloned by",
            LinkType::Supersedes => "supersedes",
            LinkType::SupersededBy => "is superseded by",
        }
    }

    /// The name a person sees in a picker: `Blocked by`.
    pub fn label(self) -> &'static str {
        match self {
            LinkType::Requires => "Requires",
            LinkType::RequiredBy => "Required by",
            LinkType::ParentOf => "Parent of",
            LinkType::ChildOf => "Child of",
            LinkType::RelatesTo => "Relates to",
            LinkType::CloneOf => "Clone of",
            LinkType::ClonedBy => "Cloned by",
            LinkType::Supersedes => "Supersedes",
            LinkType::SupersededBy => "Superseded by",
        }
    }

    /// A kind from its slug, ignoring case and treating `_` and spaces as
    /// `-`, so `RequiredBy`-style spellings written as `required_by` still
    /// work. The names the first version wrote are still read:
    /// `depends-on` and `depended-on-by` are `requires` and `required-by`,
    /// and `blocked-by` and `blocks` say the same thing the other way round
    /// (what blocks a ticket is what it requires).
    pub fn parse(text: &str) -> Option<LinkType> {
        let wanted: String = text
            .trim()
            .chars()
            .map(|c| if c == '_' || c == ' ' { '-' } else { c })
            .collect::<String>()
            .to_lowercase();
        let wanted = match wanted.as_str() {
            "depends-on" | "blocked-by" => "requires",
            "depended-on-by" | "blocks" => "required-by",
            other => other,
        };
        LinkType::ALL.into_iter().find(|kind| kind.slug() == wanted)
    }
}

impl std::fmt::Display for LinkType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.slug())
    }
}

/// One entry of a ticket's `links`: a kind and the id of the other ticket.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Link {
    pub kind: LinkType,
    pub id: String,
}

impl Link {
    /// The entry as written in `links`: `requires:012`.
    pub fn entry(&self) -> String {
        format!("{}:{}", self.kind.slug(), self.id)
    }

    /// An entry of `links`; `None` if it isn't one.
    pub fn parse(entry: &str) -> Option<Link> {
        let (kind, id) = entry.split_once(':')?;
        let id = id.trim();
        if id.is_empty() {
            return None;
        }
        Some(Link {
            kind: LinkType::parse(kind)?,
            id: id.to_string(),
        })
    }
}

/// Refuse what would leave a link half made, before anything is written.
/// A ticket has one parent and is the copy of one original; naming a
/// second would silently replace the first.
fn preflight(from: &Task, kind: LinkType, to: &Task, one_way: bool) -> Result<()> {
    let (child, parent) = match kind {
        LinkType::ChildOf => (from, to),
        LinkType::ParentOf => (to, from),
        _ => (from, to),
    };
    let (copy, original) = match kind {
        LinkType::CloneOf => (from, to),
        LinkType::ClonedBy => (to, from),
        _ => (from, to),
    };
    // With `one_way`, a ParentOf records only the parent's list; the child's
    // `parent` isn't touched, so it can't clash.
    let sets_parent = match kind {
        LinkType::ChildOf => true,
        LinkType::ParentOf => !one_way,
        _ => false,
    };
    if sets_parent
        && let Some(existing) = child.parent()
        && existing != parent.id_display()
    {
        bail!(
            "{} already has parent {existing}; remove that first",
            child.id_display()
        );
    }
    let sets_clone_of = match kind {
        LinkType::CloneOf => true,
        LinkType::ClonedBy => !one_way,
        _ => false,
    };
    if sets_clone_of
        && let Some(existing) = copy.clone_of()
        && existing != original.id_display()
    {
        bail!(
            "{} is already a clone of {existing}; remove that first",
            copy.id_display()
        );
    }
    Ok(())
}

/// Whether `from` can be linked as `kind` to a ticket that doesn't exist
/// yet (ticket 372), checked before that ticket is made: a new ticket has no
/// parent and is no one's clone, so only `from`'s own end can refuse, as
/// [`apply`] would.
pub fn can_link_to_new(from: &Task, kind: LinkType) -> Result<()> {
    if kind == LinkType::ChildOf
        && let Some(existing) = from.parent()
    {
        bail!(
            "{} already has parent {existing}; remove that first",
            from.id_display()
        );
    }
    if kind == LinkType::CloneOf
        && let Some(existing) = from.clone_of()
    {
        bail!(
            "{} is already a clone of {existing}; remove that first",
            from.id_display()
        );
    }
    Ok(())
}

/// Record (or, with `remove`, take away) on `task` that it stands to `other`
/// as `kind`. Only `task`'s own file changes. Returns whether it did.
fn write_end(task: &Task, kind: LinkType, other: &Task, remove: bool) -> Result<bool> {
    let expected = task.hash()?;
    let other_id = other.id_display();
    match kind {
        LinkType::RelatesTo if remove => task.remove_related(expected, &other_id),
        LinkType::RelatesTo => task.add_related(expected, std::slice::from_ref(&other_id)),
        LinkType::ChildOf if remove => {
            if task.parent().as_deref() == Some(other_id.as_str()) {
                task.set_parent(expected, "")?;
                Ok(true)
            } else {
                Ok(false)
            }
        }
        LinkType::ChildOf => {
            if task.parent().as_deref() == Some(other_id.as_str()) {
                return Ok(false);
            }
            task.set_parent(expected, &other_id)?;
            Ok(true)
        }
        LinkType::ParentOf if remove => task.remove_child_link(expected, &other.stem),
        LinkType::ParentOf => task.add_child_link(expected, &other.stem),
        LinkType::CloneOf if remove => {
            if task.clone_of().as_deref() == Some(other_id.as_str()) {
                task.set_clone_of(expected, "")?;
                Ok(true)
            } else {
                Ok(false)
            }
        }
        LinkType::CloneOf => {
            if task.clone_of().as_deref() == Some(other_id.as_str()) {
                return Ok(false);
            }
            task.set_clone_of(expected, &other_id)?;
            Ok(true)
        }
        LinkType::ClonedBy if remove => task.remove_clone(expected, &other_id),
        LinkType::ClonedBy => task.add_clone(expected, &other_id),
        _ => {
            let link = Link { kind, id: other_id };
            if remove {
                task.remove_link(expected, &link)
            } else {
                task.add_link(expected, &link)
            }
        }
    }
}

/// Link `from` to `to` as `kind` — or, with `remove`, unlink them — and
/// record the opposite on `to` unless `one_way`. Returns a line for each end
/// that changed (`012 requires 013`), or one saying nothing did. Each file is
/// hash-checked on its own; the caller wraps the call in one auto-commit so
/// the two ends are one change in history.
pub fn apply(
    from: &Task,
    kind: LinkType,
    to: &Task,
    remove: bool,
    one_way: bool,
) -> Result<Vec<String>> {
    Ok(apply_counted(from, kind, to, remove, one_way)?.0)
}

/// [`apply`], also saying whether either end changed.
fn apply_counted(
    from: &Task,
    kind: LinkType,
    to: &Task,
    remove: bool,
    one_way: bool,
) -> Result<(Vec<String>, bool)> {
    if from.path == to.path {
        bail!("a ticket can't be linked to itself");
    }
    if !remove {
        preflight(from, kind, to, one_way)?;
    }
    let (from_id, to_id) = (from.id_display(), to.id_display());
    let mut lines = Vec::new();
    let verb = |kind: LinkType, remove: bool| {
        if remove {
            match kind.phrase().strip_prefix("is ") {
                Some(rest) => format!("is no longer {rest}"),
                None => format!("no longer {}", kind.phrase()),
            }
        } else {
            kind.phrase().to_string()
        }
    };
    if write_end(from, kind, to, remove)? {
        lines.push(format!("{from_id} {} {to_id}", verb(kind, remove)));
    }
    if !one_way {
        let inverse = kind.inverse();
        if write_end(to, inverse, from, remove)? {
            lines.push(format!("{to_id} {} {from_id}", verb(inverse, remove)));
        }
    }
    let changed = !lines.is_empty();
    if lines.is_empty() {
        lines.push(if remove {
            format!("{from_id} wasn't linked to {to_id} as {kind}")
        } else {
            format!("{from_id} already {} {to_id}", kind.phrase())
        });
    }
    Ok((lines, changed))
}

/// Change how `from` stands to `to`: the `old` link goes from both ends and
/// the `new` one is made, as one change (the caller commits it). Checked
/// first, so a refused `new` (a second parent, a second original) leaves
/// the old link as it was. This is the way past [`waiting_on`]: a requires
/// or child-of link changed to relates-to holds nothing back.
pub fn retype(from: &Task, old: LinkType, new: LinkType, to: &Task) -> Result<Vec<String>> {
    if old == new {
        bail!(
            "{} already {} {}",
            from.id_display(),
            new.phrase(),
            to.id_display()
        );
    }
    if from.path == to.path {
        bail!("a ticket can't be linked to itself");
    }
    // The old link must be there, read from either end, before anything
    // changes.
    if !holds(from, old, to) && !holds(to, old.inverse(), from) {
        bail!(
            "{} isn't linked to {} as {old}",
            from.id_display(),
            to.id_display()
        );
    }
    // A new parent or original is only refused if it isn't the one the old
    // link is about to free.
    let freed_parent = matches!(old, LinkType::ChildOf | LinkType::ParentOf);
    let freed_clone = matches!(old, LinkType::CloneOf | LinkType::ClonedBy);
    if !(freed_parent && matches!(new, LinkType::ChildOf | LinkType::ParentOf))
        && !(freed_clone && matches!(new, LinkType::CloneOf | LinkType::ClonedBy))
    {
        preflight(from, new, to, false)?;
    }
    let (mut lines, _) = apply_counted(from, old, to, true, false)?;
    let (added, _) = apply_counted(from, new, to, false, false)?;
    lines.extend(added);
    Ok(lines)
}

/// Whether `task` itself records that it stands to `other` as `kind`.
fn holds(task: &Task, kind: LinkType, other: &Task) -> bool {
    match kind {
        LinkType::ParentOf => children_stems(task).contains(&other.stem),
        _ => all(task)
            .iter()
            .any(|l| l.kind == kind && same_id(&l.id, other)),
    }
}

/// The `## Children` wikilinks a parent lists.
pub(crate) fn children_stems(task: &Task) -> Vec<String> {
    let Ok(text) = crate::eol::read(&task.path) else {
        return Vec::new();
    };
    let mut inside = false;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("## ") {
            inside = line == "## Children";
            continue;
        }
        if inside && let Some(stem) = line.strip_prefix("- [[").and_then(|l| l.strip_suffix("]]")) {
            out.push(stem.to_string());
        }
    }
    out
}

/// Whether `id` (as written in a link: `012`, `12`) names `task`.
pub(crate) fn same_id(id: &str, task: &Task) -> bool {
    task.id.is_some() && id.trim().parse::<u32>().ok() == task.id
}

/// Why a ticket has to wait for another before it can be done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    /// The ticket requires it.
    Requires,
    /// It is one of the ticket's children.
    Child,
}

/// A ticket that has to be done first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Waiting {
    pub id: String,
    pub title: String,
    pub column: Column,
    pub why: Wait,
}

/// The tickets on `board` that must be done before `task` can be (ticket
/// 213): those it requires (its own `requires`, or another ticket's
/// `required-by` naming it) and its children (their `parent`, or its
/// `## Children` list), that aren't in done. Abandoned and missing tickets
/// aren't on the board, so they hold nothing back.
pub fn waiting_on(board: &Board, task: &Task) -> Result<Vec<Waiting>> {
    let required: Vec<std::path::PathBuf> = crate::requires::required(board, task)?
        .into_iter()
        .map(|t| t.path)
        .collect();
    let children = children_stems(task);
    let mut out = Vec::new();
    for other in board.tasks()? {
        if other.path == task.path || other.column.is(Kind::Done) {
            continue;
        }
        let requires = required.contains(&other.path);
        let child = other
            .parent()
            .is_some_and(|p| task.id.is_some() && p.trim().parse::<u32>().ok() == task.id)
            || children.contains(&other.stem);
        let why = match (requires, child) {
            (true, _) => Wait::Requires,
            (false, true) => Wait::Child,
            _ => continue,
        };
        out.push(Waiting {
            id: other.id_display(),
            title: other.title(),
            column: other.column,
            why,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// A move to done refused because other tickets come first (ticket 213).
#[derive(Debug)]
pub struct NotReady {
    pub id: String,
    pub waiting: Vec<Waiting>,
}

impl fmt::Display for NotReady {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let list: Vec<String> = self
            .waiting
            .iter()
            .map(|w| {
                let why = match w.why {
                    Wait::Requires => "required",
                    Wait::Child => "a child",
                };
                format!("{} ({why}, in {})", w.id, w.column)
            })
            .collect();
        write!(
            f,
            "{} can't be done yet: {} {} done. Finish {} first, or change the link \
             (`rite link {} <ref> --replace <type> --type relates-to`) or remove it",
            self.id,
            list.join(", "),
            if list.len() == 1 { "isn't" } else { "aren't" },
            if list.len() == 1 { "it" } else { "them" },
            self.id
        )
    }
}

impl std::error::Error for NotReady {}

/// Every link a ticket has, whichever key records it: `links`, plus
/// `related`, `parent`, `clone-of` and `clones`. A parent's children aren't
/// here: they are found by scanning the board, and the page already does.
pub fn all(task: &Task) -> Vec<Link> {
    let mut out = task.links();
    out.extend(task.related().into_iter().map(|id| Link {
        kind: LinkType::RelatesTo,
        id,
    }));
    out.extend(task.parent().map(|id| Link {
        kind: LinkType::ChildOf,
        id,
    }));
    out.extend(task.clone_of().map(|id| Link {
        kind: LinkType::CloneOf,
        id,
    }));
    out.extend(task.clones().into_iter().map(|id| Link {
        kind: LinkType::ClonedBy,
        id,
    }));
    out.sort();
    out.dedup();
    out
}

/// A ticket linked to another, as [`linked`] finds it.
#[derive(Clone, Debug)]
pub struct Linked {
    /// How the asked-about ticket stands to this one: `Requires` means the
    /// asked-about ticket requires it.
    pub kind: LinkType,
    pub task: Task,
    /// Recorded on the asked-about ticket itself; false when only the other
    /// ticket records it (a one-sided link, from `--one-way`, a hand edit
    /// or an import).
    pub recorded_here: bool,
}

/// Every ticket on `board` linked to `task` in any way, in either direction
/// (ticket 121): its own `links`, `related`, `parent`, `## Children`,
/// `clone-of` and `clones`, and every other ticket's that names it, turned
/// round (another's `requires` naming it is `RequiredBy` here). So a link
/// recorded on only one side is found from either end. Each ticket and kind
/// once, in the picker's order of kinds, then by number. Tickets not on the
/// board (abandoned, missing) aren't listed.
pub fn linked(board: &Board, task: &Task) -> Result<Vec<Linked>> {
    let others: Vec<Task> = board
        .tasks()?
        .into_iter()
        .filter(|other| other.path != task.path)
        .collect();
    let mine = all(task);
    let my_children = children_stems(task);
    let mut out: Vec<Linked> = Vec::new();
    let mut add = |kind: LinkType, other: &Task, here: bool| match out
        .iter_mut()
        .find(|l| l.kind == kind && l.task.path == other.path)
    {
        Some(found) => found.recorded_here |= here,
        None => out.push(Linked {
            kind,
            task: other.clone(),
            recorded_here: here,
        }),
    };
    for other in &others {
        for link in mine.iter().filter(|l| same_id(&l.id, other)) {
            add(link.kind, other, true);
        }
        if my_children.contains(&other.stem) {
            add(LinkType::ParentOf, other, true);
        }
        for link in all(other).iter().filter(|l| same_id(&l.id, task)) {
            add(link.kind.inverse(), other, false);
        }
        if children_stems(other).contains(&task.stem) {
            add(LinkType::ChildOf, other, false);
        }
    }
    let order = |kind: LinkType| LinkType::ALL.iter().position(|k| *k == kind).unwrap_or(0);
    out.sort_by_key(|l| (order(l.kind), l.task.id, l.task.stem.clone()));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::Board;
    use crate::create::{Draft, NewTicket};
    use crate::templates::Templates;

    fn board_with(titles: &[&str]) -> (tempfile::TempDir, Board) {
        let dir = tempfile::tempdir().unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@t.co"],
            vec!["config", "user.name", "t"],
        ] {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .status()
                .unwrap();
        }
        let board = Board {
            root: dir.path().join("tasks"),
        };
        board.create().unwrap();
        let templates = Templates::load(&board).unwrap();
        for title in titles {
            let ticket = NewTicket {
                title: (*title).to_string(),
                template: crate::templates::DEFAULT_TEMPLATE.to_string(),
                branch_type: "feature".to_string(),
                ..NewTicket::default()
            };
            Draft::prepare(&board, &templates, ticket)
                .unwrap()
                .write(&board, None)
                .unwrap();
        }
        (dir, board)
    }

    fn find(board: &Board, id: &str) -> Task {
        board.find(id).unwrap()
    }

    #[test]
    fn linked_finds_every_link_from_either_end_one_sided_too() {
        let (_dir, board) = board_with(&["One", "Two", "Three", "Four", "Five"]);
        // Both ends: 001 requires 002.
        apply(
            &find(&board, "1"),
            LinkType::Requires,
            &find(&board, "2"),
            false,
            false,
        )
        .unwrap();
        // One-sided, recorded only on 003: 003 supersedes 001.
        apply(
            &find(&board, "3"),
            LinkType::Supersedes,
            &find(&board, "1"),
            false,
            true,
        )
        .unwrap();
        // 004's parent is 001 (recorded on 004 only), 005 relates to 001 one way.
        apply(
            &find(&board, "4"),
            LinkType::ChildOf,
            &find(&board, "1"),
            false,
            true,
        )
        .unwrap();
        apply(
            &find(&board, "5"),
            LinkType::RelatesTo,
            &find(&board, "1"),
            false,
            true,
        )
        .unwrap();
        let got: Vec<(LinkType, String, bool)> = linked(&board, &find(&board, "1"))
            .unwrap()
            .into_iter()
            .map(|l| (l.kind, l.task.id_display(), l.recorded_here))
            .collect();
        assert_eq!(
            got,
            vec![
                (LinkType::RelatesTo, "005".to_string(), false),
                (LinkType::Requires, "002".to_string(), true),
                (LinkType::ParentOf, "004".to_string(), false),
                (LinkType::SupersededBy, "003".to_string(), false),
            ]
        );
        // From the other end, 002 sees it's required by 001.
        let back = linked(&board, &find(&board, "2")).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(
            (back[0].kind, back[0].recorded_here),
            (LinkType::RequiredBy, true)
        );
    }

    #[test]
    fn every_kind_round_trips_through_its_slug_and_has_an_inverse_that_points_back() {
        for kind in LinkType::ALL {
            assert_eq!(LinkType::parse(kind.slug()), Some(kind));
            assert_eq!(kind.inverse().inverse(), kind);
        }
        assert_eq!(LinkType::parse("Required_By"), Some(LinkType::RequiredBy));
        assert_eq!(LinkType::parse("nope"), None);
        assert_eq!(LinkType::parse("depends-on"), Some(LinkType::Requires));
        assert_eq!(LinkType::parse("blocked-by"), Some(LinkType::Requires));
        assert_eq!(LinkType::parse("blocks"), Some(LinkType::RequiredBy));
        assert_eq!(
            Link::parse("blocks:012").map(|l| l.entry()).as_deref(),
            Some("required-by:012"),
            "an old entry reads, and is written back under its new name"
        );
        assert_eq!(LinkType::ALL.len(), 9);
    }

    #[test]
    fn an_entry_reads_back_as_written_and_a_bad_one_is_skipped() {
        let link = Link {
            kind: LinkType::Requires,
            id: "012".into(),
        };
        assert_eq!(link.entry(), "requires:012");
        assert_eq!(Link::parse("requires:012"), Some(link));
        assert_eq!(Link::parse("requires:"), None);
        assert_eq!(Link::parse("flies-to:012"), None);
        assert_eq!(Link::parse("012"), None);
    }

    #[test]
    fn a_requires_link_is_written_on_both_tickets() {
        let (_dir, board) = board_with(&["First", "Second"]);
        let lines = apply(
            &find(&board, "001"),
            LinkType::Requires,
            &find(&board, "002"),
            false,
            false,
        )
        .unwrap();
        assert_eq!(lines, ["001 requires 002", "002 is required by 001"]);
        assert_eq!(
            find(&board, "001").links(),
            [Link {
                kind: LinkType::Requires,
                id: "002".into()
            }]
        );
        assert_eq!(
            find(&board, "002").links(),
            [Link {
                kind: LinkType::RequiredBy,
                id: "001".into()
            }]
        );
    }

    #[test]
    fn linking_again_changes_nothing_and_says_so() {
        let (_dir, board) = board_with(&["First", "Second"]);
        let (a, b) = (find(&board, "001"), find(&board, "002"));
        apply(&a, LinkType::Requires, &b, false, false).unwrap();
        let lines = apply(&a, LinkType::Requires, &b, false, false).unwrap();
        assert_eq!(lines, ["001 already requires 002"]);
        assert_eq!(a.links().len(), 1);
    }

    #[test]
    fn one_ticket_can_be_linked_to_several_others_in_several_ways() {
        let (_dir, board) = board_with(&["A", "B", "C"]);
        let a = find(&board, "001");
        apply(&a, LinkType::Requires, &find(&board, "002"), false, false).unwrap();
        apply(&a, LinkType::Requires, &find(&board, "003"), false, false).unwrap();
        apply(&a, LinkType::Supersedes, &find(&board, "003"), false, false).unwrap();
        let links = find(&board, "001").links();
        assert_eq!(links.len(), 3, "{links:?}");
        assert_eq!(find(&board, "003").links().len(), 2);
    }

    #[test]
    fn the_older_fields_keep_their_kinds() {
        let (_dir, board) = board_with(&["Parent", "Child", "Other", "Copy"]);
        let t = |id: &str| find(&board, id);
        apply(&t("002"), LinkType::ChildOf, &t("001"), false, false).unwrap();
        assert_eq!(t("002").parent().as_deref(), Some("001"));
        let parent = crate::eol::read(&t("001").path).unwrap();
        assert!(parent.contains("- [[002-child]]"), "{parent}");

        apply(&t("001"), LinkType::RelatesTo, &t("003"), false, false).unwrap();
        assert_eq!(t("001").related(), ["003"]);
        assert_eq!(t("003").related(), ["001"]);

        apply(&t("004"), LinkType::CloneOf, &t("003"), false, false).unwrap();
        assert_eq!(t("004").clone_of().as_deref(), Some("003"));
        assert_eq!(t("003").clones(), ["004"]);
        // Nothing went into the new key.
        assert!(t("004").links().is_empty());
        assert_eq!(all(&t("004")).len(), 1);
    }

    #[test]
    fn parent_of_is_the_same_link_seen_from_the_parent() {
        let (_dir, board) = board_with(&["Parent", "Child"]);
        apply(
            &find(&board, "001"),
            LinkType::ParentOf,
            &find(&board, "002"),
            false,
            false,
        )
        .unwrap();
        assert_eq!(find(&board, "002").parent().as_deref(), Some("001"));
        let parent = crate::eol::read(&find(&board, "001").path).unwrap();
        assert!(parent.contains("- [[002-child]]"), "{parent}");
    }

    #[test]
    fn a_second_parent_is_refused_before_anything_is_written() {
        let (_dir, board) = board_with(&["One", "Two", "Child"]);
        let t = |id: &str| find(&board, id);
        apply(&t("003"), LinkType::ChildOf, &t("001"), false, false).unwrap();
        let before = crate::eol::read(&t("002").path).unwrap();
        let err = apply(&t("002"), LinkType::ParentOf, &t("003"), false, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("already has parent 001"), "{err}");
        assert_eq!(crate::eol::read(&t("002").path).unwrap(), before);
        assert_eq!(t("003").parent().as_deref(), Some("001"));
    }

    #[test]
    fn a_ticket_cannot_be_the_copy_of_two_originals() {
        let (_dir, board) = board_with(&["One", "Two", "Copy"]);
        let t = |id: &str| find(&board, id);
        apply(&t("003"), LinkType::CloneOf, &t("001"), false, false).unwrap();
        let err = apply(&t("003"), LinkType::CloneOf, &t("002"), false, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("already a clone of 001"), "{err}");
    }

    #[test]
    fn removing_clears_both_ends_whatever_key_holds_the_link() {
        let (_dir, board) = board_with(&["A", "B"]);
        let t = |id: &str| find(&board, id);
        for kind in [
            LinkType::Requires,
            LinkType::RelatesTo,
            LinkType::ChildOf,
            LinkType::CloneOf,
        ] {
            apply(&t("001"), kind, &t("002"), false, false).unwrap();
            let lines = apply(&t("001"), kind, &t("002"), true, false).unwrap();
            assert_eq!(lines.len(), 2, "{kind}: {lines:?}");
            assert!(all(&t("001")).is_empty(), "{kind}");
            assert!(all(&t("002")).is_empty(), "{kind}");
        }
        let a = crate::eol::read(&t("001").path).unwrap();
        assert!(!a.contains("[["), "{a}");
    }

    #[test]
    fn removing_a_link_that_is_not_there_says_so() {
        let (_dir, board) = board_with(&["A", "B"]);
        let lines = apply(
            &find(&board, "001"),
            LinkType::Requires,
            &find(&board, "002"),
            true,
            false,
        )
        .unwrap();
        assert_eq!(lines, ["001 wasn't linked to 002 as requires"]);
    }

    #[test]
    fn one_way_leaves_the_other_ticket_alone() {
        let (_dir, board) = board_with(&["A", "B"]);
        apply(
            &find(&board, "001"),
            LinkType::Supersedes,
            &find(&board, "002"),
            false,
            true,
        )
        .unwrap();
        assert_eq!(find(&board, "001").links().len(), 1);
        assert!(find(&board, "002").links().is_empty());
    }

    #[test]
    fn a_ticket_cannot_link_to_itself() {
        let (_dir, board) = board_with(&["A"]);
        let a = find(&board, "001");
        assert!(apply(&a, LinkType::Requires, &a, false, false).is_err());
    }

    /// Walks a ticket along to `to`, one column at a time.
    fn walk(board: &Board, id: &str, to: &[Column]) {
        for column in to {
            board.move_task(&find(board, id), *column, true).unwrap();
        }
    }

    #[test]
    fn done_waits_for_what_a_ticket_requires_and_for_its_children() {
        let (_dir, board) = board_with(&["Feature", "Dependency", "Child", "Spare"]);
        apply(
            &find(&board, "001"),
            LinkType::Requires,
            &find(&board, "002"),
            false,
            false,
        )
        .unwrap();
        apply(
            &find(&board, "003"),
            LinkType::ChildOf,
            &find(&board, "001"),
            false,
            false,
        )
        .unwrap();
        walk(&board, "001", &[Column::Todo, Column::Doing]);

        let waits = waiting_on(&board, &find(&board, "001")).unwrap();
        let ids: Vec<(&str, Wait)> = waits.iter().map(|w| (w.id.as_str(), w.why)).collect();
        assert_eq!(ids, [("002", Wait::Requires), ("003", Wait::Child)]);
        // Refused, even with `force` (that's only the WIP limit).
        let err = board
            .move_task(&find(&board, "001"), Column::Done, true)
            .unwrap_err();
        let text = err.to_string();
        assert!(err.downcast_ref::<NotReady>().is_some());
        assert!(text.contains("002 (required, in backlog)"), "{text}");
        assert!(text.contains("003 (a child, in backlog)"), "{text}");
        // Other moves aren't held back.
        assert!(waiting_on(&board, &find(&board, "004")).unwrap().is_empty());

        walk(&board, "002", &[Column::Todo, Column::Doing, Column::Done]);
        walk(&board, "003", &[Column::Todo, Column::Doing, Column::Done]);
        board
            .move_task(&find(&board, "001"), Column::Done, false)
            .unwrap();
    }

    #[test]
    fn a_required_by_link_on_the_other_ticket_counts_and_an_abandoned_ticket_does_not() {
        let (_dir, board) = board_with(&["Feature", "Dependency", "Dropped"]);
        // Recorded only on 002: it is required by 001.
        apply(
            &find(&board, "002"),
            LinkType::RequiredBy,
            &find(&board, "001"),
            false,
            true,
        )
        .unwrap();
        apply(
            &find(&board, "001"),
            LinkType::Requires,
            &find(&board, "003"),
            false,
            false,
        )
        .unwrap();
        let ids = |b: &Board| -> Vec<String> {
            waiting_on(b, &find(b, "001"))
                .unwrap()
                .into_iter()
                .map(|w| w.id)
                .collect()
        };
        assert_eq!(ids(&board), ["002", "003"]);
        board.archive(&find(&board, "003")).unwrap();
        assert_eq!(
            ids(&board),
            ["002"],
            "abandoned: it won't be done, so it can't hold 001 back"
        );
    }

    #[test]
    fn a_link_changes_type_on_both_tickets_and_then_holds_nothing_back() {
        let (_dir, board) = board_with(&["Feature", "Dependency"]);
        let (a, b) = (find(&board, "001"), find(&board, "002"));
        apply(&a, LinkType::Requires, &b, false, false).unwrap();
        let lines = retype(&a, LinkType::Requires, LinkType::RelatesTo, &b).unwrap();
        assert!(
            lines.contains(&"001 no longer requires 002".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"002 is no longer required by 001".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"001 relates to 002".to_string()),
            "{lines:?}"
        );
        let (a, b) = (find(&board, "001"), find(&board, "002"));
        assert!(a.links().is_empty() && b.links().is_empty());
        assert_eq!(a.related(), ["002"]);
        assert_eq!(b.related(), ["001"]);
        assert!(waiting_on(&board, &a).unwrap().is_empty());

        // Read from the other end too: 002 is required by nothing now, and
        // a link that isn't there can't be changed.
        let err = retype(&a, LinkType::Requires, LinkType::Supersedes, &b).unwrap_err();
        assert!(
            err.to_string().contains("isn't linked to 002 as requires"),
            "{err}"
        );
        assert!(retype(&a, LinkType::RelatesTo, LinkType::RelatesTo, &b).is_err());
    }

    #[test]
    fn a_child_can_be_turned_into_a_related_ticket() {
        let (_dir, board) = board_with(&["Parent", "Child"]);
        apply(
            &find(&board, "002"),
            LinkType::ChildOf,
            &find(&board, "001"),
            false,
            false,
        )
        .unwrap();
        // Changed from the parent's end: it is the parent of 002.
        retype(
            &find(&board, "001"),
            LinkType::ParentOf,
            LinkType::RelatesTo,
            &find(&board, "002"),
        )
        .unwrap();
        assert_eq!(find(&board, "002").parent(), None);
        assert_eq!(find(&board, "001").related(), ["002"]);
        assert!(waiting_on(&board, &find(&board, "001")).unwrap().is_empty());
    }
}
