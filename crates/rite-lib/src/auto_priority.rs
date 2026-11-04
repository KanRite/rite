//! Auto priority (ticket 215): a ticket is at least as urgent as the most
//! urgent ticket that requires it, directly or down a chain, so the
//! priorities set on the end goals flow down to the work they wait on.
//! A child is one step below its parent (ticket 371: top, high, medium,
//! low; a low parent gives nothing), so a goal's breakdown spreads across
//! the priorities instead of all taking the goal's. Both rules are applied
//! together until nothing changes, so a raised parent passes its raise
//! down and a raised child lifts what it requires.
//!
//! Off unless the board's `auto-priority` setting is on. The result is
//! written into `priority`, so everything that reads a priority sees it,
//! and a raise is recorded beside it: `priority-from` (the ticket it
//! inherits from) and `priority-own` (its own priority before the raise).
//! Switching it off puts raised tickets back.
//!
//! With `priority-override` on (the default), a priority set by hand pins
//! its ticket (`priority-pinned`) and auto leaves it alone; setting it to
//! `auto` unpins it. With it off, a hand-set priority is only the ticket's
//! own, a floor auto can still raise.
//!
//! [`reconcile`] runs inside `transition::with_auto_commit` (and the two
//! ticket-creating paths), so any change carries its priority updates in
//! the same commit. Done tickets neither push nor change.

use std::cell::RefCell;

use time::Date;

use anyhow::{Result, bail};

use crate::board::{Board, Kind};
use crate::board_settings;
use crate::links::{LinkType, same_id};
use crate::task::{PRIORITIES, Task, meta_in};

const FROM: &str = "priority-from";
const OWN: &str = "priority-own";
const PINNED: &str = "priority-pinned";
/// `priority-from` for a raise by the ticket's own due date (ticket 252).
pub const DUE: &str = "due";
/// `priority-from` for a raise by the time spent in todo, and in doing,
/// against the board's limits (ticket 252).
pub const TODO: &str = "todo";
pub const DOING: &str = "doing";
/// `priority-from` for a raise by the ticket's parent, before its id
/// (`parent:003`, ticket 371), so it can't be read as a requirer's id.
pub const PARENT: &str = "parent:";

thread_local! {
    /// What reconcile changed since a front end last asked.
    static NOTES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// What auto priority changed since this was last called, for a front end
/// to say (`010 raised to high: 012 requires it`).
pub fn take_notes() -> Vec<String> {
    NOTES.with(|n| std::mem::take(&mut *n.borrow_mut()))
}

fn rank(priority: &str) -> usize {
    PRIORITIES.iter().position(|p| *p == priority).unwrap_or(0)
}

/// One ticket as auto priority sees it, read once.
struct Seen {
    task: Task,
    priority: String,
    from: Option<String>,
    own: Option<String>,
    pinned: bool,
    done: bool,
    due: Option<String>,
    created: Option<String>,
    todo_since: Option<String>,
    doing_since: Option<String>,
}

impl Seen {
    fn read(task: Task) -> Option<Seen> {
        let text = crate::eol::read(&task.path).ok()?;
        let get = |key: &str| {
            meta_in(&text, key)
                .map(|v| v.trim().trim_matches('"').to_string())
                .filter(|v| !v.is_empty())
        };
        Some(Seen {
            priority: get("priority").unwrap_or_else(|| "low".to_string()),
            from: get(FROM),
            own: get(OWN),
            pinned: get(PINNED).is_some_and(|v| v != "no" && v != "false"),
            done: task.column.is(Kind::Done),
            due: task.due(),
            created: task.meta("created").filter(|c| !c.is_empty()),
            todo_since: task.todo_since(),
            doing_since: task.doing_since(),
            task,
        })
    }

    /// Its own priority: what it had before auto raised it.
    fn own(&self) -> &str {
        match (&self.from, &self.own) {
            (Some(_), Some(own)) => own,
            _ => &self.priority,
        }
    }
}

/// The priority, by rank in [`PRIORITIES`], a due date asks for on
/// `today` (ticket 252): a step for each quarter of the time from the
/// ticket's creation to its due date used (medium from 25%, high from 50%,
/// top from 75%), so top once it's overdue. One with no creation date is
/// only raised once overdue.
pub fn due_rank(due: Option<&str>, created: Option<&str>, today: Date) -> Option<usize> {
    let due = crate::recur::parse_date(due?).ok()?;
    let Some(created) = created.and_then(|c| crate::recur::parse_date(c).ok()) else {
        return (today > due).then_some(3);
    };
    let window = (due - created).whole_days();
    let used = (today - created).whole_days();
    if window <= 0 {
        return (used >= 0).then_some(3);
    }
    time_rank(used, u32::try_from(window).unwrap_or(u32::MAX))
}

/// The priority a share of a time limit asks for (ticket 252): medium once
/// a quarter of `limit` days is used, high from half, top from three
/// quarters (and past it). A limit of 0 asks for nothing.
pub fn time_rank(used: i64, limit: u32) -> Option<usize> {
    let limit = i64::from(limit);
    if limit == 0 || used < 0 {
        return None;
    }
    match used * 4 {
        q if q >= 3 * limit => Some(3),
        q if q >= 2 * limit => Some(2),
        q if q >= limit => Some(1),
        _ => None,
    }
}

/// Days a ticket has spent in todo (up to entering doing, so it keeps
/// counting what it waited) and in doing, as of `today`, when it's in one
/// of them (ticket 252).
fn time_used(s: &Seen, today: Date) -> (Option<i64>, Option<i64>) {
    let date = |d: &Option<String>| d.as_deref().and_then(|d| crate::recur::parse_date(d).ok());
    let (todo, doing) = (date(&s.todo_since), date(&s.doing_since));
    match s.task.column.kind() {
        Kind::Todo => (todo.map(|t| (today - t).whole_days()), None),
        Kind::Doing => (
            todo.map(|t| (doing.unwrap_or(today) - t).whole_days()),
            doing.map(|d| (today - d).whole_days()),
        ),
        _ => (None, None),
    }
}

/// What raised a ticket: a ticket that requires it, its parent (ticket
/// 371), its own due date, or the time it has spent in todo or doing
/// against the board's limit.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Why {
    Requirer(usize),
    Parent(usize),
    Due,
    Todo(i64, u32),
    Doing(i64, u32),
}

/// One write [`plan`] wants, and what to say about it.
struct Planned {
    at: usize,
    priority: String,
    from: Option<String>,
    own: Option<String>,
    line: String,
}

/// Bring every ticket on `board` in line with its auto priority setting,
/// as of today. Returns (and queues for [`take_notes`]) a line for each
/// change.
pub fn reconcile(board: &Board) -> Result<Vec<String>> {
    reconcile_at(board, crate::recur::today())
}

/// [`reconcile`] with the date passed in.
pub fn reconcile_at(board: &Board, today: Date) -> Result<Vec<String>> {
    let (seen, planned) = plan(board, today)?;
    let mut lines = Vec::new();
    for p in planned {
        let s = &seen[p.at];
        let result = s.task.hash().and_then(|h| {
            s.task.write_priority(
                h,
                &p.priority,
                p.from.as_deref(),
                p.own.as_deref(),
                s.pinned,
            )
        });
        match result {
            Ok(()) => lines.push(p.line),
            Err(err) => lines.push(format!(
                "auto priority left {} alone: {err:#}",
                s.task.id_display()
            )),
        }
    }
    Ok(queue(lines))
}

/// Whether [`reconcile_at`] would change anything: on a board nobody has
/// touched, that's a due date coming near (ticket 252), which a front end
/// that's open can commit without waiting for the next change.
pub fn pending(board: &Board, today: Date) -> bool {
    plan(board, today).is_ok_and(|(_, planned)| !planned.is_empty())
}

/// Works out every priority write auto priority wants, writing nothing.
fn plan(board: &Board, today: Date) -> Result<(Vec<Seen>, Vec<Planned>)> {
    let settings = board_settings::load(board);
    let on = settings.auto_priority();
    let override_on = settings.priority_override();
    let limits = settings.time_limits();
    let seen: Vec<Seen> = board.tasks()?.into_iter().filter_map(Seen::read).collect();
    let mut planned = Vec::new();

    if !on {
        for (at, s) in seen.iter().enumerate().filter(|(_, s)| s.from.is_some()) {
            let own = s.own().to_string();
            planned.push(Planned {
                at,
                line: format!("{} back to {own} (auto priority off)", s.task.id_display()),
                priority: own,
                from: None,
                own: None,
            });
        }
        return Ok((seen, planned));
    }

    // Who requires whom: `requirers[j]` are the tickets that require j.
    let mut requirers: Vec<Vec<usize>> = vec![Vec::new(); seen.len()];
    let index = |id: &str| seen.iter().position(|s| same_id(id, &s.task));
    for (i, s) in seen.iter().enumerate() {
        for link in s.task.links() {
            let Some(j) = index(&link.id) else { continue };
            match link.kind {
                LinkType::Requires if i != j => requirers[j].push(i),
                LinkType::RequiredBy if i != j => requirers[i].push(j),
                _ => {}
            }
        }
    }
    for list in &mut requirers {
        list.sort_by_key(|&i| seen[i].task.id);
        list.dedup();
    }
    // Each ticket's parent (ticket 371): its `parent`, else the ticket
    // whose `## Children` lists it, as `links::waiting_on` reads them.
    let listed: Vec<Vec<String>> = seen
        .iter()
        .map(|s| crate::links::children_stems(&s.task))
        .collect();
    let parent: Vec<Option<usize>> = seen
        .iter()
        .enumerate()
        .map(|(j, s)| {
            let named = s.task.parent().and_then(|raw| {
                let raw = raw.trim().trim_start_matches("[[");
                let id: String = raw.chars().take_while(char::is_ascii_digit).collect();
                index(&id)
            });
            named
                .or_else(|| listed.iter().position(|c| c.contains(&s.task.stem)))
                .filter(|&i| i != j)
        })
        .collect();

    let pinned = |s: &Seen| override_on && s.pinned;
    // Start from each ticket's own, raised by its due date (ticket 252);
    // then raise until nothing changes. Three steps of priority, so this
    // ends, cycles included.
    let mut effective: Vec<usize> = Vec::with_capacity(seen.len());
    let mut source: Vec<Option<Why>> = vec![None; seen.len()];
    for (j, s) in seen.iter().enumerate() {
        if pinned(s) {
            effective.push(rank(&s.priority));
            continue;
        }
        let own = rank(s.own());
        // Its own reasons (ticket 252): the due date, and the time it has
        // spent in todo and doing; the most urgent wins, the first on a tie.
        let (todo, doing) = time_used(s, today);
        let reasons = [
            due_rank(s.due.as_deref(), s.created.as_deref(), today).map(|r| (r, Why::Due)),
            todo.and_then(|used| {
                time_rank(used, limits.todo).map(|r| (r, Why::Todo(used, limits.todo)))
            }),
            doing.and_then(|used| {
                time_rank(used, limits.doing).map(|r| (r, Why::Doing(used, limits.doing)))
            }),
        ];
        let best = reasons
            .into_iter()
            .flatten()
            .filter(|(r, _)| !s.done && *r > own)
            .fold(None, |best: Option<(usize, Why)>, (r, why)| match best {
                Some((b, _)) if b >= r => best,
                _ => Some((r, why)),
            });
        match best {
            Some((r, why)) => {
                effective.push(r);
                source[j] = Some(why);
            }
            None => effective.push(own),
        }
    }
    loop {
        let mut changed = false;
        for j in 0..seen.len() {
            if seen[j].done || pinned(&seen[j]) {
                continue;
            }
            for &i in &requirers[j] {
                if !seen[i].done && effective[i] > effective[j] {
                    effective[j] = effective[i];
                    source[j] = Some(Why::Requirer(i));
                    changed = true;
                }
            }
            // One step below its parent (ticket 371).
            if let Some(p) = parent[j]
                && !seen[p].done
                && effective[p].saturating_sub(1) > effective[j]
            {
                effective[j] = effective[p] - 1;
                source[j] = Some(Why::Parent(p));
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    for (j, s) in seen.iter().enumerate() {
        if s.done || pinned(s) {
            continue;
        }
        let own = s.own().to_string();
        match source[j].filter(|_| effective[j] > rank(&own)) {
            Some(why) => {
                let to = PRIORITIES[effective[j]];
                let (by, line) = match why {
                    Why::Requirer(i) => {
                        let by = seen[i].task.id_display();
                        let line =
                            format!("{} raised to {to}: {by} requires it", s.task.id_display());
                        (by, line)
                    }
                    Why::Parent(i) => {
                        let id = seen[i].task.id_display();
                        let line = format!(
                            "{} raised to {to}: child of {id} ({})",
                            s.task.id_display(),
                            PRIORITIES[effective[i]]
                        );
                        (format!("{PARENT}{id}"), line)
                    }
                    Why::Due => (
                        DUE.to_string(),
                        format!(
                            "{} raised to {to}: due {}",
                            s.task.id_display(),
                            s.due.as_deref().unwrap_or_default()
                        ),
                    ),
                    Why::Todo(used, limit) => (
                        TODO.to_string(),
                        format!(
                            "{} raised to {to}: {used} of {limit} days in todo",
                            s.task.id_display()
                        ),
                    ),
                    Why::Doing(used, limit) => (
                        DOING.to_string(),
                        format!(
                            "{} raised to {to}: {used} of {limit} days in doing",
                            s.task.id_display()
                        ),
                    ),
                };
                if s.priority != to
                    || s.from.as_deref() != Some(by.as_str())
                    || s.own.as_deref() != Some(own.as_str())
                {
                    planned.push(Planned {
                        at: j,
                        priority: to.to_string(),
                        from: Some(by),
                        own: Some(own),
                        line,
                    });
                }
            }
            None if s.from.is_some() || s.priority != own => planned.push(Planned {
                at: j,
                line: format!(
                    "{} back to {own}: nothing more urgent requires it or is its parent{}",
                    s.task.id_display(),
                    if s.due.is_some() {
                        ", and it isn't due soon"
                    } else {
                        ""
                    }
                ),
                priority: own,
                from: None,
                own: None,
            }),
            None => {}
        }
    }
    Ok((seen, planned))
}

/// Records a reconcile that failed outright, for [`take_notes`].
pub fn note_failure(err: &anyhow::Error) {
    NOTES.with(|n| {
        n.borrow_mut()
            .push(format!("auto priority couldn't run: {err:#}"))
    });
}

fn queue(lines: Vec<String>) -> Vec<String> {
    NOTES.with(|n| n.borrow_mut().extend(lines.iter().cloned()));
    lines
}

/// Set a ticket's priority by hand (ticket 215): `rite set <ref> priority`,
/// the editor, the AI tool. It becomes the ticket's own priority, any raise
/// is cleared, and with auto priority and override both on it's pinned.
/// `auto` unpins instead, leaving the priority to auto. Doesn't commit and
/// doesn't reconcile: the caller's `with_auto_commit` does both.
pub fn set_by_hand(board: &Board, task: &Task, value: &str) -> Result<String> {
    let settings = board_settings::load(board);
    let value = value.trim().to_lowercase();
    let id = task.id_display();
    if value == "auto" {
        if !settings.auto_priority() {
            bail!("auto priority is off for this board (`rite auto-priority on`)");
        }
        let Some(s) = Seen::read(task.clone()) else {
            bail!("couldn't read {id}");
        };
        let own = s.own().to_string();
        task.write_priority(task.hash()?, &own, None, None, false)?;
        return Ok(format!("{id} priority follows auto priority again"));
    }
    // Waiting to be triaged is top, full stop (tickets 248, 252).
    if value != crate::task::TOP && task.has_tag(crate::task::TRIAGE_TAG) {
        bail!(
            "{id} is waiting to be triaged, so it's top: take {} off first",
            crate::task::TRIAGE_TAG
        );
    }
    let pin = settings.auto_priority() && settings.priority_override();
    task.write_priority(task.hash()?, &value, None, None, pin)?;
    Ok(if pin {
        format!("{id} priority {value} (pinned: auto priority leaves it)")
    } else {
        format!("{id} priority {value}")
    })
}

/// What `rite show` and the editor say about a ticket's priority, if auto
/// priority has touched it: raised, or pinned.
pub fn note(board: &Board, task: &Task) -> Option<String> {
    let s = Seen::read(task.clone())?;
    let settings = board_settings::load(board);
    if let (Some(from), Some(own)) = (&s.from, &s.own)
        && (from == TODO || from == DOING)
    {
        let limits = settings.time_limits();
        let (todo, doing) = time_used(&s, crate::recur::today());
        let (used, limit) = if from == TODO {
            (todo, limits.todo)
        } else {
            (doing, limits.doing)
        };
        return Some(format!(
            "priority {}: raised by auto priority, {} of {limit} days in {from} (own: {own})",
            s.priority,
            used.unwrap_or_default()
        ));
    }
    if let (Some(from), Some(own)) = (&s.from, &s.own)
        && from == DUE
    {
        return Some(format!(
            "priority {}: raised by auto priority, due {} (own: {own})",
            s.priority,
            s.due.as_deref().unwrap_or("soon")
        ));
    }
    if let (Some(from), Some(own)) = (&s.from, &s.own)
        && let Some(id) = from.strip_prefix(PARENT)
    {
        let theirs = board
            .find(id)
            .map(|p| format!(" ({})", p.priority()))
            .unwrap_or_default();
        return Some(format!(
            "priority {}: raised by auto priority, child of {id}{theirs} (own: {own})",
            s.priority
        ));
    }
    if let (Some(from), Some(own)) = (&s.from, &s.own) {
        return Some(format!(
            "priority {}: raised by auto priority, {from} requires it (own: {own})",
            s.priority
        ));
    }
    (s.pinned && settings.auto_priority() && settings.priority_override()).then(|| {
        format!(
            "priority {} pinned by hand: auto priority leaves it",
            s.priority
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::Column;
    use crate::create::{Draft, NewTicket};
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

    fn parent_of(board: &Board, parent: &str, child: &str) {
        apply(
            &find(board, parent),
            LinkType::ParentOf,
            &find(board, child),
            false,
            false,
        )
        .unwrap();
    }

    fn settings(board: &Board, auto: bool, override_on: bool) {
        let mut s = board_settings::load(board);
        s.set_auto_priority(auto);
        s.set_priority_override(override_on);
        s.save(board).unwrap();
    }

    fn priorities(board: &Board) -> Vec<String> {
        let mut tasks = board.tasks().unwrap();
        tasks.sort_by_key(|t| t.id);
        tasks.iter().map(Task::priority).collect()
    }

    fn set(board: &Board, id: &str, value: &str) {
        let t = find(board, id);
        t.set_priority(t.hash().unwrap(), value).unwrap();
    }

    fn due(board: &Board, id: &str, date: &str) {
        let t = find(board, id);
        t.set_due(t.hash().unwrap(), date).unwrap();
    }

    fn on(y: i32, m: u8, d: u8) -> Date {
        Date::from_calendar_date(y, time::Month::try_from(m).unwrap(), d).unwrap()
    }

    #[test]
    fn auto_priority_is_on_unless_a_board_turns_it_off() {
        let (_dir, board) = board_with(&[]);
        assert!(board_settings::load(&board).auto_priority());
        settings(&board, false, true);
        assert!(!board_settings::load(&board).auto_priority());
    }

    #[test]
    fn a_due_date_raises_a_step_for_each_quarter_from_creation() {
        // Created on the 1st, due on the 21st: 20 days.
        let rank = |today: Date| due_rank(Some("2026-10-21"), Some("2026-10-01"), today);
        assert_eq!(rank(on(2026, 10, 5)), None);
        assert_eq!(rank(on(2026, 10, 6)), Some(1));
        assert_eq!(rank(on(2026, 10, 11)), Some(2));
        assert_eq!(rank(on(2026, 10, 16)), Some(3));
        assert_eq!(rank(on(2026, 10, 30)), Some(3));
        // No creation date: only once overdue. Due the day it was made: top.
        assert_eq!(due_rank(Some("2026-10-21"), None, on(2026, 10, 20)), None);
        assert_eq!(
            due_rank(Some("2026-10-21"), None, on(2026, 10, 22)),
            Some(3)
        );
        assert_eq!(
            due_rank(Some("2026-10-01"), Some("2026-10-01"), on(2026, 10, 1)),
            Some(3)
        );
        assert_eq!(due_rank(None, Some("2026-10-01"), on(2026, 10, 1)), None);
        assert_eq!(
            due_rank(Some("soon"), Some("2026-10-01"), on(2026, 10, 1)),
            None
        );
    }

    /// Sets when a ticket was made, which due dates count from.
    fn created(board: &Board, id: &str, date: &str) {
        let t = find(board, id);
        let text = crate::eol::read(&t.path).unwrap();
        let text = crate::task::set_meta(&text, "created", &format!("{date:?}")).unwrap();
        std::fs::write(&t.path, text).unwrap();
    }

    #[test]
    fn a_ticket_is_raised_as_its_due_date_nears_and_passes_it_on() {
        let (_dir, board) = board_with(&["Ship", "Build"]);
        settings(&board, true, true);
        requires(&board, "1", "2");
        // Made on the 1st, due on the 21st: 20 days, a step every 5.
        created(&board, "1", "2026-10-01");
        due(&board, "1", "2026-10-21");
        reconcile_at(&board, on(2026, 10, 5)).unwrap();
        assert_eq!(priorities(&board), ["low", "low"]);
        assert!(!pending(&board, on(2026, 10, 5)));
        // A quarter used: medium, and what it requires with it.
        assert!(pending(&board, on(2026, 10, 6)));
        let lines = reconcile_at(&board, on(2026, 10, 6)).unwrap();
        assert_eq!(priorities(&board), ["medium", "medium"]);
        assert!(
            lines.contains(&"001 raised to medium: due 2026-10-21".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"002 raised to medium: 001 requires it".to_string()),
            "{lines:?}"
        );
        let note = note(&board, &find(&board, "1")).unwrap();
        assert!(note.contains("due 2026-10-21 (own: low)"), "{note}");
        // Half: high. Three quarters: top.
        reconcile_at(&board, on(2026, 10, 11)).unwrap();
        assert_eq!(priorities(&board), ["high", "high"]);
        reconcile_at(&board, on(2026, 10, 16)).unwrap();
        assert_eq!(priorities(&board), ["top", "top"]);
        // Moved out to the end of the year: back to its own.
        due(&board, "1", "2026-12-31");
        reconcile_at(&board, on(2026, 10, 16)).unwrap();
        assert_eq!(priorities(&board), ["low", "low"]);
    }

    #[test]
    fn each_quarter_of_a_time_limit_raises_a_step() {
        assert_eq!(time_rank(0, 8), None);
        assert_eq!(time_rank(1, 8), None);
        assert_eq!(time_rank(2, 8), Some(1));
        assert_eq!(time_rank(4, 8), Some(2));
        assert_eq!(time_rank(6, 8), Some(3));
        assert_eq!(time_rank(20, 8), Some(3));
        assert_eq!(time_rank(20, 0), None);
    }

    /// Writes when a ticket entered todo and doing, as a move would.
    fn since(board: &Board, id: &str, todo: Option<&str>, doing: Option<&str>) {
        let t = find(board, id);
        t.set_column_dates(t.hash().unwrap(), todo, doing).unwrap();
    }

    #[test]
    fn time_in_todo_raises_a_ticket_and_stays_counted_in_doing() {
        let (_dir, board) = board_with(&["Waiting", "Started"]);
        settings(&board, true, true);
        let mut s = board_settings::load(&board);
        s.set_time_limits(board_settings::TimeLimits { todo: 8, doing: 4 });
        s.save(&board).unwrap();
        board
            .move_task(&find(&board, "1"), Column::Todo, false)
            .unwrap();
        since(&board, "1", Some("2026-10-10"), None);
        // 4 of 8 days in todo: high.
        let lines = reconcile_at(&board, on(2026, 10, 14)).unwrap();
        assert_eq!(priorities(&board)[0], "high");
        assert!(
            lines.contains(&"001 raised to high: 4 of 8 days in todo".to_string()),
            "{lines:?}"
        );
        // 6 of 8: top.
        reconcile_at(&board, on(2026, 10, 16)).unwrap();
        assert_eq!(priorities(&board)[0], "top");
        // Picked up on the 16th: its 6 days in todo still count, so it
        // stays top while doing's own clock starts.
        board
            .move_task(&find(&board, "1"), Column::Doing, false)
            .unwrap();
        since(&board, "1", Some("2026-10-10"), Some("2026-10-16"));
        reconcile_at(&board, on(2026, 10, 17)).unwrap();
        assert_eq!(priorities(&board)[0], "top");
        // A ticket started straight away: doing's clock alone, 2 of 4 days.
        board
            .move_task(&find(&board, "2"), Column::Todo, false)
            .unwrap();
        board
            .move_task(&find(&board, "2"), Column::Doing, false)
            .unwrap();
        since(&board, "2", Some("2026-10-15"), Some("2026-10-15"));
        let lines = reconcile_at(&board, on(2026, 10, 17)).unwrap();
        assert_eq!(priorities(&board)[1], "high");
        assert!(
            lines.contains(&"002 raised to high: 2 of 4 days in doing".to_string()),
            "{lines:?}"
        );
    }

    #[test]
    fn a_pinned_or_done_ticket_is_left_alone_and_off_puts_it_back() {
        let (_dir, board) = board_with(&["Pinned", "Raised"]);
        settings(&board, true, true);
        for id in ["1", "2"] {
            created(&board, id, "2026-10-01");
            due(&board, id, "2026-10-19");
        }
        set_by_hand(&board, &find(&board, "1"), "low").unwrap();
        reconcile_at(&board, on(2026, 10, 18)).unwrap();
        assert_eq!(priorities(&board), ["low", "top"]);
        settings(&board, false, true);
        reconcile_at(&board, on(2026, 10, 18)).unwrap();
        assert_eq!(priorities(&board), ["low", "low"]);
    }

    #[test]
    fn a_chain_inherits_the_most_urgent_priority_and_says_why() {
        let (_dir, board) = board_with(&["Goal", "Middle", "Base", "Other"]);
        set(&board, "001", "high");
        requires(&board, "001", "002");
        requires(&board, "002", "003");
        // Off: nothing moves. (On by default since ticket 252.)
        settings(&board, false, true);
        reconcile(&board).unwrap();
        assert_eq!(priorities(&board), ["high", "low", "low", "low"]);

        settings(&board, true, true);
        let lines = reconcile(&board).unwrap();
        assert_eq!(priorities(&board), ["high", "high", "high", "low"]);
        assert!(
            lines.contains(&"003 raised to high: 002 requires it".to_string()),
            "{lines:?}"
        );
        assert_eq!(
            find(&board, "003").meta("priority-from").as_deref(),
            Some("002")
        );
        assert_eq!(
            find(&board, "003").meta("priority-own").as_deref(),
            Some("low")
        );
        assert!(
            note(&board, &find(&board, "003"))
                .unwrap()
                .contains("002 requires it")
        );
        // Settled: running again changes nothing.
        assert!(reconcile(&board).unwrap().is_empty());

        // The link goes: 003 is back to its own.
        apply(
            &find(&board, "002"),
            LinkType::Requires,
            &find(&board, "003"),
            true,
            false,
        )
        .unwrap();
        reconcile(&board).unwrap();
        assert_eq!(priorities(&board), ["high", "high", "low", "low"]);
        assert!(find(&board, "003").meta("priority-from").is_none());
    }

    #[test]
    fn a_done_requirer_pushes_nothing_and_a_cycle_ends() {
        let (_dir, board) = board_with(&["Finished", "Needed", "A", "B"]);
        set(&board, "001", "high");
        // Done first (done waits for what it requires, ticket 213), linked
        // after.
        for column in [Column::Todo, Column::Doing, Column::Done] {
            board.move_task(&find(&board, "001"), column, true).unwrap();
        }
        requires(&board, "001", "002");
        set(&board, "003", "medium");
        requires(&board, "003", "004");
        requires(&board, "004", "003");
        settings(&board, true, true);
        reconcile(&board).unwrap();
        assert_eq!(priorities(&board), ["high", "low", "medium", "medium"]);
    }

    #[test]
    fn override_pins_a_hand_set_priority_and_auto_unpins_it() {
        let (_dir, board) = board_with(&["Goal", "Needed"]);
        set(&board, "001", "high");
        requires(&board, "001", "002");
        settings(&board, true, true);
        reconcile(&board).unwrap();
        assert_eq!(priorities(&board), ["high", "high"]);

        // By hand, with override on: pinned, and it stays low.
        let line = set_by_hand(&board, &find(&board, "002"), "low").unwrap();
        assert!(line.contains("pinned"), "{line}");
        reconcile(&board).unwrap();
        assert_eq!(priorities(&board), ["high", "low"]);
        assert!(
            note(&board, &find(&board, "002"))
                .unwrap()
                .contains("pinned")
        );

        // `auto` hands it back.
        set_by_hand(&board, &find(&board, "002"), "auto").unwrap();
        reconcile(&board).unwrap();
        assert_eq!(priorities(&board), ["high", "high"]);

        // Override off: a hand-set priority is only a floor.
        settings(&board, true, false);
        set_by_hand(&board, &find(&board, "002"), "low").unwrap();
        reconcile(&board).unwrap();
        assert_eq!(priorities(&board), ["high", "high"]);
        assert!(set_by_hand(&board, &find(&board, "002"), "urgent").is_err());
    }

    #[test]
    fn switching_off_puts_raised_tickets_back_and_keeps_pins() {
        let (_dir, board) = board_with(&["Goal", "Needed", "Pinned"]);
        set(&board, "001", "high");
        requires(&board, "001", "002");
        requires(&board, "001", "003");
        settings(&board, true, true);
        set_by_hand(&board, &find(&board, "003"), "medium").unwrap();
        reconcile(&board).unwrap();
        assert_eq!(priorities(&board), ["high", "high", "medium"]);

        settings(&board, false, true);
        let lines = reconcile(&board).unwrap();
        assert_eq!(priorities(&board), ["high", "low", "medium"]);
        assert_eq!(lines, ["002 back to low (auto priority off)"]);
        assert!(
            find(&board, "003").meta("priority-pinned").is_some(),
            "the pin is kept"
        );
        assert!(set_by_hand(&board, &find(&board, "002"), "auto").is_err());
    }

    #[test]
    fn a_ticket_someone_else_has_locked_is_left_alone_with_a_note() {
        let (_dir, board) = board_with(&["Goal", "Locked"]);
        set(&board, "001", "high");
        requires(&board, "001", "002");
        let t = find(&board, "002");
        let until = (time::OffsetDateTime::now_utc() + time::Duration::hours(2))
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        let text = crate::eol::read(&t.path).unwrap();
        let text = crate::task::set_meta(&text, "locked-by", "\"someone@else.example\"").unwrap();
        let text = crate::task::set_meta(&text, "locked-until", &format!("{until:?}")).unwrap();
        std::fs::write(&t.path, text).unwrap();
        settings(&board, true, true);
        let lines = reconcile(&board).unwrap();
        assert_eq!(priorities(&board), ["high", "low"]);
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("auto priority left 002 alone")),
            "{lines:?}"
        );
    }

    #[test]
    fn a_child_is_one_step_below_its_parent_and_requires_lifts_what_it_needs() {
        // The shape of oberi-kitchen's board (ticket 371): a goal set high
        // by hand, broken down into parts, with real blockers as requires.
        let titles: Vec<String> = (1..=15).map(|n| format!("Ticket {n}")).collect();
        let titles: Vec<&str> = titles.iter().map(String::as_str).collect();
        let (_dir, board) = board_with(&titles);
        for (parent, child) in [
            ("003", "004"),
            ("003", "007"),
            ("007", "008"),
            ("007", "009"),
            ("010", "011"),
            ("010", "012"),
        ] {
            parent_of(&board, parent, child);
        }
        for (from, to) in [
            ("004", "005"),
            ("004", "006"),
            ("006", "013"),
            ("006", "014"),
            ("010", "001"),
        ] {
            requires(&board, from, to);
        }
        set(&board, "003", "high");
        settings(&board, true, true);
        let lines = reconcile(&board).unwrap();
        let (low, medium, high) = ("low", "medium", "high");
        assert_eq!(
            priorities(&board),
            [
                low, low, high, medium, medium, medium, medium, low, low, low, low, low, medium,
                medium, low
            ],
            "{lines:?}"
        );
        assert_eq!(
            find(&board, "004").meta("priority-from").as_deref(),
            Some("parent:003")
        );
        assert_eq!(
            find(&board, "005").meta("priority-from").as_deref(),
            Some("004")
        );
        assert!(
            lines
                .iter()
                .any(|l| l == "004 raised to medium: child of 003 (high)"),
            "{lines:?}"
        );
        assert_eq!(
            note(&board, &find(&board, "007")).as_deref(),
            Some("priority medium: raised by auto priority, child of 003 (high) (own: low)")
        );

        // The goal comes down: its breakdown goes back with it.
        set(&board, "003", "medium");
        reconcile(&board).unwrap();
        assert_eq!(
            priorities(&board),
            [
                low, low, medium, low, low, low, low, low, low, low, low, low, low, low, low
            ]
        );
    }

    #[test]
    fn a_raised_parent_passes_its_raise_down_and_a_done_one_nothing() {
        let (_dir, board) =
            board_with(&["Goal", "Needed", "Part", "Part of part", "Finished", "Left"]);
        set(&board, "001", "top");
        requires(&board, "001", "002");
        parent_of(&board, "002", "003");
        parent_of(&board, "003", "004");
        // A done parent pushes nothing.
        set(&board, "005", "top");
        for column in [Column::Todo, Column::Doing, Column::Done] {
            board.move_task(&find(&board, "005"), column, true).unwrap();
        }
        parent_of(&board, "005", "006");
        settings(&board, true, true);
        reconcile(&board).unwrap();
        // 002 is top through requires, so 003 is high and 004 medium.
        assert_eq!(
            priorities(&board),
            ["top", "top", "high", "medium", "top", "low"]
        );
    }

    #[test]
    fn a_child_listed_only_under_its_parents_children_is_found() {
        let (_dir, board) = board_with(&["Goal", "Part"]);
        parent_of(&board, "001", "002");
        // Take the child's `parent` away, leaving the parent's list.
        let child = find(&board, "002");
        let text = crate::eol::read(&child.path).unwrap();
        std::fs::write(
            &child.path,
            crate::task::set_meta(&text, "parent", "\"\"").unwrap(),
        )
        .unwrap();
        set(&board, "001", "high");
        settings(&board, true, true);
        reconcile(&board).unwrap();
        assert_eq!(priorities(&board), ["high", "medium"]);
    }

    #[test]
    fn a_pinned_child_keeps_its_priority_and_still_passes_it_down() {
        let (_dir, board) = board_with(&["Goal", "Part", "Part of part"]);
        parent_of(&board, "001", "002");
        parent_of(&board, "002", "003");
        set(&board, "001", "top");
        settings(&board, true, true);
        set_by_hand(&board, &find(&board, "002"), "low").unwrap();
        reconcile(&board).unwrap();
        // 002 is pinned low, so 003 takes nothing from it.
        assert_eq!(priorities(&board), ["top", "low", "low"]);
        set_by_hand(&board, &find(&board, "002"), "top").unwrap();
        reconcile(&board).unwrap();
        assert_eq!(priorities(&board), ["top", "top", "high"]);
    }
}
