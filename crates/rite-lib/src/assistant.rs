//! The board as tools an AI assistant can call (ticket 186).
//!
//! `rite mcp` offers these over the Model Context Protocol, so whichever AI
//! tool someone uses (Claude Code, Copilot in VS Code, Cursor, Codex,
//! Gemini CLI…) can read the board, tickets, documents and ideas and change
//! tickets. Nothing here knows about MCP or stdio: each tool is a name, a
//! description, a JSON Schema for its input and a handler returning text
//! and structured data, so another front end could offer the same.
//!
//! Every change goes through the same rite-lib calls as `rite` and riteui,
//! with their rules, leases and auto-commit. Abandoning tickets, breaking
//! leases, settings and branches are deliberately not offered: those stay
//! with a person, through the buttons or `rite`.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::board::{Board, Column, Kind};
use crate::sync::Purpose;
use crate::task::Task;
use crate::task::with_note;
use crate::transition::{self, Event, TicketFields};
use crate::{
    archive, authorship, board_settings, documents, documents_edit, ideas, model, my_work,
    project_trust, screen,
};

/// One tool, as an MCP client lists it.
pub struct Tool {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// Only reads: an AI tool may run it without asking.
    pub read_only: bool,
    /// Its input, as JSON Schema.
    pub input: Value,
}

/// What a tool did: text for the model to read, and the same as data.
#[derive(Debug)]
pub struct Reply {
    pub text: String,
    pub data: Value,
}

/// The most cards one column lists before saying how many more there are.
const COLUMN_LIMIT: usize = 60;
/// The most tickets a search returns.
const SEARCH_LIMIT: usize = 30;

/// Every tool, in the order a client shows them.
pub fn tools() -> Vec<Tool> {
    let reference = json!({"type": "string", "description": "A ticket: its number (`42`), `repo#42` for another repository's board, or part of its file name"});
    vec![
        Tool {
            name: "screen",
            title: "What's on screen",
            description: "What riteui is showing for this project right now: the screen (home, board, documents, ideas or roadmaps), the open ticket, document or idea, the open epic or sprint, and the board's filters. Use it to find what the user means by \"this ticket\" or \"this document\".",
            read_only: true,
            input: json!({"type": "object", "properties": {}}),
        },
        Tool {
            name: "board",
            title: "The board",
            description: "The project's board: each repository's columns (backlog, with its epics and sprints, todo, doing, done) and their tickets. Done is counted, not listed, unless asked for. Filters narrow it down.",
            read_only: true,
            input: json!({"type": "object", "properties": {
                "column": {"type": "string", "description": "Only this column: its name or folder, e.g. backlog, todo, doing, done, or one of the project's own"},
                "repo": {"type": "string", "description": "Only this repository's board"},
                "tag": {"type": "string", "description": "Only tickets with this tag; -TAG leaves them out"},
                "assignee": {"type": "string", "description": "Only tickets assigned to this name; `nobody` for unassigned"},
                "epic": {"type": "string", "description": "Only tickets in this epic or sprint"},
                "include_done": {"type": "boolean", "description": "List done tickets too"}
            }}),
        },
        Tool {
            name: "ticket",
            title: "Read a ticket",
            description: "A ticket's whole file (frontmatter and body), where it is, its lock, and who created and last edited it. `version` can be passed back to edit_ticket so an edit is refused if the ticket changed meanwhile.",
            read_only: true,
            input: json!({"type": "object", "properties": {"ticket": reference}, "required": ["ticket"]}),
        },
        Tool {
            name: "search_tickets",
            title: "Search tickets",
            description: "Tickets on any of the project's boards whose title or text contains the words, ignoring case.",
            read_only: true,
            input: json!({"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]}),
        },
        Tool {
            name: "my_work",
            title: "My work",
            description: "The user's todo and doing tickets across every project on this machine, highest priority first; with nothing assigned, work they could pick up.",
            read_only: true,
            input: json!({"type": "object", "properties": {}}),
        },
        Tool {
            name: "search_documents",
            title: "Search documents",
            description: "The project's documents (its own documents repository and the shared one) whose title or text contains the words: a few matching lines each.",
            read_only: true,
            input: json!({"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]}),
        },
        Tool {
            name: "document",
            title: "Read a document",
            description: "A document's markdown, by its path in the documents repository (as search_documents or start_page give it), and its `version` for edit_document.",
            read_only: true,
            input: json!({"type": "object", "properties": {
                "path": {"type": "string"},
                "repo": {"type": "string", "description": "The documents repository; the project's own when left out"}
            }, "required": ["path"]}),
        },
        Tool {
            name: "start_page",
            title: "Documents start page",
            description: "The project's documents start page, and the tree of documents it links to.",
            read_only: true,
            input: json!({"type": "object", "properties": {}}),
        },
        Tool {
            name: "ideas",
            title: "Ideas",
            description: "The project's ideas for apps and improvements, best voted first, with their scores and any tickets made from them.",
            read_only: true,
            input: json!({"type": "object", "properties": {"include_archived": {"type": "boolean"}}}),
        },
        Tool {
            name: "idea",
            title: "Read an idea",
            description: "One idea's write-up, votes and notes.",
            read_only: true,
            input: json!({"type": "object", "properties": {"idea": {"type": "string", "description": "Its path, file name or part of its title"}}, "required": ["idea"]}),
        },
        Tool {
            name: "create_ticket",
            title: "Create a ticket",
            description: "Make a ticket in the backlog, as `rite new` does, and commit it. Only the title is needed.",
            read_only: false,
            input: json!({"type": "object", "properties": {
                "title": {"type": "string"},
                "body": {"type": "string", "description": "Markdown after the frontmatter, e.g. `## Notes` and `## Acceptance criteria` sections; the template's when left out"},
                "priority": {"type": "string", "enum": ["low", "medium", "high", "top"]},
                "points": {"type": "string", "enum": ["1", "2", "3", "5", "8", "13"]},
                "tags": {"type": "array", "items": {"type": "string"}},
                "assignee": {"type": "string", "description": "A name from the project's users, or `@me`"},
                "assignees": {"type": "array", "items": {"type": "string"}, "description": "Several people, first the main one; instead of `assignee`"},
                "epic": {"type": "string", "description": "An epic or sprint to file it in"},
                "parent": {"type": "string", "description": "The parent ticket's number"},
                "requires": {"type": "array", "items": {"type": "string"}, "description": "Tickets this one requires (numbers): linked both ways; they must be done before it, and be in the same sprint for a sprint to start"},
                "repository": {"type": "string", "description": "The code repository it's about"},
                "branch_type": {"type": "string", "description": "feature, bugfix, hotfix, release, chore (the project may differ), or `none` for no branch. With a repository it must be given (else the ticket is a draft); a branch type needs a repository"},
                "draft": {"type": "boolean", "description": "Mark it DRAFT: it can't leave the backlog until it's ready"},
                "board": {"type": "string", "description": "Which repository's board to put it on; the project's own when left out"}
            }, "required": ["title"]}),
        },
        Tool {
            name: "clone_ticket",
            title: "Duplicate a ticket",
            description: "Copy a ticket into the backlog of the same board and commit it. The copy keeps the title, body, tags, branch type, repository, parent, priority and points, but starts unassigned, unlocked and in no epic, with its checklist unticked. It is linked to the original as a clone: `clone-of` on the copy, `clones` on the original.",
            read_only: false,
            input: json!({"type": "object", "properties": {"ticket": reference}, "required": ["ticket"]}),
        },
        Tool {
            name: "link_tickets",
            title: "Link two tickets",
            description: "Link a ticket to another on the same board with a type, or remove the link, and commit it. The other ticket records the opposite (`requires` there is `required-by` here), so either end shows it. Types: relates-to, requires, required-by, parent-of, child-of, clone-of, cloned-by, supersedes, superseded-by. A ticket has one parent and is the clone of one original. A ticket can't move to done while tickets it requires or its children aren't done; `replace` changes such a link's type (e.g. to relates-to) instead of adding one.",
            read_only: false,
            input: json!({"type": "object", "properties": {
                "ticket": reference,
                "to": {"type": "string", "description": "The other ticket: its number, or part of its file name"},
                "replace": {"type": "string", "enum": crate::links::LinkType::ALL.map(crate::links::LinkType::slug), "description": "Change the existing link of this type to `type`, on both tickets"},
                "type": {"type": "string", "enum": crate::links::LinkType::ALL.map(crate::links::LinkType::slug), "description": "How `ticket` stands to `to`; relates-to when left out"},
                "remove": {"type": "boolean", "description": "Take the link away instead of adding it"}
            }, "required": ["ticket", "to"]}),
        },
        Tool {
            name: "move_ticket",
            title: "Move a ticket",
            description: "Move a ticket one column along (by default backlog, todo, doing, done; a project can have its own columns and allow other moves, which `board` lists) and commit it. Entering a doing column takes the user's lock on it; leaving gives it back. Refused when a doing column is at its WIP limit unless `force`, and refused into done while tickets it requires or its children aren't done (no override: finish them, or change or remove the link). Doesn't switch git branches.",
            read_only: false,
            input: json!({"type": "object", "properties": {
                "ticket": reference,
                "to": {"type": "string", "description": "The column: its name or folder, e.g. backlog, todo, doing, done, or one of the project's own (`board` lists them)"},
                "force": {"type": "boolean", "description": "Go past doing's WIP limit; only when the user says so"}
            }, "required": ["ticket", "to"]}),
        },
        Tool {
            name: "edit_ticket",
            title: "Edit a ticket",
            description: "Change some of a ticket's fields and commit it. Fields left out stay as they are; an empty string clears one. Pass `version` from the ticket tool to have the edit refused if someone changed the ticket since.",
            read_only: false,
            input: json!({"type": "object", "properties": {
                "ticket": reference,
                "title": {"type": "string"},
                "body": {"type": "string", "description": "The whole body after the frontmatter"},
                "priority": {"type": "string", "enum": ["low", "medium", "high", "top", "auto"], "description": "Set by hand; with auto priority and override on it pins the ticket, and `auto` hands it back to auto priority"},
                "points": {"type": "string", "enum": ["", "1", "2", "3", "5", "8", "13"]},
                "tags": {"type": "array", "items": {"type": "string"}},
                "assignee": {"type": "string", "description": "One person, replacing whoever is assigned; empty for nobody"},
                "assignees": {"type": "array", "items": {"type": "string"}, "description": "Everyone assigned, first the main one; instead of `assignee`"},
                "parent": {"type": "string"},
                "repository": {"type": "string"},
                "branch_type": {"type": "string", "description": "feature, bugfix, hotfix, release, chore (the project may differ), or `none` for no branch. With a repository it must be given (else the ticket is a draft); a branch type needs a repository"},
                "version": {"type": "string"}
            }, "required": ["ticket"]}),
        },
        Tool {
            name: "assign_ticket",
            title: "Assign a ticket",
            description: "Assign a ticket to someone in the project's users (`@me` for the user), or to nobody with an empty name, and commit it. A ticket can have several people: `assignees` sets them all, `add` adds to whoever is assigned and `remove` takes people off.",
            read_only: false,
            input: json!({"type": "object", "properties": {
                "ticket": reference,
                "assignee": {"type": "string"},
                "assignees": {"type": "array", "items": {"type": "string"}, "description": "Everyone assigned, first the main one"},
                "add": {"type": "array", "items": {"type": "string"}, "description": "People to add after whoever is assigned"},
                "remove": {"type": "array", "items": {"type": "string"}, "description": "People to take off"}
            }, "required": ["ticket"]}),
        },
        Tool {
            name: "add_note",
            title: "Add a note to a ticket",
            description: "Append a dated note, signed with the user's name, to the ticket's `## Notes`, and commit it.",
            read_only: false,
            input: json!({"type": "object", "properties": {"ticket": reference, "text": {"type": "string"}}, "required": ["ticket", "text"]}),
        },
        Tool {
            name: "file_ticket",
            title: "File a ticket in an epic or sprint",
            description: "Put a ticket in an epic or sprint, or take it out with an empty name, and commit it. A started sprint takes nothing more and gives nothing back.",
            read_only: false,
            input: json!({"type": "object", "properties": {"ticket": reference, "epic": {"type": "string"}}, "required": ["ticket", "epic"]}),
        },
        Tool {
            name: "create_document",
            title: "Create a document",
            description: "Make a document in a folder of a documents repository, as riteui's New document does, and commit and push it straight to main. In adr/adr and adr/rfc it takes the next number and its heading says so; a body's own top heading gives way to that one.",
            read_only: false,
            input: json!({"type": "object", "properties": {
                "folder": {"type": "string", "description": "e.g. adr/adr, adr/rfc, docs; empty for the repository's top"},
                "title": {"type": "string"},
                "body": {"type": "string", "description": "Markdown below the heading"},
                "repo": {"type": "string", "description": "The documents repository; the project's own when left out"}
            }, "required": ["folder", "title"]}),
        },
        Tool {
            name: "edit_document",
            title: "Change a document",
            description: "Replace a document's whole text, and commit and push it straight to main. Give the `version` `document` returned to refuse a document someone changed since you read it.",
            read_only: false,
            input: json!({"type": "object", "properties": {
                "path": {"type": "string"},
                "text": {"type": "string", "description": "The whole new markdown"},
                "version": {"type": "string", "description": "From document; the file as it is now when left out"},
                "repo": {"type": "string", "description": "The documents repository; the project's own when left out"}
            }, "required": ["path", "text"]}),
        },
        Tool {
            name: "create_group",
            title: "Create an epic or sprint",
            description: "Make an epic or a (pending) sprint in a board's backlog, as `rite group epic`/`sprint` does, and commit it.",
            read_only: false,
            input: json!({"type": "object", "properties": {
                "name": {"type": "string", "description": "Its folder name, which tickets record"},
                "kind": {"type": "string", "enum": ["epic", "sprint"], "description": "epic when left out"},
                "description": {"type": "string", "description": "One line"},
                "days": {"type": "integer", "description": "A sprint's length in days"},
                "board": {"type": "string", "description": "Which repository's board; the project's own when left out"}
            }, "required": ["name"]}),
        },
        Tool {
            name: "describe_group",
            title: "Change an epic's or sprint's description",
            description: "Set the one-line description of an epic or sprint, a started one included, and commit it. Empty clears it.",
            read_only: false,
            input: json!({"type": "object", "properties": {
                "name": {"type": "string"},
                "description": {"type": "string"},
                "board": {"type": "string", "description": "Which repository's board; the project's own when left out"}
            }, "required": ["name", "description"]}),
        },
    ]
}

/// Runs the tool called `name` with `args`, from the current directory's
/// project.
pub fn call(name: &str, args: &Value) -> Result<Reply> {
    match name {
        "screen" => screen_tool(),
        "board" => board_tool(args),
        "ticket" => ticket_tool(args),
        "search_tickets" => search_tickets(args),
        "my_work" => my_work_tool(),
        "search_documents" => search_documents(args),
        "document" => document_tool(args),
        "create_document" => create_document_tool(args),
        "edit_document" => edit_document_tool(args),
        "start_page" => start_page_tool(),
        "ideas" => ideas_tool(args),
        "idea" => idea_tool(args),
        "create_ticket" => create_ticket(args),
        "clone_ticket" => clone_ticket(args),
        "link_tickets" => link_tickets(args),
        "move_ticket" => move_ticket(args),
        "edit_ticket" => edit_ticket(args),
        "assign_ticket" => assign_ticket(args),
        "add_note" => add_note(args),
        "file_ticket" => file_ticket(args),
        "create_group" => create_group_tool(args),
        "describe_group" => describe_group_tool(args),
        _ => bail!("there's no tool called {name}"),
    }
}

/// The project's boards, caught up with the remote the way `rite` does.
struct Project {
    /// `(name, repository root, board)`, the project's own first.
    boards: Vec<(String, PathBuf, Board)>,
    /// What catching up had to say.
    notes: Vec<String>,
}

impl Project {
    fn open(purpose: Purpose) -> Result<Project> {
        let found = model::find_repos()?;
        let mut project = Project {
            boards: found.repos,
            notes: Vec::new(),
        };
        let own = project.own();
        project_trust::check(own)?;
        if purpose == Purpose::Write {
            archive::ensure_writable(own)?;
        }
        let events = crate::sync::catch_up(own, purpose)?;
        project.notes.extend(events.into_iter().map(event_text));
        Ok(project)
    }

    fn own(&self) -> &Board {
        &self.boards[0].2
    }

    fn name(&self) -> String {
        crate::current_project::project_name(&self.boards[0].1)
    }

    /// The project as people know it (ticket 326): `KanRite (kanrite)`
    /// when it has a display name, else just its name.
    fn titled(&self) -> String {
        let name = self.name();
        match board_settings::load(self.own()).display_name() {
            Some(shown) if shown != name => format!("{shown} ({name})"),
            _ => name,
        }
    }

    /// The board of the repository called `name`, or the project's own.
    fn index(&self, name: Option<&str>) -> Result<usize> {
        let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) else {
            return Ok(0);
        };
        self.boards
            .iter()
            .position(|(n, _, _)| n == name)
            .ok_or_else(|| {
                anyhow!(
                    "{name} isn't one of this project's boards ({})",
                    self.boards
                        .iter()
                        .map(|(n, _, _)| n.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }

    /// A ticket by `repo#id`, or by reference on the project's own board,
    /// else on whichever other board alone has it.
    fn find(&self, reference: &str) -> Result<(usize, Task)> {
        let reference = reference.trim().trim_start_matches('#');
        if let Some((repo, id)) = reference.split_once('#') {
            let i = self.index(Some(repo))?;
            return Ok((i, self.boards[i].2.find(id)?));
        }
        let own = self.own().find(reference);
        if let Ok(task) = own {
            return Ok((0, task));
        }
        let mut others: Vec<(usize, Task)> = self
            .boards
            .iter()
            .enumerate()
            .skip(1)
            .filter_map(|(i, (_, _, board))| board.find(reference).ok().map(|t| (i, t)))
            .collect();
        match others.len() {
            1 => Ok(others.remove(0)),
            0 => own.map(|t| (0, t)),
            _ => bail!(
                "{reference} is on more than one board; say which, e.g. {}#{reference}",
                self.boards[others[0].0].0
            ),
        }
    }

    /// Ready board `i` for a change: the project's own is caught up already;
    /// another repository's is caught up now.
    fn ready(&mut self, i: usize) -> Result<&Board> {
        if i != 0 {
            let events = crate::sync::catch_up(&self.boards[i].2, Purpose::Write)?;
            self.notes.extend(events.into_iter().map(event_text));
        }
        Ok(&self.boards[i].2)
    }

    fn reply(self, text: String, data: Value) -> Reply {
        let mut text = text;
        for note in self.notes {
            text.push_str(&format!("\n{note}"));
        }
        Reply { text, data }
    }
}

fn event_text(event: Event) -> String {
    match event {
        Event::Info(text) => text,
        Event::Warn(text) => format!("warning: {text}"),
    }
}

fn events_text(outcome: &transition::Outcome) -> Vec<String> {
    outcome.events.iter().cloned().map(event_text).collect()
}

fn arg(args: &Value, key: &str) -> Option<String> {
    match args.get(key)? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

fn required(args: &Value, key: &str) -> Result<String> {
    arg(args, key)
        .filter(|v| !v.trim().is_empty())
        .with_context(|| format!("`{key}` is needed"))
}

fn flag(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// A list given as an array, or as one comma-separated string.
fn list(args: &Value, key: &str) -> Option<Vec<String>> {
    match args.get(key)? {
        Value::Array(items) => Some(
            items
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect(),
        ),
        Value::String(s) => Some(crate::task::parse_list(s)),
        _ => None,
    }
}

/// `@me` is whoever git says you are, as `rite assign @me`.
fn assignee(board: &Board, who: &str) -> String {
    match who.trim() {
        "@me" => crate::git::config(&board.root, "user.name").unwrap_or_else(|| "me".into()),
        who => who.to_string(),
    }
}

/// The people `args` names: `assignees` (a list), else `assignee` (one,
/// empty for nobody), `@me` resolved; `None` when it names neither.
fn people(board: &Board, args: &Value) -> Option<Vec<String>> {
    let names = match list(args, "assignees") {
        Some(names) => names,
        None => {
            let one = arg(args, "assignee")?;
            if one.trim().is_empty() {
                Vec::new()
            } else {
                vec![one]
            }
        }
    };
    Some(names.iter().map(|who| assignee(board, who)).collect())
}

/// Where a ticket is, in words.
fn place(task: &Task) -> String {
    match (&task.epic, task.epic_meta()) {
        (Some(folder), _) => format!("backlog › {folder}"),
        (None, Some(epic)) if !epic.is_empty() => format!("{} (from epic {epic})", task.column),
        _ => task.column.to_string(),
    }
}

fn card(repo: &str, task: &Task) -> Value {
    json!({
        "ref": format!("{repo}#{}", task.id_display()),
        "repo": repo,
        "id": task.id_display(),
        "file": task.file_name(),
        "title": task.title(),
        "column": task.column.as_str(),
        "epic": task.epic.clone().or_else(|| task.epic_meta().filter(|e| !e.is_empty())),
        "priority": task.priority(),
        "points": task.points().filter(|p| !p.is_empty()),
        "assignee": task.assignee().filter(|a| !a.is_empty()),
        "assignees": task.assignees(),
        "tags": task.tags(),
        "locked_by": task.lease_active().then(|| task.locked_by()).flatten(),
    })
}

/// One ticket on one line.
fn line(repo: &str, task: &Task) -> String {
    let mut out = format!(
        "{repo}#{} {} · {}",
        task.id_display(),
        task.title(),
        task.priority()
    );
    if let Some(points) = task.points().filter(|p| !p.is_empty()) {
        out.push_str(&format!(" · {points} pts"));
    }
    let people = task.assignees();
    if !people.is_empty() {
        out.push_str(&format!(" · {}", people.join(", ")));
    }
    let tags = task.tags();
    if !tags.is_empty() {
        out.push_str(&format!(" · {}", tags.join(" ")));
    }
    if task.lease_active()
        && let Some(by) = task.locked_by()
    {
        out.push_str(&format!(" · locked by {by}"));
    }
    out
}

fn screen_tool() -> Result<Reply> {
    let project = Project::open(Purpose::Read)?;
    let Some(note) = screen::read(&project.boards[0].1) else {
        let text = format!(
            "riteui isn't open on {}, so nothing is on screen.",
            project.name()
        );
        return Ok(project.reply(text, json!({"open": false})));
    };
    let view = &note.view;
    let mut text = format!("riteui shows {}'s {}.", note.project, view.screen);
    let mut ticket = Value::Null;
    if let Some(open) = &view.ticket {
        let found = project
            .index(Some(&open.repo))
            .ok()
            .and_then(|i| project.boards[i].2.find(&open.path).ok().map(|t| (i, t)));
        match found {
            Some((i, task)) => {
                let repo = &project.boards[i].0;
                text.push_str(&format!(
                    "\nOpen ticket: {} ({})",
                    line(repo, &task),
                    place(&task)
                ));
                ticket = card(repo, &task);
            }
            None => text.push_str(&format!("\nOpen ticket: {}/{}", open.repo, open.path)),
        }
    }
    if let Some(doc) = &view.document {
        text.push_str(&format!("\nOpen document: {}/{}", doc.repo, doc.path));
    }
    if let Some(idea) = &view.idea {
        text.push_str(&format!("\nOpen idea: {}/{}", idea.repo, idea.path));
    }
    if let Some(epic) = &view.epic {
        text.push_str(&format!(
            "\nOpen epic or sprint: {} ({})",
            epic.path, epic.repo
        ));
    }
    let mut filters = Vec::new();
    if !view.search.is_empty() {
        filters.push(format!("search \"{}\"", view.search));
    }
    if let Some(about) = &view.about {
        filters.push(format!("about {about}"));
    }
    if !view.assignee.is_empty() {
        filters.push(format!("assigned to {}", view.assignee));
    }
    if !filters.is_empty() {
        text.push_str(&format!("\nBoard filtered by {}", filters.join(", ")));
    }
    let mut data = serde_json::to_value(&note)?;
    data["open"] = json!(true);
    data["ticket_card"] = ticket;
    Ok(project.reply(text, data))
}

fn board_tool(args: &Value) -> Result<Reply> {
    let project = Project::open(Purpose::Read)?;
    let only = match arg(args, "column") {
        Some(c) => Some(
            project.boards[0]
                .2
                .workflow()
                .find(&c)
                .with_context(|| format!("{c} isn't a column"))?,
        ),
        None => None,
    };
    let repo = arg(args, "repo");
    let wanted = repo
        .as_deref()
        .map(|r| project.index(Some(r)))
        .transpose()?;
    let tag = arg(args, "tag");
    let who = arg(args, "assignee");
    let epic = arg(args, "epic");
    let include_done = flag(args, "include_done") || only.is_some_and(|c| c.is(Kind::Done));
    let keep = |task: &Task| {
        tag.as_ref()
            .is_none_or(|t| task.matches_tags(std::slice::from_ref(t)))
            && who.as_deref().is_none_or(|w| {
                if w.eq_ignore_ascii_case("nobody") {
                    task.assignees().is_empty()
                } else {
                    task.is_assigned_to(w.trim())
                }
            })
            && epic.as_deref().is_none_or(|e| {
                task.epic.as_deref() == Some(e) || task.epic_meta().as_deref() == Some(e)
            })
    };
    let mut text = format!("Project {}", project.titled());
    let mut data = Vec::new();
    for (i, (name, _, board)) in project.boards.iter().enumerate() {
        if wanted.is_some_and(|w| w != i) {
            continue;
        }
        text.push_str(&format!("\n\n## {name}"));
        // The team's flow (ticket 225), so a move is asked for only where
        // it's allowed.
        let workflow = board.workflow();
        let flow = match workflow.flow() {
            crate::workflow::Flow::Any => "any column to any other".to_string(),
            crate::workflow::Flow::Next if workflow.moves().is_empty() => {
                "one column at a time".to_string()
            }
            crate::workflow::Flow::Next => format!(
                "one column at a time, plus {}",
                workflow
                    .moves()
                    .iter()
                    .map(|(f, t)| format!("{} → {}", workflow.label(*f), workflow.label(*t)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        text.push_str(&format!(
            "\nMoves: {flow}. New tickets land in {}.",
            workflow.label(workflow.landing())
        ));
        let settings = board_settings::load(board);
        let mut columns = Vec::new();
        for column in board.workflow().columns() {
            if only.is_some_and(|o| o != column) {
                continue;
            }
            let mut cards: Vec<model::Card> = board
                .tasks_in(column)?
                .into_iter()
                .filter(|t| keep(t))
                .map(model::card_from)
                .collect();
            model::sort_cards(&mut cards);
            let limit = match settings.wip_limit(column) {
                Some(n) => format!(", WIP limit {n}"),
                _ => String::new(),
            };
            text.push_str(&format!("\n### {column} ({}{limit})", cards.len()));
            if column.is(Kind::Done) && !include_done {
                columns.push(json!({"column": column.as_str(), "count": cards.len()}));
                continue;
            }
            if column == Column::Backlog {
                let groups = board.groups()?;
                let mut by_group: Vec<(Option<&crate::group::Group>, Vec<&model::Card>)> =
                    vec![(None, Vec::new())];
                by_group.extend(groups.iter().map(|g| (Some(g), Vec::new())));
                for card in &cards {
                    let slot = by_group
                        .iter_mut()
                        .find(|(g, _)| g.map(|g| g.name.as_str()) == card.task.epic.as_deref());
                    if let Some((_, list)) = slot {
                        list.push(card);
                    }
                }
                for (group, list) in &by_group {
                    if let Some(g) = group {
                        if list.is_empty() && (tag.is_some() || who.is_some()) {
                            continue;
                        }
                        let state = if g.is_sprint() {
                            format!(", {}", g.state.as_str())
                        } else {
                            String::new()
                        };
                        text.push_str(&format!(
                            "\n{} {} ({}{state}):",
                            g.kind.as_str(),
                            g.name,
                            list.len()
                        ));
                    }
                    push_cards(&mut text, name, list);
                }
            } else {
                push_cards(&mut text, name, &cards.iter().collect::<Vec<_>>());
            }
            columns.push(json!({
                "column": column.as_str(),
                "count": cards.len(),
                "tickets": cards.iter().map(|c| card(name, &c.task)).collect::<Vec<_>>(),
            }));
        }
        data.push(json!({"repo": name, "columns": columns}));
    }
    let display_name = board_settings::load(project.own()).display_name();
    let data = json!({"project": project.name(), "display_name": display_name, "boards": data});
    Ok(project.reply(text, data))
}

fn push_cards(text: &mut String, repo: &str, cards: &[&model::Card]) {
    for card in cards.iter().take(COLUMN_LIMIT) {
        text.push_str(&format!("\n- {}", line(repo, &card.task)));
    }
    if cards.len() > COLUMN_LIMIT {
        text.push_str(&format!(
            "\n- …and {} more (filter to see them)",
            cards.len() - COLUMN_LIMIT
        ));
    }
}

fn ticket_tool(args: &Value) -> Result<Reply> {
    let project = Project::open(Purpose::Lookup)?;
    let (i, task) = project.find(&required(args, "ticket")?)?;
    let (repo, board) = (&project.boards[i].0, &project.boards[i].2);
    let content =
        crate::eol::read(&task.path).with_context(|| format!("reading {}", task.path.display()))?;
    let version = task.hash()?.to_string();
    let mut text = format!("{repo}#{} is in {}.", task.id_display(), place(&task));
    if let Some(by) = task.locked_by() {
        let state = if task.lease_active() {
            "locked"
        } else {
            "lock lapsed"
        };
        text.push_str(&format!(" {state} by {by}."));
    }
    if let Some(summary) = authorship::summary(board, &task) {
        text.push_str(&format!(" {summary}."));
    }
    // Every ticket linked to it, either way (ticket 121): a link recorded
    // only on the other ticket isn't in this one's text.
    let linked = crate::links::linked(board, &task).unwrap_or_default();
    if !linked.is_empty() {
        text.push_str("\nLinked:");
        for l in &linked {
            text.push_str(&format!(
                "\n- {} {} {} ({})",
                l.kind.phrase(),
                l.task.id_display(),
                l.task.title(),
                place(&l.task)
            ));
        }
    }
    text.push_str(&format!("\nversion: {version}\n\n{content}"));
    let mut data = card(repo, &task);
    data["linked"] = json!(
        linked
            .iter()
            .map(|l| json!({
                "kind": l.kind.slug(),
                "ticket": format!("{repo}#{}", l.task.id_display()),
                "title": l.task.title(),
                "recorded_here": l.recorded_here,
            }))
            .collect::<Vec<_>>()
    );
    data["content"] = json!(content);
    data["version"] = json!(version);
    Ok(project.reply(text, data))
}

fn search_tickets(args: &Value) -> Result<Reply> {
    let project = Project::open(Purpose::Read)?;
    let query = required(args, "query")?;
    let needle = query.to_lowercase();
    let mut hits = Vec::new();
    let mut text = String::new();
    'boards: for (name, _, board) in &project.boards {
        for task in board.tasks()? {
            let body = crate::eol::read(&task.path).unwrap_or_default();
            let in_title = task.title().to_lowercase().contains(&needle);
            let matched = body
                .lines()
                .find(|l| l.to_lowercase().contains(&needle) && !l.starts_with("title:"));
            if !in_title && matched.is_none() {
                continue;
            }
            text.push_str(&format!("\n- {} ({})", line(name, &task), task.column));
            if let Some(m) = matched {
                text.push_str(&format!("\n    {}", m.trim()));
            }
            let mut found = card(name, &task);
            found["line"] = json!(matched.map(str::trim));
            hits.push(found);
            if hits.len() >= SEARCH_LIMIT {
                text.push_str("\n(more not shown; narrow the search)");
                break 'boards;
            }
        }
    }
    let text = if hits.is_empty() {
        format!("No ticket mentions \"{query}\".")
    } else {
        format!("Tickets mentioning \"{query}\":{text}")
    };
    Ok(project.reply(text, json!({"tickets": hits})))
}

fn my_work_tool() -> Result<Reply> {
    let project = Project::open(Purpose::Read)?;
    let (email, name) = my_work::me(&project.boards[0].1)
        .context("git has no user.email here, so rite can't tell whose work is whose")?;
    let mut items = my_work::my_work(&email, name.as_deref(), false, my_work::Sort::Priority);
    let mut heading = "Your todo and doing tickets";
    if items.is_empty() {
        items = my_work::to_pick(&email, name.as_deref(), my_work::include_unassigned());
        heading = "Nothing is assigned to you; work you could pick up";
    }
    let mut text = format!("{heading}:");
    if items.is_empty() {
        text = "Nothing assigned to you, and nothing to pick up.".into();
    }
    for item in &items {
        let points = if item.points.is_empty() {
            String::new()
        } else {
            format!(" · {} pts", item.points)
        };
        text.push_str(&format!(
            "\n- {}: {}#{} {} · {} · {}{points}",
            item.project, item.repo, item.id, item.title, item.column, item.priority
        ));
    }
    Ok(project.reply(text, json!({"items": items})))
}

/// The project's documents repositories that are cloned here.
fn document_sources(board: &Board) -> Vec<(String, PathBuf)> {
    documents::sources(board)
        .into_iter()
        .filter_map(|s| s.root.map(|root| (s.name, root)))
        .collect()
}

fn search_documents(args: &Value) -> Result<Reply> {
    let project = Project::open(Purpose::Read)?;
    let query = required(args, "query")?;
    let sources = document_sources(project.own());
    if sources.is_empty() {
        bail!("this project's documents repository isn't cloned here");
    }
    let mut text = String::new();
    let mut hits = Vec::new();
    for (name, root) in &sources {
        for hit in documents_edit::search(root, &query, 3)? {
            text.push_str(&format!("\n{name}/{}:{}: {}", hit.path, hit.line, hit.text));
            hits.push(json!({"repo": name, "path": hit.path, "title": hit.title, "line": hit.line, "text": hit.text}));
        }
    }
    let text = if hits.is_empty() {
        format!("No document mentions \"{query}\".")
    } else {
        format!("Documents mentioning \"{query}\" (repo/path:line):{text}")
    };
    Ok(project.reply(text, json!({"hits": hits})))
}

fn document_tool(args: &Value) -> Result<Reply> {
    let project = Project::open(Purpose::Read)?;
    let path = required(args, "path")?;
    let sources = document_sources(project.own());
    let wanted = arg(args, "repo").filter(|r| !r.trim().is_empty());
    let (name, root) = match &wanted {
        Some(repo) => sources.iter().find(|(n, _)| n == repo).with_context(|| {
            format!("{repo} isn't one of this project's documents repositories")
        })?,
        None => sources
            .first()
            .context("this project's documents repository isn't cloned here")?,
    };
    // A path may come with its repository in front, as search_documents
    // shows it.
    let path = path
        .strip_prefix(&format!("{name}/"))
        .unwrap_or(&path)
        .to_string();
    let text = documents::read(root, &path)?;
    let version = documents_edit::version(&text);
    let data = json!({"repo": name, "path": path, "content": text, "version": version});
    Ok(project.reply(format!("{name}/{path}\n\n{text}"), data))
}

/// `create_document` (ticket 380): `rite docs new`.
fn create_document_tool(args: &Value) -> Result<Reply> {
    let project = Project::open(Purpose::Write)?;
    let (name, root) =
        documents_edit::writable_source(project.own(), arg(args, "repo").as_deref())?;
    let folder = arg(args, "folder").unwrap_or_default();
    let folder = documents_edit::strip_source(&name, &folder).to_string();
    let title = required(args, "title")?;
    let body = arg(args, "body");
    let (path, done) =
        documents_edit::create_document_with(&root, &folder, &title, body.as_deref())?;
    let text = documents::read(&root, &path)?;
    let mut lines = vec![format!("created {name}/{path}"), done.message.clone()];
    lines.extend(done.warning.iter().map(|w| format!("warning: {w}")));
    let data = json!({"repo": name, "path": path, "version": documents_edit::version(&text), "pushed": done.pushed});
    Ok(project.reply(lines.join("\n"), data))
}

/// `edit_document` (ticket 380): `rite docs edit`.
fn edit_document_tool(args: &Value) -> Result<Reply> {
    let project = Project::open(Purpose::Write)?;
    let (name, root) =
        documents_edit::writable_source(project.own(), arg(args, "repo").as_deref())?;
    let path = required(args, "path")?;
    let path = documents_edit::strip_source(&name, &path).to_string();
    let text = arg(args, "text").context("`text` is needed")?;
    let done = documents_edit::replace(&root, &path, &text, arg(args, "version").as_deref())?;
    let now = documents::read(&root, &path)?;
    let mut lines = vec![format!("saved {name}/{path}"), done.message.clone()];
    lines.extend(done.warning.iter().map(|w| format!("warning: {w}")));
    let data = json!({"repo": name, "path": path, "version": documents_edit::version(&now), "pushed": done.pushed});
    Ok(project.reply(lines.join("\n"), data))
}

fn start_page_tool() -> Result<Reply> {
    let project = Project::open(Purpose::Read)?;
    let (name, root) = document_sources(project.own())
        .into_iter()
        .next()
        .context("this project's documents repository isn't cloned here")?;
    let start =
        documents_edit::start_page(&root).context("the documents repository has no start page")?;
    let content = documents::read(&root, &start)?;
    let tree = documents_edit::link_tree(&root, &start)?;
    let mut text = format!(
        "{}'s start page: {name}/{start}\n\nDocuments it leads to:",
        project.titled()
    );
    for page in tree.iter().skip(1) {
        text.push_str(&format!(
            "\n{}- {} ({})",
            "  ".repeat(page.depth.saturating_sub(1)),
            page.title,
            page.path
        ));
    }
    text.push_str(&format!("\n\n{content}"));
    let pages: Vec<Value> = tree
        .iter()
        .map(|p| json!({"depth": p.depth, "path": p.path, "title": p.title}))
        .collect();
    let data = json!({"repo": name, "path": start, "content": content, "tree": pages});
    Ok(project.reply(text, data))
}

fn idea_list(board: &Board, include_archived: bool) -> Result<Vec<ideas::Idea>> {
    let source = ideas::source(board);
    if source.root.is_none() {
        bail!("this project's documents repository isn't cloned here, and ideas live there");
    }
    let mut list: Vec<ideas::Idea> = ideas::list(&source)
        .into_iter()
        .filter(|i| include_archived || !i.archived)
        .collect();
    ideas::sort(&mut list);
    Ok(list)
}

fn idea_json(idea: &ideas::Idea) -> Value {
    json!({
        "path": idea.path,
        "title": idea.title,
        "score": idea.score(),
        "voters": idea.votes.len(),
        "author": idea.author,
        "created": idea.created,
        "tickets": idea.tickets,
        "archived": idea.archived,
    })
}

fn ideas_tool(args: &Value) -> Result<Reply> {
    let project = Project::open(Purpose::Read)?;
    let list = idea_list(project.own(), flag(args, "include_archived"))?;
    let mut text = String::from("Ideas, best first:");
    if list.is_empty() {
        text = "No ideas yet.".into();
    }
    for idea in &list {
        text.push_str(&format!(
            "\n- {} {} ({} voters) · {}",
            ideas::signed(idea.score()),
            idea.title,
            idea.votes.len(),
            idea.path
        ));
        if !idea.tickets.is_empty() {
            text.push_str(&format!(" · tickets {}", idea.tickets.join(", ")));
        }
    }
    let data = json!({"ideas": list.iter().map(idea_json).collect::<Vec<_>>()});
    Ok(project.reply(text, data))
}

fn idea_tool(args: &Value) -> Result<Reply> {
    let project = Project::open(Purpose::Read)?;
    let list = idea_list(project.own(), true)?;
    let idea = ideas::find(&list, &required(args, "idea")?)?;
    let mut text = format!(
        "{} ({}, score {})\n\n{}",
        idea.title,
        idea.path,
        ideas::signed(idea.score()),
        ideas::write_up(&idea.text)
    );
    if !idea.votes.is_empty() {
        text.push_str("\n\nVotes:");
        for vote in &idea.votes {
            text.push_str(&format!("\n- {} {}", vote.name, ideas::signed(vote.value)));
        }
    }
    if !idea.notes.is_empty() {
        text.push_str("\n\nNotes:");
        for note in &idea.notes {
            text.push_str(&format!("\n- {}, {}: {}", note.name, note.date, note.text));
        }
    }
    let mut data = idea_json(idea);
    data["content"] = json!(idea.text);
    Ok(project.reply(text, data))
}

fn create_ticket(args: &Value) -> Result<Reply> {
    let mut project = Project::open(Purpose::Write)?;
    let i = project.index(arg(args, "board").as_deref())?;
    let repo = project.boards[i].0.clone();
    let board = project.ready(i)?;
    let mut tags = list(args, "tags").unwrap_or_default();
    if flag(args, "draft") {
        tags.push(crate::task::DRAFT_TAG.to_string());
    }
    let tag_warning = board_settings::load(board).check_tags(&tags)?;
    let fields = TicketFields {
        title: required(args, "title")?,
        assignees: people(board, args).unwrap_or_default(),
        repository: arg(args, "repository").filter(|r| !r.trim().is_empty()),
        points: arg(args, "points").unwrap_or_default(),
        epic: arg(args, "epic").filter(|e| !e.trim().is_empty()),
        tags,
        branch_type: arg(args, "branch_type").unwrap_or_default(),
        parent: arg(args, "parent").unwrap_or_default(),
        related: Vec::new(),
        clone_of: String::new(),
        body: arg(args, "body").unwrap_or_default(),
        priority: arg(args, "priority").unwrap_or_default(),
        requires: list(args, "requires").unwrap_or_default(),
        required_by: Vec::new(),
        linked_from: Vec::new(),
    };
    let unknown = board_settings::unknown_assignees(board, &fields.assignees);
    let outcome = transition::create_ticket(board, &fields)?;
    let made = outcome
        .dest
        .as_ref()
        .and_then(|dest| Task::from_path(dest, Column::Backlog))
        .context("the new ticket wasn't where it was written")?;
    let mut lines = vec![format!("Created {}", line(&repo, &made))];
    lines.extend(events_text(&outcome));
    lines.extend(tag_warning.into_iter().map(|w| format!("warning: {w}")));
    lines.extend(unknown.into_iter().map(|w| format!("warning: {w}")));
    let data = card(&repo, &made);
    Ok(project.reply(lines.join("\n"), data))
}

fn clone_ticket(args: &Value) -> Result<Reply> {
    let mut project = Project::open(Purpose::Write)?;
    let (i, original) = project.find(&required(args, "ticket")?)?;
    let repo = project.boards[i].0.clone();
    let board = project.ready(i)?;
    let outcome = transition::clone_ticket(board, &original)?;
    let made = outcome
        .dest
        .as_ref()
        .and_then(|dest| Task::from_path(dest, Column::Backlog))
        .context("the new ticket wasn't where it was written")?;
    let mut lines = vec![format!(
        "Copied {repo}#{} as {}",
        original.id_display(),
        line(&repo, &made)
    )];
    lines.extend(events_text(&outcome));
    let data = card(&repo, &made);
    Ok(project.reply(lines.join("\n"), data))
}

fn link_tickets(args: &Value) -> Result<Reply> {
    let mut project = Project::open(Purpose::Write)?;
    let (i, task) = project.find(&required(args, "ticket")?)?;
    let repo = project.boards[i].0.clone();
    let kind = match arg(args, "type") {
        Some(text) => crate::links::LinkType::parse(&text)
            .with_context(|| format!("unknown link type '{text}'"))?,
        None => crate::links::LinkType::RelatesTo,
    };
    let remove = args.get("remove").and_then(Value::as_bool).unwrap_or(false);
    let board = project.ready(i)?;
    let other = board.find(&required(args, "to")?)?;
    if let Some(old) = arg(args, "replace").filter(|o| !o.trim().is_empty()) {
        let old = crate::links::LinkType::parse(&old)
            .with_context(|| format!("unknown link type '{old}'"))?;
        let outcome = transition::retype_link(board, &task, old, kind, &other)?;
        let data = card(&repo, &task);
        return Ok(project.reply(events_text(&outcome).join("\n"), data));
    }
    let outcome = transition::link_tickets(
        board,
        &task,
        kind,
        std::slice::from_ref(&other),
        remove,
        false,
    )?;
    let data = card(&repo, &task);
    Ok(project.reply(events_text(&outcome).join("\n"), data))
}

fn move_ticket(args: &Value) -> Result<Reply> {
    let mut project = Project::open(Purpose::Write)?;
    let (i, task) = project.find(&required(args, "ticket")?)?;
    let to = required(args, "to")?;
    let to = project.boards[i]
        .2
        .workflow()
        .find(&to)
        .with_context(|| format!("{to} isn't a column"))?;
    let repo = project.boards[i].0.clone();
    let board = project.ready(i)?;
    let outcome = transition::move_to(board, &task, to, flag(args, "force"))?;
    let moved = outcome
        .dest
        .as_ref()
        .and_then(|dest| Task::from_path(dest, to))
        .unwrap_or(task);
    let mut lines = vec![format!("{repo}#{} is now in {to}", moved.id_display())];
    lines.extend(events_text(&outcome));
    let data = card(&repo, &moved);
    Ok(project.reply(lines.join("\n"), data))
}

/// The hash `version` names, or the ticket's own now.
fn expected(task: &Task, args: &Value) -> Result<crate::task::ContentHash> {
    match arg(args, "version").filter(|v| !v.trim().is_empty()) {
        Some(version) => version
            .trim()
            .parse()
            .map_err(|_| anyhow!("{version} isn't a ticket version")),
        None => task.hash(),
    }
}

fn edit_ticket(args: &Value) -> Result<Reply> {
    let mut project = Project::open(Purpose::Write)?;
    let (i, task) = project.find(&required(args, "ticket")?)?;
    let repo = project.boards[i].0.clone();
    let board = project.ready(i)?;
    let expected = expected(&task, args)?;
    let keep = |key: &str, now: String| arg(args, key).unwrap_or(now);
    let title = keep("title", task.title());
    let named = people(board, args);
    let who = named.clone().unwrap_or_else(|| task.assignees());
    let points = keep("points", task.points().unwrap_or_default());
    let tags = list(args, "tags").unwrap_or_else(|| task.tags());
    let branch_type = keep("branch_type", task.branch_type().unwrap_or_default());
    let parent = keep("parent", task.parent().unwrap_or_default());
    let related = task.related();
    let body = keep("body", task.body()?);
    let priority = arg(args, "priority");
    let repository = arg(args, "repository");
    let tag_warning = match list(args, "tags") {
        Some(tags) => board_settings::load(board).check_tags(&tags)?,
        None => None,
    };
    let (id_display, old_title) = (task.id_display(), task.title());
    // A repository and a branch type go together (ticket 066).
    let why = transition::check_pairing(
        board,
        &task,
        &repository
            .as_deref()
            .map(crate::task::repo_name)
            .unwrap_or_else(|| task.repository().unwrap_or_default()),
        &branch_type,
    )?;
    let (drafted, warning) =
        transition::with_auto_commit(board, "edit", &id_display, &old_title, || {
            task.apply_edit(
                expected,
                board,
                crate::task::EditFields {
                    title: &title,
                    assignees: &who,
                    points: &points,
                    tags: &tags,
                    branch_type: &branch_type,
                    parent: &parent,
                    related: &related,
                    body: &body,
                },
            )?;
            if let Some(priority) = &priority
                && priority.trim() != task.priority()
            {
                crate::auto_priority::set_by_hand(board, &task, priority)?;
            }
            if let Some(repository) = &repository
                && crate::task::repo_name(repository) != task.repository().unwrap_or_default()
            {
                task.set_repository(task.hash()?, repository)?;
            }
            transition::settle_pairing(&task, why)
        })?;
    let mut lines = vec![format!("Saved {}", line(&repo, &task))];
    lines.extend(drafted.into_iter().map(|d| format!("warning: {d}")));
    lines.extend(warning.into_iter().map(|w| format!("warning: {w}")));
    lines.extend(tag_warning.into_iter().map(|w| format!("warning: {w}")));
    if named.is_some() {
        lines.extend(
            board_settings::unknown_assignees(board, &who)
                .into_iter()
                .map(|w| format!("warning: {w}")),
        );
    }
    let data = card(&repo, &task);
    Ok(project.reply(lines.join("\n"), data))
}

fn assign_ticket(args: &Value) -> Result<Reply> {
    let mut project = Project::open(Purpose::Write)?;
    let (i, task) = project.find(&required(args, "ticket")?)?;
    let repo = project.boards[i].0.clone();
    let board = project.ready(i)?;
    let resolve = |key: &str| -> Vec<String> {
        list(args, key)
            .unwrap_or_default()
            .iter()
            .map(|who| assignee(board, who))
            .collect()
    };
    let (add, remove) = (resolve("add"), resolve("remove"));
    let named = people(board, args);
    if named.is_none() && add.is_empty() && remove.is_empty() {
        bail!("say who: `assignee`, `assignees`, `add` or `remove`");
    }
    let who = crate::task::regroup(&named.unwrap_or_else(|| task.assignees()), &add, &remove);
    let (id_display, title) = (task.id_display(), task.title());
    let ((), warning) = transition::with_auto_commit(board, "assign", &id_display, &title, || {
        task.set_assignees(task.hash()?, &who)
    })?;
    let mut lines = vec![if who.is_empty() {
        format!("{repo}#{id_display} unassigned")
    } else {
        format!("{repo}#{id_display} assigned to {}", who.join(", "))
    }];
    lines.extend(warning.into_iter().map(|w| format!("warning: {w}")));
    lines.extend(
        board_settings::unknown_assignees(board, &who)
            .into_iter()
            .map(|w| format!("warning: {w}")),
    );
    let data = card(&repo, &task);
    Ok(project.reply(lines.join("\n"), data))
}

fn add_note(args: &Value) -> Result<Reply> {
    let mut project = Project::open(Purpose::Write)?;
    let (i, task) = project.find(&required(args, "ticket")?)?;
    let note = required(args, "text")?;
    let repo = project.boards[i].0.clone();
    let board = project.ready(i)?;
    let who = authorship::current_person(board)
        .map(|p| p.split(" <").next().unwrap_or(&p).to_string())
        .unwrap_or_else(|| "someone".into());
    let today = time::OffsetDateTime::now_utc().date();
    let signed = format!("**{who}, {today}:** {}", note.trim());
    let (id_display, title) = (task.id_display(), task.title());
    let ((), warning) = transition::with_auto_commit(board, "note", &id_display, &title, || {
        let body = with_note(&task.body()?, &signed);
        task.set_body(task.hash()?, &body)
    })?;
    let mut lines = vec![format!("Added a note to {repo}#{id_display}")];
    lines.extend(warning.into_iter().map(|w| format!("warning: {w}")));
    let data = card(&repo, &task);
    Ok(project.reply(lines.join("\n"), data))
}

fn file_ticket(args: &Value) -> Result<Reply> {
    let mut project = Project::open(Purpose::Write)?;
    let (i, task) = project.find(&required(args, "ticket")?)?;
    let epic = arg(args, "epic").unwrap_or_default();
    let epic = Some(epic.trim()).filter(|e| !e.is_empty());
    let repo = project.boards[i].0.clone();
    let board = project.ready(i)?;
    let (id_display, title) = (task.id_display(), task.title());
    let (dest, warning) = transition::with_auto_commit(board, "set", &id_display, &title, || {
        board.set_epic(&task, epic)
    })?;
    let moved = Task::from_path(&dest, task.column).unwrap_or(task);
    let mut lines = vec![match epic {
        Some(e) => format!("{repo}#{id_display} filed under {e}"),
        None => format!("{repo}#{id_display} taken out of its epic"),
    }];
    lines.extend(warning.into_iter().map(|w| format!("warning: {w}")));
    let data = card(&repo, &moved);
    Ok(project.reply(lines.join("\n"), data))
}

/// `create_group` (ticket 384): `rite group epic`/`sprint`, with the same
/// commit verbs.
fn create_group_tool(args: &Value) -> Result<Reply> {
    use crate::group::{DEFAULT_DAYS, Group, Kind, State};
    let mut project = Project::open(Purpose::Write)?;
    let i = project.index(arg(args, "board").as_deref())?;
    let name = required(args, "name")?.trim().to_string();
    let description = arg(args, "description")
        .unwrap_or_default()
        .trim()
        .to_string();
    let kind = match arg(args, "kind").as_deref().map(str::trim) {
        None | Some("") | Some("epic") => Kind::Epic,
        Some("sprint") => Kind::Sprint,
        Some(other) => bail!("kind is epic or sprint, not {other}"),
    };
    let days = match args.get("days").and_then(Value::as_u64) {
        Some(0) => bail!("a sprint lasts at least a day"),
        Some(days) => u32::try_from(days).unwrap_or(u32::MAX),
        None => DEFAULT_DAYS,
    };
    let repo = project.boards[i].0.clone();
    let board = project.ready(i)?;
    let verb = if kind == Kind::Sprint {
        "group-sprint"
    } else {
        "group-epic"
    };
    let group = Group {
        dir: board.dir(Column::Backlog).join(&name),
        name: name.clone(),
        kind,
        description: description.clone(),
        days,
        state: State::Pending,
    };
    let ((), warning) = transition::with_auto_commit(board, verb, &name, &description, || {
        board.create_group(&group)
    })?;
    let what = if kind == Kind::Sprint {
        format!("sprint {name} ({days} days)")
    } else {
        format!("epic {name}")
    };
    let mut lines = vec![format!("created {what} on {repo}")];
    lines.extend(warning.into_iter().map(|w| format!("warning: {w}")));
    let data = json!({"repo": repo, "name": name, "kind": if kind == Kind::Sprint { "sprint" } else { "epic" }, "description": description, "days": days});
    Ok(project.reply(lines.join("\n"), data))
}

/// `describe_group` (ticket 384): `rite group describe`.
fn describe_group_tool(args: &Value) -> Result<Reply> {
    let mut project = Project::open(Purpose::Write)?;
    let i = project.index(arg(args, "board").as_deref())?;
    let name = required(args, "name")?.trim().to_string();
    let description = arg(args, "description")
        .unwrap_or_default()
        .trim()
        .to_string();
    let repo = project.boards[i].0.clone();
    let board = project.ready(i)?;
    let ((), warning) =
        transition::with_auto_commit(board, "group-describe", &name, &description, || {
            board.set_group_description(&name, &description)
        })?;
    let mut lines = vec![match description.as_str() {
        "" => format!("{repo}: {name} has no description now"),
        text => format!("{repo}: {name}: {text}"),
    }];
    lines.extend(warning.into_iter().map(|w| format!("warning: {w}")));
    Ok(project.reply(
        lines.join("\n"),
        json!({"repo": repo, "name": name, "description": description}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_an_object_schema_and_a_handler() {
        let tools = tools();
        let mut names: Vec<&str> = tools.iter().map(|t| t.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), tools.len(), "names are unique");
        for tool in &tools {
            assert_eq!(tool.input["type"], "object", "{}", tool.name);
            if let Some(required) = tool.input["required"].as_array() {
                for key in required {
                    let key = key.as_str().unwrap();
                    assert!(
                        tool.input["properties"].get(key).is_some(),
                        "{} requires {key}",
                        tool.name
                    );
                }
            }
        }
        let err = call("nothing", &json!({})).unwrap_err();
        assert!(err.to_string().contains("no tool called nothing"));
        // Nothing that abandons, unlocks or changes settings.
        for name in &names {
            assert!(
                !["abandon", "unlock", "settings", "delete"]
                    .iter()
                    .any(|bad| name.contains(bad)),
                "{name}"
            );
        }
    }

    #[test]
    fn a_note_goes_at_the_end_of_the_notes_section() {
        let body = "## Notes\n\nFirst thought.\n\n## Acceptance criteria\n\n- [ ] it works\n";
        assert_eq!(
            with_note(body, "**Sam, 2026-10-12:** more"),
            "## Notes\n\nFirst thought.\n\n**Sam, 2026-10-12:** more\n\n## Acceptance criteria\n\n- [ ] it works\n"
        );
        assert_eq!(
            with_note("## Notes\n", "n"),
            "## Notes\n\nn\n",
            "an empty notes section"
        );
        assert_eq!(
            with_note("## Acceptance criteria\n\n- [ ] x\n", "n"),
            "## Acceptance criteria\n\n- [ ] x\n\n## Notes\n\nn\n",
            "no notes section: one is made"
        );
        assert_eq!(with_note("", "n"), "## Notes\n\nn\n");
    }

    #[test]
    fn lists_come_as_arrays_or_one_string() {
        assert_eq!(
            list(&json!({"tags": ["ux", " api "]}), "tags"),
            Some(vec!["ux".to_string(), "api".to_string()])
        );
        assert_eq!(
            list(&json!({"tags": "ux, api"}), "tags"),
            Some(vec!["ux".to_string(), "api".to_string()])
        );
        assert_eq!(list(&json!({}), "tags"), None);
        assert_eq!(arg(&json!({"ticket": 42}), "ticket").as_deref(), Some("42"));
        assert!(required(&json!({"ticket": " "}), "ticket").is_err());
    }
}
