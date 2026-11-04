//! What a ticket requires (ticket 214), read one way for every rule that
//! needs it: done waits for it (ticket 213, `links::waiting_on`), a sprint
//! can't start without it, and the tree draws it.
//!
//! A requirement is the typed `requires` link (ticket 207), recorded on
//! both tickets: `requires:016` on the one that needs it, `required-by:012`
//! on the other. Either record counts, so a link written one way only
//! (`rite link --one-way`) is still read. Parent and child are not
//! requirements here; ticket 213 makes a parent wait for its children
//! before done, separately.

use std::fmt;

use anyhow::{Result, bail};

use crate::board::{Board, Column, Kind as ColumnKind};
use crate::group::Kind;
use crate::links::{LinkType, same_id};
use crate::task::Task;

/// The tickets on `board` that `task` requires: its own `requires` links,
/// and other tickets' `required-by` naming it. Abandoned or missing
/// tickets aren't on the board, so they don't appear. Sorted by number.
pub fn required(board: &Board, task: &Task) -> Result<Vec<Task>> {
    let mine = task.links();
    let mut out: Vec<Task> = board
        .tasks()?
        .into_iter()
        .filter(|other| other.path != task.path)
        .filter(|other| {
            mine.iter()
                .any(|l| l.kind == LinkType::Requires && same_id(&l.id, other))
                || other
                    .links()
                    .iter()
                    .any(|l| l.kind == LinkType::RequiredBy && same_id(&l.id, task))
        })
        .collect();
    out.sort_by_key(|t| (t.id, t.stem.clone()));
    Ok(out)
}

/// Where a ticket is, in words: `backlog`, `backlog › epic checkout`,
/// `sprint s13`, `todo`, `done`.
pub fn place(board: &Board, task: &Task) -> String {
    match (&task.epic, task.column) {
        (Some(group), Column::Backlog) => match board.group(group) {
            Ok(g) if g.kind == Kind::Sprint => format!("sprint {group}"),
            _ => format!("backlog › epic {group}"),
        },
        (_, column) => column.to_string(),
    }
}

/// Whether `task` belongs to the epic or sprint `name`: by its folder in
/// the backlog, by its `epic` label once it has moved on (as
/// `Board::tasks_in_group` reads it).
fn in_group(task: &Task, name: &str) -> bool {
    match task.column {
        Column::Backlog => task.epic.as_deref() == Some(name),
        _ => task.epic_meta().as_deref() == Some(name),
    }
}

/// A ticket in a sprint that requires one outside it, not done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gap {
    /// The sprint's ticket.
    pub id: String,
    pub title: String,
    /// What it requires.
    pub required_id: String,
    pub required_title: String,
    /// Where the required ticket is ([`place`]).
    pub required_place: String,
}

/// Every ticket in sprint `name` that requires a ticket neither in the
/// sprint nor done (ticket 214). Empty means the sprint can start.
pub fn sprint_gaps(board: &Board, name: &str) -> Result<Vec<Gap>> {
    let mut gaps = Vec::new();
    for task in board.tasks_in_group(name)? {
        for needed in required(board, &task)? {
            if needed.column.is(ColumnKind::Done) || in_group(&needed, name) {
                continue;
            }
            gaps.push(Gap {
                id: task.id_display(),
                title: task.title(),
                required_id: needed.id_display(),
                required_title: needed.title(),
                required_place: place(board, &needed),
            });
        }
    }
    gaps.sort_by(|a, b| (&a.id, &a.required_id).cmp(&(&b.id, &b.required_id)));
    Ok(gaps)
}

/// A sprint start refused because its tickets require others outside it.
#[derive(Debug)]
pub struct SprintNotReady {
    pub sprint: String,
    pub gaps: Vec<Gap>,
}

impl fmt::Display for SprintNotReady {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let list: Vec<String> = self
            .gaps
            .iter()
            .map(|g| format!("{} requires {} ({})", g.id, g.required_id, g.required_place))
            .collect();
        write!(
            f,
            "{} can't start: {}. Put {} in {}, finish {}, or change the link",
            self.sprint,
            list.join(", "),
            if self.gaps.len() == 1 { "it" } else { "them" },
            self.sprint,
            if self.gaps.len() == 1 { "it" } else { "them" },
        )
    }
}

impl std::error::Error for SprintNotReady {}

/// Start sprint `name` (`rite group start`, riteui's Start sprint), unless
/// one of its tickets requires a ticket outside it that isn't done.
pub fn start_sprint(board: &Board, name: &str) -> Result<()> {
    let mut group = board.group(name)?;
    if !group.is_sprint() {
        bail!("{name} is an epic, not a sprint");
    }
    let gaps = sprint_gaps(board, name)?;
    if !gaps.is_empty() {
        return Err(SprintNotReady {
            sprint: name.to_string(),
            gaps,
        }
        .into());
    }
    group.start()
}

/// How a node hangs from its parent in the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Edge {
    /// A family's top: nothing in it requires this one.
    Root,
    /// The parent requires it: below the ticket asked about, or anywhere in
    /// a board's trees.
    Requires,
    /// It requires the parent: above the ticket asked about, what waits on
    /// it (ticket 373).
    WaitsOn,
    /// The parent's child, in the hierarchy (ticket 261, `hierarchy`).
    Child,
}

/// One ticket in the tree (ticket 214).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TreeNode {
    pub id: String,
    pub title: String,
    pub file: String,
    /// The column's directory name: `backlog`, `todo`, `doing`, `done`.
    pub column: String,
    /// The backlog folder (an epic or sprint) it's filed in, if any.
    pub epic: Option<String>,
    /// Where it is, in words ([`place`]).
    pub place: String,
    pub done: bool,
    pub edge: Edge,
    /// What it requires that isn't done yet.
    pub unmet: Vec<String>,
    /// In a started sprint, which the tree leaves out: its name. Not
    /// expanded.
    pub stub: Option<String>,
    /// Already drawn above; not expanded again.
    pub seen: bool,
    /// The ticket the tree was asked about.
    pub current: bool,
    pub children: Vec<TreeNode>,
}

/// What the tree knows about each ticket, read once.
struct Facts {
    tasks: Vec<Task>,
    /// For each ticket, the indices of those it requires.
    requires: Vec<Vec<usize>>,
    /// The started sprint each ticket is in, if any.
    started: Vec<Option<String>>,
    places: Vec<String>,
}

impl Facts {
    fn read(board: &Board) -> Result<Facts> {
        let tasks = board.tasks()?;
        let index = |id: &str| tasks.iter().position(|t| same_id(id, t));
        let mut requires = vec![Vec::new(); tasks.len()];
        for (i, task) in tasks.iter().enumerate() {
            for link in task.links() {
                let Some(j) = index(&link.id) else { continue };
                if i == j {
                    continue;
                }
                match link.kind {
                    LinkType::Requires => requires[i].push(j),
                    LinkType::RequiredBy => requires[j].push(i),
                    _ => {}
                }
            }
        }
        for list in &mut requires {
            list.sort_by_key(|&j| (tasks[j].id, tasks[j].stem.clone()));
            list.dedup();
        }
        let running: Vec<String> = board
            .groups()?
            .into_iter()
            .filter(|g| g.is_sprint() && !g.accepts_changes())
            .map(|g| g.name)
            .collect();
        let started = tasks
            .iter()
            .map(|t| running.iter().find(|s| in_group(t, s)).cloned())
            .collect();
        let places = tasks.iter().map(|t| place(board, t)).collect();
        Ok(Facts {
            tasks,
            requires,
            started,
            places,
        })
    }

    /// Leaves done tickets out (ticket 217): their links are dropped, so
    /// they join nothing and appear nowhere. `keep` (the ticket asked
    /// about) stays, done or not.
    fn hide_done(&mut self, keep: Option<usize>) {
        let gone: Vec<bool> = (0..self.tasks.len())
            .map(|i| self.tasks[i].column.is(ColumnKind::Done) && keep != Some(i))
            .collect();
        for i in 0..self.tasks.len() {
            if gone[i] {
                self.requires[i].clear();
            } else {
                self.requires[i].retain(|&j| !gone[j]);
            }
        }
    }

    fn linked(&self, i: usize) -> bool {
        !self.requires[i].is_empty() || self.waited_on(i)
    }

    /// Whether anything requires `i`.
    fn waited_on(&self, i: usize) -> bool {
        self.requires.iter().any(|r| r.contains(&i))
    }

    /// What requires `i`: those that others wait on in turn first, so the
    /// longest chain is drawn out and the rest point back to it, then by
    /// number.
    fn waiting(&self, i: usize) -> Vec<usize> {
        let mut out: Vec<usize> = (0..self.tasks.len())
            .filter(|&j| self.requires[j].contains(&i))
            .collect();
        out.sort_by_key(|&j| {
            (
                !self.waited_on(j),
                self.tasks[j].id,
                self.tasks[j].stem.clone(),
            )
        });
        out
    }

    /// Every ticket joined to `start` by requires, either way, not passing
    /// through a started sprint's tickets (they are stubs).
    fn component(&self, start: usize) -> Vec<usize> {
        let mut seen = vec![start];
        let mut todo = vec![start];
        while let Some(i) = todo.pop() {
            if self.started[i].is_some() {
                continue;
            }
            let near = self.requires[i]
                .iter()
                .copied()
                .chain((0..self.tasks.len()).filter(|&j| self.requires[j].contains(&i)));
            for j in near.collect::<Vec<_>>() {
                if !seen.contains(&j) {
                    seen.push(j);
                    todo.push(j);
                }
            }
        }
        seen.sort_by_key(|&j| (self.tasks[j].id, self.tasks[j].stem.clone()));
        seen
    }

    fn node(&self, i: usize, edge: Edge, current: Option<usize>) -> TreeNode {
        let task = &self.tasks[i];
        TreeNode {
            id: task.id_display(),
            title: task.title(),
            file: task.file_name(),
            column: task.column.as_str().to_string(),
            epic: task.epic.clone(),
            place: self.places[i].clone(),
            done: task.column.is(ColumnKind::Done),
            edge,
            unmet: self.requires[i]
                .iter()
                .filter(|&&j| !self.tasks[j].column.is(ColumnKind::Done))
                .map(|&j| self.tasks[j].id_display())
                .collect(),
            stub: self.started[i].clone(),
            seen: false,
            current: current == Some(i),
            children: Vec::new(),
        }
    }

    fn grow(
        &self,
        i: usize,
        edge: Edge,
        drawn: &mut Vec<usize>,
        current: Option<usize>,
    ) -> TreeNode {
        let mut node = self.node(i, edge, current);
        drawn.push(i);
        if node.stub.is_some() {
            return node;
        }
        for &j in &self.requires[i] {
            if drawn.contains(&j) {
                node.children.push(self.pointer(j, Edge::Requires));
            } else {
                node.children
                    .push(self.grow(j, Edge::Requires, drawn, current));
            }
        }
        node
    }

    /// Only a pointer to where `i` is drawn: no marks of its own.
    fn pointer(&self, i: usize, edge: Edge) -> TreeNode {
        let mut back = self.node(i, edge, None);
        back.seen = true;
        back.unmet.clear();
        back
    }

    /// `i` and, under it, what waits on it, up the chains (ticket 373).
    fn grow_up(&self, i: usize, drawn: &mut Vec<usize>, current: Option<usize>) -> TreeNode {
        let mut node = self.node(i, Edge::WaitsOn, current);
        drawn.push(i);
        if node.stub.is_some() {
            return node;
        }
        for j in self.waiting(i) {
            if drawn.contains(&j) {
                node.children.push(self.pointer(j, Edge::WaitsOn));
            } else {
                node.children.push(self.grow_up(j, drawn, current));
            }
        }
        node
    }

    /// The tree around `i` (ticket 373): `i` as the root, its children
    /// first what waits on it ([`Edge::WaitsOn`], each with what waits on
    /// that), then what it requires ([`Edge::Requires`], down the chains).
    /// Nothing else those tickets link to.
    fn around(&self, i: usize) -> TreeNode {
        let mut drawn = vec![i];
        let mut root = self.node(i, Edge::Root, Some(i));
        for j in self.waiting(i) {
            if drawn.contains(&j) {
                root.children.push(self.pointer(j, Edge::WaitsOn));
            } else {
                root.children.push(self.grow_up(j, &mut drawn, Some(i)));
            }
        }
        for &j in &self.requires[i] {
            if drawn.contains(&j) {
                root.children.push(self.pointer(j, Edge::Requires));
            } else {
                root.children
                    .push(self.grow(j, Edge::Requires, &mut drawn, Some(i)));
            }
        }
        root
    }

    /// The trees of one family: its roots (nothing in it requires them; a
    /// family that is only a cycle starts at its lowest number), and then
    /// anything not reached from them.
    fn trees(&self, family: &[usize]) -> Vec<TreeNode> {
        let mut drawn = Vec::new();
        let mut out = Vec::new();
        // Tops that require something first.
        let mut roots: Vec<usize> = family
            .iter()
            .copied()
            .filter(|&i| {
                self.started[i].is_none() && !family.iter().any(|&j| self.requires[j].contains(&i))
            })
            .collect();
        roots.sort_by_key(|&i| self.requires[i].is_empty());
        for i in roots {
            if !drawn.contains(&i) {
                out.push(self.grow(i, Edge::Root, &mut drawn, None));
            }
        }
        while let Some(&i) = family.iter().find(|i| !drawn.contains(i)) {
            out.push(self.grow(i, Edge::Root, &mut drawn, None));
        }
        out
    }
}

/// The requires tree (ticket 214), one board. With `around`, one tree
/// centred on that ticket (ticket 373, [`Facts::around`]): what can't be
/// done until it is, and what it requires, and nothing else. Empty when
/// it has neither. Without, every family on the board joined by requires,
/// lowest number first. Tickets in a started sprint aren't expanded: they
/// appear as stubs where something links to them. Each ticket is drawn
/// once; meeting it again is a `seen` back-reference. Related links,
/// parent and child aren't drawn. Done tickets are left out unless
/// `include_done` (ticket 217); the ticket asked about is always there.
pub fn family(board: &Board, around: Option<&Task>, include_done: bool) -> Result<Vec<TreeNode>> {
    let mut facts = Facts::read(board)?;
    let asked = match around {
        Some(task) => match facts.tasks.iter().position(|t| t.path == task.path) {
            Some(i) => Some(i),
            None => bail!("{} isn't on this board's columns", task.id_display()),
        },
        None => None,
    };
    if !include_done {
        facts.hide_done(asked);
    }
    match (around, asked) {
        (Some(task), Some(i)) => {
            if let Some(sprint) = &facts.started[i] {
                bail!(
                    "{} is in started sprint {sprint}, which the tree leaves out",
                    task.id_display()
                );
            }
            let tree = facts.around(i);
            if tree.children.is_empty() {
                return Ok(Vec::new());
            }
            Ok(vec![tree])
        }
        _ => {
            let mut done: Vec<usize> = Vec::new();
            let mut out = Vec::new();
            let mut order: Vec<usize> = (0..facts.tasks.len()).collect();
            order.sort_by_key(|&j| (facts.tasks[j].id, facts.tasks[j].stem.clone()));
            for i in order {
                if done.contains(&i) || facts.started[i].is_some() || !facts.linked(i) {
                    continue;
                }
                let family = facts.component(i);
                done.extend(&family);
                out.extend(facts.trees(&family));
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create::{Draft, NewTicket};
    use crate::group::{DEFAULT_DAYS, Group, State};
    use crate::links::apply;
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

    fn requires(board: &Board, from: &str, to: &str) {
        apply(
            &find(board, from),
            LinkType::Requires,
            &find(board, to),
            false,
            false,
        )
        .unwrap();
    }

    fn sprint(board: &Board, name: &str) {
        board
            .create_group(&Group {
                dir: board.dir(Column::Backlog).join(name),
                name: name.to_string(),
                kind: Kind::Sprint,
                description: String::new(),
                days: DEFAULT_DAYS,
                state: State::Pending,
            })
            .unwrap();
    }

    /// Sizes a ticket (a sprint takes only sized ones) and files it.
    fn file(board: &Board, id: &str, sprint: &str) {
        let t = find(board, id);
        t.set_points(t.hash().unwrap(), "3").unwrap();
        board.set_epic(&find(board, id), Some(sprint)).unwrap();
    }

    fn walk(board: &Board, id: &str, to: &[Column]) {
        for column in to {
            board.move_task(&find(board, id), *column, true).unwrap();
        }
    }

    fn ids(tasks: Vec<Task>) -> Vec<String> {
        tasks.iter().map(Task::id_display).collect()
    }

    #[test]
    fn what_a_ticket_requires_is_read_from_either_end() {
        let (_dir, board) = board_with(&["A", "B", "C"]);
        requires(&board, "001", "002");
        // Only on 003: it is required by 001.
        apply(
            &find(&board, "003"),
            LinkType::RequiredBy,
            &find(&board, "001"),
            false,
            true,
        )
        .unwrap();
        assert_eq!(
            ids(required(&board, &find(&board, "001")).unwrap()),
            ["002", "003"]
        );
        assert!(required(&board, &find(&board, "002")).unwrap().is_empty());
    }

    #[test]
    fn a_sprint_starts_only_when_what_its_tickets_require_is_in_it_or_done() {
        let (_dir, board) = board_with(&[
            "In",
            "Inside too",
            "Loose",
            "Elsewhere",
            "Doing",
            "Finished",
            "Dropped",
        ]);
        sprint(&board, "s12");
        sprint(&board, "s13");
        file(&board, "001", "s12");
        file(&board, "002", "s12");
        file(&board, "004", "s13");
        walk(&board, "005", &[Column::Todo]);
        walk(&board, "006", &[Column::Todo, Column::Doing, Column::Done]);
        for needed in ["002", "003", "004", "005", "006", "007"] {
            requires(&board, "001", needed);
        }
        board.archive(&find(&board, "007")).unwrap();

        let gaps: Vec<(String, String)> = sprint_gaps(&board, "s12")
            .unwrap()
            .into_iter()
            .map(|g| (g.required_id, g.required_place))
            .collect();
        assert_eq!(
            gaps,
            [
                ("003".to_string(), "backlog".to_string()),
                ("004".to_string(), "sprint s13".to_string()),
                ("005".to_string(), "todo".to_string()),
            ],
            "in the sprint, done and abandoned are fine"
        );

        let err = start_sprint(&board, "s12").unwrap_err();
        assert!(err.downcast_ref::<SprintNotReady>().is_some());
        let text = err.to_string();
        assert!(
            text.starts_with("s12 can't start: 001 requires 003 (backlog)"),
            "{text}"
        );
        assert_eq!(board.group("s12").unwrap().state, State::Pending);

        // Filed in the sprint or finished, and the sprint starts.
        file(&board, "003", "s12");
        walk(&board, "005", &[Column::Doing, Column::Done]);
        walk(&board, "004", &[Column::Todo, Column::Doing, Column::Done]);
        start_sprint(&board, "s12").unwrap();
        assert_eq!(board.group("s12").unwrap().state, State::Started);
        // An epic isn't something to start.
        assert!(start_sprint(&board, "nope").is_err());
    }

    #[test]
    fn set_requires_makes_the_list_exactly_what_is_given() {
        let (_dir, board) = board_with(&["A", "B", "C", "D"]);
        requires(&board, "001", "002");
        requires(&board, "001", "003");
        let wanted = [find(&board, "003"), find(&board, "004")];
        crate::transition::set_requires(&board, &find(&board, "001"), &wanted).unwrap();
        assert_eq!(
            ids(required(&board, &find(&board, "001")).unwrap()),
            ["003", "004"]
        );
        assert!(
            find(&board, "002").links().is_empty(),
            "the other end is cleared too"
        );
        assert_eq!(find(&board, "004").links()[0].entry(), "required-by:001");
        crate::transition::set_requires(&board, &find(&board, "001"), &[]).unwrap();
        assert!(required(&board, &find(&board, "001")).unwrap().is_empty());
    }

    fn shape(node: &TreeNode, depth: usize, out: &mut Vec<String>) {
        let edge = match node.edge {
            Edge::Root => "root",
            Edge::Requires => "requires",
            Edge::WaitsOn => "waits-on",
            Edge::Child => "child",
        };
        let mut line = format!("{}{} {}", "  ".repeat(depth), edge, node.id);
        if node.seen {
            line.push_str(" seen");
        }
        if let Some(s) = &node.stub {
            line.push_str(&format!(" stub:{s}"));
        }
        if !node.unmet.is_empty() {
            line.push_str(&format!(" unmet:{}", node.unmet.join(",")));
        }
        if node.current {
            line.push_str(" *");
        }
        out.push(line);
        for child in &node.children {
            shape(child, depth + 1, out);
        }
    }

    fn drawn(trees: &[TreeNode]) -> Vec<String> {
        let mut out = Vec::new();
        for tree in trees {
            shape(tree, 0, &mut out);
        }
        out
    }

    #[test]
    fn the_tree_draws_requires_each_ticket_once_and_no_related() {
        let (_dir, board) = board_with(&["Top", "Middle", "Shared", "Side", "Lone", "Finished"]);
        requires(&board, "001", "002");
        requires(&board, "001", "003");
        requires(&board, "002", "003"); // 003 again, under 002: a back-reference
        requires(&board, "002", "006");
        apply(
            &find(&board, "004"),
            LinkType::RelatesTo,
            &find(&board, "001"),
            false,
            false,
        )
        .unwrap();
        walk(&board, "006", &[Column::Todo, Column::Doing, Column::Done]);

        // Without a ticket: every family joined by requires. 004 is only
        // related, so it's in none, and 005 has no links.
        let trees = family(&board, None, true).unwrap();
        assert_eq!(
            drawn(&trees),
            [
                "root 001 unmet:002,003",
                "  requires 002 unmet:003",
                "    requires 003",
                "    requires 006",
                "  requires 003 seen",
            ]
        );
        assert!(trees[0].children[0].children[1].done);
        assert_eq!(trees[0].place, "backlog");

        // Around a ticket (ticket 373): what waits on it, up the chains.
        let trees = family(&board, Some(&find(&board, "003")), true).unwrap();
        assert_eq!(
            drawn(&trees),
            [
                "root 003 *",
                "  waits-on 002 unmet:003",
                "    waits-on 001 unmet:002,003",
                "  waits-on 001 seen",
            ]
        );
        // A ticket with no links, or only related ones, has no tree.
        for id in ["004", "005"] {
            assert!(
                family(&board, Some(&find(&board, id)), true)
                    .unwrap()
                    .is_empty()
            );
        }

        // Done left out (ticket 217), unless it's the one asked about.
        let trees = family(&board, Some(&find(&board, "002")), false).unwrap();
        assert_eq!(
            drawn(&trees),
            [
                "root 002 unmet:003 *",
                "  waits-on 001 unmet:002,003",
                "  requires 003",
            ]
        );
        let trees = family(&board, Some(&find(&board, "006")), false).unwrap();
        assert_eq!(
            drawn(&trees),
            [
                "root 006 *",
                "  waits-on 002 unmet:003",
                "    waits-on 001 unmet:002,003",
            ]
        );
    }

    /// Ticket 373, oberi-kitchen's 006 (here 004): only what can't be done
    /// until it is, and what it requires. The other things those tickets
    /// require (003 and 009 here, 005 and 014 there) are left out.
    #[test]
    fn the_tree_around_a_ticket_is_what_waits_on_it_and_what_it_needs() {
        let (_dir, board) = board_with(&[
            "Android",
            "API",
            "Research",
            "Barcode data",
            "Tauri",
            "AWS infra",
            "iOS",
            "Vault",
            "Collector",
            "KitchenData",
        ]);
        for (from, to) in [
            ("001", "003"),
            ("001", "005"),
            ("001", "006"),
            ("002", "003"),
            ("002", "004"),
            ("005", "006"),
            ("006", "004"),
            ("006", "008"),
            ("006", "009"),
            ("007", "003"),
            ("007", "005"),
            ("007", "006"),
            ("004", "008"),
            ("004", "010"),
        ] {
            requires(&board, from, to);
        }
        let trees = family(&board, Some(&find(&board, "004")), false).unwrap();
        assert_eq!(
            drawn(&trees),
            [
                "root 004 unmet:008,010 *",
                // What others wait on comes first: the long chain drawn out.
                "  waits-on 006 unmet:004,008,009",
                "    waits-on 005 unmet:006",
                "      waits-on 001 unmet:003,005,006",
                "      waits-on 007 unmet:003,005,006",
                "    waits-on 001 seen",
                "    waits-on 007 seen",
                "  waits-on 002 unmet:003,004",
                "  requires 008",
                "  requires 010",
            ]
        );
    }

    #[test]
    fn a_family_held_together_by_a_done_ticket_comes_apart_without_it() {
        let (_dir, board) = board_with(&["Left", "Middle", "Right"]);
        requires(&board, "001", "002");
        requires(&board, "003", "002");
        walk(&board, "002", &[Column::Todo, Column::Doing, Column::Done]);
        assert_eq!(family(&board, None, true).unwrap().len(), 2);
        assert!(family(&board, None, false).unwrap().is_empty());
        assert!(
            family(&board, Some(&find(&board, "001")), false)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_started_sprint_ticket_is_a_stub_and_a_cycle_ends() {
        let (_dir, board) = board_with(&["A", "B", "Running"]);
        sprint(&board, "s12");
        file(&board, "003", "s12");
        requires(&board, "002", "003");
        start_sprint(&board, "s12").unwrap();
        requires(&board, "001", "002");
        requires(&board, "002", "001"); // a cycle
        requires(&board, "003", "001");
        let trees = family(&board, Some(&find(&board, "001")), false).unwrap();
        assert_eq!(
            drawn(&trees),
            [
                "root 001 unmet:002 *",
                "  waits-on 002 unmet:001,003",
                "    waits-on 001 seen",
                "  waits-on 003 stub:s12 unmet:001",
                "  requires 002 seen",
            ]
        );
        // Without a ticket, a stub isn't expanded.
        assert!(
            drawn(&family(&board, None, false).unwrap())
                .contains(&"    requires 003 stub:s12 unmet:001".to_string())
        );
        let err = family(&board, Some(&find(&board, "003")), false).unwrap_err();
        assert!(err.to_string().contains("started sprint s12"), "{err}");
    }
}
