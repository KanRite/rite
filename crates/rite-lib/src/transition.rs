//! Moving a ticket between columns the way `rite todo`/`doing`/`done` do —
//! the lease, the branch on entering `todo/`, and the auto-commit — as one
//! path that the command line and every other front end share.
//!
//! This used to live in `rite-cli`'s `main.rs`, and the desktop board only
//! reproduced half of it (a lease, but no branch and no commit). It moved so
//! that a move from any front end leaves the board, the git history and the
//! working tree in the same state.
//!
//! Nothing here prints. A front end that owns the screen must not have a
//! library writing over it, so what the CLI used to print is returned as
//! ordered [`Event`]s and the caller decides how to show them.

use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::board::{Board, Column, Kind};
use crate::task::Task;
use crate::{board_settings, branch, git};

/// One thing a transition wants the user to know, in the order it happened.
/// `Info` is what the CLI prints on stdout; `Warn` is what it prints on
/// stderr, for a best-effort step that failed without undoing the move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Info(String),
    Warn(String),
}

/// What a transition did.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Where the ticket landed, when it moved.
    pub dest: Option<PathBuf>,
    /// Everything to tell the user, in order.
    pub events: Vec<Event>,
}

/// How to enter `todo/`: which branch to put the working tree on.
#[derive(Debug, Default, Clone)]
pub struct TodoOptions {
    /// Skip the WIP limit.
    pub force: bool,
    /// Use this branch instead of the ticket's own.
    pub branch: Option<String>,
    /// Leave the branch alone entirely (`--no-branch`).
    pub no_branch: bool,
    /// Which todo-kind column (ticket 224); the standard todo when `None`.
    pub to: Option<Column>,
}

/// Runs `mutate`, then — if `commit` is on for this board — commits and
/// pushes exactly the paths it changed, determined by diffing `git status`
/// before and after rather than a hand-maintained list. That's not a
/// simplification: `Board::record_id` writes the `.next-id` counter
/// alongside a new ticket, so `rite new` genuinely touches two files in one
/// operation, and a hand-picked paths argument at each call site is exactly
/// the shape of place a second incidental write like that gets missed.
/// Anything already dirty *before* `mutate` runs — this repo's own
/// unrelated in-progress state — is excluded by construction, since it
/// appears in both snapshots.
///
/// Best-effort in the same spirit as the lease: a commit or push failure (a
/// locked signing key, no agent, the network down, a moved remote) must
/// never undo a board change that already succeeded on disk. It comes back as
/// the second tuple element for the caller to warn about — never an error,
/// never a rollback — because failing at this point would misreport a move
/// that actually worked, and there is nothing sane to roll back to that
/// wouldn't be its own surprise.
pub fn with_auto_commit<T>(
    board: &Board,
    verb: &str,
    subject: &str,
    detail: &str,
    mutate: impl FnOnce() -> Result<T>,
) -> Result<(T, Option<String>)> {
    let before = snapshot(board);
    let result = mutate()?;
    // When the change itself touched nothing (a settings migration with
    // nothing left to move, run before every command), whatever settling
    // moves is only priorities, and is committed as that rather than under
    // the change's name (ticket 331).
    let changed_nothing = before.as_ref().is_some_and(|before| {
        git::changed_paths(
            before,
            &git::dirty_paths(board.repo_root()).unwrap_or_default(),
        )
        .is_empty()
    });
    settle_priorities(board);
    let message = if changed_nothing && verb != PRIORITY {
        commit_message(PRIORITY, "auto", "")
    } else {
        commit_message(verb, subject, detail)
    };
    let warning = commit_since(board, before, &message)?;
    Ok((result, warning))
}

/// The verb of a commit that only moves priorities (`priority: auto`,
/// riteui's hourly `priority: due dates`).
pub const PRIORITY: &str = "priority";

/// Auto priority (ticket 215), after a change and before its commit, so
/// the commit carries the priorities it moved. Never undoes the change: a
/// failure is only noted (`auto_priority::take_notes`).
pub fn settle_priorities(board: &Board) {
    if let Err(err) = crate::auto_priority::reconcile(board) {
        crate::auto_priority::note_failure(&err);
    }
}

/// `verb: subject "detail"`, or `verb: subject` when there is no detail
/// (`register`, `group start`, …), rather than ending in an empty `""`.
/// Public so a front end's own report of the change says what the commit
/// says (ticket 117).
pub fn commit_message(verb: &str, subject: &str, detail: &str) -> String {
    if detail.is_empty() {
        format!("{verb}: {subject}")
    } else {
        format!("{verb}: {subject} \"{detail}\"")
    }
}

/// The repository's dirty paths as they stand, when auto-commit is on for
/// this board; `None` when it is off, so a caller can pass it straight back
/// to [`commit_since`] without asking again.
fn snapshot(
    board: &Board,
) -> Option<std::collections::BTreeMap<PathBuf, Option<std::time::SystemTime>>> {
    board_settings::load(board)
        .auto_commit()
        .then(|| git::dirty_paths(board.repo_root()).unwrap_or_default())
}

/// Commit and push whatever changed since `before` was taken, with `message`.
/// Returns the warning to show if that couldn't be done.
///
/// A push rejected because someone else pushed in the meantime goes to
/// `sync::after_rejected_push` (ticket 102): the change is replayed on top
/// of theirs when that's clean, and otherwise undone — the one case this
/// returns an `Err` (`sync::ConcurrentChange`) rather than a warning, since
/// nothing was applied.
pub fn commit_since(
    board: &Board,
    before: Option<std::collections::BTreeMap<PathBuf, Option<std::time::SystemTime>>>,
    message: &str,
) -> Result<Option<String>> {
    let Some(before) = before else {
        return Ok(None);
    };
    let repo = board.repo_root();
    let after = git::dirty_paths(repo).unwrap_or_default();
    let changed = git::changed_paths(&before, &after);
    if changed.is_empty() {
        return Ok(None);
    }
    let pre = git::head(repo);
    if let Err(err) = git::commit_paths_locally(repo, message, &changed) {
        return Ok(Some(format!(
            "could not commit/push automatically: {err:#}"
        )));
    }
    // A board with no remote (`rite init --local-only`, a scratch board) has
    // nowhere to push: the commit is the whole job, said nothing about
    // (ticket 142). Same as `sync::catch_up`, which does nothing there.
    if git::remote_url(repo).is_none() {
        return Ok(None);
    }
    match git::push(repo) {
        Ok(()) => Ok(None),
        Err(err) => crate::sync::after_rejected_push(board, pre.as_deref(), &changed, &err),
    }
}

/// Move a ticket, claiming its lease when it enters `doing/` and giving it
/// back when it leaves (ticket 156),
/// auto-committing the result. The wording of the `moved` line is the CLI's
/// — `Board::move_task` returns where the ticket landed and leaves the
/// reporting to whoever asked for the move.
pub fn move_to(board: &Board, task: &Task, to: Column, force: bool) -> Result<Outcome> {
    // Captured before the move: id_display()/title()/file_name() read the
    // ticket's own file, which move_task is about to relocate.
    let (id_display, title) = (task.id_display(), task.title());
    let file_name = task.file_name();
    let from = task.column;
    let (mut outcome, warning) = with_auto_commit(board, to.as_str(), &id_display, &title, || {
        let mut outcome = Outcome::default();
        let dest = board.move_task(task, to, force)?;
        outcome
            .events
            .push(Event::Info(format!("moved {file_name} -> {to}/")));
        // A ticket is locked exactly while it's in doing (ticket 156): the
        // lease is claimed on entering doing ("I'm working on this now"),
        // not earlier, since todo only means it's next. A move onto a
        // ticket someone else has leased is refused earlier, in move_task.
        // Any doing-kind column (ticket 223): Doing to Review renews it.
        if to.is(Kind::Doing)
            && let Some(moved) = Task::from_path(&dest, to)
            && let Some(warning) = maybe_take_lease(board, &moved)
        {
            outcome.events.push(Event::Warn(warning));
        }
        // ...and given back on leaving doing in any direction, or on
        // reaching done or the backlog (ticket 092), in the same commit as
        // the move. Only your own lease is ever released.
        if (from.is(Kind::Doing) || to.is(Kind::Done) || to.is(Kind::Backlog))
            && !to.is(Kind::Doing)
            && let Some(moved) = Task::from_path(&dest, to)
            && let Err(err) = moved.release_if_mine()
        {
            outcome.events.push(Event::Warn(format!(
                "could not release the lease automatically: {err:#}"
            )));
        }
        // When it entered todo and doing (ticket 252), for auto priority's
        // time limits: entering todo from the backlog starts the todo clock,
        // entering doing starts doing's and keeps todo's, so a ticket raised
        // while it waited stays raised once someone picks it up. Back in the
        // backlog, both go.
        if let Some(moved) = Task::from_path(&dest, to)
            && let Some(warning) = stamp_column_dates(&moved, from, to)
        {
            outcome.events.push(Event::Warn(warning));
        }
        // A recurring ticket comes round again (ticket 143): reaching done
        // makes the next one, in this same commit.
        if to.is(Kind::Done)
            && !from.is(Kind::Done)
            && let Some(moved) = Task::from_path(&dest, to)
            && let Some(next) = crate::recur::make_next(board, &moved, crate::recur::today())?
        {
            outcome.events.push(Event::Info(format!(
                "{id_display} repeats: made {}, due {}",
                next.id, next.due
            )));
            // Due soon: straight to todo (ticket 253).
            if let Ok(made) = board.find(&next.id)
                && let Some(line) = straight_to_todo(board, &made)
            {
                outcome.events.push(Event::Info(line));
            }
        }
        outcome.dest = Some(dest);
        Ok(outcome)
    })?;
    if let Some(warning) = warning {
        outcome.events.push(Event::Warn(warning));
    }
    Ok(outcome)
}

/// What moving a group's tickets did (ticket 256): the tickets moved, and
/// those left where they were, with why.
#[derive(Debug, Default)]
pub struct GroupMoved {
    pub moved: Vec<String>,
    pub skipped: Vec<(String, String)>,
}

/// Every ticket of the epic or sprint `name` still in the backlog, moved to
/// todo in one go (ticket 256): only that group's, each keeping its `epic`
/// label (so sent back, it returns to the group), with when it entered
/// todo recorded. A ticket the rules keep back (a draft, someone else's
/// lease) is skipped and said; one already past the backlog is left alone.
/// No branches are made, as on the desktop board. Doesn't commit: the
/// caller wraps it, so the whole move is one commit.
pub fn group_to_todo(board: &Board, name: &str) -> Result<GroupMoved> {
    board.group(name)?;
    let todo = board.workflow().first(Kind::Todo);
    let mut result = GroupMoved::default();
    for task in board.tasks_in(Column::Backlog)? {
        if task.epic.as_deref() != Some(name) {
            continue;
        }
        let label = task.id_display();
        match board.move_task(&task, todo, false) {
            Ok(dest) => {
                if let Some(moved) = Task::from_path(&dest, todo) {
                    // Outside the backlog the label is what says it's the
                    // group's; one written by hand into the folder may lack it.
                    if moved.epic_meta().as_deref() != Some(name)
                        && let Ok(hash) = moved.hash()
                    {
                        let _ = moved.set_epic_meta(hash, name);
                    }
                    if let Some(moved) = Task::from_path(&dest, todo) {
                        stamp_column_dates(&moved, task.column, todo);
                    }
                }
                result.moved.push(label);
            }
            Err(err) => result.skipped.push((label, format!("{err:#}"))),
        }
    }
    Ok(result)
}

/// A ticket with a due date this near goes straight to todo (ticket 253):
/// one in the loose backlog, not a draft, due within the board's
/// `due-to-todo-days` (or overdue). Moves it there, recording when, and says
/// so; anything that stops the move (a lease, say) is only a note, since the
/// ticket's own change already happened. Doesn't commit: callers are inside
/// one, or commit after.
pub fn straight_to_todo(board: &Board, task: &Task) -> Option<String> {
    let days = crate::board_settings::load(board).due_to_todo_days();
    if days == 0 || !task.column.is(Kind::Backlog) || task.epic.is_some() || task.is_draft() {
        return None;
    }
    let due = task.due()?;
    let left = (crate::recur::parse_date(&due).ok()? - crate::recur::today()).whole_days();
    if left > i64::from(days) {
        return None;
    }
    let id = task.id_display();
    let todo = board.workflow().first(Kind::Todo);
    match board.move_task(task, todo, false) {
        Ok(dest) => {
            if let Some(moved) = Task::from_path(&dest, todo) {
                stamp_column_dates(&moved, task.column, todo);
            }
            Some(format!(
                "{id} is due {due}, within {days} days: moved straight to {todo}"
            ))
        }
        Err(err) => Some(format!(
            "{id} is due {due} but stays in the backlog: {err:#}"
        )),
    }
}

/// Writes when a ticket entered todo and doing for a move `from` -> `to`
/// (ticket 252). A failure is only a warning: the move happened.
fn stamp_column_dates(task: &Task, from: Column, to: Column) -> Option<String> {
    let today = crate::recur::today().to_string();
    let (todo, doing) = (task.todo_since(), task.doing_since());
    let (want_todo, want_doing) = match to.kind() {
        Kind::Backlog => (None, None),
        Kind::Todo if from.is(Kind::Backlog) => (Some(today), None),
        Kind::Todo => (todo.clone().or(Some(today)), None),
        Kind::Doing if from.is(Kind::Doing) => (todo.clone(), doing.clone().or(Some(today))),
        Kind::Doing => (todo.clone(), Some(today)),
        Kind::Done => return None,
    };
    if (&want_todo, &want_doing) == (&todo, &doing) {
        return None;
    }
    task.hash()
        .and_then(|h| task.set_column_dates(h, want_todo.as_deref(), want_doing.as_deref()))
        .err()
        .map(|err| format!("couldn't record when it moved: {err:#}"))
}

/// Move a task into `todo/` and put the working tree on its branch — what
/// `rite start` used to do on entry to `doing/`; the trigger moved earlier, to
/// `todo/`-entry, because "todo/ is what you have committed to" — the
/// workspace should be ready once you've committed to the work, not only once
/// you start typing (docs/adr/0006).
///
/// Order matters: the `git mv` is staged first so it travels with the switch
/// and lands on the task's own branch.
pub fn enter_todo(board: &Board, task: &Task, options: &TodoOptions) -> Result<Outcome> {
    // Read everything off the ticket before the move — afterwards the path is
    // stale. A ticket with no branch type has nothing to check out, so there's
    // nothing to switch to unless a branch was asked for explicitly.
    let wanted = match &options.branch {
        Some(name) => Some(name.clone()),
        None => match task.branch() {
            Some(name) => Some(name),
            // A ticket edited by hand may name a branch type but no branch.
            None => match task.branch_type() {
                Some(kind) => {
                    let name = branch::for_title(&kind, &task.title())?;
                    (!name.is_empty()).then_some(name)
                }
                None => None,
            },
        },
    };

    // Where the branch goes (ticket 107): the clone of the repository the
    // ticket is about, when it names one — not whichever repository rite
    // happens to be run from. Read before the move, like everything else.
    let repository = task.repository();
    // `none` says no branch on purpose (ticket 066), so nothing to say.
    let on_purpose = task.branch_type().as_deref() == Some(branch::NO_BRANCH);
    let branching =
        !(options.no_branch || crate::renamed::var("RITE_NO_BRANCH").as_deref() == Ok("1"));

    // A dedicated tickets repository holds tickets, not code (ticket 053):
    // a ticket that wants a branch must say which repository it's about,
    // or its branch would land here. Refused before the move, so nothing
    // is half done. A board inside a code repository (`rite init
    // --local-only` there) is that code repository, and branches as before.
    if branching && wanted.is_some() && repository.is_none() && board.is_tickets_board() {
        let id = task.id_display();
        bail!(
            "{id} wants a branch but names no repository, and this board's repository only holds tickets: \
             set one with `rite set {id} repository <name>`, or move it with --no-branch"
        );
    }

    let mut outcome = move_to(
        board,
        task,
        options.to.unwrap_or(Column::Todo),
        options.force,
    )?;

    if !branching {
        return Ok(outcome);
    }
    let target = match &repository {
        None => board.root.clone(),
        Some(name) => match crate::registry::clone_of(&board.root, name) {
            Some(clone) => clone,
            None => {
                outcome.events.push(Event::Info(format!(
                    "{name} isn't cloned on this machine; left its branch alone"
                )));
                return Ok(outcome);
            }
        },
    };
    let Some(name) = wanted else {
        if !on_purpose {
            outcome.events.push(Event::Warn(
                "no branch type on this ticket; left the branch alone".to_string(),
            ));
        }
        return Ok(outcome);
    };
    branch::validate(&name)?;
    let place = repository
        .as_deref()
        .map(|repo| format!(" in {repo}"))
        .unwrap_or_default();
    if git::current_branch(&target).as_deref() == Some(name.as_str()) {
        outcome
            .events
            .push(Event::Info(format!("already on {name}{place}")));
        return Ok(outcome);
    }
    let exists = git::branch_exists(&target, &name);
    git::switch_branch(&target, &name, !exists)?;
    outcome.events.push(Event::Info(format!(
        "{} {name}{place}",
        if exists { "switched to" } else { "branch" }
    )));
    Ok(outcome)
}

/// Best-effort: a failure here (no git email configured, or someone else
/// grabbing the lease in the instant between the move and this call) must
/// never undo a move that already succeeded — it only gets a note, the same
/// tone as "no branch type on this ticket; left the branch alone".
fn maybe_take_lease(board: &Board, task: &Task) -> Option<String> {
    let Ok(hash) = task.hash() else { return None };
    let hours = board_settings::load(board).lease_hours();
    task.take_lease(hash, hours)
        .err()
        .map(|err| format!("could not claim the lease automatically: {err:#}"))
}

/// Everything a front end gathers to make or change a ticket.
#[derive(Debug, Default, Clone)]
pub struct TicketFields {
    pub title: String,
    /// Who it's assigned to, in order; empty for nobody (ticket 254).
    pub assignees: Vec<String>,
    /// The repository it's about. On creation, `None` or empty means none;
    /// on an edit, `None` leaves it as it is and `Some("")` clears it.
    pub repository: Option<String>,
    /// One of `task::VALID_POINTS`, or empty.
    pub points: String,
    /// The epic or sprint to file it under, if any.
    pub epic: Option<String>,
    pub tags: Vec<String>,
    /// A branch type, or empty for a ticket with no branch.
    pub branch_type: String,
    /// A ref (id or slug), or empty.
    pub parent: String,
    pub related: Vec<String>,
    /// A ref (id or slug) for the ticket this one is a copy of, or empty.
    /// Only creation reads it; an edit leaves the link alone.
    pub clone_of: String,
    /// The ticket body — everything after the frontmatter.
    pub body: String,
    /// `low`, `medium` or `high`; empty keeps what the template (on
    /// creation) or the ticket (on an edit) says.
    pub priority: String,
    /// On creation, refs of the tickets it requires (ticket 214), linked
    /// both ways in the same commit. Ignored on an edit.
    pub requires: Vec<String>,
    /// On creation, refs of tickets that require it (ticket 227: riteui's
    /// Save and New, for a ticket found to depend on one that's missing),
    /// linked in the same commit.
    pub required_by: Vec<String>,
    /// On creation, tickets that stand to the new one as the kind given
    /// (ticket 372: riteui's Add link, `(ref, kind)`), linked both ways in
    /// the same commit.
    pub linked_from: Vec<(String, crate::links::LinkType)>,
}

/// What a new ticket's points, priority and epic would be refused for, checked
/// before anything is written (ticket 382): writing first and checking after
/// left an uncommitted ticket behind when a value was wrong. Mirrors the
/// rules `set_points`, `set_priority` and `Board::set_epic` apply: points
/// from `VALID_POINTS`, a priority from `PRIORITIES`, and an epic or sprint
/// that exists in the backlog, isn't started or closed, and, for a sprint,
/// gets a sized ticket that isn't a draft. `due` is refused with an epic.
pub fn check_new_ticket(
    board: &Board,
    points: &str,
    priority: &str,
    epic: Option<&str>,
    draft: bool,
    due: Option<&str>,
) -> Result<()> {
    crate::task::validate_points(points.trim())?;
    if !priority.trim().is_empty() {
        crate::task::validate_priority(priority.trim())?;
    }
    let Some(epic) = epic.map(str::trim).filter(|e| !e.is_empty()) else {
        return Ok(());
    };
    if !board.epics_in(Column::Backlog)?.iter().any(|e| e == epic) {
        bail!("no epic or sprint called {epic} in the backlog (rite group lists them)");
    }
    let group = board.group(epic)?;
    if !group.accepts_changes() {
        bail!("{epic} {}: it takes no new tickets", group.locked_word());
    }
    if group.is_sprint() {
        if points.trim().is_empty() {
            bail!("the ticket needs points before it can go in {epic}");
        }
        if draft {
            bail!("a draft can't go in a sprint: {epic}");
        }
    }
    board.refuse_due_in_epic("the new ticket", Some(epic), due)?;
    Ok(())
}

/// Make a ticket the way `rite new` does: render it from the templates, write
/// it into `backlog/`, size it, file it under its epic, and auto-commit.
/// `outcome.dest` is where it landed.
pub fn create_ticket(board: &Board, fields: &TicketFields) -> Result<Outcome> {
    // Every ticket it requires must exist before anything is written.
    for reference in fields.requires.iter().chain(&fields.required_by) {
        board.find(reference)?;
    }
    let draft_tag = fields
        .tags
        .iter()
        .any(|t| t.eq_ignore_ascii_case(crate::task::DRAFT_TAG));
    check_new_ticket(
        board,
        &fields.points,
        &fields.priority,
        fields.epic.as_deref(),
        draft_tag,
        None,
    )?;
    for (reference, kind) in &fields.linked_from {
        crate::links::can_link_to_new(&board.find(reference)?, *kind)?;
    }
    let templates = crate::templates::Templates::load(board)?;
    let draft = crate::create::Draft::prepare(
        board,
        &templates,
        crate::create::NewTicket {
            title: fields.title.trim().to_string(),
            template: crate::templates::DEFAULT_TEMPLATE.to_string(),
            branch_type: fields.branch_type.trim().to_string(),
            assignees: fields.assignees.clone(),
            repository: fields.repository.clone().unwrap_or_default(),
            parent: Some(fields.parent.trim().to_string()).filter(|p| !p.is_empty()),
            related: fields.related.clone(),
            clone_of: Some(fields.clone_of.trim().to_string()).filter(|c| !c.is_empty()),
            tags: fields.tags.clone(),
            extra: Default::default(),
            land_in: fields.epic.as_ref().map(|_| Column::Backlog),
        },
    )?;
    // `write` takes the whole file, not just the notes — the same shape `rite
    // new` hands it after the editor has been over the rendered ticket.
    let text = crate::model::splice_body(&draft.body, &fields.body);
    // A new ticket's id isn't allocated until `write` runs, so the commit
    // message can't be built up front the way `with_auto_commit` wants it.
    let before = snapshot(board);
    let created = draft.write(board, Some(text))?;
    if !fields.points.trim().is_empty() {
        created
            .task
            .set_points(created.task.hash()?, fields.points.trim())?;
    }
    if !fields.priority.trim().is_empty() {
        created
            .task
            .set_priority(created.task.hash()?, fields.priority.trim())?;
    }
    let mut required_lines = Vec::new();
    for reference in &fields.required_by {
        let other = board.find(reference)?;
        required_lines.extend(crate::links::apply(
            &other,
            crate::links::LinkType::Requires,
            &crate::task::Task::from_path(&created.task.path, created.task.column)
                .unwrap_or_else(|| created.task.clone()),
            false,
            false,
        )?);
    }
    for reference in &fields.requires {
        let other = board.find(reference)?;
        required_lines.extend(crate::links::apply(
            &created.task,
            crate::links::LinkType::Requires,
            &other,
            false,
            false,
        )?);
    }
    for (reference, kind) in &fields.linked_from {
        let other = board.find(reference)?;
        let new = crate::task::Task::from_path(&created.task.path, created.task.column)
            .unwrap_or_else(|| created.task.clone());
        required_lines.extend(crate::links::apply(&other, *kind, &new, false, false)?);
    }
    // Read what the report and the commit message need before filing it under
    // an epic: inside `backlog/` that moves the file, after which `created.task`
    // points at a path that is gone and would read back as an empty title.
    let (id_display, title, file_name) = (
        created.task.id_display(),
        created.task.title(),
        created.task.file_name(),
    );
    let dest = board.set_epic(&created.task, fields.epic.as_deref())?;
    let mut outcome = Outcome::default();
    for id in &created.linked {
        outcome
            .events
            .push(Event::Info(format!("{id} now links to {id_display}")));
    }
    if let Some(reason) = &created.unlinked {
        outcome.events.push(Event::Warn(format!(
            "could not list {id_display} as a clone on {reason}"
        )));
    }
    if let Some(drafted) = &created.drafted {
        outcome.events.push(Event::Warn(drafted.clone()));
    }
    // Where it landed (ticket 225): the board's landing column, or the
    // backlog for one filed straight into an epic or sprint.
    let landed = match fields.epic {
        Some(_) => Column::Backlog,
        None => board.workflow().landing(),
    };
    outcome
        .events
        .push(Event::Info(format!("created {landed}/{file_name}")));
    outcome
        .events
        .extend(required_lines.into_iter().map(Event::Info));
    let message = format!("new: {id_display} \"{title}\"");
    settle_priorities(board);
    if let Some(warning) = commit_since(board, before, &message)? {
        outcome.events.push(Event::Warn(warning));
    }
    outcome.dest = Some(dest);
    Ok(outcome)
}

/// Make a draft ticket titled `title` and link `from` to it as `kind`, in
/// one commit (ticket 372): riteui's Add link, for a ticket thought of while
/// breaking the work down, before anyone has written it. It goes on
/// `from`'s board, in its landing column, tagged DRAFT, with `from`'s
/// repository and branch type, so it's ready once DRAFT comes off. A
/// repository this machine hasn't cloned can't go on a new ticket, so then
/// it takes neither, rather than refusing.
pub fn create_linked(
    board: &Board,
    from: &Task,
    title: &str,
    kind: crate::links::LinkType,
) -> Result<Outcome> {
    let repository = from
        .repository()
        .filter(|r| crate::task::check_repository(&board.root, r).is_ok());
    let branch_type = match repository {
        Some(_) => from.branch_type().unwrap_or_default(),
        None => String::new(),
    };
    let fields = TicketFields {
        title: title.trim().to_string(),
        repository,
        branch_type,
        tags: vec![crate::task::DRAFT_TAG.to_string()],
        linked_from: vec![(from.id_display(), kind)],
        ..TicketFields::default()
    };
    create_ticket(board, &fields)
}

/// Copy a ticket: a new backlog ticket with the original's title, body, tags,
/// branch type, repository, parent, priority and points, linked to it as a
/// clone (`clone-of` on the copy, `clones` on the original). It is raised
/// unassigned, unleased and unsized into no epic; its checklist starts
/// unticked and it lists no children, since those belong to the original.
pub fn clone_ticket(board: &Board, original: &Task) -> Result<Outcome> {
    let fields = TicketFields {
        title: original.title(),
        repository: original.repository(),
        points: original.points().unwrap_or_default(),
        tags: original.tags(),
        branch_type: original.branch_type().unwrap_or_default(),
        parent: original.parent().unwrap_or_default(),
        clone_of: original.id_display(),
        body: clone_body(&original.body()?),
        priority: original.priority(),
        ..TicketFields::default()
    };
    create_ticket(board, &fields)
}

/// The body of a copy: the original's, less its `## Children` section, with
/// every ticked checklist item unticked.
pub(crate) fn clone_body(body: &str) -> String {
    let mut out = String::new();
    let mut in_children = false;
    for line in body.lines() {
        if line.starts_with("## ") {
            in_children = line.trim_end() == "## Children";
        }
        if in_children {
            continue;
        }
        let line = match line.trim_start() {
            rest if rest.starts_with("- [x]") || rest.starts_with("- [X]") => {
                let indent = &line[..line.len() - rest.len()];
                format!("{indent}- [ ]{}", &rest["- [x]".len()..])
            }
            _ => line.to_string(),
        };
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// Link `from` to each of `to` as `kind` — or unlink them, with `remove` —
/// recording the opposite on the other ticket unless `one_way` (ticket 207).
/// Every end is one change in history: one auto-commit, `link: 001 -> 002`.
/// The events are a line for each ticket that changed.
pub fn link_tickets(
    board: &Board,
    from: &Task,
    kind: crate::links::LinkType,
    to: &[Task],
    remove: bool,
    one_way: bool,
) -> Result<Outcome> {
    let id = from.id_display();
    let ids: Vec<String> = to.iter().map(|other| other.id_display()).collect();
    let subject = format!("{id} -> {}", ids.join(", "));
    let verb = if remove { "unlink" } else { "link" };
    let (lines, warning) = with_auto_commit(board, verb, &subject, &from.title(), || {
        let mut lines = Vec::new();
        for other in to {
            lines.extend(crate::links::apply(from, kind, other, remove, one_way)?);
        }
        Ok(lines)
    })?;
    let mut outcome = Outcome::default();
    outcome.events.extend(lines.into_iter().map(Event::Info));
    if let Some(warning) = warning {
        outcome.events.push(Event::Warn(warning));
    }
    Ok(outcome)
}

/// Make `task`'s requires links exactly `wanted` (ticket 214): those not
/// there yet are made, those not listed are removed, each on both tickets,
/// in one commit (`rite set <ref> requires 3,7`; an empty list clears them).
pub fn set_requires(board: &Board, task: &Task, wanted: &[Task]) -> Result<Outcome> {
    use crate::links::{LinkType, apply};
    let have = crate::requires::required(board, task)?;
    let (id, title) = (task.id_display(), task.title());
    let (lines, warning) = with_auto_commit(board, "set", &id, &title, || {
        let mut lines = Vec::new();
        for gone in have
            .iter()
            .filter(|h| !wanted.iter().any(|w| w.path == h.path))
        {
            lines.extend(apply(task, LinkType::Requires, gone, true, false)?);
        }
        for new in wanted
            .iter()
            .filter(|w| !have.iter().any(|h| h.path == w.path))
        {
            lines.extend(apply(task, LinkType::Requires, new, false, false)?);
        }
        if lines.is_empty() {
            lines.push(format!("{id} already requires exactly that"));
        }
        Ok(lines)
    })?;
    let mut outcome = Outcome::default();
    outcome.events.extend(lines.into_iter().map(Event::Info));
    if let Some(warning) = warning {
        outcome.events.push(Event::Warn(warning));
    }
    Ok(outcome)
}

/// Change the type of the link between `from` and `to` from `old` to `new`
/// (ticket 213): both ends, one commit (`relink: 012 -> 016 "…"`). The way
/// past "done waits": a requires or child-of link changed to relates-to
/// holds nothing back.
pub fn retype_link(
    board: &Board,
    from: &Task,
    old: crate::links::LinkType,
    new: crate::links::LinkType,
    to: &Task,
) -> Result<Outcome> {
    let subject = format!("{} -> {}", from.id_display(), to.id_display());
    let (lines, warning) = with_auto_commit(board, "relink", &subject, &from.title(), || {
        crate::links::retype(from, old, new, to)
    })?;
    let mut outcome = Outcome::default();
    outcome.events.extend(lines.into_iter().map(Event::Info));
    if let Some(warning) = warning {
        outcome.events.push(Event::Warn(warning));
    }
    Ok(outcome)
}

/// A repository and a branch type go together (ticket 066), on a save:
/// `check` before writing, which refuses one past the backlog; then
/// [`settle_pairing`] after it.
pub fn check_pairing(
    board: &Board,
    task: &Task,
    repository: &str,
    branch_type: &str,
) -> Result<Option<String>> {
    crate::pairing::check(
        board,
        &task.id_display(),
        task.column,
        repository,
        branch_type,
    )
}

/// After a save [`check_pairing`] said must be a draft: the DRAFT tag (back)
/// on, and what to tell the person.
pub fn settle_pairing(task: &Task, why: Option<String>) -> Result<Option<String>> {
    let Some(why) = why else {
        return Ok(None);
    };
    task.set_draft(task.hash()?, true)?;
    Ok(Some(crate::pairing::drafted(&task.id_display(), &why)))
}

/// Save every editable field of an existing ticket in one write, checked
/// against `expected` — the fingerprint taken when the form opened — so a
/// ticket that changed underneath it is refused with the conflict rather than
/// overwritten. Auto-commits like `rite edit`.
pub fn edit_ticket(
    board: &Board,
    task: &Task,
    expected: crate::task::ContentHash,
    fields: &TicketFields,
) -> Result<Outcome> {
    let (id_display, title) = (task.id_display(), task.title());
    let repository = match &fields.repository {
        Some(repository) => crate::task::repo_name(repository),
        None => task.repository().unwrap_or_default(),
    };
    let why = check_pairing(board, task, &repository, &fields.branch_type)?;
    let (drafted, warning) = with_auto_commit(board, "edit", &id_display, &title, || {
        task.apply_edit(
            expected,
            board,
            crate::task::EditFields {
                title: &fields.title,
                assignees: &fields.assignees,
                points: &fields.points,
                tags: &fields.tags,
                branch_type: &fields.branch_type,
                parent: &fields.parent,
                related: &fields.related,
                body: &fields.body,
            },
        )?;
        if let Some(repository) = &fields.repository
            && crate::task::repo_name(repository) != task.repository().unwrap_or_default()
        {
            task.set_repository(task.hash()?, repository)?;
        }
        if !fields.priority.trim().is_empty() && fields.priority.trim() != task.priority() {
            crate::auto_priority::set_by_hand(board, task, fields.priority.trim())?;
        }
        let drafted = settle_pairing(task, why)?;
        board.set_epic(task, fields.epic.as_deref())?;
        Ok(drafted)
    })?;
    let mut outcome = Outcome::default();
    outcome
        .events
        .push(Event::Info(format!("saved {id_display}")));
    if let Some(drafted) = drafted {
        outcome.events.push(Event::Warn(drafted));
    }
    if let Some(warning) = warning {
        outcome.events.push(Event::Warn(warning));
    }
    Ok(outcome)
}

/// Save a ticket's whole text, as an editor showing the raw file does —
/// hash-checked against `expected` (taken when the editor opened) and
/// auto-committed like `rite edit`. See [`Task::set_text`] for the checks.
pub fn save_text(
    board: &Board,
    task: &Task,
    expected: crate::task::ContentHash,
    text: &str,
) -> Result<Outcome> {
    let (id_display, title) = (task.id_display(), task.title());
    // A ticket from before `created-by` (ticket 155) gets it from git's
    // first commit as it's saved; one that has it keeps it.
    let text = match crate::authorship::created(board, task) {
        Some(c) if !c.email.is_empty() => {
            crate::authorship::ensure_created_by(text, &format!("{} <{}>", c.name, c.email))?
        }
        _ => text.to_string(),
    };
    // A priority changed in the editor is set by hand (ticket 215): `auto`
    // goes to auto priority, anything else pins when override is on.
    let before = task.priority();
    let wanted = crate::task::meta_in(&text, "priority")
        .map(|p| p.trim().trim_matches('"').to_lowercase())
        .filter(|p| !p.is_empty());
    let text = match wanted.as_deref() {
        Some("auto") => crate::task::set_meta(&text, "priority", &format!("{before:?}"))?,
        _ => text,
    };
    // A repository and a branch type go together (ticket 066): refused past
    // the backlog, and a draft in it, even if the editor took DRAFT off.
    let drafted = crate::pairing::check_text(board, &id_display, task.column, &text)?
        .map(|why| crate::pairing::drafted(&id_display, &why));
    let text = match drafted {
        Some(_) => crate::pairing::with_draft_tag(&text)?,
        None => text,
    };
    // A due date can't go on a ticket in an epic (ticket 253).
    let due = crate::task::meta_in(&text, "due")
        .map(|d| d.trim().trim_matches('"').to_string())
        .filter(|d| !d.is_empty());
    let epic = match task.column.kind() {
        Kind::Backlog => task.epic.clone(),
        _ => crate::task::meta_in(&text, "epic").map(|e| e.trim().trim_matches('"').to_string()),
    };
    board.refuse_due_in_epic(&id_display, epic.as_deref(), due.as_deref())?;
    let due_changed = due != task.due();
    let (moved, warning) = with_auto_commit(board, "edit", &id_display, &title, || {
        task.set_text(expected, &text)?;
        if let Some(wanted) = wanted.filter(|w| *w == "auto" || *w != before) {
            crate::auto_priority::set_by_hand(board, task, &wanted)?;
        }
        // Given a due date that's soon: straight to todo (ticket 253).
        Ok(if due_changed {
            Task::from_path(&task.path, task.column).and_then(|t| straight_to_todo(board, &t))
        } else {
            None
        })
    })?;
    let mut outcome = Outcome::default();
    outcome
        .events
        .push(Event::Info(format!("saved {id_display}")));
    if let Some(drafted) = drafted {
        outcome.events.push(Event::Warn(drafted));
    }
    if let Some(moved) = moved {
        outcome.events.push(Event::Info(moved));
    }
    if let Some(warning) = warning {
        outcome.events.push(Event::Warn(warning));
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_message_leaves_out_an_empty_detail() {
        assert_eq!(
            commit_message("doing", "007", "Fix the thing"),
            "doing: 007 \"Fix the thing\""
        );
        assert_eq!(
            commit_message("register", "typofix", ""),
            "register: typofix"
        );
    }

    fn board_with(dir: &std::path::Path, name: &str, column: Column) -> (Board, Task) {
        let board = Board::at(dir.join("tasks"));
        board.create().expect("board");
        let body = format!("---\nid: \"{}\"\ntitle: \"T\"\n---\n\nbody\n", &name[..3]);
        let path = board.dir(column).join(name);
        std::fs::write(&path, body).unwrap();
        let task = Task::from_path(&path, column).unwrap();
        (board, task)
    }

    /// `days` from today, as `YYYY-MM-DD`.
    fn in_days(days: i64) -> String {
        (crate::recur::today() + time::Duration::days(days)).to_string()
    }

    #[test]
    fn a_ticket_due_soon_goes_straight_to_todo_and_records_when() {
        let dir = tempfile::tempdir().unwrap();
        let (board, task) = board_with(dir.path(), "001-t.md", Column::Backlog);
        task.set_due(task.hash().unwrap(), &in_days(2)).unwrap();
        let task = Task::from_path(&task.path, Column::Backlog).unwrap();
        let line = straight_to_todo(&board, &task).unwrap();
        assert!(line.contains("moved straight to todo"), "{line}");
        let moved = board.find("1").unwrap();
        assert_eq!(moved.column, Column::Todo);
        assert_eq!(moved.todo_since(), Some(crate::recur::today().to_string()));
    }

    #[test]
    fn a_ticket_due_later_a_draft_or_one_in_a_sprint_stays_in_the_backlog() {
        let dir = tempfile::tempdir().unwrap();
        let (board, task) = board_with(dir.path(), "001-t.md", Column::Backlog);
        task.set_due(task.hash().unwrap(), &in_days(10)).unwrap();
        let later = Task::from_path(&task.path, Column::Backlog).unwrap();
        assert_eq!(straight_to_todo(&board, &later), None);
        // A draft stays, however soon.
        later.set_due(later.hash().unwrap(), &in_days(1)).unwrap();
        let soon = Task::from_path(&task.path, Column::Backlog).unwrap();
        soon.set_draft(soon.hash().unwrap(), true).unwrap();
        let draft = Task::from_path(&task.path, Column::Backlog).unwrap();
        assert_eq!(straight_to_todo(&board, &draft), None);
        // Nor when the board says 0.
        draft.set_draft(draft.hash().unwrap(), false).unwrap();
        std::fs::write(dir.path().join(".settings"), "due-to-todo-days = 0\n").unwrap();
        let ready = Task::from_path(&task.path, Column::Backlog).unwrap();
        assert_eq!(straight_to_todo(&board, &ready), None);
        assert_eq!(board.find("1").unwrap().column, Column::Backlog);
    }

    #[test]
    fn a_ticket_with_a_due_date_cant_go_into_an_epic_but_can_into_a_sprint() {
        let dir = tempfile::tempdir().unwrap();
        let (board, task) = board_with(dir.path(), "001-t.md", Column::Backlog);
        for (name, kind) in [
            ("checkout", crate::group::Kind::Epic),
            ("s1", crate::group::Kind::Sprint),
        ] {
            board
                .create_group(&crate::group::Group {
                    dir: board.dir(Column::Backlog).join(name),
                    name: name.to_string(),
                    kind,
                    description: String::new(),
                    days: crate::group::DEFAULT_DAYS,
                    state: crate::group::State::Pending,
                })
                .unwrap();
        }
        task.set_due(task.hash().unwrap(), &in_days(20)).unwrap();
        task.set_points(task.hash().unwrap(), "3").unwrap();
        let dated = Task::from_path(&task.path, Column::Backlog).unwrap();
        let err = board.set_epic(&dated, Some("checkout")).unwrap_err();
        assert!(
            format!("{err:#}").contains("can't be in the epic checkout"),
            "{err:#}"
        );
        board.set_epic(&dated, Some("s1")).unwrap();
        assert!(
            board
                .refuse_due_in_epic("001", Some("checkout"), Some(""))
                .is_ok()
        );
        assert!(
            board
                .refuse_due_in_epic("001", None, Some("2026-10-30"))
                .is_ok()
        );
    }

    #[test]
    fn an_epics_tickets_move_to_todo_together_keeping_their_epic_and_drafts_stay() {
        let dir = tempfile::tempdir().unwrap();
        let (board, _) = board_with(dir.path(), "009-loose.md", Column::Backlog);
        board
            .create_group(&crate::group::Group {
                dir: board.dir(Column::Backlog).join("updates"),
                name: "updates".to_string(),
                kind: crate::group::Kind::Epic,
                description: String::new(),
                days: crate::group::DEFAULT_DAYS,
                state: crate::group::State::Pending,
            })
            .unwrap();
        for (file, tags) in [
            ("001-a.md", "[]"),
            ("002-b.md", "[]"),
            ("003-c.md", "[DRAFT]"),
        ] {
            std::fs::write(
                board.dir(Column::Backlog).join("updates").join(file),
                format!(
                    "---\nid: \"{}\"\ntitle: \"T\"\ntags: {tags}\n---\n\nbody\n",
                    &file[..3]
                ),
            )
            .unwrap();
        }
        let result = group_to_todo(&board, "updates").unwrap();
        assert_eq!(result.moved, ["001", "002"]);
        assert_eq!(result.skipped.len(), 1, "{:?}", result.skipped);
        assert!(
            result.skipped[0].1.contains("draft"),
            "{:?}",
            result.skipped
        );
        for id in ["1", "2"] {
            let moved = board.find(id).unwrap();
            assert_eq!(moved.column, Column::Todo);
            assert_eq!(moved.epic_meta().as_deref(), Some("updates"));
            assert!(moved.todo_since().is_some());
        }
        // The loose ticket and the draft stay.
        assert_eq!(board.find("9").unwrap().column, Column::Backlog);
        assert_eq!(board.find("3").unwrap().column, Column::Backlog);
        assert!(group_to_todo(&board, "nonesuch").is_err());
    }

    #[test]
    fn a_copy_leaves_out_the_children_and_unticks_the_checklist() {
        let body = "## Notes\n\nkeep\n\n## Acceptance criteria\n\n- [x] done\n  - [X] nested\n- [ ] open\n\n## Children\n\n- [[002-child]]\n";
        let copy = clone_body(body);
        assert!(!copy.contains("Children"), "{copy}");
        assert!(!copy.contains("002-child"), "{copy}");
        assert!(
            copy.contains("- [ ] done\n  - [ ] nested\n- [ ] open\n"),
            "{copy}"
        );
        assert!(copy.contains("## Notes\n\nkeep"), "{copy}");
    }

    const TEAM: &str = "Triage=triage:backlog, Backlog=backlog:backlog, To do=todo:todo, \
                        Doing=doing:doing, Review=review:doing, Done=done:done, Released=released:done";

    /// A board with a team's workflow (ticket 223): Triage before the
    /// backlog, Review after doing, Released after done. A git repository
    /// with an identity, auto-commit off, and tickets 001 to `n`.
    fn team_board(dir: &std::path::Path, n: u32) -> Board {
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@t.co"],
            vec!["config", "user.name", "t"],
        ] {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .status()
                .unwrap();
        }
        std::fs::write(
            dir.join(".settings"),
            format!("commit = off\nsync = off\ncolumns = {TEAM}\n"),
        )
        .unwrap();
        let board = Board::at(dir.join("tasks"));
        board.create().unwrap();
        for id in 1..=n {
            let body = format!("---\nid: \"{id:03}\"\ntitle: \"T{id}\"\n---\n\nbody\n");
            std::fs::write(board.dir(Column::Doing).join(format!("{id:03}-t.md")), body).unwrap();
        }
        board
    }

    fn column_of(board: &Board, id: &str) -> Column {
        board.find(id).unwrap().column
    }

    #[test]
    fn a_team_workflow_leases_in_every_doing_kind_column_and_releases_after() {
        let dir = tempfile::tempdir().unwrap();
        let board = team_board(dir.path(), 1);
        let review = board.workflow().find("review").unwrap();
        // The board made the extra folders too.
        assert!(board.dir(review).is_dir());
        move_to(&board, &board.find("001").unwrap(), review, false).unwrap();
        let task = board.find("001").unwrap();
        assert_eq!(task.column, review);
        assert!(task.lease_active(), "Review is a doing-kind column: locked");
        move_to(&board, &task, Column::Done, false).unwrap();
        let task = board.find("001").unwrap();
        assert!(!task.lease_active(), "released on reaching done");
        // Doing to Done skips Review: one column at a time.
        assert!(!board.workflow().next_to(Column::Doing, Column::Done));
    }

    #[test]
    fn a_doing_kind_column_has_its_own_wip_limit() {
        let dir = tempfile::tempdir().unwrap();
        let board = team_board(dir.path(), 2);
        std::fs::write(
            dir.path().join(".settings"),
            format!("commit = off\nsync = off\ncolumns = {TEAM}\nwip-limit-review = 1\n"),
        )
        .unwrap();
        let review = board.workflow().find("review").unwrap();
        move_to(&board, &board.find("001").unwrap(), review, false).unwrap();
        let err = move_to(&board, &board.find("002").unwrap(), review, false).unwrap_err();
        assert!(
            err.downcast_ref::<crate::board::WipLimitExceeded>()
                .is_some(),
            "{err:#}"
        );
        assert_eq!(column_of(&board, "002"), Column::Doing);
    }

    #[test]
    fn done_waits_in_every_done_kind_column_and_any_of_them_counts_as_done() {
        let dir = tempfile::tempdir().unwrap();
        let board = team_board(dir.path(), 2);
        let review = board.workflow().find("review").unwrap();
        let released = board.workflow().find("released").unwrap();
        crate::links::apply(
            &board.find("001").unwrap(),
            crate::links::LinkType::Requires,
            &board.find("002").unwrap(),
            false,
            false,
        )
        .unwrap();
        move_to(&board, &board.find("001").unwrap(), review, false).unwrap();
        let err = move_to(&board, &board.find("001").unwrap(), Column::Done, false).unwrap_err();
        assert!(err.to_string().contains("002"), "{err:#}");
        // 002 goes all the way to Released: done for every rule.
        for to in [review, Column::Done, released] {
            move_to(&board, &board.find("002").unwrap(), to, false).unwrap();
        }
        move_to(&board, &board.find("001").unwrap(), Column::Done, false).unwrap();
        assert_eq!(column_of(&board, "001"), Column::Done);
    }

    #[test]
    fn a_started_sprints_tickets_only_move_on_and_a_draft_stays_in_backlog_kinds() {
        use crate::group::{Group, Kind as GroupKind, State};
        let dir = tempfile::tempdir().unwrap();
        let board = team_board(dir.path(), 2);
        let triage = board.workflow().find("triage").unwrap();
        for id in ["001", "002"] {
            let task = board.find(id).unwrap();
            let to = board.dir(Column::Backlog).join(task.file_name());
            std::fs::rename(&task.path, to).unwrap();
        }
        board
            .create_group(&Group {
                dir: board.dir(Column::Backlog).join("s1"),
                name: "s1".into(),
                kind: GroupKind::Sprint,
                description: String::new(),
                days: 10,
                state: State::Pending,
            })
            .unwrap();
        let task = board.find("001").unwrap();
        task.set_points(task.hash().unwrap(), "3").unwrap();
        board
            .set_epic(&board.find("001").unwrap(), Some("s1"))
            .unwrap();
        board.group("s1").unwrap().start().unwrap();
        let err = board
            .move_task(&board.find("001").unwrap(), triage, false)
            .unwrap_err();
        assert!(err.to_string().contains("started sprint s1"), "{err:#}");
        // A draft may go back to Triage (still the backlog) but not on.
        let task = board.find("002").unwrap();
        task.set_draft(task.hash().unwrap(), true).unwrap();
        board
            .move_task(&board.find("002").unwrap(), triage, false)
            .unwrap();
        assert_eq!(column_of(&board, "002"), triage);
    }

    #[test]
    fn moving_to_done_reports_the_move_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let (board, task) = board_with(dir.path(), "001-t.md", Column::Doing);
        let outcome = move_to(&board, &task, Column::Done, false).unwrap();
        assert_eq!(
            outcome.events,
            [Event::Info("moved 001-t.md -> done/".to_string())]
        );
        assert!(board.dir(Column::Done).join("001-t.md").exists());
        assert_eq!(outcome.dest, Some(board.dir(Column::Done).join("001-t.md")));
    }

    #[test]
    fn a_refused_move_is_an_error_and_moves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (board, task) = board_with(dir.path(), "001-t.md", Column::Done);
        assert!(move_to(&board, &task, Column::Done, false).is_err());
        assert!(board.dir(Column::Done).join("001-t.md").exists());
    }

    #[test]
    fn entering_todo_without_a_branch_type_says_so_and_leaves_the_branch() {
        let dir = tempfile::tempdir().unwrap();
        let (board, task) = board_with(dir.path(), "002-t.md", Column::Backlog);
        let outcome = enter_todo(&board, &task, &TodoOptions::default()).unwrap();
        assert!(board.dir(Column::Todo).join("002-t.md").exists());
        assert!(
            outcome.events.contains(&Event::Warn(
                "no branch type on this ticket; left the branch alone".to_string()
            )),
            "{:?}",
            outcome.events
        );
    }

    #[test]
    fn no_branch_skips_the_branch_step_entirely() {
        let dir = tempfile::tempdir().unwrap();
        let (board, task) = board_with(dir.path(), "002-t.md", Column::Backlog);
        let outcome = enter_todo(
            &board,
            &task,
            &TodoOptions {
                no_branch: true,
                ..TodoOptions::default()
            },
        )
        .unwrap();
        assert!(
            !outcome
                .events
                .iter()
                .any(|event| matches!(event, Event::Warn(text) if text.contains("branch")))
        );
    }

    fn empty_board(dir: &std::path::Path) -> Board {
        let board = Board::at(dir.join("tasks"));
        board.create().expect("board");
        board
    }

    fn fields(title: &str) -> TicketFields {
        TicketFields {
            title: title.to_string(),
            points: "3".to_string(),
            tags: vec!["ux".to_string()],
            body: "## Notes\nhello\n".to_string(),
            ..TicketFields::default()
        }
    }

    /// Ticket 259: Add child makes a ticket under the one being edited:
    /// its `parent`, and the parent's `## Children` link, in one commit.
    #[test]
    fn a_new_ticket_can_be_a_child_of_an_existing_one_in_one_commit() {
        let dir = tempfile::tempdir().unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@t.co"],
            vec!["config", "user.name", "t"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .status()
                .unwrap();
        }
        std::fs::write(dir.path().join(".settings"), "sync = off\n").unwrap();
        let board = empty_board(dir.path());
        create_ticket(&board, &fields("Checkout")).unwrap();
        create_ticket(
            &board,
            &TicketFields {
                parent: "001".into(),
                ..fields("Card form")
            },
        )
        .unwrap();
        let child = board.find("002").unwrap();
        assert_eq!(child.parent().as_deref(), Some("001"));
        let parent = crate::eol::read(board.find("001").unwrap().path).unwrap();
        assert!(parent.contains("## Children"), "{parent}");
        assert!(parent.contains("[[002-card-form]]"), "{parent}");
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["show", "--name-only", "--format=%s", "HEAD"])
            .output()
            .unwrap();
        let shown = String::from_utf8_lossy(&out.stdout);
        assert!(shown.contains("001-checkout.md"), "{shown}");
        assert!(shown.contains("002-card-form.md"), "{shown}");
    }

    /// Ticket 372: Add link makes a ticket that doesn't exist yet, a draft
    /// with the open ticket's repository and branch type, linked as the kind
    /// picked, all in one commit.
    #[test]
    fn a_draft_made_from_add_link_is_linked_in_one_commit() {
        use crate::links::LinkType;
        // A project folder: the tickets repository, and `api` cloned beside it.
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("shop-tickets");
        for repo in [&dir, &base.path().join("api")] {
            std::fs::create_dir_all(repo).unwrap();
            for args in [
                vec!["init", "-q"],
                vec!["config", "user.email", "t@t.co"],
                vec!["config", "user.name", "t"],
                vec!["config", "commit.gpgsign", "false"],
            ] {
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(repo)
                    .args(args)
                    .status()
                    .unwrap();
            }
        }
        std::fs::write(dir.join(".settings"), "sync = off\n").unwrap();
        let board = empty_board(&dir);
        create_ticket(
            &board,
            &TicketFields {
                repository: Some("api".into()),
                branch_type: "feature".into(),
                ..fields("Barcode scan API")
            },
        )
        .unwrap();
        let api = board.find("001").unwrap();
        create_linked(&board, &api, " Barcode infrastructure ", LinkType::Requires).unwrap();

        let draft = board.find("002").unwrap();
        assert_eq!(draft.title(), "Barcode infrastructure");
        assert!(draft.is_draft());
        assert_eq!(draft.column, board.workflow().landing());
        assert_eq!(draft.repository().as_deref(), Some("api"));
        assert_eq!(draft.branch_type().as_deref(), Some("feature"));
        let ids: Vec<String> = crate::requires::required(&board, &board.find("001").unwrap())
            .unwrap()
            .iter()
            .map(|t| t.id_display())
            .collect();
        assert_eq!(ids, ["002"]);
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        let shown = git(&["show", "--name-only", "--format=%s", "HEAD"]);
        assert!(
            shown.starts_with("new: 002 \"Barcode infrastructure\""),
            "{shown}"
        );
        assert!(shown.contains("001-barcode-scan-api.md"), "{shown}");
        assert!(shown.contains("002-barcode-infrastructure.md"), "{shown}");
        assert_eq!(
            git(&["status", "--porcelain", "--", "tasks/backlog/*.md"]),
            "",
            "no ticket left uncommitted"
        );

        // Child of a new ticket, for one that already has a parent: refused
        // before anything is written.
        let part = board.find("002").unwrap();
        create_linked(&board, &part, "Another parent", LinkType::ParentOf).unwrap();
        let err = create_linked(
            &board,
            &board.find("003").unwrap(),
            "Second",
            LinkType::ChildOf,
        )
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
        assert!(err.contains("already has parent 002"), "{err}");
        assert!(board.find("004").is_err());
        // A repository not cloned here goes on neither field.
        let elsewhere = board.find("001").unwrap();
        let text = crate::eol::read(&elsewhere.path).unwrap();
        let text = crate::task::set_meta(&text, "repository", "\"web\"").unwrap();
        std::fs::write(&elsewhere.path, text).unwrap();
        create_linked(
            &board,
            &board.find("001").unwrap(),
            "Web form",
            LinkType::Requires,
        )
        .unwrap();
        let web = board.find("004").unwrap();
        assert_eq!(web.repository(), None);
        assert_eq!(web.branch_type(), None);
        // A title that can't be a ticket is refused too.
        assert!(create_linked(&board, &api, "!!!", LinkType::RelatesTo).is_err());
        assert!(board.find("005").is_err());
    }

    /// Ticket 227: Save and New makes the missing ticket already required
    /// by the one being edited, both ends written in the one commit.
    #[test]
    fn a_new_ticket_can_be_required_by_an_existing_one() {
        let dir = tempfile::tempdir().unwrap();
        let board = empty_board(dir.path());
        create_ticket(&board, &fields("Checkout")).unwrap();
        let outcome = create_ticket(
            &board,
            &TicketFields {
                required_by: vec!["001".into()],
                ..fields("Payment provider")
            },
        )
        .unwrap();
        assert!(
            outcome
                .events
                .iter()
                .any(|e| matches!(e, Event::Info(t) if t.contains("001") && t.contains("002"))),
            "{:?}",
            outcome.events
        );
        let checkout = board.find("001").unwrap();
        let ids: Vec<String> = crate::requires::required(&board, &checkout)
            .unwrap()
            .iter()
            .map(|t| t.id_display())
            .collect();
        assert_eq!(ids, ["002"]);
        assert!(
            create_ticket(
                &board,
                &TicketFields {
                    required_by: vec!["999".into()],
                    ..fields("Nowhere")
                }
            )
            .is_err(),
            "an unknown ticket refuses before anything is written"
        );
        assert!(board.find("003").is_err());
    }

    #[test]
    fn creating_a_ticket_writes_it_into_the_backlog_sized_and_tagged() {
        let dir = tempfile::tempdir().unwrap();
        let board = empty_board(dir.path());
        let outcome = create_ticket(&board, &fields("Make a thing")).unwrap();
        let path = outcome.dest.expect("where it landed");
        assert!(path.starts_with(board.dir(Column::Backlog)), "{path:?}");
        let task = Task::from_path(&path, Column::Backlog).unwrap();
        assert_eq!(task.title(), "Make a thing");
        assert_eq!(task.points().as_deref(), Some("3"));
        assert_eq!(task.tags(), ["UX"], "saved in uppercase (ticket 122)");
        assert!(task.body().unwrap().contains("hello"));
        assert!(
            matches!(outcome.events.first(), Some(Event::Info(text)) if text.starts_with("created backlog/001-")),
            "{:?}",
            outcome.events
        );
    }

    #[test]
    fn a_ticket_filed_under_an_epic_reports_where_it_really_is() {
        let dir = tempfile::tempdir().unwrap();
        let board = empty_board(dir.path());
        board
            .create_group(&crate::group::Group {
                dir: board.dir(Column::Backlog).join("Ep"),
                name: "Ep".to_string(),
                kind: crate::group::Kind::Epic,
                description: String::new(),
                days: 0,
                state: crate::group::State::Pending,
            })
            .unwrap();
        let mut with_epic = fields("Filed away");
        with_epic.epic = Some("Ep".to_string());
        let outcome = create_ticket(&board, &with_epic).unwrap();
        let path = outcome.dest.unwrap();
        assert!(
            path.exists(),
            "dest must be where the file is now: {path:?}"
        );
        assert!(
            path.parent().and_then(|dir| dir.file_name()) == Some("Ep".as_ref()),
            "{path:?}"
        );
    }

    #[test]
    fn a_ticket_with_nothing_sluggable_in_its_title_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let board = empty_board(dir.path());
        assert!(create_ticket(&board, &fields("!!!")).is_err());
        assert!(board.tasks_in(Column::Backlog).unwrap().is_empty());
    }

    #[test]
    fn editing_saves_every_field_in_one_write() {
        let dir = tempfile::tempdir().unwrap();
        let board = empty_board(dir.path());
        let path = create_ticket(&board, &fields("Old title"))
            .unwrap()
            .dest
            .unwrap();
        let task = Task::from_path(&path, Column::Backlog).unwrap();
        let hash = task.hash().unwrap();
        let mut edited = fields("New title");
        edited.assignees = vec!["sam".to_string()];
        edited.body = "## Notes\nchanged\n".to_string();
        edit_ticket(&board, &task, hash, &edited).unwrap();
        let task = Task::from_path(&path, Column::Backlog).unwrap();
        assert_eq!(task.title(), "New title");
        assert_eq!(task.assignee().as_deref(), Some("sam"));
        assert!(task.body().unwrap().contains("changed"));
    }

    #[test]
    fn editing_a_ticket_that_changed_underneath_is_refused_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let board = empty_board(dir.path());
        let path = create_ticket(&board, &fields("Old title"))
            .unwrap()
            .dest
            .unwrap();
        let task = Task::from_path(&path, Column::Backlog).unwrap();
        let stale = task.hash().unwrap();
        // Someone else saves first.
        task.set_title(stale, "Theirs").unwrap();
        let result = edit_ticket(&board, &task, stale, &fields("Mine"));
        assert!(result.is_err());
        let task = Task::from_path(&path, Column::Backlog).unwrap();
        assert_eq!(task.title(), "Theirs");
    }
}
