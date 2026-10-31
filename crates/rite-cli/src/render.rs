//! Board rendering.
//!
//! Tickets nest — a task's `parent` puts it under another — so each column is
//! drawn as a tree, ordered by id at every level. Two renderers, chosen by
//! where the output is going:
//!
//! - **rich** (`richrs`) when stdout is a terminal: tree guides, a colour per
//!   column, and ids as OSC-8 hyperlinks so the ticket opens on click.
//! - **plain** when it isn't (a pipe, a file, CI) or with `--plain`/`NO_COLOR`:
//!   the same tree as indented text, greppable and escape-free. That's the
//!   output the test suite asserts on.
//!
//! Anything that pipes `rite board` into `grep` therefore keeps working, and
//! nobody gets box-drawing characters or escape sequences in a log file.

use std::io::{self, IsTerminal};
use std::sync::OnceLock;

use anyhow::Result;
use richrs::prelude::{Console, Style, Tree, TreeNode};

use rite_lib::board::{Board, Column, Kind};
use rite_lib::task::Task;

static PLAIN: OnceLock<bool> = OnceLock::new();

/// Record the `--plain` flag. Called once, from `main`.
pub fn set_plain(plain: bool) {
    let _ = PLAIN.set(plain);
}

/// Should we draw the rich board?
fn rich() -> bool {
    if *PLAIN.get().unwrap_or(&false) {
        return false;
    }
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    io::stdout().is_terminal()
}

/// Do we dare emit OSC-8 hyperlinks? Terminals that don't understand them
/// mostly ignore them, but `TERM=dumb` says not to try.
fn hyperlinks() -> bool {
    rich() && std::env::var("TERM").as_deref() != Ok("dumb")
}

/// One task and the tasks nested under it. The parent id is read once, when
/// the column is loaded, rather than on every lookup — each read is a file read.
pub struct Node {
    pub task: Task,
    pub parent: Option<String>,
    pub children: Vec<Node>,
}

/// Read a column and nest it, reading each task's parent exactly once.
/// `tags`, when given, drop every task that doesn't pass them (ticket 123: `FUTURE`, `-FUTURE`, see `Task::matches_tags`) — a task left
/// out this way can still anchor a child from another column, same as any
/// other task that isn't in the set being nested.
pub fn column_forest(board: &Board, column: Column, tags: &[String]) -> Result<Vec<Node>> {
    let with_parents = board
        .tasks_in(column)?
        .into_iter()
        .filter(|task| task.matches_tags(tags))
        .map(|task| {
            let parent = task.parent();
            (task, parent)
        })
        .collect();
    Ok(forest(with_parents))
}

/// Print one or more columns of the board. `tags` narrow every column to the
/// tasks passing them.
pub fn columns(board: &Board, columns: &[Column], tags: &[String]) -> Result<()> {
    let workflow = board.workflow();
    let mut loaded = Vec::new();
    for column in columns {
        loaded.push((
            *column,
            heading(&workflow, *column),
            column_forest(board, *column, tags)?,
        ));
    }
    if rich() {
        // Falling back rather than failing: a rendering problem should never
        // stop you seeing the board.
        if rich_columns(&loaded).is_ok() {
            return Ok(());
        }
    }
    plain_columns(&loaded);
    Ok(())
}

/// Print one flat, named list of tasks — an epic or the archive, not a board
/// column — in the same shape `columns` prints a single column in. The no-ref
/// form of a destination verb (`rite <destination>`) goes through this. `tags`,
/// when given, narrow the list to tasks passing them.
pub fn named(title: &str, tasks: Vec<Task>, tags: &[String]) -> Result<()> {
    let with_parents = tasks
        .into_iter()
        .filter(|task| task.matches_tags(tags))
        .map(|task| {
            let parent = task.parent();
            (task, parent)
        })
        .collect();
    let nodes = forest(with_parents);
    if rich() && rich_named(title, &nodes).is_ok() {
        return Ok(());
    }
    plain_named(title, &nodes);
    Ok(())
}

fn plain_named(title: &str, nodes: &[Node]) {
    println!();
    println!("{}", title.to_uppercase());
    if nodes.is_empty() {
        println!("  (empty)");
        return;
    }
    plain_nodes(nodes, 0);
}

fn rich_named(title: &str, nodes: &[Node]) -> Result<()> {
    let mut console = Console::new();
    console.print("")?;
    let count = count(nodes);
    let mut tree = Tree::new(format!("{}  ({count})", title.to_uppercase()));
    if nodes.is_empty() {
        tree.add(TreeNode::new("(empty)"));
    } else {
        for node in nodes {
            tree.add(rich_node(node, true));
        }
    }
    console.write_segments(&tree.render())?;
    console.print("")?;
    console.flush()?;
    Ok(())
}

/// Nest the column's tasks by `parent`, ordered by id at every level.
///
/// A task whose parent is in another column stays at the top level here — the
/// columns are the board's primary structure — but keeps a `^NNN` marker so
/// the relationship is still visible.
pub fn forest(tasks: Vec<(Task, Option<String>)>) -> Vec<Node> {
    let ids: Vec<String> = tasks.iter().map(|(task, _)| task.id_display()).collect();
    let mut nodes: Vec<Option<Node>> = tasks
        .into_iter()
        .map(|(task, parent)| {
            Some(Node {
                task,
                parent,
                children: Vec::new(),
            })
        })
        .collect();

    // Deepest ids first, so a child is attached before its own parent moves.
    let order: Vec<usize> = (0..nodes.len()).rev().collect();
    for index in order {
        let Some(node) = nodes[index].as_ref() else {
            continue;
        };
        let Some(parent) = node.parent.clone() else {
            continue;
        };
        let Some(at) = ids.iter().position(|id| *id == parent) else {
            continue; // parent lives in another column
        };
        if at == index {
            continue;
        }
        let node = nodes[index].take().expect("checked above");
        if let Some(parent) = nodes[at].as_mut() {
            parent.children.push(node);
        } else {
            nodes[index] = Some(node);
        }
    }

    let mut roots: Vec<Node> = nodes.into_iter().flatten().collect();
    sort(&mut roots);
    roots
}

fn sort(nodes: &mut [Node]) {
    nodes.sort_by(|a, b| {
        a.task
            .id
            .cmp(&b.task.id)
            .then(a.task.stem.cmp(&b.task.stem))
    });
    for node in nodes {
        sort(&mut node.children);
    }
}

/// A column's heading: its folder in capitals, as it always was, unless the
/// team has named it something else (ticket 224), then that name.
fn heading(workflow: &rite_lib::workflow::Workflow, column: Column) -> String {
    let label = workflow.label(column);
    let standard = rite_lib::workflow::Workflow::standard().label(column);
    if label == standard {
        column.as_str().to_uppercase()
    } else {
        label.to_uppercase()
    }
}

/// The stable text form: `  [id] title  (labels)`, indented by depth.
fn plain_columns(columns: &[(Column, String, Vec<Node>)]) {
    for (_, heading, nodes) in columns {
        println!();
        println!("{heading}");
        if nodes.is_empty() {
            println!("  (empty)");
            continue;
        }
        plain_nodes(nodes, 0);
    }
}

fn plain_nodes(nodes: &[Node], depth: usize) {
    for node in nodes {
        let indent = "  ".repeat(depth + 1);
        let labels = labels(node, depth == 0);
        let suffix = if labels.is_empty() {
            String::new()
        } else {
            format!("  ({labels})")
        };
        println!(
            "{indent}[{}] {}{suffix}",
            node.task.id_display(),
            node.task.title()
        );
        plain_nodes(&node.children, depth + 1);
    }
}

fn rich_columns(columns: &[(Column, String, Vec<Node>)]) -> Result<()> {
    let mut console = Console::new();
    console.print("")?;
    for (column, heading, nodes) in columns {
        let count = count(nodes);
        let mut tree = Tree::new(format!("{heading}  ({count})")).guide_style(guide_style(*column));
        if nodes.is_empty() {
            tree.add(TreeNode::new("(empty)"));
        } else {
            for node in nodes {
                tree.add(rich_node(node, true));
            }
        }
        console.write_segments(&tree.render())?;
        console.print("")?;
    }
    console.flush()?;
    Ok(())
}

fn rich_node(node: &Node, root: bool) -> TreeNode {
    let labels = labels(node, root);
    let suffix = if labels.is_empty() {
        String::new()
    } else {
        format!("  ({labels})")
    };
    let label = format!("{} {}{suffix}", link(&node.task), node.task.title());
    let mut rendered = TreeNode::new(label);
    for child in &node.children {
        rendered.add_child(rich_node(child, false));
    }
    rendered
}

/// `[003]`, as an OSC-8 hyperlink to the ticket when the terminal can take one:
/// clicking the id opens the file.
fn link(task: &Task) -> String {
    let id = format!("[{}]", task.id_display());
    if !hyperlinks() {
        return id;
    }
    match dunce::canonicalize(&task.path) {
        Ok(path) => format!("\x1b]8;;file://{}\x1b\\{id}\x1b]8;;\x1b\\", path.display()),
        Err(_) => id,
    }
}

fn count(nodes: &[Node]) -> usize {
    nodes
        .iter()
        .map(|node| 1 + count(&node.children))
        .sum::<usize>()
}

/// Columns read left-to-right as work progresses, so they're coloured that way.
fn guide_style(column: Column) -> Style {
    // By kind (ticket 223), so a team's Review reads as work in progress.
    let spec = match column.kind() {
        // The backlog is everything not yet committed to, so it reads as the
        // quietest column rather than another stage of progress.
        Kind::Backlog => "bright_black",
        Kind::Todo => "blue",
        Kind::Doing => "yellow",
        Kind::Done => "green",
    };
    Style::parse(spec).unwrap_or_default()
}

/// `feat ^002` — the ticket type when it isn't the usual one, the change type,
/// and a marker when the parent is in another column (in the same column the
/// tree already shows it).
fn labels(node: &Node, root: bool) -> String {
    let task = &node.task;
    [
        task.branch_type(),
        Some(task.assignees())
            .filter(|people| !people.is_empty())
            .map(|people| format!("@{}", people.join(" @"))),
        // In the same column the tree already shows the parent; this marker is
        // for a child whose parent sits in another column.
        node.parent
            .clone()
            .filter(|_| root)
            .map(|parent| format!("^{parent}")),
        task.lease_active()
            .then(|| task.locked_by())
            .flatten()
            .map(|holder| format!("locked:{holder}")),
        // Ticket 143: when it's due (overdue said so), and that it repeats.
        task.due().map(|due| {
            if rite_lib::recur::overdue(task, rite_lib::recur::today()) {
                format!("overdue:{due}")
            } else {
                format!("due:{due}")
            }
        }),
        task.recur()
            .map(|rule| format!("repeats:{}", rule.replace(' ', "-"))),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<String>>()
    .join(" ")
}

/// `rite tree` (ticket 214): the requires tree, solid edges for what a
/// ticket requires and dashed ones (spaced, to stand out) for related. On a
/// terminal: ids link to their tickets, a started sprint's stub is dimmed
/// and "waits on" is in colour. In a pipe: the same characters, no colour,
/// and a stub says it's in a started sprint.
pub fn tree(board: &Board, trees: &[rite_lib::requires::TreeNode]) {
    let colour = rich();
    for (n, root) in trees.iter().enumerate() {
        if n > 0 {
            println!();
        }
        println!("{}", tree_line(board, root, colour));
        tree_children(board, &root.children, "", colour);
    }
}

fn tree_children(
    board: &Board,
    children: &[rite_lib::requires::TreeNode],
    prefix: &str,
    colour: bool,
) {
    use rite_lib::requires::Edge;
    for (n, child) in children.iter().enumerate() {
        let last = n + 1 == children.len();
        let branch = match (last, child.edge) {
            (false, Edge::Related) => "├ ─ ─ ",
            (true, Edge::Related) => "└ ─ ─ ",
            (false, _) => "├──── ",
            (true, _) => "└──── ",
        };
        println!("{prefix}{branch}{}", tree_line(board, child, colour));
        let deeper = format!("{prefix}{}", if last { "      " } else { "│     " });
        tree_children(board, &child.children, &deeper, colour);
    }
}

fn tree_line(board: &Board, node: &rite_lib::requires::TreeNode, colour: bool) -> String {
    let (dim, yellow, bold, reset) = if colour {
        ("\x1b[2m", "\x1b[33m", "\x1b[1m", "\x1b[0m")
    } else {
        ("", "", "", "")
    };
    let id = if colour {
        board
            .find(&node.id)
            .map(|t| link(&t))
            .unwrap_or_else(|_| node.id.clone())
    } else {
        node.id.clone()
    };
    if node.seen {
        return format!("{dim}↑ {} {} (above){reset}", node.id, node.title);
    }
    if let Some(sprint) = &node.stub {
        return format!(
            "{dim}{} {} · in started sprint {sprint}{reset}",
            node.id, node.title
        );
    }
    let mut line = format!("{id} {} · {}", node.title, node.place);
    if node.current {
        line = format!("{bold}{line}{reset}  ◀");
    }
    if node.done {
        line.push_str(" ✓");
    }
    if !node.unmet.is_empty() {
        line.push_str(&format!(
            "  {yellow}waits on {}{reset}",
            node.unmet.join(", ")
        ));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn task(id: u32, parent: Option<&str>) -> (Task, Option<String>) {
        let task = Task::from_path(
            Path::new(&format!("/b/todo/{id:03}-task-{id}.md")),
            Column::Todo,
        )
        .expect("valid task path");
        (task, parent.map(str::to_string))
    }

    fn shape(nodes: &[Node]) -> Vec<(u32, Vec<u32>)> {
        nodes
            .iter()
            .map(|node| {
                (
                    node.task.id.unwrap_or_default(),
                    node.children
                        .iter()
                        .map(|child| child.task.id.unwrap_or_default())
                        .collect(),
                )
            })
            .collect()
    }

    #[test]
    fn tasks_nest_under_their_parent() {
        let nodes = forest(vec![
            task(1, None),
            task(2, Some("001")),
            task(3, Some("001")),
            task(4, None),
        ]);
        assert_eq!(shape(&nodes), [(1, vec![2, 3]), (4, vec![])]);
    }

    #[test]
    fn nesting_goes_deeper_than_two_levels() {
        let nodes = forest(vec![
            task(1, None),
            task(2, Some("001")),
            task(3, Some("002")),
        ]);
        assert_eq!(shape(&nodes), [(1, vec![2])]);
        assert_eq!(shape(&nodes[0].children), [(2, vec![3])]);
    }

    #[test]
    fn a_parent_in_another_column_leaves_the_child_at_the_top() {
        let nodes = forest(vec![task(2, Some("001"))]);
        assert_eq!(shape(&nodes), [(2, vec![])]);
    }

    #[test]
    fn everything_is_ordered_by_id() {
        let nodes = forest(vec![
            task(3, Some("001")),
            task(2, Some("001")),
            task(1, None),
        ]);
        assert_eq!(shape(&nodes), [(1, vec![2, 3])]);
    }

    #[test]
    fn counting_includes_the_nested_tasks() {
        let nodes = forest(vec![
            task(1, None),
            task(2, Some("001")),
            task(3, Some("002")),
        ]);
        assert_eq!(count(&nodes), 3);
    }
}
