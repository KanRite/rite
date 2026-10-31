//! Closing a sprint and backfilling one (ticket 052, kanrite-documents'
//! RFC 0005).
//!
//! **Closing** makes a started sprint read-only: nothing goes in or out.
//! It needs no all-done precondition: a sprint ends on schedule, and what's
//! left is the team's to decide. Unfinished tickets still in the sprint's
//! backlog folder go back to the loose backlog; tickets in todo or doing keep
//! their sprint label and carry on. A report goes into the project's
//! documents repository, `sprint/history/<YYYYMMDDTHHMMSSZ>-<sprint>.md`,
//! with the burn-up and burn-down as an SVG beside it, pushed straight to
//! main like any document (ADR 0007).
//!
//! **Backfill** is the one deliberate exception to "a started sprint takes
//! nothing more": sized, unassigned, non-draft tickets from the loose backlog
//! (never from an epic), highest priority first, then oldest, up to a number
//! of points. Always a command, never automatic.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use time::OffsetDateTime;

use crate::board::{Board, Column, Kind};
use crate::documents::{Restored, commit_change};
use crate::group::State;
use crate::stats::{self, Burn};
use crate::task::Task;

/// Where close reports go in the documents repository.
pub const HISTORY: &str = "sprint/history";

/// What closing a sprint did on its board.
#[derive(Clone, Debug)]
pub struct Closed {
    pub sprint: String,
    /// The burn as it closed.
    pub burn: Burn,
    /// Tickets done, and not done (with where they are now), as
    /// `(id, title, points, column)`.
    pub done: Vec<(String, String, u32)>,
    pub not_done: Vec<(String, String, u32, String)>,
    /// Ids of the unfinished tickets moved back to the loose backlog.
    pub returned: Vec<String>,
}

/// Closes the started sprint `name` on `board` (`repo` names its
/// repository in the report), as of `now`. Changes the board only; the
/// caller commits it and files the report with [`file_report`].
pub fn close(board: &Board, repo: &str, name: &str, now: i64) -> Result<Closed> {
    crate::archive::ensure_writable(board)?;
    let mut group = board.group(name)?;
    if !group.is_sprint() {
        bail!("{name} is an epic, not a sprint");
    }
    match group.state {
        State::Pending => bail!("{name} hasn't started, so there's nothing to close"),
        State::Closed => bail!("{name} is closed already"),
        State::Started => {}
    }
    let members = board.tasks_in_group(name)?;
    // Nothing is changed if a ticket that must move is held by someone else.
    for task in members.iter().filter(|t| t.column == Column::Backlog) {
        task.ensure_not_leased()?;
    }
    let burn = stats::burn_of(board, repo, &group, now)?;
    let mut done = Vec::new();
    let mut not_done = Vec::new();
    let mut returned = Vec::new();
    for task in &members {
        // Read before any move: the file isn't where `task` says afterwards.
        let id = task.id_display();
        let title = task.title();
        let points = stats::points_of(task);
        if task.column.is(Kind::Done) {
            done.push((id, title, points));
        } else if task.column == Column::Backlog {
            board.file(task, None, true)?;
            not_done.push((id.clone(), title, points, "back to the backlog".to_string()));
            returned.push(id);
        } else {
            let label = board
                .workflow()
                .columns()
                .find(|c| *c == task.column)
                .map(|c| c.as_str().to_string())
                .unwrap_or_default();
            not_done.push((id, title, points, label));
        }
    }
    group.close()?;
    Ok(Closed {
        sprint: name.to_string(),
        burn,
        done,
        not_done,
        returned,
    })
}

/// The report's file stem: `<YYYYMMDDTHHMMSSZ>-<sprint>`, time first so a
/// plain listing sorts by when.
pub fn report_stem(sprint: &str, now: i64) -> String {
    let t = OffsetDateTime::from_unix_timestamp(now).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z-{}",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
        crate::task::slugify(sprint)
    )
}

/// The report's markdown, linking `svg` (a file name beside it).
pub fn report_text(closed: &Closed, now: i64, svg: &str) -> String {
    let b = &closed.burn;
    let done: u32 = closed.done.iter().map(|d| d.2).sum();
    let share = (done * 100).checked_div(b.total).unwrap_or(0);
    let mut out = format!(
        "# Sprint {} closed\n\n- Board: {}\n- Started {}, closed {}, {} days planned\n- Points: {} in scope, {done} done ({share}%)\n- Tickets: {} done, {} not done\n\n![Burn-up and burn-down]({svg})\n",
        closed.sprint,
        b.repo,
        b.start,
        stats::date_of(now),
        b.days,
        b.total,
        closed.done.len(),
        closed.not_done.len(),
    );
    out.push_str("\n## Done\n\n");
    if closed.done.is_empty() {
        out.push_str("Nothing.\n");
    }
    for (id, title, points) in &closed.done {
        out.push_str(&format!("- {id} {title} ({points} pts)\n"));
    }
    out.push_str("\n## Not done\n\n");
    if closed.not_done.is_empty() {
        out.push_str("Nothing: every ticket was done.\n");
    }
    for (id, title, points, place) in &closed.not_done {
        out.push_str(&format!("- {id} {title} ({points} pts), {place}\n"));
    }
    out
}

/// The burn-up and burn-down side by side, as a standalone SVG.
pub fn report_svg(burn: &Burn) -> String {
    let (w, h) = (300.0_f64, 140.0_f64);
    let days = f64::from(burn.days.max(1));
    let total = f64::from(burn.total.max(1));
    let chart = |left: f64, down: bool| {
        let x = |d: f64| left + d / days * w;
        let y = |v: f64| 44.0 + (h - v / total * h);
        let mut points = vec![(0.0, if down { f64::from(burn.total) } else { 0.0 })];
        for (i, &v) in burn.done_by_day.iter().enumerate() {
            let v = f64::from(v);
            points.push((
                (i + 1) as f64,
                if down { f64::from(burn.total) - v } else { v },
            ));
        }
        let line = points
            .iter()
            .map(|(d, v)| format!("{:.1},{:.1}", x(*d), y(*v)))
            .collect::<Vec<_>>()
            .join(" ");
        let top = y(f64::from(burn.total));
        let guide = if down {
            format!("{:.1},{top:.1} {:.1},{:.1}", x(0.0), x(days), y(0.0))
        } else {
            format!("{:.1},{top:.1} {:.1},{top:.1}", x(0.0), x(days))
        };
        let (title, what) = if down {
            ("Burn-down", "points left, dashed: ideal")
        } else {
            ("Burn-up", "points done, dashed: scope")
        };
        format!(
            r##"<g><text x="{left}" y="16" font-weight="600">{title}</text><text x="{left}" y="{cap:.0}" fill="#5b6778">{what}</text><line x1="{left}" y1="{base:.1}" x2="{right:.1}" y2="{base:.1}" stroke="#a3afbf"/><polyline points="{guide}" fill="none" stroke="#a3afbf" stroke-dasharray="4 3"/><polyline points="{line}" fill="none" stroke="#0962b7" stroke-width="2"/><text x="{left}" y="{lab:.0}" fill="#5b6778">day 1</text><text x="{right:.1}" y="{lab:.0}" fill="#5b6778" text-anchor="end">day {d}</text><text x="{left}" y="{ptop:.0}" fill="#5b6778">{t} pts</text></g>"##,
            cap = 44.0 + h + 34.0,
            base = y(0.0),
            right = x(days),
            lab = y(0.0) + 14.0,
            ptop = top - 4.0,
            d = burn.days,
            t = burn.total,
        )
    };
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="700" height="{hh:.0}" viewBox="0 0 700 {hh:.0}" font-family="sans-serif" font-size="12"><rect width="700" height="{hh:.0}" fill="#ffffff"/>{a}{b}</svg>
"##,
        hh = 44.0 + h + 44.0,
        a = chart(20.0, false),
        b = chart(380.0, true),
    )
}

/// Files the report for `closed` into the documents clone `root`, pushed
/// to main. Returns the report's path in the repository and the commit's
/// outcome.
pub fn file_report(root: &Path, closed: &Closed, now: i64) -> Result<(String, Restored)> {
    let stem = report_stem(&closed.sprint, now);
    let md = format!("{HISTORY}/{stem}.md");
    let svg = format!("{HISTORY}/{stem}.svg");
    let svg_name = format!("{stem}.svg");
    let message = format!("sprint: {} closed", closed.sprint);
    let done = commit_change(root, &message, || {
        fs::create_dir_all(root.join(HISTORY)).with_context(|| format!("creating {HISTORY}"))?;
        fs::write(root.join(&svg), report_svg(&closed.burn))
            .with_context(|| format!("writing {svg}"))?;
        fs::write(root.join(&md), report_text(closed, now, &svg_name))
            .with_context(|| format!("writing {md}"))?;
        Ok(vec![PathBuf::from(&md), PathBuf::from(&svg)])
    })?;
    Ok((md, done))
}

/// Pulls tickets from the loose backlog into the started sprint `name`, up
/// to `points` points (ticket 052): sized, unassigned, not drafts and not
/// held by someone else, highest priority first, then oldest. Returns the
/// ids filed. Changes the board only; the caller commits.
pub fn backfill(board: &Board, name: &str, points: u32) -> Result<Vec<String>> {
    crate::archive::ensure_writable(board)?;
    let group = board.group(name)?;
    if !group.is_sprint() {
        bail!("{name} is an epic: backfill is for a running sprint");
    }
    match group.state {
        State::Pending => bail!("{name} hasn't started: file tickets into it the usual way"),
        State::Closed => bail!("{name} is closed: nothing more goes in"),
        State::Started => {}
    }
    if points == 0 {
        bail!("say how many points to backfill (--points N)");
    }
    let rank = |p: &str| match p {
        "top" => 0,
        "high" => 1,
        "medium" => 2,
        _ => 3,
    };
    let mut candidates: Vec<Task> = board
        .tasks_in(Column::Backlog)?
        .into_iter()
        .filter(|t| t.epic.is_none())
        .filter(|t| stats::points_of(t) > 0)
        .filter(|t| t.assignees().is_empty())
        .filter(|t| !t.is_draft())
        .filter(|t| t.ensure_not_leased().is_ok())
        .collect();
    candidates.sort_by_key(|t| {
        (
            rank(&t.priority()),
            t.meta("created").unwrap_or_default(),
            t.id.unwrap_or(u32::MAX),
        )
    });
    let mut left = points;
    let mut filed = Vec::new();
    for task in candidates {
        let p = stats::points_of(&task);
        if p > left {
            continue;
        }
        board.file(&task, Some(name), true)?;
        left -= p;
        filed.push(task.id_display());
        if left == 0 {
            break;
        }
    }
    if filed.is_empty() {
        bail!(
            "nothing in the loose backlog fits: backfill takes sized, unassigned tickets that aren't drafts, up to {points} points"
        );
    }
    Ok(filed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::{Group, Kind as GroupKind};

    fn board() -> (tempfile::TempDir, Board) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("acme-tickets").join("tasks");
        for column in ["backlog", "todo", "doing", "done"] {
            fs::create_dir_all(root.join(column)).unwrap();
        }
        (dir, Board::at(root))
    }

    fn ticket(board: &Board, at: &str, stem: &str, extra: &str) {
        fs::write(
            board.root.join(at).join(format!("{stem}.md")),
            format!(
                "---\nid: \"{}\"\ntitle: \"{stem}\"\n{extra}---\n\nBody\n",
                &stem[..3]
            ),
        )
        .unwrap();
    }

    fn sprint(board: &Board, name: &str, state: State) {
        Group {
            dir: board.dir(Column::Backlog).join(name),
            name: name.to_string(),
            kind: GroupKind::Sprint,
            description: String::new(),
            days: 10,
            state,
        }
        .save()
        .unwrap();
    }

    #[test]
    fn closing_returns_the_unstarted_and_makes_it_read_only() {
        let (_dir, board) = board();
        sprint(&board, "s1", State::Started);
        ticket(
            &board,
            "backlog/s1",
            "001-left",
            "points: \"3\"\nepic: \"s1\"\n",
        );
        // A real title, so the report shows it and not the file name.
        let left = board.root.join("backlog/s1/001-left.md");
        let text = crate::eol::read(&left)
            .unwrap()
            .replace("title: \"001-left\"", "title: \"Left behind\"");
        fs::write(&left, text).unwrap();
        ticket(
            &board,
            "doing",
            "002-going",
            "points: \"2\"\nepic: \"s1\"\n",
        );
        ticket(
            &board,
            "done",
            "003-finished",
            "points: \"5\"\nepic: \"s1\"\n",
        );
        let closed = close(&board, "acme-tickets", "s1", 1_800_000_000).unwrap();
        assert_eq!(closed.returned, ["001"]);
        assert_eq!(closed.done.len(), 1);
        assert_eq!(closed.not_done.len(), 2);
        assert_eq!(closed.burn.total, 10);
        assert_eq!(board.group("s1").unwrap().state, State::Closed);
        assert!(
            board.root.join("backlog/001-left.md").exists(),
            "back in the loose backlog"
        );
        // Read-only: nothing in or out.
        ticket(&board, "backlog", "004-new", "points: \"1\"\n");
        let err = board
            .set_epic(&board.find("004").unwrap(), Some("s1"))
            .unwrap_err();
        assert!(err.to_string().contains("is closed"), "{err}");
        assert!(close(&board, "acme-tickets", "s1", 1_800_000_000).is_err());
        let text = report_text(&closed, 1_800_000_000, "x.svg");
        assert!(text.contains("10 in scope, 5 done (50%)"), "{text}");
        assert!(
            text.contains("- 001 Left behind (3 pts), back to the backlog"),
            "{text}"
        );
        assert!(report_svg(&closed.burn).starts_with("<svg"));
    }

    #[test]
    fn a_pending_sprint_or_an_epic_cannot_be_closed() {
        let (_dir, board) = board();
        sprint(&board, "s2", State::Pending);
        assert!(close(&board, "r", "s2", 0).is_err());
    }

    #[test]
    fn backfill_takes_the_best_unassigned_sized_tickets_up_to_the_points() {
        let (_dir, board) = board();
        sprint(&board, "s1", State::Started);
        ticket(
            &board,
            "backlog",
            "001-low",
            "points: \"2\"\npriority: \"low\"\n",
        );
        ticket(
            &board,
            "backlog",
            "002-high",
            "points: \"3\"\npriority: \"high\"\n",
        );
        ticket(
            &board,
            "backlog",
            "003-mine",
            "points: \"1\"\npriority: \"high\"\nassignee: \"sam\"\n",
        );
        ticket(&board, "backlog", "004-unsized", "priority: \"high\"\n");
        ticket(
            &board,
            "backlog",
            "005-draft",
            "points: \"1\"\npriority: \"high\"\ntags: [DRAFT]\n",
        );
        ticket(
            &board,
            "backlog",
            "006-big",
            "points: \"8\"\npriority: \"medium\"\n",
        );
        fs::create_dir_all(board.dir(Column::Backlog).join("epic")).unwrap();
        ticket(
            &board,
            "backlog/epic",
            "007-in-epic",
            "points: \"1\"\npriority: \"high\"\n",
        );
        let filed = backfill(&board, "s1", 5).unwrap();
        assert_eq!(
            filed,
            ["002", "001"],
            "high first; the 8 doesn't fit; never the epic's"
        );
        assert!(board.root.join("backlog/s1/002-high.md").exists());
        assert!(backfill(&board, "s1", 0).is_err());
        sprint(&board, "s3", State::Pending);
        assert!(
            backfill(&board, "s3", 5).is_err(),
            "a pending sprint is filed the usual way"
        );
    }

    #[test]
    fn report_names_sort_by_time() {
        let a = report_stem("Sprint 9", 1_800_000_000);
        let b = report_stem("s10", 1_800_086_400);
        assert!(
            a.starts_with("20270115T") && a.ends_with("Z-sprint-9"),
            "{a}"
        );
        assert!(a < b);
    }
}
