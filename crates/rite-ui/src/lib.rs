//! riteui's backend: the Tauri commands the board's window calls. Each one
//! reads the project the way `rite` does and makes its change through
//! rite-lib — the same moves, leases, WIP limit, groups, templates and
//! auto-commit as the command line (ticket 084, docs/adr/0004). There is no
//! ticket, git or settings logic here of its own.

mod boards;
mod documents;
mod exe_watch;
mod speech;
mod terminal;
mod updates;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rite_lib::board::{Board, Column};
use rite_lib::group::{DEFAULT_DAYS, Group, Kind, State};
use rite_lib::settings::Config;
use rite_lib::task::{ContentHash, Task};
use rite_lib::templates::{DEFAULT_TEMPLATE, TEMPLATE_SUFFIX, Templates, example_context};
use rite_lib::transition::{self, Event, Outcome, TicketFields};
use rite_lib::{board_settings, current_project, settings};
use rite_ui_model::appearance::Appearance;
use rite_ui_model::epic::Place;
use rite_ui_model::wip::WipLimits;
use rite_ui_model::{Created, SyncReport, TicketDocument, TicketTemplate};
use tauri::State as Tauri;

use boards::{Project, Result, err, task_at, to_column};

/// The global settings key remembering the board shown last, for a launch
/// from somewhere that isn't inside a project.
const LAST_BOARD: &str = "ui.board";

/// Board changes one at a time: two drags in quick succession must not
/// interleave their moves and commits.
#[derive(Default)]
struct AppState {
    writing: Arc<Mutex<()>>,
    /// When each project's GitHub issues were last pulled in from the
    /// landing page (ticket 173), by its board repository, so going back to
    /// the landing page doesn't call GitHub every time.
    issues_pulled: Arc<Mutex<std::collections::HashMap<PathBuf, std::time::Instant>>>,
    /// When each board was last checked for due dates coming near (ticket
    /// 252), so the 30-second sync reads every ticket at most hourly.
    due_checked: Arc<Mutex<std::collections::HashMap<PathBuf, std::time::Instant>>>,
    /// The bottom panel's terminals (ticket 185), by id.
    terminals: Arc<Mutex<std::collections::HashMap<u32, terminal::Session>>>,
    next_terminal: Arc<std::sync::atomic::AtomicU32>,
    /// Speech to text (ticket 187): the loaded model (only after the first
    /// dictation), a recording in progress, a download in progress.
    speech: Arc<Mutex<Option<speech::whisper::Engine>>>,
    recording: Arc<Mutex<Option<speech::audio::Recorder>>>,
    downloading: Arc<std::sync::atomic::AtomicBool>,
    /// Text being read aloud (ticket 286), and a count so a reading's
    /// watcher knows when another has taken its place.
    speaking: Arc<Mutex<Option<Speaking>>>,
    readings: Arc<std::sync::atomic::AtomicU64>,
    /// A Piper set-up or removal running (ticket 287).
    piper_busy: Arc<std::sync::atomic::AtomicBool>,
    /// The board repository whose screen note this window last wrote
    /// (ticket 186), taken away when the window closes.
    screen: Arc<Mutex<Option<PathBuf>>>,
    /// riteui's own program as it started, to notice it being replaced
    /// (ticket 208); `None` when it couldn't be found.
    exe_watch: Arc<Mutex<Option<exe_watch::Watch>>>,
}

/// The AI tab's command when Preferences doesn't name one (ticket 185).
const DEFAULT_AI_COMMAND: &str = "claude";

/// How long the landing page leaves a project's issues before asking
/// GitHub again (ticket 173).
const ISSUE_PULL_EVERY: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Runs a change against the project off the async runtime, alone.
async fn change<T: Send + 'static>(
    state: &Tauri<'_, AppState>,
    f: impl FnOnce(&Project) -> Result<T> + Send + 'static,
) -> Result<T> {
    let writing = Arc::clone(&state.writing);
    blocking(move || {
        let _guard = writing
            .lock()
            .map_err(|_| "the board is in a bad state — restart riteui".to_string())?;
        // In step with the remote before any change (ticket 102): never
        // write on a stale clone. A clone that can't be brought up to date
        // refuses the change; no network only warns, so is ignored here.
        let project = Project::open()?;
        // An archived project only reads (ticket 157).
        rite_lib::archive::ensure_writable(&project.repos[0].1).map_err(err)?;
        for (_, board) in &project.repos {
            rite_lib::sync::catch_up(board, rite_lib::sync::Purpose::Write).map_err(err)?;
            // Settings still in a documents repository come across, once
            // (ticket 111).
            transition::with_auto_commit(board, "settings", "migrate", "from documents", || {
                board_settings::migrate_from_documents(board).map(|_| ())
            })
            .map_err(err)?;
        }
        f(&Project::open()?)
    })
    .await
}

/// Runs blocking work (files, git) off the async runtime, so a slow `git
/// push` never freezes the window.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("background task failed: {e}"))?
}

/// What git did, as the header reports it. `message` is the commit message
/// rite-lib writes (`doing: 026 "title"`); a warning is a commit or push that
/// failed without undoing the change.
fn report(board: &Board, message: String, warning: Option<String>) -> SyncReport {
    let auto = board_settings::load(board).auto_commit();
    let done = auto && warning.is_none();
    SyncReport {
        message,
        committed: done,
        pushed: done,
        warning,
    }
}

/// [`report`] for an rite-lib transition, whose warnings come as events.
fn report_outcome(board: &Board, message: String, outcome: &Outcome) -> SyncReport {
    let warnings: Vec<&str> = outcome
        .events
        .iter()
        .filter_map(|e| match e {
            Event::Warn(w) => Some(w.as_str()),
            Event::Info(_) => None,
        })
        .collect();
    let warning = (!warnings.is_empty()).then(|| warnings.join("; "));
    report(board, message, warning)
}

fn message(verb: &str, task: &Task) -> String {
    format!("{verb}: {} \"{}\"", task.id_display(), task.title())
}

/// Runs a change through rite-lib's auto-commit and reports it.
fn committed(
    board: &Board,
    verb: &str,
    subject: &str,
    detail: &str,
    f: impl FnOnce() -> anyhow::Result<()>,
) -> Result<SyncReport> {
    let ((), warning) =
        transition::with_auto_commit(board, verb, subject, detail, f).map_err(err)?;
    // The status line says what the commit says, empty detail and all.
    Ok(report(
        board,
        transition::commit_message(verb, subject, detail),
        warning,
    ))
}

#[tauri::command]
async fn load_board(window: tauri::WebviewWindow) -> Result<rite_ui_model::Board> {
    titled(&window, blocking(|| Project::open()?.summary()).await)
}

/// A window's title for the project it shows: `<project> — riteui`, so two
/// boards' windows can be told apart in the window switcher (ticket 116).
fn window_title(project: &str) -> String {
    match project.trim() {
        "" => "riteui".to_string(),
        project => format!("{project} — riteui"),
    }
}

/// Titles `window` after the board it is about to show. Every command that
/// hands the page a whole board goes through this, so the title follows a
/// change of project too. Only cosmetic, so a failure to set it is ignored.
fn titled(
    window: &tauri::WebviewWindow,
    board: Result<rite_ui_model::Board>,
) -> Result<rite_ui_model::Board> {
    if let Ok(board) = &board {
        let _ = window.set_title(&window_title(rite_ui_model::shown_name(
            &board.project,
            &board.shown,
        )));
    }
    board
}

/// What's on disk for the project on screen, summarised (ticket 091): the
/// page asks every second and reloads only when this changes, so the board
/// follows changes made anywhere — a terminal, an editor, a pull.
#[tauri::command]
async fn board_fingerprint() -> Result<String> {
    blocking(|| {
        let project = Project::open()?;
        let mut hasher = std::hash::DefaultHasher::new();
        for (name, board) in &project.repos {
            std::hash::Hash::hash(&(name, rite_lib::model::fingerprint(board)), &mut hasher);
        }
        Ok(format!("{:016x}", std::hash::Hasher::finish(&hasher)))
    })
    .await
}

/// Brings every repository on screen up to date with its remote, the way a
/// listing in the CLI does (ticket 102). The page calls it every 30 seconds;
/// whatever it pulls in changes the fingerprint, and the board refreshes.
/// Warnings (offline, a diverged clone) are returned, not raised.
#[tauri::command]
async fn sync_board(state: Tauri<'_, AppState>) -> Result<rite_ui_model::SyncStatus> {
    let writing = Arc::clone(&state.writing);
    let due_checked = Arc::clone(&state.due_checked);
    blocking(move || {
        let _guard = writing
            .lock()
            .map_err(|_| "the board is in a bad state — restart riteui".to_string())?;
        let mut warnings = Vec::new();
        let mut offline: Option<u32> = None;
        for (_, board) in &Project::open()?.repos {
            match rite_lib::sync::catch_up(board, rite_lib::sync::Purpose::Read) {
                Ok(events) => warnings.extend(events.into_iter().filter_map(|event| match event {
                    Event::Warn(text) => Some(text),
                    Event::Info(_) => None,
                })),
                Err(e) => warnings.push(format!("{e:#}")),
            }
            // Origin out of reach (ticket 197): the page's bar says so.
            if let Some(waiting) = rite_lib::sync::offline(board) {
                offline = Some(offline.unwrap_or(0) + waiting);
            }
            if let Err(e) = raise_for_due_dates(board, &due_checked) {
                warnings.push(format!("{e:#}"));
            }
        }
        Ok(rite_ui_model::SyncStatus { warnings, offline })
    })
    .await
}

/// How often an open board is checked for due dates coming near.
const DUE_CHECK: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// A due date coming near changes nothing on disk, so on a board nobody
/// touches the raise would wait for the next change (ticket 252). An open
/// riteui commits it instead, at most hourly a board: `priority: due dates`,
/// only when auto priority would change something.
fn raise_for_due_dates(
    board: &Board,
    checked: &Mutex<std::collections::HashMap<PathBuf, std::time::Instant>>,
) -> anyhow::Result<()> {
    {
        let mut checked = checked
            .lock()
            .map_err(|_| anyhow::anyhow!("the due-date check is in a bad state"))?;
        if checked
            .get(&board.root)
            .is_some_and(|at| at.elapsed() < DUE_CHECK)
        {
            return Ok(());
        }
        checked.insert(board.root.clone(), std::time::Instant::now());
    }
    let settings = board_settings::load(board);
    if !settings.auto_priority() || settings.archived() {
        return Ok(());
    }
    if !rite_lib::auto_priority::pending(board, rite_lib::recur::today()) {
        return Ok(());
    }
    transition::with_auto_commit(board, transition::PRIORITY, "due dates", "", || Ok(()))?;
    rite_lib::auto_priority::take_notes();
    Ok(())
}

/// The ticket's current version on disk, as `open_ticket` reports it, or
/// `None` once it's no longer at `place` (moved or removed) — how an open
/// editor learns its ticket changed underneath it.
#[tauri::command]
async fn ticket_version(place: Place, file: String) -> Result<Option<String>> {
    blocking(move || {
        let project = Project::open()?;
        let board = project.board(&place.repo)?;
        Ok(task_at(board, &place, &file)
            .ok()
            .and_then(|task| task.hash().ok())
            .map(|hash| hash.to_string()))
    })
    .await
}

#[tauri::command]
async fn open_ticket(place: Place, file: String) -> Result<TicketDocument> {
    blocking(move || {
        let project = Project::open()?;
        let board = project.board(&place.repo)?;
        let task = task_at(board, &place, &file)?;
        let content = rite_lib::eol::read(&task.path).map_err(|e| e.to_string())?;
        Ok(TicketDocument {
            repo: place.repo.clone(),
            file,
            column: place.column,
            epic: place.epic.clone(),
            lock: project.lock_of(&task),
            lapsed: project.lapsed_of(&task),
            version: task.hash().map_err(err)?.to_string(),
            authorship: rite_lib::authorship::summary(board, &task).unwrap_or_default(),
            priority_note: rite_lib::auto_priority::note(board, &task).unwrap_or_default(),
            content,
        })
    })
    .await
}

/// Saves the editor's text through rite-lib (`Task::set_text`): refused if
/// the ticket changed since it was opened, or is locked by someone else.
#[tauri::command]
async fn save_ticket(
    state: Tauri<'_, AppState>,
    place: Place,
    file: String,
    content: String,
    version: String,
) -> Result<SyncReport> {
    change(&state, move |project| {
        let board = project.board(&place.repo)?;
        let task = task_at(board, &place, &file)?;
        let expected: ContentHash = version
            .parse()
            .map_err(|_| "the editor lost track of this ticket — reopen it".to_string())?;
        // The project's tag list decides (ticket 122): an unlisted tag is
        // refused when only listed tags are allowed. Saved in uppercase.
        let tags = rite_ui_model::parse_frontmatter(&content).tags;
        board_settings::load(&project.repos[0].1)
            .check_tags(&tags)
            .map_err(err)?;
        let outcome = transition::save_text(board, &task, expected, &content).map_err(err)?;
        Ok(report_outcome(board, message("edit", &task), &outcome))
    })
    .await
}

#[tauri::command]
async fn move_ticket(
    state: Tauri<'_, AppState>,
    file: String,
    from: Place,
    to: Place,
    force: Option<bool>,
) -> Result<SyncReport> {
    change(&state, move |project| {
        move_in(project, &file, &from, &to, force.unwrap_or(false))
    })
    .await
}

/// A drag on the board, as rite-lib moves: into an epic or sprint is
/// `set_epic` (from todo, back to the backlog first); out of one to the
/// loose backlog is `set_epic(None)`; anything else is a column move, which
/// takes the lease on entering todo or doing. The window's own `Place`
/// (a column, or an epic in the backlog) maps onto these directly.
fn move_in(
    project: &Project,
    file: &str,
    from: &Place,
    to: &Place,
    force: bool,
) -> Result<SyncReport> {
    if from.repo != to.repo {
        return Err("A ticket can't move between repositories.".into());
    }
    let board = project.board(&from.repo)?;
    let task = task_at(board, from, file)?;
    let (id, title) = (task.id_display(), task.title());
    match (&from.epic, &to.epic, to.column) {
        (_, Some(epic), _) => committed(board, "epic", &id, &title, || {
            let task = if task.column == Column::Todo {
                let dest = board.move_task(&task, Column::Backlog, force)?;
                let mut back = Task::from_path(&dest, Column::Backlog)
                    .ok_or_else(|| anyhow::anyhow!("{} is not a ticket", dest.display()))?;
                back.epic = dest
                    .parent()
                    .filter(|dir| *dir != board.dir(Column::Backlog))
                    .and_then(|dir| dir.file_name())
                    .map(|name| name.to_string_lossy().into_owned());
                back
            } else {
                task
            };
            board.set_epic(&task, Some(epic)).map(|_| ())
        }),
        (Some(_), None, rite_ui_model::Column::Backlog) => {
            committed(board, "backlog", &id, &title, || {
                board.set_epic(&task, None).map(|_| ())
            })
        }
        _ => {
            let outcome =
                transition::move_to(board, &task, to_column(to.column), force).map_err(err)?;
            Ok(report_outcome(
                board,
                format!("{}: {id} \"{title}\"", to.column.dir_name()),
                &outcome,
            ))
        }
    }
}

/// Creates a ticket in `repo`'s backlog, the way `rite new` does.
#[tauri::command]
// A command's arguments are the page's named fields, not a call site.
#[allow(clippy::too_many_arguments)]
async fn create_ticket(
    state: Tauri<'_, AppState>,
    title: String,
    repo: Option<String>,
    repository: Option<String>,
    draft: Option<bool>,
    required_by: Option<String>,
    related_to: Option<String>,
    parent: Option<String>,
) -> Result<Created> {
    change(&state, move |project| {
        let repo = repo.unwrap_or_default();
        let board = project.board(&repo)?;
        // Save and New (ticket 227): linked back to the ticket it came from;
        // Add child (ticket 259): under it.
        let required_by: Vec<String> = required_by.into_iter().collect();
        let related: Vec<String> = related_to.into_iter().collect();
        let fields = TicketFields {
            title: title.clone(),
            assignees: Vec::new(),
            // What it's about: the repository being filtered to, if any.
            repository,
            points: String::new(),
            epic: None,
            // A draft is a ticket carrying DRAFT (ticket 114).
            tags: if draft.unwrap_or(false) {
                vec![rite_lib::task::DRAFT_TAG.to_string()]
            } else {
                Vec::new()
            },
            branch_type: String::new(),
            parent: parent.unwrap_or_default(),
            related,
            clone_of: String::new(),
            body: String::new(),
            priority: String::new(),
            requires: Vec::new(),
            required_by,
        };
        let outcome = transition::create_ticket(board, &fields).map_err(err)?;
        let name = if repo.is_empty() {
            project.repos[0].0.clone()
        } else {
            repo
        };
        created_from(board, name, &outcome)
    })
    .await
}

/// Where a new repository would go, and its defaults (ticket 228).
#[tauri::command]
async fn repo_defaults() -> Result<rite_ui_model::RepoDefaults> {
    blocking(|| {
        let project = Project::open()?;
        let board = &project.repos[0].1;
        let prefix = board_settings::load(board)
            .project()
            .map(|p| format!("{p}-"))
            .unwrap_or_default();
        let admin = boards::admin_access(board);
        if !admin.allowed {
            return Ok(rite_ui_model::RepoDefaults {
                prefix,
                refused: Some("Only the project's admins can make repositories.".into()),
                ..Default::default()
            });
        }
        use rite_lib::new_repo::{self, Forge};
        Ok(match new_repo::defaults(board, &mut new_repo::gh) {
            Ok(d) => rite_ui_model::RepoDefaults {
                place: match d.forge {
                    Forge::GitHub { owner } => format!("on GitHub, under {owner}"),
                    Forge::Local => "beside the tickets repository's remote".into(),
                    Forge::None => {
                        "on this machine only (the tickets repository has no remote)".into()
                    }
                },
                private: d.private,
                prefix,
                refused: None,
            },
            Err(e) => rite_ui_model::RepoDefaults {
                prefix,
                refused: Some(format!("{e:#}")),
                ..Default::default()
            },
        })
    })
    .await
}

/// Makes (or connects) a repository for the project and registers it
/// (ticket 228): the ticket at `place`/`file`, if given, gets it as its
/// repository and a note. Admins only.
#[tauri::command]
async fn create_repository(
    state: Tauri<'_, AppState>,
    name: String,
    description: String,
    private: bool,
    readme: bool,
    place: Option<Place>,
    file: Option<String>,
) -> Result<SyncReport> {
    change(&state, move |project| {
        use rite_lib::new_repo::{self, NewRepo};
        let board = &project.repos[0].1;
        if !boards::admin_access(board).allowed {
            return Err("Only the project's admins can make repositories.".into());
        }
        let task = match (&place, &file) {
            (Some(place), Some(file)) => {
                let on = project.board(&place.repo)?;
                Some((on, task_at(on, place, file)?))
            }
            _ => None,
        };
        let new = NewRepo {
            name: name.trim().to_string(),
            description: description.trim().to_string(),
            private: Some(private),
            readme,
            for_ticket: task.as_ref().map(|(_, t)| (t.id_display(), t.title())),
        };
        let made = new_repo::create(board, &new, &mut new_repo::gh).map_err(err)?;
        new_repo::register(&made).map_err(err)?;
        let same_board = task.as_ref().is_none_or(|(on, _)| on.root == board.root);
        let mut drafted = None;
        let mut report = committed(board, "repo", "new", &made.name, || {
            new_repo::declare(board, &made.name)?;
            if same_board && let Some((_, task)) = &task {
                drafted = new_repo::record_on_ticket(board, task, &made)?;
            }
            Ok(())
        })?;
        // A ticket on one of the project's other boards: its own commit.
        if !same_board && let Some((on, task)) = &task {
            committed(on, "repo", "new", &made.name, || {
                drafted = new_repo::record_on_ticket(on, task, &made)?;
                Ok(())
            })?;
        }
        let notes: Vec<String> = made.notes.iter().cloned().chain(drafted).collect();
        if !notes.is_empty() {
            let notes = notes.join("; ");
            report.warning = Some(match report.warning {
                Some(w) => format!("{w}; {notes}"),
                None => notes,
            });
        }
        Ok(report)
    })
    .await
}

/// What the page needs to open a ticket that was just made: where it landed,
/// its text, and the report of what the change did.
fn created_from(board: &Board, repo: String, outcome: &transition::Outcome) -> Result<Created> {
    let dest = outcome
        .dest
        .clone()
        .ok_or("the ticket was created but not found")?;
    let task = Task::from_path(&dest, Column::Backlog).ok_or("the new ticket can't be read")?;
    let sync = report_outcome(board, message("new", &task), outcome);
    Ok(Created {
        doc: TicketDocument {
            repo,
            file: task.file_name(),
            column: rite_ui_model::Column::Backlog,
            epic: None,
            lock: None,
            lapsed: None,
            version: task.hash().map_err(err)?.to_string(),
            authorship: rite_lib::authorship::summary(board, &task).unwrap_or_default(),
            priority_note: rite_lib::auto_priority::note(board, &task).unwrap_or_default(),
            content: rite_lib::eol::read(&task.path).map_err(|e| e.to_string())?,
        },
        sync,
    })
}

/// Duplicates a ticket into the backlog of its board, linked to it as a
/// clone (`rite duplicate`).
#[tauri::command]
async fn clone_ticket(state: Tauri<'_, AppState>, place: Place, file: String) -> Result<Created> {
    change(&state, move |project| {
        let board = project.board(&place.repo)?;
        let original = task_at(board, &place, &file)?;
        let outcome = transition::clone_ticket(board, &original).map_err(err)?;
        let name = if place.repo.is_empty() {
            project.repos[0].0.clone()
        } else {
            place.repo.clone()
        };
        created_from(board, name, &outcome)
    })
    .await
}

fn new_group(board: &Board, name: &str, kind: Kind, days: u32) -> Result<SyncReport> {
    let name = name.trim().to_string();
    let verb = match kind {
        Kind::Epic => "group-epic",
        Kind::Sprint => "group-sprint",
    };
    committed(board, verb, &name, "", || {
        board.create_group(&Group {
            dir: board.dir(Column::Backlog).join(&name),
            name: name.clone(),
            kind,
            description: String::new(),
            days,
            state: State::Pending,
        })
    })
}

/// Creates an epic in `repo`'s backlog (`rite group epic`).
#[tauri::command]
async fn create_epic(
    state: Tauri<'_, AppState>,
    name: String,
    repo: Option<String>,
) -> Result<SyncReport> {
    change(&state, move |p| {
        new_group(
            p.board(&repo.unwrap_or_default())?,
            &name,
            Kind::Epic,
            DEFAULT_DAYS,
        )
    })
    .await
}

fn roadmap_card(
    r: &rite_lib::roadmap::Roadmap,
    today: time::Date,
    target: Option<&rite_lib::board::Board>,
) -> rite_ui_model::RoadmapCard {
    // A milestone's tickets are counted on the roadmap's own board.
    let progress = |m: &rite_lib::roadmap::Milestone| -> (usize, usize) {
        let Some(board) = target else { return (0, 0) };
        let tasks = board.tasks_in_group(&m.name).unwrap_or_default();
        let done = tasks
            .iter()
            .filter(|t| t.column == rite_lib::board::Column::Done)
            .count();
        (done, tasks.len())
    };
    let summary = r.summary(today, &progress);
    rite_ui_model::RoadmapCard {
        slug: r.slug.clone(),
        name: r.name.clone(),
        board: r.board.clone(),
        goal: r.goal.clone(),
        sprints: r.sprints,
        duration: summary.duration,
        remaining: summary.remaining,
        late: summary.late,
        milestones: r
            .milestones
            .iter()
            .map(|m| {
                let (done, total) = progress(m);
                rite_ui_model::MilestoneCard {
                    name: m.name.clone(),
                    description: m.description.clone(),
                    kind: m.kind.as_str().to_string(),
                    start: m.start.clone(),
                    due: m.due.clone(),
                    days_per_week: m.days_per_week,
                    duration: m.duration,
                    total: m.working_duration(),
                    remaining: m.remaining(today),
                    late: m.late(today, done, total),
                }
            })
            .collect(),
    }
}

/// The project's roadmaps, and the boards without one (ticket 206).
#[tauri::command]
async fn list_roadmaps() -> Result<rite_ui_model::RoadmapsPage> {
    blocking(|| {
        let project = Project::open()?;
        let today = time::OffsetDateTime::now_utc().date();
        let mut roadmaps = rite_lib::roadmap::list(&project.repos[0].1).map_err(err)?;
        let cards: Vec<_> = roadmaps
            .drain(..)
            .map(|r| {
                let target = project.board(&r.board).ok().filter(|_| !r.board.is_empty());
                roadmap_card(&r, today, target)
            })
            .collect();
        let free_boards = project
            .repos
            .iter()
            .map(|(name, _)| name.clone())
            .filter(|name| !cards.iter().any(|r| &r.board == name))
            .collect();
        Ok(rite_ui_model::RoadmapsPage {
            roadmaps: cards,
            free_boards,
        })
    })
    .await
}

/// Starts the roadmap of a board (ticket 206).
#[tauri::command]
async fn create_roadmap(
    state: Tauri<'_, AppState>,
    board: String,
    alias: String,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let store = &p.repos[0].1;
        p.board(&board)?;
        let board = board.trim().to_string();
        committed(store, "roadmap", &board, "", || {
            rite_lib::roadmap::create(store, &board, &alias).map(|_| ())
        })
    })
    .await
}

/// Gives a roadmap from before roadmaps had boards its board (ticket 206).
#[tauri::command]
async fn adopt_roadmap(
    state: Tauri<'_, AppState>,
    roadmap: String,
    board: String,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let store = &p.repos[0].1;
        p.board(&board)?;
        committed(store, "roadmap-board", &board, &roadmap, || {
            rite_lib::roadmap::adopt(store, &roadmap, &board).map(|_| ())
        })
    })
    .await
}

/// Saves a roadmap's goal and whether its milestones are sprints.
#[tauri::command]
async fn save_roadmap(
    state: Tauri<'_, AppState>,
    roadmap: String,
    goal: String,
    sprints: bool,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let store = &p.repos[0].1;
        committed(store, "roadmap", &roadmap, "", || {
            rite_lib::roadmap::set_details(store, &roadmap, &goal, sprints).map(|_| ())
        })
    })
    .await
}

/// Renames a roadmap; its file moves to the new name's slug.
#[tauri::command]
async fn rename_roadmap(
    state: Tauri<'_, AppState>,
    roadmap: String,
    name: String,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let store = &p.repos[0].1;
        committed(store, "roadmap-rename", name.trim(), &roadmap, || {
            rite_lib::roadmap::rename(store, &roadmap, &name).map(|_| ())
        })
    })
    .await
}

/// Adds a milestone to a roadmap and makes its epic (or sprint) on the
/// roadmap's board, in one commit per repository (ticket 206).
#[tauri::command]
async fn add_milestone(
    state: Tauri<'_, AppState>,
    roadmap: String,
    milestone: rite_ui_model::MilestoneCard,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let store = &p.repos[0].1;
        let found = rite_lib::roadmap::list(store)
            .map_err(err)?
            .into_iter()
            .find(|r| r.slug == roadmap)
            .ok_or_else(|| format!("No roadmap called {roadmap}."))?;
        if found.board.is_empty() {
            return Err(format!("Give {} a board first.", found.name));
        }
        let target = p.board(&found.board)?;
        let name = milestone.name.trim().to_string();
        // A started sprint of the same name is used as it is (ticket 306).
        let mut warnings = Vec::new();
        let add = || {
            rite_lib::roadmap::add_milestone(
                store,
                target,
                &roadmap,
                rite_lib::roadmap::Milestone {
                    name: milestone.name.clone(),
                    description: milestone.description.clone(),
                    kind: Kind::Epic,
                    start: milestone.start.trim().to_string(),
                    due: milestone.due.trim().to_string(),
                    days_per_week: milestone.days_per_week,
                    duration: milestone.duration,
                },
            )
            .map(|(_, found)| warnings = found)
        };
        let report = if target.repo_root() == store.repo_root() {
            committed(store, "roadmap-milestone", &name, &roadmap, add)
        } else {
            // The epic is in another repository: it takes its own commit.
            committed(target, "roadmap-milestone", &name, &roadmap, || {
                transition::with_auto_commit(store, "roadmap-milestone", &name, &roadmap, add)
                    .map(|_| ())
            })
        }?;
        Ok(with_notes(report, warnings))
    })
    .await
}

/// A report with notes added to its warning (ticket 306).
fn with_notes(mut report: SyncReport, notes: Vec<String>) -> SyncReport {
    if !notes.is_empty() {
        let mut all: Vec<String> = report.warning.take().into_iter().collect();
        all.extend(notes);
        report.warning = Some(all.join("; "));
    }
    report
}

/// Takes a milestone off a roadmap, and its epic or sprint off the board
/// when that's empty and not started (ticket 306). Admins only.
#[tauri::command]
async fn delete_milestone(
    state: Tauri<'_, AppState>,
    roadmap: String,
    name: String,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let store = &p.repos[0].1;
        if !boards::admin_access(store).allowed {
            return Err("Only the project's admins can delete milestones.".into());
        }
        let found = rite_lib::roadmap::list(store)
            .map_err(err)?
            .into_iter()
            .find(|r| r.slug == roadmap)
            .ok_or_else(|| format!("No roadmap called {roadmap}."))?;
        let target = p.board(&found.board)?;
        let mut notes = Vec::new();
        let remove = || {
            rite_lib::roadmap::delete_milestone(store, target, &roadmap, &name)
                .map(|found| notes = found)
        };
        let verb = "roadmap-milestone-delete";
        let report = if target.repo_root() == store.repo_root() {
            committed(store, verb, &name, &roadmap, remove)
        } else {
            committed(target, verb, &name, &roadmap, || {
                transition::with_auto_commit(store, verb, &name, &roadmap, remove).map(|_| ())
            })
        }?;
        Ok(with_notes(report, notes))
    })
    .await
}

/// Deletes a roadmap, and each milestone's epic or sprint that's empty and
/// not started (ticket 306). Admins only.
#[tauri::command]
async fn delete_roadmap(state: Tauri<'_, AppState>, roadmap: String) -> Result<SyncReport> {
    change(&state, move |p| {
        let store = &p.repos[0].1;
        if !boards::admin_access(store).allowed {
            return Err("Only the project's admins can delete roadmaps.".into());
        }
        let found = rite_lib::roadmap::list(store)
            .map_err(err)?
            .into_iter()
            .find(|r| r.slug == roadmap)
            .ok_or_else(|| format!("No roadmap called {roadmap}."))?;
        let target = if found.board.is_empty() {
            None
        } else {
            p.board(&found.board).ok()
        };
        let name = found.name.clone();
        let mut notes = Vec::new();
        let remove =
            || rite_lib::roadmap::delete(store, target, &roadmap).map(|found| notes = found);
        let verb = "roadmap-delete";
        let report = match target {
            Some(t) if t.repo_root() != store.repo_root() => committed(t, verb, &name, "", || {
                transition::with_auto_commit(store, verb, &name, "", remove).map(|_| ())
            }),
            _ => committed(store, verb, &name, "", remove),
        }?;
        Ok(with_notes(report, notes))
    })
    .await
}

/// Changes a milestone's start and due dates (ticket 235). A pending sprint
/// it made takes the new length, in its own commit when it's on another
/// repository's board.
#[tauri::command]
async fn set_milestone_dates(
    state: Tauri<'_, AppState>,
    roadmap: String,
    milestone: String,
    start: String,
    due: String,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let store = &p.repos[0].1;
        let found = rite_lib::roadmap::list(store)
            .map_err(err)?
            .into_iter()
            .find(|r| r.slug == roadmap)
            .ok_or_else(|| format!("No roadmap called {roadmap}."))?;
        if found.board.is_empty() {
            return Err(format!("Give {} a board first.", found.name));
        }
        let target = p.board(&found.board)?;
        let set = || {
            rite_lib::roadmap::set_milestone_dates(
                store, target, &roadmap, &milestone, &start, &due,
            )
            .map(|_| ())
        };
        if target.repo_root() == store.repo_root() {
            committed(store, "roadmap-dates", &milestone, &roadmap, set)
        } else {
            committed(target, "roadmap-dates", &milestone, &roadmap, || {
                transition::with_auto_commit(store, "roadmap-dates", &milestone, &roadmap, set)
                    .map(|_| ())
            })
        }
    })
    .await
}

/// Creates a pending sprint in `repo`'s backlog (`rite group sprint`).
#[tauri::command]
async fn create_sprint(
    state: Tauri<'_, AppState>,
    name: String,
    days: u32,
    repo: Option<String>,
) -> Result<SyncReport> {
    change(&state, move |p| {
        new_group(
            p.board(&repo.unwrap_or_default())?,
            &name,
            Kind::Sprint,
            days,
        )
    })
    .await
}

/// Closes a started sprint (`rite group close`, ticket 052): read-only from
/// now, its unstarted tickets back to the backlog, and its report filed in
/// the project's documents repository (a note when that isn't cloned).
#[tauri::command]
async fn close_sprint(
    state: Tauri<'_, AppState>,
    repo: Option<String>,
    name: String,
) -> Result<SyncReport> {
    change(&state, move |p| {
        use rite_lib::{documents, sprints, stats};
        let (repo_name, board) = p
            .repos
            .iter()
            .find(|(n, _)| repo.as_deref().is_none_or(|r| r.is_empty() || r == n))
            .map(|(n, b)| (n.clone(), b))
            .ok_or_else(|| "no such repository".to_string())?;
        let now = stats::now();
        let mut closed = None;
        let mut report = committed(board, "group-close", &name, "", || {
            closed = Some(sprints::close(board, &repo_name, &name, now)?);
            Ok(())
        })?;
        let Some(closed) = closed else {
            return Ok(report);
        };
        let own = documents::sources(&p.repos[0].1)
            .into_iter()
            .find(|s| s.kind == documents::Kind::Own)
            .and_then(|s| s.root.map(|root| (s.name, root)));
        let note = match own {
            Some((source, root)) => match sprints::file_report(&root, &closed, now) {
                Ok((path, filed)) => {
                    report.message = format!("{name} closed; report in {source}/{path}");
                    filed.warning
                }
                Err(e) => Some(format!("the close report wasn't written: {e:#}")),
            },
            None => Some("no documents repository is cloned here, so no close report".into()),
        };
        if let Some(note) = note {
            report.warning = Some(match report.warning.take() {
                Some(w) => format!("{w}; {note}"),
                None => note,
            });
        }
        Ok(report)
    })
    .await
}

/// Backfills a running sprint (`rite group backfill`, ticket 052): sized,
/// unassigned tickets from the loose backlog, up to `points`.
#[tauri::command]
async fn backfill_sprint(
    state: Tauri<'_, AppState>,
    repo: Option<String>,
    name: String,
    points: u32,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = p.board(&repo.unwrap_or_default())?;
        committed(board, "group-backfill", &name, "", || {
            rite_lib::sprints::backfill(board, &name, points).map(|_| ())
        })
    })
    .await
}

/// Starts a sprint (`rite group start`): no more tickets in or back out.
#[tauri::command]
async fn start_sprint(
    state: Tauri<'_, AppState>,
    repo: Option<String>,
    name: String,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = p.board(&repo.unwrap_or_default())?;
        // Refused while its tickets require others outside it (ticket 214).
        committed(board, "group-start", &name, "", || {
            rite_lib::requires::start_sprint(board, &name)
        })
    })
    .await
}

/// Renames an epic or sprint everywhere it is recorded (`rite group rename`).
#[tauri::command]
async fn rename_epic(
    state: Tauri<'_, AppState>,
    repo: Option<String>,
    from: String,
    to: String,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = p.board(&repo.unwrap_or_default())?;
        committed(board, "group-rename", &from, to.trim(), || {
            board.rename_group(&from, &to).map(|_| ())
        })
    })
    .await
}

/// Every ticket of an epic or sprint still in the backlog, moved to the
/// loose backlog (`rite group empty`) or to todo (`--to todo`) in one commit
/// (ticket 256); the ones the rules keep back are said in the warning.
#[tauri::command]
async fn move_epic_tickets(
    state: Tauri<'_, AppState>,
    repo: Option<String>,
    name: String,
    to_todo: bool,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = p.board(&repo.unwrap_or_default())?;
        let mut skipped: Vec<(String, String)> = Vec::new();
        let verb = if to_todo { "group-todo" } else { "group-empty" };
        let mut report = committed(board, verb, &name, "", || {
            if to_todo {
                skipped = transition::group_to_todo(board, &name)?.skipped;
            } else {
                skipped = board.empty_group(&name)?.skipped;
            }
            Ok(())
        })?;
        if !skipped.is_empty() {
            let left: Vec<String> = skipped
                .iter()
                .map(|(t, why)| format!("{t} not moved: {why}"))
                .collect();
            let left = left.join("; ");
            report.warning = Some(match report.warning {
                Some(w) => format!("{w}; {left}"),
                None => left,
            });
        }
        Ok(report)
    })
    .await
}

/// A newer KanRite to offer in the update bar (ticket 192), if any.
#[tauri::command]
async fn update_status() -> Result<Option<rite_ui_model::UpdateInfo>> {
    blocking(|| Ok(updates::status())).await
}

/// The version riteui's program on disk is now, when it was replaced while
/// running and isn't this one (ticket 208): the page offers a Restart.
#[tauri::command]
async fn binary_changed(state: Tauri<'_, AppState>) -> Result<Option<String>> {
    let watch = Arc::clone(&state.exe_watch);
    blocking(move || {
        let mut watch = watch
            .lock()
            .map_err(|_| "the program check is in a bad state".to_string())?;
        Ok(watch.as_mut().and_then(exe_watch::Watch::check))
    })
    .await
}

/// The update bar's Update (ticket 191): installs the newest release; the
/// page then calls `restart_app`.
#[tauri::command]
async fn apply_update() -> Result<rite_ui_model::UpdateResult> {
    blocking(|| Ok(updates::apply())).await
}

/// Starts riteui again: after an update, into the new version (ticket 191).
#[tauri::command]
fn restart_app(app: tauri::AppHandle) {
    app.restart();
}

/// The update bar's Later (ticket 192): this version, hidden for a day.
#[tauri::command]
async fn snooze_update(version: String) -> Result<()> {
    blocking(move || updates::later(&version).map_err(err)).await
}

/// Check for updates, asked for by hand (ticket 304).
#[tauri::command]
async fn check_for_update() -> Result<rite_ui_model::ManualCheck> {
    blocking(|| Ok(updates::check_now())).await
}

/// What's new (ticket 336): the notes of each release newer than this one.
#[tauri::command]
async fn whats_new() -> Result<Vec<rite_ui_model::ReleaseNotes>> {
    blocking(updates::whats_new).await
}

/// When GitHub last answered a check for updates (unix seconds).
#[tauri::command]
async fn update_checked() -> Result<Option<i64>> {
    blocking(|| Ok(updates::last_checked())).await
}

/// Deletes an epic or sprint (`rite group delete`, ticket 204): refused while
/// any ticket in it isn't done.
#[tauri::command]
async fn delete_epic(
    state: Tauri<'_, AppState>,
    repo: Option<String>,
    name: String,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = p.board(&repo.unwrap_or_default())?;
        committed(board, "group-delete", &name, "", || {
            board.delete_group(&name)
        })
    })
    .await
}

/// Links a ticket to another on the same board as `kind`, or unlinks them
/// (`rite link`, ticket 207). The other ticket records the opposite, and both
/// files go in one commit.
#[tauri::command]
async fn link_ticket(
    state: Tauri<'_, AppState>,
    place: Place,
    file: String,
    other: String,
    kind: String,
    remove: bool,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = p.board(&place.repo)?;
        let task = task_at(board, &place, &file)?;
        let kind = rite_lib::links::LinkType::parse(&kind)
            .ok_or_else(|| format!("unknown link type {kind:?}"))?;
        let other = board.find(&other).map_err(err)?;
        let outcome = transition::link_tickets(
            board,
            &task,
            kind,
            std::slice::from_ref(&other),
            remove,
            false,
        )
        .map_err(err)?;
        let verb = if remove { "unlink" } else { "link" };
        Ok(report_outcome(board, message(verb, &task), &outcome))
    })
    .await
}

/// Changes the type of the link between a ticket and `other` (ticket 213),
/// on both tickets, in one commit.
#[tauri::command]
async fn retype_link(
    state: Tauri<'_, AppState>,
    place: Place,
    file: String,
    other: String,
    old: String,
    new: String,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = p.board(&place.repo)?;
        let task = task_at(board, &place, &file)?;
        let parse = |kind: &str| {
            rite_lib::links::LinkType::parse(kind)
                .ok_or_else(|| format!("unknown link type {kind:?}"))
        };
        let (old, new) = (parse(&old)?, parse(&new)?);
        let other = board.find(&other).map_err(err)?;
        let outcome = transition::retype_link(board, &task, old, new, &other).map_err(err)?;
        Ok(report_outcome(board, message("relink", &task), &outcome))
    })
    .await
}

/// The requires tree around a ticket (ticket 214): its family on its own
/// board, as `rite tree <ref>` draws it; done tickets only with
/// `include_done` (ticket 217).
#[tauri::command]
fn ticket_family(
    place: Place,
    file: String,
    include_done: bool,
) -> Result<Vec<rite_lib::requires::TreeNode>> {
    let project = Project::open()?;
    let board = project.board(&place.repo)?;
    let task = task_at(board, &place, &file)?;
    rite_lib::requires::family(board, Some(&task), include_done).map_err(err)
}

/// Parent and child around a ticket (ticket 261), as `rite tree <ref>
/// --children` prints it; done tickets only with `include_done`.
#[tauri::command]
fn ticket_hierarchy(
    place: Place,
    file: String,
    include_done: bool,
) -> Result<Vec<rite_lib::requires::TreeNode>> {
    let project = Project::open()?;
    let board = project.board(&place.repo)?;
    let task = task_at(board, &place, &file)?;
    rite_lib::hierarchy::hierarchy(board, Some(&task), include_done).map_err(err)
}

/// Takes a ticket off the board into the archive (`rite abandon`).
#[tauri::command]
async fn archive_ticket(
    state: Tauri<'_, AppState>,
    place: Place,
    file: String,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = p.board(&place.repo)?;
        let task = task_at(board, &place, &file)?;
        let (id, title) = (task.id_display(), task.title());
        committed(board, "abandon", &id, &title, || {
            board.archive(&task).map(|_| ())
        })
    })
    .await
}

/// Locks a ticket for the user (`rite lock`), or renews their own lock.
#[tauri::command]
async fn lock_ticket(state: Tauri<'_, AppState>, place: Place, file: String) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = p.board(&place.repo)?;
        let task = task_at(board, &place, &file)?;
        let hours = board_settings::load(board).lease_hours();
        let (id, title) = (task.id_display(), task.title());
        committed(board, "lock", &id, &title, || {
            task.take_lease(task.hash()?, hours)
        })
    })
    .await
}

/// Releases the user's own lock, or with `force` breaks someone else's
/// (`rite unlock [--force]`).
#[tauri::command]
async fn unlock_ticket(
    state: Tauri<'_, AppState>,
    place: Place,
    file: String,
    force: Option<bool>,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = p.board(&place.repo)?;
        let task = task_at(board, &place, &file)?;
        let (id, title) = (task.id_display(), task.title());
        committed(board, "unlock", &id, &title, || {
            if force.unwrap_or(false) {
                task.break_lease(task.hash()?)
            } else {
                task.release_lease(task.hash()?)
            }
        })
    })
    .await
}

/// Saves a board setting through rite-lib, committed and pushed like any
/// change, since every setting lives in the tickets repository (ticket 111).
fn save_setting(
    board: &Board,
    what: &str,
    change: impl FnOnce(&mut board_settings::BoardSettings) -> anyhow::Result<()>,
) -> Result<SyncReport> {
    let mut settings = board_settings::load(board);
    change(&mut settings).map_err(err)?;
    committed(board, "settings", what, "", || settings.save(board))
}

/// Where a column goes, from the page: after or before another, else last.
fn column_position(
    after: Option<rite_ui_model::Column>,
    before: Option<rite_ui_model::Column>,
) -> rite_lib::columns::Position {
    use rite_lib::columns::Position;
    match (after, before) {
        (Some(c), _) => Position::After(boards::to_column(c)),
        (None, Some(c)) => Position::Before(boards::to_column(c)),
        (None, None) => Position::End,
    }
}

/// Adds a column to the project's board (ticket 224), of `kind`.
#[tauri::command]
async fn add_column(
    state: Tauri<'_, AppState>,
    label: String,
    kind: String,
    after: Option<rite_ui_model::Column>,
    before: Option<rite_ui_model::Column>,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = &p.repos[0].1;
        let kind = rite_lib::board::Kind::parse(&kind)
            .ok_or_else(|| format!("{kind:?} isn't a kind: backlog, todo, doing or done"))?;
        let at = column_position(after, before);
        committed(board, "column", "add", label.trim(), || {
            rite_lib::columns::add(board, &label, kind, at, None).map(|_| ())
        })
    })
    .await
}

/// Gives one of the project's columns a new name; its folder stays.
#[tauri::command]
async fn rename_column(
    state: Tauri<'_, AppState>,
    column: rite_ui_model::Column,
    label: String,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = &p.repos[0].1;
        committed(board, "column", "rename", label.trim(), || {
            rite_lib::columns::rename(board, boards::to_column(column), &label)
        })
    })
    .await
}

/// Moves one of the project's columns along the board.
#[tauri::command]
async fn move_column(
    state: Tauri<'_, AppState>,
    column: rite_ui_model::Column,
    after: Option<rite_ui_model::Column>,
    before: Option<rite_ui_model::Column>,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = &p.repos[0].1;
        let col = boards::to_column(column);
        let label = board.workflow().label(col);
        committed(board, "column", "move", &label, || {
            rite_lib::columns::reorder(board, col, column_position(after, before))
        })
    })
    .await
}

/// Removes an added column, its tickets moved to `move_to` first.
#[tauri::command]
async fn remove_column(
    state: Tauri<'_, AppState>,
    column: rite_ui_model::Column,
    move_to: Option<rite_ui_model::Column>,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = &p.repos[0].1;
        let col = boards::to_column(column);
        let label = board.workflow().label(col);
        committed(board, "column", "remove", &label, || {
            rite_lib::columns::remove(board, col, move_to.map(boards::to_column)).map(|_| ())
        })
    })
    .await
}

/// Sets a doing-kind column's own WIP limit, or clears it.
#[tauri::command]
async fn set_column_wip(
    state: Tauri<'_, AppState>,
    column: rite_ui_model::Column,
    limit: Option<u32>,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = &p.repos[0].1;
        let col = boards::to_column(column);
        let label = board.workflow().label(col);
        committed(board, "column", "wip", &label, || {
            rite_lib::columns::set_wip(board, col, limit)
        })
    })
    .await
}

/// Any column to any other, or the next column plus the extra moves
/// (ticket 225).
#[tauri::command]
async fn set_flow(state: Tauri<'_, AppState>, any: bool) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = &p.repos[0].1;
        let flow = if any {
            rite_lib::workflow::Flow::Any
        } else {
            rite_lib::workflow::Flow::Next
        };
        committed(board, "column", "flow", flow.as_str(), || {
            rite_lib::columns::set_flow(board, flow)
        })
    })
    .await
}

/// Allows an extra one-way move, or stops allowing it.
#[tauri::command]
async fn allow_move(
    state: Tauri<'_, AppState>,
    from: rite_ui_model::Column,
    to: rite_ui_model::Column,
    on: bool,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = &p.repos[0].1;
        let (f, t) = (boards::to_column(from), boards::to_column(to));
        let what = format!("{}>{}", f.as_str(), t.as_str());
        committed(
            board,
            "column",
            if on { "allow" } else { "disallow" },
            &what,
            || rite_lib::columns::allow_move(board, f, t, on),
        )
    })
    .await
}

/// Where new tickets land: a backlog-kind column.
#[tauri::command]
async fn set_landing(
    state: Tauri<'_, AppState>,
    column: rite_ui_model::Column,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = &p.repos[0].1;
        let col = boards::to_column(column);
        let label = board.workflow().label(col);
        committed(board, "column", "landing", &label, || {
            rite_lib::columns::set_landing(board, col)
        })
    })
    .await
}

/// Back to the standard four columns, tickets moved by kind.
#[tauri::command]
async fn reset_columns(state: Tauri<'_, AppState>) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = &p.repos[0].1;
        committed(board, "column", "reset", "standard", || {
            rite_lib::columns::reset(board).map(|_| ())
        })
    })
    .await
}

/// Adds a tag to the project's list, in UPPERCASE (ticket 122).
#[tauri::command]
async fn add_tag(state: Tauri<'_, AppState>, tag: String) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = &p.repos[0].1;
        let name = rite_lib::task::normalize_tag(&tag);
        let mut settings = board_settings::load(board);
        settings.add_tag(&tag).map_err(err)?;
        committed(board, "tags", "add", &name, || settings.save(board))
    })
    .await
}

/// Takes an unused tag off the project's list: refused while any ticket on
/// the project's boards carries it (ticket 122).
#[tauri::command]
async fn remove_tag(state: Tauri<'_, AppState>, tag: String) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = &p.repos[0].1;
        let boards = p.boards();
        let name = rite_lib::task::normalize_tag(&tag);
        committed(board, "tags", "remove", &name, || {
            rite_lib::model::remove_unused_tag(board, &boards, &tag).map(|_| ())
        })
    })
    .await
}

/// Whether only listed tags may be put on a ticket (ticket 122).
#[tauri::command]
async fn set_tags_only(state: Tauri<'_, AppState>, on: bool) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = &p.repos[0].1;
        save_setting(
            board,
            &format!("prevent-arbitrary-tags {}", if on { "on" } else { "off" }),
            |s| {
                s.set_only_listed_tags(on);
                Ok(())
            },
        )
    })
    .await
}

/// Sets the name people read for the project (ticket 326), or clears it
/// when `name` is empty, so the project's own name shows again.
#[tauri::command]
async fn set_display_name(state: Tauri<'_, AppState>, name: String) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = &p.repos[0].1;
        let name = name.trim().to_string();
        let what = if name.is_empty() {
            "display-name cleared".to_string()
        } else {
            format!("display-name {name}")
        };
        save_setting(board, &what, |s| s.set_display_name(Some(name.as_str())))
    })
    .await
}

/// Names the documents repository every project shares (ticket 129), or
/// clears it when `name` is empty.
#[tauri::command]
async fn set_shared_documents(state: Tauri<'_, AppState>, name: String) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = &p.repos[0].1;
        let name = name.trim().to_string();
        let what = if name.is_empty() {
            "shared-documents cleared".to_string()
        } else {
            format!("shared-documents {name}")
        };
        save_setting(board, &what, |s| {
            s.set_shared_documents(Some(name.as_str()))
        })
    })
    .await
}

/// Pulls in open GitHub issues for every project on this machine whose
/// switch is on (ticket 173), when the landing page loads: at most once per
/// project every five minutes. Quiet about a project with nothing on
/// GitHub, no `gh`, or no network; one report per project that got
/// something.
///
/// GitHub is asked **without** the write lock (ticket 181): this runs on
/// every cold start and takes seconds per project, and holding the lock
/// made every button that opens a project wait for it. Only a project
/// with something new takes the lock, to catch up, plan again against
/// the counter as it is then, write and commit.
#[tauri::command]
async fn pull_issues(state: Tauri<'_, AppState>) -> Result<Vec<SyncReport>> {
    let writing = Arc::clone(&state.writing);
    let pulled = Arc::clone(&state.issues_pulled);
    blocking(move || {
        use rite_lib::import_issues as issues;
        let mut reports = Vec::new();
        for project in rite_lib::my_work::projects() {
            if project.archived {
                continue;
            }
            {
                let mut seen = pulled.lock().map_err(|_| "restart riteui".to_string())?;
                if seen
                    .get(&project.root)
                    .is_some_and(|at| at.elapsed() < ISSUE_PULL_EVERY)
                {
                    continue;
                }
                seen.insert(project.root.clone(), std::time::Instant::now());
            }
            let board = Board::in_repo(&project.root);
            if !board_settings::load(&board).import_issues() {
                continue;
            }
            let Ok(targets) = issues::targets(&board, None) else {
                continue;
            };
            // The network, unlocked: each repository's open issues, once.
            let fetched: Vec<(String, std::result::Result<Vec<issues::Issue>, String>)> = targets
                .iter()
                .map(|t| {
                    (
                        t.slug.clone(),
                        issues::fetch(&t.slug, None).map_err(|e| format!("{e:#}")),
                    )
                })
                .collect();
            let answer = |slug: &str, _: Option<&str>| -> anyhow::Result<Vec<issues::Issue>> {
                match fetched.iter().find(|(s, _)| s == slug) {
                    Some((_, Ok(found))) => Ok(found.clone()),
                    Some((_, Err(e))) => Err(anyhow::anyhow!("{e}")),
                    None => Ok(Vec::new()),
                }
            };
            // Anything new? Read-only, so still unlocked.
            match issues::plan_with(&board, &targets, None, answer) {
                Ok(plan) if !plan.new.is_empty() => {}
                _ => continue,
            }
            let _guard = writing
                .lock()
                .map_err(|_| "the board is in a bad state — restart riteui".to_string())?;
            if rite_lib::sync::catch_up(&board, rite_lib::sync::Purpose::Write).is_err() {
                continue;
            }
            let Ok(plan) = issues::plan_with(&board, &targets, None, answer) else {
                continue;
            };
            if plan.new.is_empty() {
                continue;
            }
            let detail = format!("{} from GitHub", plan.new.len());
            let Ok((done, warning)) =
                transition::with_auto_commit(&board, "import", "issues", &detail, || {
                    issues::apply(&board, &plan)
                })
            else {
                continue;
            };
            let n = done.len();
            reports.push(report(
                &board,
                format!(
                    "Imported {n} GitHub issue{} into {}",
                    if n == 1 { "" } else { "s" },
                    project.name
                ),
                warning,
            ));
        }
        Ok(reports)
    })
    .await
}

/// Whether the landing page pulls in this project's GitHub issues (ticket
/// 173).
#[tauri::command]
async fn set_import_issues(state: Tauri<'_, AppState>, on: bool) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = &p.repos[0].1;
        save_setting(
            board,
            &format!("import-issues {}", if on { "on" } else { "off" }),
            |s| {
                s.set_import_issues(on);
                Ok(())
            },
        )
    })
    .await
}

/// Whether priorities follow requires links (ticket 215). Switching it
/// commits the setting and every priority it moves, together.
#[tauri::command]
async fn set_auto_priority(state: Tauri<'_, AppState>, on: bool) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = &p.repos[0].1;
        save_setting(
            board,
            &format!("auto-priority {}", if on { "on" } else { "off" }),
            |s| {
                s.set_auto_priority(on);
                Ok(())
            },
        )
    })
    .await
}

/// How long a ticket should take in todo and in doing (ticket 252); 0 is
/// no limit.
#[tauri::command]
async fn set_time_limits(state: Tauri<'_, AppState>, todo: u32, doing: u32) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = &p.repos[0].1;
        save_setting(board, &format!("time-limits {todo} {doing}"), |s| {
            s.set_time_limits(board_settings::TimeLimits { todo, doing });
            Ok(())
        })
    })
    .await
}

/// How near a due date sends a ticket straight to todo (ticket 253).
#[tauri::command]
async fn set_due_to_todo_days(state: Tauri<'_, AppState>, days: u32) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = &p.repos[0].1;
        save_setting(board, &format!("due-to-todo-days {days}"), |s| {
            s.set_due_to_todo_days(days);
            Ok(())
        })
    })
    .await
}

/// Whether the backlog column can be folded away (ticket 253).
#[tauri::command]
async fn set_backlog_always_shown(state: Tauri<'_, AppState>, on: bool) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = &p.repos[0].1;
        save_setting(
            board,
            &format!("backlog-always-shown {}", if on { "on" } else { "off" }),
            |s| {
                s.set_backlog_always_shown(on);
                Ok(())
            },
        )
    })
    .await
}

/// Whether a priority set by hand pins its ticket (ticket 215).
#[tauri::command]
async fn set_priority_override(state: Tauri<'_, AppState>, on: bool) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = &p.repos[0].1;
        save_setting(
            board,
            &format!("priority-override {}", if on { "on" } else { "off" }),
            |s| {
                s.set_priority_override(on);
                Ok(())
            },
        )
    })
    .await
}

/// Brings a repository's open GitHub issues into the backlog, tagged
/// TRIAGE (ticket 158), as `rite issues` does; `preview` only says
/// what it would do. `repo` empty means every declared repository on
/// GitHub; `label` empty means every issue.
#[tauri::command]
async fn import_issues(
    state: Tauri<'_, AppState>,
    repo: String,
    label: String,
    preview: bool,
) -> Result<rite_ui_model::IssueImport> {
    use rite_lib::import_issues as issues;
    let some = |s: &str| Some(s.trim().to_string()).filter(|s| !s.is_empty());
    let (repo, label) = (some(&repo), some(&label));
    let run = move |p: &Project| -> Result<rite_ui_model::IssueImport> {
        let board = &p.repos[0].1;
        let targets = issues::targets(board, repo.as_deref()).map_err(err)?;
        let plan = issues::plan(board, &targets, label.as_deref()).map_err(err)?;
        if preview || plan.new.is_empty() {
            return Ok(rite_ui_model::IssueImport {
                lines: plan.new.iter().map(issues::Planned::line).collect(),
                summary: plan.summary(preview),
                sync: None,
            });
        }
        // Only the repositories it could read (ticket 180).
        let slugs: Vec<&str> = targets
            .iter()
            .map(|t| t.slug.as_str())
            .filter(|s| !plan.skipped.iter().any(|(k, _)| k == s))
            .collect();
        let detail = format!("{} from {}", plan.new.len(), slugs.join(", "));
        let (done, warning) =
            transition::with_auto_commit(board, "import", "issues", &detail, || {
                issues::apply(board, &plan)
            })
            .map_err(err)?;
        Ok(rite_ui_model::IssueImport {
            lines: done.iter().map(issues::Planned::line).collect(),
            summary: plan.summary(false),
            sync: Some(report(
                board,
                transition::commit_message("import", "issues", &detail),
                warning,
            )),
        })
    };
    if preview {
        blocking(move || run(&Project::open()?)).await
    } else {
        change(&state, run).await
    }
}

/// The project's documents repositories and what's in them (ticket 129):
/// its own, then the shared one. One not cloned here is listed empty.
#[tauri::command]
async fn list_documents() -> Result<Vec<rite_ui_model::DocSource>> {
    blocking(|| {
        let project = Project::open()?;
        Ok(rite_lib::documents::sources(&project.repos[0].1)
            .into_iter()
            .map(|source| {
                let documents = source
                    .root
                    .as_deref()
                    .and_then(|root| rite_lib::documents::list(root).ok())
                    .unwrap_or_default();
                let folders = source
                    .root
                    .as_deref()
                    .map(rite_lib::documents_edit::folders)
                    .unwrap_or_default();
                // A start page is the project's own (ticket 179).
                let own = source.kind == rite_lib::documents::Kind::Own;
                let start = source
                    .root
                    .as_deref()
                    .filter(|_| own)
                    .and_then(rite_lib::documents_edit::start_page);
                let pinned = own
                    && source
                        .root
                        .as_deref()
                        .and_then(rite_lib::documents_edit::pinned)
                        .is_some();
                rite_ui_model::DocSource {
                    start,
                    pinned,
                    shared: source.kind == rite_lib::documents::Kind::Shared,
                    cloned: source.root.is_some(),
                    name: source.name,
                    documents: documents
                        .into_iter()
                        .map(|d| rite_ui_model::DocEntry {
                            path: d.path,
                            title: d.title,
                            archived: d.archived,
                        })
                        .collect(),
                    folders,
                }
            })
            .collect())
    })
    .await
}

/// The document `path` in the documents repository `source`, rendered for
/// the reader (ticket 129). Only the project's own documents repositories
/// can be read, and nothing outside them.
#[tauri::command]
async fn read_document(source: String, path: String) -> Result<rite_ui_model::RenderedDoc> {
    blocking(move || {
        let root = documents_root(&source)?;
        let text = rite_lib::documents::read(&root, &path).map_err(err)?;
        Ok(rite_ui_model::RenderedDoc {
            html: documents::render(&root, &path, &text),
            archived: rite_lib::documents_edit::is_archived(&text),
            source,
            path,
            version: None,
        })
    })
    .await
}

/// The project's ideas, best first (ticket 131), with your own vote on
/// each.
#[tauri::command]
async fn list_ideas() -> Result<rite_ui_model::IdeaList> {
    blocking(|| {
        let project = Project::open()?;
        let source = rite_lib::ideas::source(&project.repos[0].1);
        let me = source
            .root
            .as_deref()
            .and_then(|root| rite_lib::ideas::Person::of(root).ok())
            .map(|p| p.email)
            .unwrap_or_default();
        Ok(rite_ui_model::IdeaList {
            cloned: source.root.is_some(),
            ideas: rite_lib::ideas::list(&source)
                .iter()
                .map(|i| idea_card(i, &me))
                .collect(),
            source: source.name,
        })
    })
    .await
}

fn idea_card(idea: &rite_lib::ideas::Idea, me: &str) -> rite_ui_model::IdeaCard {
    rite_ui_model::IdeaCard {
        path: idea.path.clone(),
        title: idea.title.clone(),
        author: idea
            .author
            .as_deref()
            .map(|a| a.split('<').next().unwrap_or(a).trim().to_string()),
        created: idea.created.clone(),
        score: idea.score(),
        voters: idea.votes.len(),
        mine: idea.vote_of(me),
        notes: idea.notes.len(),
        tickets: idea.tickets.clone(),
        epics: idea.epics.clone(),
        sprints: idea.sprints.clone(),
        archived: idea.archived,
    }
}

/// Where the project's ideas are, and who "you" are there.
fn ideas_root() -> Result<(Board, PathBuf, String, rite_lib::ideas::Person)> {
    let project = Project::open()?;
    let board = Board::at(project.repos[0].1.root.clone());
    let source = rite_lib::ideas::source(&board);
    let root = source
        .root
        .ok_or_else(|| format!("{} isn't cloned on this machine", source.name))?;
    let me = rite_lib::ideas::Person::of(&root).map_err(err)?;
    Ok((board, root, source.name, me))
}

/// One idea opened: the write-up rendered, the votes and the notes.
#[tauri::command]
async fn read_idea(path: String) -> Result<rite_ui_model::IdeaDetail> {
    blocking(move || {
        let (_, root, source, me) = ideas_root()?;
        let idea = rite_lib::ideas::read(&root, &source, &path).map_err(err)?;
        Ok(rite_ui_model::IdeaDetail {
            card: idea_card(&idea, &me.email),
            html: documents::render(&root, &path, &rite_lib::ideas::write_up(&idea.text)),
            votes: idea
                .votes
                .iter()
                .map(|v| rite_ui_model::IdeaVote {
                    name: v.name.clone(),
                    value: v.value,
                })
                .collect(),
            notes: idea
                .notes
                .iter()
                .map(|n| rite_ui_model::IdeaNote {
                    name: n.name.clone(),
                    date: n.date.clone(),
                    text: n.text.clone(),
                })
                .collect(),
            source,
        })
    })
    .await
}

/// Runs a change to the project's ideas under the write lock; refused in an
/// archived project (ticket 157).
async fn idea_change(
    state: &Tauri<'_, AppState>,
    change: impl FnOnce(
        &Path,
        &rite_lib::ideas::Person,
    ) -> anyhow::Result<(Option<String>, rite_lib::documents::Restored)>
    + Send
    + 'static,
) -> Result<rite_ui_model::DocChange> {
    let writing = Arc::clone(&state.writing);
    blocking(move || {
        let _guard = writing
            .lock()
            .map_err(|_| "the board is in a bad state — restart riteui".to_string())?;
        let (board, root, _, me) = ideas_root()?;
        rite_lib::archive::ensure_writable(&board).map_err(err)?;
        let (path, done) = change(&root, &me).map_err(err)?;
        Ok(rite_ui_model::DocChange {
            sync: SyncReport {
                committed: true,
                pushed: done.pushed,
                warning: done.warning,
                message: done.message,
            },
            path,
        })
    })
    .await
}

/// Puts an idea forward (ticket 131), with your name on it when `named`.
#[tauri::command]
async fn create_idea(
    state: Tauri<'_, AppState>,
    title: String,
    body: String,
    named: bool,
) -> Result<rite_ui_model::DocChange> {
    idea_change(&state, move |root, me| {
        let (path, done) = rite_lib::ideas::create(root, &title, &body, named.then_some(me))?;
        Ok((Some(path), done))
    })
    .await
}

/// One click on an idea's + or −.
#[tauri::command]
async fn vote_idea(
    state: Tauri<'_, AppState>,
    path: String,
    delta: i32,
) -> Result<rite_ui_model::DocChange> {
    idea_change(&state, move |root, me| {
        let (_, done) = rite_lib::ideas::vote(root, &path, delta.clamp(-1, 1), me)?;
        Ok((Some(path), done))
    })
    .await
}

/// Adds your note under an idea.
#[tauri::command]
async fn note_idea(
    state: Tauri<'_, AppState>,
    path: String,
    text: String,
) -> Result<rite_ui_model::DocChange> {
    idea_change(&state, move |root, me| {
        let done = rite_lib::ideas::note(root, &path, &text, me)?;
        Ok((Some(path), done))
    })
    .await
}

/// Makes a backlog ticket from an idea, linked both ways.
#[tauri::command]
async fn idea_to_ticket(
    state: Tauri<'_, AppState>,
    path: String,
) -> Result<rite_ui_model::IdeaTicket> {
    change(&state, move |p| {
        let board = &p.repos[0].1;
        let source = rite_lib::ideas::source(board);
        let root = source
            .root
            .ok_or_else(|| format!("{} isn't cloned on this machine", source.name))?;
        let (id, events, done) =
            rite_lib::ideas::to_ticket(board, &root, &source.name, &path).map_err(err)?;
        let task = board.find(&id).map_err(err)?;
        let mut warnings: Vec<String> = events
            .into_iter()
            .filter_map(|e| match e {
                Event::Warn(w) => Some(w),
                Event::Info(_) => None,
            })
            .collect();
        warnings.extend(done.warning.clone());
        Ok(rite_ui_model::IdeaTicket {
            sync: SyncReport {
                committed: true,
                pushed: warnings.is_empty(),
                warning: (!warnings.is_empty()).then(|| warnings.join("; ")),
                message: format!("ticket {id} made from the idea"),
            },
            repo: board
                .repo_root()
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            file: task.file_name(),
            id,
        })
    })
    .await
}

/// Makes an epic or a pending sprint from an idea (ticket 231): empty, its
/// description the idea's write-up. The project's admins only; anyone can
/// make a backlog ticket.
#[tauri::command]
async fn idea_to_group(
    state: Tauri<'_, AppState>,
    path: String,
    sprint: bool,
    name: String,
    days: u32,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = &p.repos[0].1;
        if !boards::admin_access(board).allowed {
            return Err(
                "Only the project's admins can make an epic or a sprint from an idea.".into(),
            );
        }
        let source = rite_lib::ideas::source(board);
        let root = source
            .root
            .ok_or_else(|| format!("{} isn't cloned on this machine", source.name))?;
        let kind = if sprint { Kind::Sprint } else { Kind::Epic };
        let (made, warning, done) =
            rite_lib::ideas::to_group(board, &root, &source.name, &path, kind, &name, days)
                .map_err(err)?;
        let warnings: Vec<String> = warning.into_iter().chain(done.warning.clone()).collect();
        Ok(SyncReport {
            committed: true,
            pushed: warnings.is_empty(),
            warning: (!warnings.is_empty()).then(|| warnings.join("; ")),
            message: format!(
                "{} {made} made from the idea",
                if sprint { "sprint" } else { "epic" }
            ),
        })
    })
    .await
}

/// Where the documents repository `source` (the project's own or the
/// shared one) is cloned; an error for any other name, or one not cloned.
fn documents_root(source: &str) -> Result<PathBuf> {
    let project = Project::open()?;
    rite_lib::documents::sources(&project.repos[0].1)
        .into_iter()
        .find(|s| s.name == source)
        .ok_or_else(|| format!("{source} isn't one of this project's documents repositories"))?
        .root
        .ok_or_else(|| format!("{source} isn't cloned on this machine"))
}

fn doc_version(v: rite_lib::documents::Version) -> rite_ui_model::DocVersion {
    rite_ui_model::DocVersion {
        commit: v.commit,
        short: v.short,
        author: v.author,
        date: v.date,
        subject: v.subject,
        path: v.path,
    }
}

/// The last 20 versions of a document, newest first (ticket 169).
#[tauri::command]
async fn document_history(source: String, path: String) -> Result<Vec<rite_ui_model::DocVersion>> {
    blocking(move || {
        let root = documents_root(&source)?;
        Ok(rite_lib::documents::history(&root, &path, 20)
            .map_err(err)?
            .into_iter()
            .map(doc_version)
            .collect())
    })
    .await
}

/// An earlier version of a document, rendered for the reader against
/// today's tree (images, links) (ticket 169).
#[tauri::command]
async fn read_document_version(
    source: String,
    path: String,
    commit: String,
) -> Result<rite_ui_model::RenderedDoc> {
    blocking(move || {
        let root = documents_root(&source)?;
        let version = rite_lib::documents::history(&root, &path, usize::MAX)
            .map_err(err)?
            .into_iter()
            .find(|v| v.commit == commit)
            .ok_or_else(|| format!("{commit} isn't a version of {path}"))?;
        let text = rite_lib::documents::read_version(&root, &version.path, &version.commit)
            .map_err(err)?;
        Ok(rite_ui_model::RenderedDoc {
            html: documents::render(&root, &path, &text),
            archived: rite_lib::documents_edit::is_archived(&text),
            source,
            path,
            version: Some(doc_version(version)),
        })
    })
    .await
}

/// Runs a documents-repository change under the write lock and reports it
/// (ticket 176).
async fn doc_change(
    state: &Tauri<'_, AppState>,
    source: String,
    change: impl FnOnce(&Path) -> anyhow::Result<(Option<String>, rite_lib::documents::Restored)>
    + Send
    + 'static,
) -> Result<rite_ui_model::DocChange> {
    let writing = Arc::clone(&state.writing);
    blocking(move || {
        let _guard = writing
            .lock()
            .map_err(|_| "the board is in a bad state — restart riteui".to_string())?;
        let root = documents_root(&source)?;
        // Not the documents of an archived project (ticket 157); a shared
        // repository belongs to no one project.
        let project = Project::open()?;
        let board = &project.repos[0].1;
        if source
            == format!(
                "{}-documents",
                rite_lib::current_project::project_name(board.repo_root())
            )
        {
            rite_lib::archive::ensure_writable(board).map_err(err)?;
        }
        let (path, done) = change(&root).map_err(err)?;
        Ok(rite_ui_model::DocChange {
            sync: SyncReport {
                committed: true,
                pushed: done.pushed,
                warning: done.warning,
                message: done.message,
            },
            path,
        })
    })
    .await
}

/// Your pinned documents (ticket 232), kept in the project's own documents
/// repository so they follow you across machines. Empty when it isn't
/// cloned or git has no email for you there.
#[tauri::command]
async fn my_pins() -> Result<Vec<rite_ui_model::DocPin>> {
    blocking(|| {
        let project = Project::open()?;
        let sources = rite_lib::documents::sources(&project.repos[0].1);
        let Some(own) = sources
            .iter()
            .find(|s| s.kind == rite_lib::documents::Kind::Own)
            .and_then(|s| s.root.clone())
        else {
            return Ok(Vec::new());
        };
        let Ok(me) = rite_lib::doc_pins::me(&own) else {
            return Ok(Vec::new());
        };
        let mut lists = std::collections::HashMap::new();
        Ok(rite_lib::doc_pins::pins(&own, &me)
            .into_iter()
            .map(|pin| {
                let docs = lists.entry(pin.source.clone()).or_insert_with(|| {
                    sources
                        .iter()
                        .find(|s| s.name == pin.source)
                        .and_then(|s| s.root.as_deref())
                        .and_then(|root| rite_lib::documents::list(root).ok())
                        .unwrap_or_default()
                });
                let found = docs.iter().find(|d| d.path == pin.path);
                rite_ui_model::DocPin {
                    title: found.map_or_else(
                        || pin.path.rsplit('/').next().unwrap_or(&pin.path).to_string(),
                        |d| d.title.clone(),
                    ),
                    exists: found.is_some(),
                    source: pin.source,
                    path: pin.path,
                }
            })
            .collect())
    })
    .await
}

/// Pins a document to your own list (ticket 232), or unpins it: committed
/// to the project's own documents repository, wherever the document is.
#[tauri::command]
async fn set_doc_pin(
    state: Tauri<'_, AppState>,
    source: String,
    path: String,
    on: bool,
) -> Result<rite_ui_model::DocChange> {
    let own = blocking(|| {
        let project = Project::open()?;
        rite_lib::documents::sources(&project.repos[0].1)
            .into_iter()
            .find(|s| s.kind == rite_lib::documents::Kind::Own)
            .map(|s| s.name)
            .ok_or_else(|| "this project has no documents repository".to_string())
    })
    .await?;
    doc_change(&state, own, move |root| {
        let me = rite_lib::doc_pins::me(root)?;
        Ok((
            None,
            rite_lib::doc_pins::set(root, &me, &source, &path, on)?,
        ))
    })
    .await
}

/// Pins a document as the project's start page (ticket 179), or with no
/// path goes back to the README.
#[tauri::command]
async fn pin_start_page(
    state: Tauri<'_, AppState>,
    source: String,
    path: Option<String>,
) -> Result<rite_ui_model::DocChange> {
    doc_change(&state, source, move |root| {
        Ok((
            None,
            rite_lib::documents_edit::pin_start_page(root, path.as_deref())?,
        ))
    })
    .await
}

/// Searches the project's documents and the shared ones (ticket 179):
/// titles and text, ignoring case, at most 3 lines a document and 200 in
/// all.
#[tauri::command]
async fn search_documents(query: String) -> Result<Vec<rite_ui_model::DocHit>> {
    blocking(move || {
        let project = Project::open()?;
        let mut out = Vec::new();
        for source in rite_lib::documents::sources(&project.repos[0].1) {
            let Some(root) = source.root else { continue };
            for hit in rite_lib::documents_edit::search(&root, &query, 3).map_err(err)? {
                out.push(rite_ui_model::DocHit {
                    source: source.name.clone(),
                    path: hit.path,
                    title: hit.title,
                    line: hit.line,
                    text: hit.text,
                });
            }
        }
        out.truncate(200);
        Ok(out)
    })
    .await
}

/// Marks a document archived, or not (ticket 176).
#[tauri::command]
async fn archive_document(
    state: Tauri<'_, AppState>,
    source: String,
    path: String,
    on: bool,
) -> Result<rite_ui_model::DocChange> {
    doc_change(&state, source, move |root| {
        Ok((
            None,
            rite_lib::documents_edit::set_archived(root, &path, on)?,
        ))
    })
    .await
}

/// Makes a folder in a documents repository (ticket 176).
#[tauri::command]
async fn create_document_folder(
    state: Tauri<'_, AppState>,
    source: String,
    parent: String,
    name: String,
) -> Result<rite_ui_model::DocChange> {
    doc_change(&state, source, move |root| {
        Ok((
            None,
            rite_lib::documents_edit::create_folder(root, &parent, &name)?,
        ))
    })
    .await
}

/// Removes an empty folder from a documents repository (ticket 176).
#[tauri::command]
async fn delete_document_folder(
    state: Tauri<'_, AppState>,
    source: String,
    folder: String,
) -> Result<rite_ui_model::DocChange> {
    doc_change(&state, source, move |root| {
        Ok((
            None,
            rite_lib::documents_edit::delete_folder(root, &folder)?,
        ))
    })
    .await
}

/// Moves a document into a folder, its links kept working (ticket 176).
#[tauri::command]
async fn move_document(
    state: Tauri<'_, AppState>,
    source: String,
    path: String,
    folder: String,
) -> Result<rite_ui_model::DocChange> {
    doc_change(&state, source, move |root| {
        let (to, done) = rite_lib::documents_edit::move_document(root, &path, &folder)?;
        Ok((Some(to), done))
    })
    .await
}

/// Makes a new document titled `title` in `folder` (ticket 176).
#[tauri::command]
async fn create_document(
    state: Tauri<'_, AppState>,
    source: String,
    folder: String,
    title: String,
) -> Result<rite_ui_model::DocChange> {
    doc_change(&state, source, move |root| {
        let (path, done) = rite_lib::documents_edit::create_document(root, &folder, &title)?;
        Ok((Some(path), done))
    })
    .await
}

/// A document's text for editing: its frontmatter apart, the body, its
/// version, and what (if anything) keeps it out of rich text (ticket 176).
#[tauri::command]
async fn open_document_for_edit(source: String, path: String) -> Result<rite_ui_model::DocEdit> {
    blocking(move || {
        let root = documents_root(&source)?;
        let text = rite_lib::documents::read(&root, &path).map_err(err)?;
        let (front, body) = rite_lib::documents_edit::split_frontmatter(&text);
        Ok(rite_ui_model::DocEdit {
            version: rite_lib::documents_edit::version(&text),
            frontmatter: front.unwrap_or_default().to_string(),
            unsupported: rite_ui_model::rich::unsupported(body)
                .into_iter()
                .map(str::to_string)
                .collect(),
            body: body.to_string(),
            source,
            path,
        })
    })
    .await
}

/// Saves an edited document, unless it changed since it was opened
/// (ticket 176).
#[tauri::command]
async fn save_document(
    state: Tauri<'_, AppState>,
    source: String,
    path: String,
    text: String,
    version: String,
) -> Result<rite_ui_model::DocChange> {
    doc_change(&state, source, move |root| {
        Ok((
            None,
            rite_lib::documents_edit::save(root, &path, &text, &version)?,
        ))
    })
    .await
}

/// Puts a document back as it was at `commit`, as a new commit, pushed
/// (ticket 169; ADR 0007). Serialised with every other write.
#[tauri::command]
async fn restore_document(
    state: Tauri<'_, AppState>,
    source: String,
    path: String,
    commit: String,
) -> Result<SyncReport> {
    let writing = Arc::clone(&state.writing);
    blocking(move || {
        let _guard = writing
            .lock()
            .map_err(|_| "the board is in a bad state — restart riteui".to_string())?;
        let root = documents_root(&source)?;
        let done = rite_lib::documents::restore(&root, &path, &commit).map_err(err)?;
        Ok(SyncReport {
            committed: true,
            pushed: done.pushed,
            warning: done.warning,
            message: done.message,
        })
    })
    .await
}

/// Opens a web or mail link from a document in the system's own browser or
/// mail program, never in riteui's window (ticket 129).
#[tauri::command]
async fn open_external(url: String) -> Result<()> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("https://")
        || lower.starts_with("http://")
        || lower.starts_with("mailto:"))
    {
        return Err(format!("rite only opens web and mail links, not {url}"));
    }
    // Windows (ticket 279): `rundll32 url.dll,FileProtocolHandler` hands the
    // link to the default browser or mail program; `cmd /c start` would
    // split it at every `&`.
    let mut opener = if cfg!(windows) {
        let mut cmd = std::process::Command::new("rundll32");
        cmd.arg("url.dll,FileProtocolHandler");
        cmd
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else {
        std::process::Command::new("xdg-open")
    };
    opener
        .arg(&url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("couldn't open {url}: {e}"))
}

/// Sets `repo`'s WIP limit on doing (0 turns it off).
#[tauri::command]
async fn set_wip_limits(
    state: Tauri<'_, AppState>,
    limits: WipLimits,
    repo: Option<String>,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = p.board(&repo.unwrap_or_default())?;
        save_setting(board, &format!("wip-limit {}", limits.doing), |s| {
            s.set_wip_limit(Column::Doing, Some(limits.doing))
        })
    })
    .await
}

/// Sets how long a lock lasts on `repo`'s board.
#[tauri::command]
async fn set_lease_hours(
    state: Tauri<'_, AppState>,
    hours: u32,
    repo: Option<String>,
) -> Result<SyncReport> {
    change(&state, move |p| {
        let board = p.board(&repo.unwrap_or_default())?;
        save_setting(board, &format!("lease-hours {hours}"), |s| {
            s.set_lease_hours(f64::from(hours))
        })
    })
    .await
}

/// Whether the project's board commits and pushes each change (`commit`).
#[tauri::command]
async fn auto_commit() -> Result<bool> {
    blocking(|| {
        let project = Project::open()?;
        Ok(board_settings::load(project.board("")?).auto_commit())
    })
    .await
}

/// Turns auto-commit on or off for the project's board.
#[tauri::command]
async fn set_auto_commit(state: Tauri<'_, AppState>, on: bool) -> Result<bool> {
    change(&state, move |p| {
        let board = p.board("")?;
        let what = if on { "commit on" } else { "commit off" };
        save_setting(board, what, |s| {
            s.set_auto_commit(on);
            Ok(())
        })?;
        Ok(board_settings::load(board).auto_commit())
    })
    .await
}

/// The user's colour scheme, from KanRite's own settings on this machine.
#[tauri::command]
fn appearance() -> Appearance {
    let config = Config::load();
    Appearance::from_keys(
        config.get("ui.theme").unwrap_or_default(),
        config.get("ui.accent").unwrap_or_default(),
    )
}

/// Applies the board's saved zoom (ticket 024) and returns it, in percent.
#[tauri::command]
fn zoom(window: tauri::WebviewWindow) -> Result<u32> {
    let percent = rite_ui_model::zoom::parse(Config::load().get("ui.zoom").unwrap_or_default());
    window
        .set_zoom(f64::from(percent) / 100.0)
        .map_err(|e| format!("zooming the board: {e}"))?;
    Ok(percent)
}

/// Zooms the board to `percent` (snapped to a step) and remembers it on this
/// machine, so it opens at that size next time.
#[tauri::command]
fn set_zoom(window: tauri::WebviewWindow, percent: u32) -> Result<u32> {
    let percent = rite_ui_model::zoom::normalise(percent);
    window
        .set_zoom(f64::from(percent) / 100.0)
        .map_err(|e| format!("zooming the board: {e}"))?;
    let mut config = Config::load();
    config.set("ui.zoom", &percent.to_string());
    config.save().map_err(err)?;
    Ok(percent)
}

/// Saves the user's colour scheme, so it is used again next time.
#[tauri::command]
fn set_appearance(appearance: Appearance) -> Result<Appearance> {
    let mut config = Config::load();
    config.set("ui.theme", appearance.theme.key());
    config.set("ui.accent", appearance.accent.key());
    config.save().map_err(err)?;
    Ok(appearance)
}

fn template_file(board: &Board) -> PathBuf {
    board
        .templates_dir()
        .join(format!("{DEFAULT_TEMPLATE}{TEMPLATE_SUFFIX}"))
}

fn template_of(board: &Board) -> Result<TicketTemplate> {
    let path = template_file(board);
    Ok(TicketTemplate {
        source: Templates::load(board)
            .and_then(|t| t.source_text(DEFAULT_TEMPLATE))
            .map_err(err)?,
        customised: path.is_file(),
        path: path.display().to_string(),
    })
}

/// `source` rendered for a made-up ticket, or why it can't be.
fn preview(board: &Board, source: &str) -> Result<String> {
    Templates::load(board)
        .and_then(|t| t.with_source(DEFAULT_TEMPLATE, source))
        .and_then(|t| t.render(DEFAULT_TEMPLATE, example_context(board)))
        .map_err(err)
}

#[tauri::command]
async fn get_template() -> Result<TicketTemplate> {
    blocking(|| template_of(Project::open()?.board("")?)).await
}

#[tauri::command]
async fn preview_template(source: String) -> Result<String> {
    blocking(move || preview(Project::open()?.board("")?, &source)).await
}

/// Saves the board's own ticket template (what `rite templates eject` makes),
/// once it renders.
#[tauri::command]
async fn save_template(state: Tauri<'_, AppState>, source: String) -> Result<TicketTemplate> {
    change(&state, move |p| {
        let board = p.board("")?;
        preview(board, &source)?;
        let path = template_file(board);
        committed(board, "template", DEFAULT_TEMPLATE, "", || {
            std::fs::create_dir_all(board.templates_dir())?;
            std::fs::write(&path, &source)?;
            Ok(())
        })?;
        template_of(board)
    })
    .await
}

/// Goes back to the built-in template by removing the board's own.
#[tauri::command]
async fn reset_template(state: Tauri<'_, AppState>) -> Result<TicketTemplate> {
    change(&state, move |p| {
        let board = p.board("")?;
        let path = template_file(board);
        if path.is_file() {
            committed(board, "template", DEFAULT_TEMPLATE, "built-in", || {
                std::fs::remove_file(&path)?;
                Ok(())
            })?;
        }
        template_of(board)
    })
    .await
}

/// Where the board on screen lives.
#[tauri::command]
async fn tasks_dir() -> Result<Option<String>> {
    blocking(|| {
        Ok(Project::open()
            .ok()
            .map(|p| p.repos[0].1.root.display().to_string()))
    })
    .await
}

/// Shows the project around `path` instead (a tickets repository, its tasks
/// directory, or the folder holding a project's repositories), and
/// remembers it for a launch from outside any project.
#[tauri::command]
async fn set_tasks_dir(
    window: tauri::WebviewWindow,
    state: Tauri<'_, AppState>,
    path: String,
) -> Result<rite_ui_model::Board> {
    let writing = Arc::clone(&state.writing);
    let board = blocking(move || {
        let _guard = writing
            .lock()
            .map_err(|_| "the board is in a bad state — restart riteui".to_string())?;
        show_from(Path::new(path.trim()))
    })
    .await;
    titled(&window, board)
}

/// The landing page (ticket 020): your todo and doing work across every
/// project on this machine, and the projects. Read fresh each time.
#[tauri::command]
async fn home() -> Result<rite_ui_model::Home> {
    blocking(|| {
        use rite_lib::my_work;
        let here = std::env::current_dir().unwrap_or_default();
        let me = my_work::me(&here);
        let work = match &me {
            Some((email, name)) => {
                my_work::my_work(email, name.as_deref(), false, my_work::Sort::Priority)
            }
            None => Vec::new(),
        };
        // Nothing to do (ticket 160): work to pick from the backlogs. A
        // hidden project stays hidden (ticket 263), though its TRIAGE
        // tickets and work to pick are listed.
        let include_unassigned = my_work::include_unassigned();
        let to_pick = match &me {
            Some((email, name)) if work.is_empty() => {
                my_work::to_pick(email, name.as_deref(), include_unassigned)
            }
            _ => Vec::new(),
        };
        let convert = |w: my_work::WorkItem| rite_ui_model::WorkItem {
            project: w.project,
            project_shown: w.project_shown,
            project_root: w.project_root.display().to_string(),
            repo: w.repo,
            column: rite_lib::workflow::Workflow::standard()
                .find(&w.column)
                .map(boards::from_column)
                .unwrap_or(rite_ui_model::Column::Todo),
            id: w.id,
            file: format!("{}.md", w.stem),
            title: w.title,
            priority: w.priority,
            points: w.points,
            created: w.created,
            folder: w.folder,
            triage: w.triage,
        };
        let folded = my_work::folded_graphs();
        Ok(rite_ui_model::Home {
            work: work.into_iter().map(convert).collect(),
            to_pick: to_pick.into_iter().map(convert).collect(),
            include_unassigned,
            projects: my_work::projects()
                .into_iter()
                .map(|p| {
                    // Which documents buttons can work here (ticket 168).
                    use rite_lib::documents::{Kind, sources};
                    let docs = sources(&rite_lib::board::Board::in_repo(&p.root));
                    let own = docs.iter().find(|d| d.kind == Kind::Own);
                    let shared = docs.iter().find(|d| d.kind == Kind::Shared);
                    rite_ui_model::HomeProject {
                        shown: p.shown,
                        name: p.name,
                        root: p.root.display().to_string(),
                        dir: p.dir.display().to_string(),
                        hidden: p.hidden,
                        documents: own.is_some_and(|d| d.root.is_some()),
                        shared: shared.map(|d| d.name.clone()),
                        shared_cloned: shared.is_some_and(|d| d.root.is_some()),
                        archived: p.archived,
                        graphs_folded: folded.contains(&p.root),
                        broken: p.broken,
                    }
                })
                .collect(),
            me: me.map(|(email, _)| email).unwrap_or_default(),
            stats_days: rite_lib::stats::period(),
            // The project open in this process: what the launch directory
            // found, or the last one opened (ticket 168).
            current: Project::open()
                .ok()
                .map(|p| p.repos[0].1.repo_root().display().to_string())
                .unwrap_or_default(),
        })
    })
    .await
}

/// A project's open tickets, and how many are top and TRIAGE (ticket 349),
/// across its boards: what the landing page shows by each project. Reads each
/// ticket's own file, no git history.
#[tauri::command]
async fn project_counts(root: String) -> Result<rite_ui_model::TicketCounts> {
    blocking(move || {
        use rite_lib::my_work;
        let project = my_work::projects()
            .into_iter()
            .find(|p| p.root == Path::new(&root))
            .ok_or_else(|| format!("no project at {root}"))?;
        let boards = my_work::boards_of(&project, &rite_lib::registry::Registry::load());
        let refs: Vec<&Board> = boards.iter().map(|(_, b)| b).collect();
        let counts = my_work::counts(&refs).map_err(err)?;
        Ok(rite_ui_model::TicketCounts {
            open: counts.open,
            top: counts.top,
            triage: counts.triage,
        })
    })
    .await
}

/// A project's statistics (ticket 233) over `days`, across its boards:
/// raised, done and closed, by week, and each running sprint's burn.
#[tauri::command]
async fn project_stats(root: String, days: u32) -> Result<rite_ui_model::ProjectStats> {
    blocking(move || {
        use rite_lib::{my_work, stats};
        let project = my_work::projects()
            .into_iter()
            .find(|p| p.root == Path::new(&root))
            .ok_or_else(|| format!("no project at {root}"))?;
        let boards = my_work::boards_of(&project, &rite_lib::registry::Registry::load());
        let now = stats::now();
        let refs: Vec<&Board> = boards.iter().map(|(_, b)| b).collect();
        let counts = stats::counts(&refs, days, now).map_err(err)?;
        let mut sprints = Vec::new();
        for (name, board) in &boards {
            for b in stats::burns(board, name, now).map_err(err)? {
                sprints.push(rite_ui_model::SprintBurn {
                    repo: b.repo,
                    sprint: b.sprint,
                    start: b.start,
                    days: b.days,
                    total: b.total,
                    done_by_day: b.done_by_day,
                });
            }
        }
        Ok(rite_ui_model::ProjectStats {
            days,
            raised: counts.raised,
            done: counts.done,
            closed: counts.closed,
            weeks: counts
                .weeks
                .into_iter()
                .map(|w| rite_ui_model::StatWeek {
                    raised: w.raised,
                    done: w.done,
                    closed: w.closed,
                })
                .collect(),
            sprints,
        })
    })
    .await
}

/// Remembers the statistics period (7, 30 or 90 days) on this machine.
#[tauri::command]
async fn set_stats_days(days: u32) -> Result<()> {
    blocking(move || rite_lib::stats::set_period(days).map_err(err)).await
}

/// Archives a project, or brings it back (tickets 157, 263), its
/// repositories on GitHub too; archiving takes it off this machine. Works on any project on this machine, from
/// the landing page as well as from inside it, so it doesn't go through
/// [`change`], which refuses an archived project.
#[tauri::command]
async fn set_project_archived(
    state: Tauri<'_, AppState>,
    root: String,
    on: bool,
) -> Result<SyncReport> {
    let writing = Arc::clone(&state.writing);
    blocking(move || {
        let _guard = writing
            .lock()
            .map_err(|_| "the board is in a bad state — restart riteui".to_string())?;
        // A repository's root (the landing page) or its board (Admin).
        let board = match Board::in_repo(Path::new(&root)) {
            board if board.root.is_dir() => board,
            _ => Board::at(PathBuf::from(&root)),
        };
        rite_lib::sync::catch_up(&board, rite_lib::sync::Purpose::Write).map_err(err)?;
        // Always on GitHub, and archiving forgets it here (ticket 263).
        let events = rite_lib::archive::set_archived(
            &board,
            on,
            true,
            &mut rite_lib::archive::gh,
            &mut rite_lib::archive::forget,
        )
        .map_err(err)?;
        let mut info = Vec::new();
        let mut warnings = Vec::new();
        for event in events {
            match event {
                Event::Info(text) => info.push(text),
                Event::Warn(text) => warnings.push(text),
            }
        }
        Ok(report(
            &board,
            info.join("; "),
            (!warnings.is_empty()).then(|| warnings.join("; ")),
        ))
    })
    .await
}

/// Hides a project from My work, or shows it again (ticket 020): saved in
/// your own config on this machine, never committed.
#[tauri::command]
async fn set_project_hidden(root: String, hidden: bool) -> Result<()> {
    blocking(move || rite_lib::my_work::set_hidden(Path::new(&root), hidden).map_err(err)).await
}

/// Folds a project's sprint graphs on the landing page, or opens them
/// (ticket 305), remembered in your own config on this machine.
#[tauri::command]
async fn set_project_graphs_folded(root: String, folded: bool) -> Result<()> {
    blocking(move || rite_lib::my_work::set_graphs_folded(Path::new(&root), folded).map_err(err))
        .await
}

/// My work's "Include unassigned tickets" (ticket 160), remembered in your
/// own config on this machine.
#[tauri::command]
async fn set_include_unassigned(on: bool) -> Result<()> {
    blocking(move || rite_lib::my_work::set_include_unassigned(on).map_err(err)).await
}

/// Switches to another project in the same folder (ticket 093): records
/// it in that folder's `.current-project` — the same file `rite project use`
/// writes — and shows it. The window then runs from the folder itself, so
/// the file is what decides, not whichever tickets repository it was
/// launched inside.
#[tauri::command]
async fn use_project(
    window: tauri::WebviewWindow,
    state: Tauri<'_, AppState>,
    name: String,
) -> Result<rite_ui_model::Board> {
    let writing = Arc::clone(&state.writing);
    let board = blocking(move || {
        let _guard = writing
            .lock()
            .map_err(|_| "the board is in a bad state — restart riteui".to_string())?;
        let open = Project::open()?;
        let dir = open.repos[0]
            .1
            .repo_root()
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| "this board isn't inside a folder of projects".to_string())?;
        if !current_project::projects(&dir)
            .iter()
            .any(|p| p.name == name)
        {
            return Err(format!("no project '{name}' in {}", dir.display()));
        }
        let before = current_project::read(&dir);
        current_project::write(&dir, &name).map_err(err)?;
        show_from(&dir).inspect_err(|_| {
            let _ = match &before {
                Some(before) => current_project::write(&dir, before),
                None => std::fs::remove_file(dir.join(current_project::FILE)).map_err(Into::into),
            };
        })
    })
    .await;
    titled(&window, board)
}

/// Moves the process to `path`, opens the project found from there, and
/// remembers it for a launch from outside any project. Changes nothing if
/// no project opens.
fn show_from(path: &Path) -> Result<rite_ui_model::Board> {
    // Say which folder is the trouble, not that the current directory isn't
    // in a repository (ticket 341).
    if path.is_dir() && !rite_lib::git::is_repo_dir(path) {
        return Err(rite_lib::git::not_a_repo(path));
    }
    let previous = std::env::current_dir().ok();
    std::env::set_current_dir(path).map_err(|e| format!("{}: {e}", path.display()))?;
    match Project::open().and_then(|p| p.summary()) {
        Ok(board) => {
            let mut config = Config::load();
            if let Ok(here) = std::env::current_dir() {
                config.set(LAST_BOARD, &here.display().to_string());
                config.save().map_err(err)?;
            }
            Ok(board)
        }
        Err(e) => {
            if let Some(previous) = previous {
                let _ = std::env::set_current_dir(previous);
            }
            Err(e)
        }
    }
}

/// Starts the board. Launched from outside any project (a desktop launcher
/// starts in the home directory), it opens the board it showed last.
/// Renames a read-aloud voice set up before 5.0 (ticket 310): Piper's
/// speech-dispatcher module was `agile-md-piper`. Linux only, where Piper
/// is offered; once.
pub fn adopt_old_speech_setup() {
    if cfg!(target_os = "linux") && speech::piper::adopt_new_name() {
        eprintln!("renamed the Piper voice's speech-dispatcher module to kanrite-piper");
    }
}

pub fn run() {
    boards::init_local_offset();
    if Board::discover().is_err()
        && let Some(last) = settings::Config::load().get(LAST_BOARD).map(PathBuf::from)
    {
        let _ = std::env::set_current_dir(last);
    }
    tauri::Builder::default()
        .manage(AppState {
            // Recorded now: once replaced, `current_exe` can name a
            // deleted file (ticket 208).
            exe_watch: Arc::new(Mutex::new(
                std::env::current_exe()
                    .ok()
                    .map(|exe| exe_watch::Watch::new(exe, env!("CARGO_PKG_VERSION"))),
            )),
            ..AppState::default()
        })
        // No shell outlives the window (ticket 185).
        .on_window_event(|window, event| {
            use tauri::Manager;
            if let tauri::WindowEvent::Destroyed = event
                && let Some(state) = window.try_state::<AppState>()
                && let Ok(mut terminals) = state.terminals.lock()
            {
                terminals.clear();
            }
            if let tauri::WindowEvent::Destroyed = event
                && let Some(state) = window.try_state::<AppState>()
            {
                stop_reading(window.app_handle(), &state.speaking);
            }
            if let tauri::WindowEvent::Destroyed = event
                && let Some(state) = window.try_state::<AppState>()
                && let Ok(screen) = state.screen.lock()
                && let Some(repo) = screen.as_ref()
            {
                rite_lib::screen::clear(repo);
            }
        })
        .invoke_handler(tauri::generate_handler![
            load_board,
            home,
            set_project_hidden,
            set_project_graphs_folded,
            set_project_archived,
            project_stats,
            project_counts,
            set_stats_days,
            pin_start_page,
            my_pins,
            set_doc_pin,
            search_documents,
            list_ideas,
            read_idea,
            create_idea,
            vote_idea,
            note_idea,
            idea_to_ticket,
            idea_to_group,
            set_include_unassigned,
            board_fingerprint,
            sync_board,
            ticket_version,
            open_ticket,
            save_ticket,
            move_ticket,
            create_ticket,
            clone_ticket,
            link_ticket,
            retype_link,
            ticket_family,
            ticket_hierarchy,
            repo_defaults,
            create_repository,
            add_column,
            rename_column,
            move_column,
            remove_column,
            set_column_wip,
            set_flow,
            allow_move,
            set_landing,
            reset_columns,
            create_epic,
            list_roadmaps,
            create_roadmap,
            adopt_roadmap,
            save_roadmap,
            rename_roadmap,
            add_milestone,
            delete_milestone,
            delete_roadmap,
            set_milestone_dates,
            create_sprint,
            start_sprint,
            close_sprint,
            backfill_sprint,
            rename_epic,
            delete_epic,
            move_epic_tickets,
            update_status,
            snooze_update,
            check_for_update,
            whats_new,
            update_checked,
            apply_update,
            binary_changed,
            restart_app,
            archive_ticket,
            lock_ticket,
            unlock_ticket,
            set_wip_limits,
            set_lease_hours,
            add_tag,
            remove_tag,
            set_tags_only,
            set_shared_documents,
            set_display_name,
            import_issues,
            pull_issues,
            set_import_issues,
            set_auto_priority,
            set_priority_override,
            set_time_limits,
            set_due_to_todo_days,
            set_backlog_always_shown,
            list_documents,
            document_history,
            read_document_version,
            restore_document,
            archive_document,
            create_document_folder,
            delete_document_folder,
            move_document,
            create_document,
            open_document_for_edit,
            save_document,
            read_document,
            open_external,
            auto_commit,
            set_auto_commit,
            appearance,
            set_appearance,
            zoom,
            set_zoom,
            get_template,
            preview_template,
            save_template,
            reset_template,
            tasks_dir,
            set_tasks_dir,
            use_project,
            clone_repository,
            page_panic,
            speech_status,
            speech_download,
            speech_remove,
            set_speech_model,
            set_speech_enabled,
            speech_start,
            speech_stop,
            speech_cancel,
            read_aloud_status,
            read_aloud,
            read_aloud_stop,
            set_read_aloud_enabled,
            piper_status,
            piper_install,
            piper_remove,
            terminal_open,
            terminal_input,
            terminal_resize,
            terminal_scroll,
            terminal_close,
            ai_command,
            set_screen,
            set_ai_command,
            editor_command,
            set_editor_command,
            open_in_editor,
        ])
        .run(tauri::generate_context!())
        .expect("error while running riteui");
}

/// Speech to text's settings: `ui.speech` (off disables it) and
/// `ui.speech-model` (the chosen model).
fn speech_enabled() -> bool {
    Config::load().get("ui.speech").map(str::trim) != Some("off")
}

/// The model dictation uses: the chosen one if it's installed, else the
/// first installed, else none.
fn speech_model() -> Option<speech::model::Model> {
    use speech::model::Model;
    let chosen = Config::load()
        .get("ui.speech-model")
        .and_then(Model::from_id)
        .filter(|m| m.installed());
    chosen.or_else(|| Model::ALL.into_iter().find(|m| m.installed()))
}

/// Where speech to text stands (ticket 187).
#[tauri::command]
async fn speech_status() -> Result<rite_ui_model::terminal::SpeechStatus> {
    blocking(|| {
        use speech::model::Model;
        Ok(rite_ui_model::terminal::SpeechStatus {
            enabled: speech_enabled(),
            model: speech_model().map(|m| m.id().to_string()),
            models: Model::ALL
                .into_iter()
                .map(|m| rite_ui_model::terminal::SpeechModel {
                    id: m.id().to_string(),
                    size_mb: m.size_mb(),
                    installed: m.installed(),
                })
                .collect(),
            microphone: speech::audio::microphone(),
        })
    })
    .await
}

/// Downloads a model, saying how it's going through `speech-progress`
/// events. One download at a time. The first model installed becomes the
/// one used.
#[tauri::command]
async fn speech_download(
    app: tauri::AppHandle,
    state: Tauri<'_, AppState>,
    model: String,
) -> Result<()> {
    use std::sync::atomic::Ordering;
    use tauri::Emitter;
    let which = speech::model::Model::from_id(&model)
        .ok_or_else(|| format!("no speech model called {model}"))?;
    let downloading = Arc::clone(&state.downloading);
    if downloading.swap(true, Ordering::AcqRel) {
        return Err("a model is downloading already".to_string());
    }
    let result = blocking(move || {
        let mut last = std::time::Instant::now() - std::time::Duration::from_secs(1);
        let id = which.id().to_string();
        let progress_app = app.clone();
        let outcome = which.download(|done, total| {
            if last.elapsed() >= std::time::Duration::from_millis(150) {
                last = std::time::Instant::now();
                let _ = progress_app.emit(
                    "speech-progress",
                    rite_ui_model::terminal::SpeechProgress {
                        model: id.clone(),
                        done,
                        total,
                        ..Default::default()
                    },
                );
            }
        });
        let error = outcome.as_ref().err().map(|e| format!("{e:#}"));
        if error.is_none() && Config::load().get("ui.speech-model").is_none() {
            let mut config = Config::load();
            config.set("ui.speech-model", &id);
            config.save().map_err(err)?;
        }
        let _ = app.emit(
            "speech-progress",
            rite_ui_model::terminal::SpeechProgress {
                model: id,
                finished: true,
                error: error.clone(),
                ..Default::default()
            },
        );
        error.map_or(Ok(()), Err)
    })
    .await;
    downloading.store(false, Ordering::Release);
    result
}

/// Uninstalls a model: its files deleted, and unloaded if it was loaded.
#[tauri::command]
async fn speech_remove(state: Tauri<'_, AppState>, model: String) -> Result<()> {
    let engine = Arc::clone(&state.speech);
    blocking(move || {
        let which = speech::model::Model::from_id(&model)
            .ok_or_else(|| format!("no speech model called {model}"))?;
        let mut loaded = engine.lock().map_err(|_| "restart riteui".to_string())?;
        if loaded.as_ref().is_some_and(|e| e.model == which) {
            *loaded = None;
        }
        which.remove().map_err(err)?;
        let mut config = Config::load();
        if config.get("ui.speech-model") == Some(which.id()) {
            config.remove("ui.speech-model");
            config.save().map_err(err)?;
        }
        Ok(())
    })
    .await
}

/// Chooses the model dictation uses.
#[tauri::command]
async fn set_speech_model(state: Tauri<'_, AppState>, model: String) -> Result<()> {
    let engine = Arc::clone(&state.speech);
    blocking(move || {
        let which = speech::model::Model::from_id(&model)
            .ok_or_else(|| format!("no speech model called {model}"))?;
        let mut config = Config::load();
        config.set("ui.speech-model", which.id());
        config.save().map_err(err)?;
        let mut loaded = engine.lock().map_err(|_| "restart riteui".to_string())?;
        if loaded.as_ref().is_some_and(|e| e.model != which) {
            *loaded = None;
        }
        Ok(())
    })
    .await
}

/// The "Use speech to text" switch. Off hides dictation and frees the
/// model's memory; the files stay, so on again needs no download.
#[tauri::command]
async fn set_speech_enabled(state: Tauri<'_, AppState>, on: bool) -> Result<()> {
    let engine = Arc::clone(&state.speech);
    let recording = Arc::clone(&state.recording);
    blocking(move || {
        let mut config = Config::load();
        if on {
            config.remove("ui.speech");
        } else {
            config.set("ui.speech", "off");
            *engine.lock().map_err(|_| "restart riteui".to_string())? = None;
            *recording.lock().map_err(|_| "restart riteui".to_string())? = None;
        }
        config.save().map_err(err)?;
        Ok(())
    })
    .await
}

/// Starts recording from the microphone.
#[tauri::command]
async fn speech_start(app: tauri::AppHandle, state: Tauri<'_, AppState>) -> Result<()> {
    // Never read aloud into the microphone (ticket 286).
    stop_reading(&app, &state.speaking);
    let recording = Arc::clone(&state.recording);
    blocking(move || {
        if !speech_enabled() || speech_model().is_none() {
            return Err("speech to text isn't set up: choose a model in Preferences".to_string());
        }
        let mut slot = recording.lock().map_err(|_| "restart riteui".to_string())?;
        if slot.is_none() {
            *slot = Some(speech::audio::Recorder::start().map_err(err)?);
        }
        Ok(())
    })
    .await
}

/// Stops recording and returns what was said. The model is loaded on the
/// first dictation and kept until it's switched off or removed.
#[tauri::command]
async fn speech_stop(state: Tauri<'_, AppState>) -> Result<String> {
    let recording = Arc::clone(&state.recording);
    let engine = Arc::clone(&state.speech);
    blocking(move || {
        let recorder = recording
            .lock()
            .map_err(|_| "restart riteui".to_string())?
            .take()
            .ok_or_else(|| "nothing is recording".to_string())?;
        let pcm = recorder.stop().map_err(err)?;
        // Under a third of a second is a click, not speech.
        if pcm.len() < speech::dsp::WHISPER_RATE as usize / 3 {
            return Ok(String::new());
        }
        let model = speech_model().ok_or_else(|| "no speech model is installed".to_string())?;
        let mut loaded = engine.lock().map_err(|_| "restart riteui".to_string())?;
        if loaded.as_ref().is_none_or(|e| e.model != model) {
            *loaded = Some(speech::whisper::Engine::load(model).map_err(err)?);
        }
        let engine = loaded
            .as_mut()
            .ok_or_else(|| "the speech model didn't load".to_string())?;
        engine.transcribe(&pcm).map_err(err)
    })
    .await
}

/// Throws a recording away.
#[tauri::command]
fn speech_cancel(state: Tauri<'_, AppState>) -> Result<()> {
    let recorder = state
        .recording
        .lock()
        .map_err(|_| "restart riteui".to_string())?
        .take();
    drop(recorder);
    Ok(())
}

/// A reading aloud in progress (ticket 286).
struct Speaking {
    engine: speech::say::Engine,
    child: std::process::Child,
    n: u64,
}

/// Read aloud's switch: `ui.read-aloud` (off hides it).
fn read_aloud_enabled() -> bool {
    Config::load().get("ui.read-aloud").map(str::trim) != Some("off")
}

/// Stops any reading, saying so to the page.
fn stop_reading(app: &tauri::AppHandle, speaking: &Mutex<Option<Speaking>>) {
    use tauri::Emitter;
    let taken = speaking.lock().ok().and_then(|mut s| s.take());
    if let Some(mut reading) = taken {
        reading.engine.stop(&mut reading.child);
        let _ = app.emit("read-aloud-done", ());
    }
}

/// Where read aloud stands (ticket 286).
#[tauri::command]
fn read_aloud_status(
    state: Tauri<'_, AppState>,
) -> Result<rite_ui_model::terminal::ReadAloudStatus> {
    Ok(rite_ui_model::terminal::ReadAloudStatus {
        enabled: read_aloud_enabled(),
        engine: speech::say::Engine::find().map(|e| e.name().to_string()),
        speaking: state.speaking.lock().is_ok_and(|s| s.is_some()),
    })
}

/// Reads aloud `text` if given (the selection, or Preferences' test), else
/// terminal `id`'s output since Enter was last typed. Anything already being
/// read stops first; `read-aloud-done` says when it's over.
#[tauri::command]
fn read_aloud(
    app: tauri::AppHandle,
    state: Tauri<'_, AppState>,
    id: Option<u32>,
    text: Option<String>,
) -> Result<()> {
    use std::sync::atomic::Ordering;
    use tauri::Emitter;
    if !read_aloud_enabled() {
        return Err("read aloud is off: switch it on in Preferences".to_string());
    }
    let engine = speech::say::Engine::find()
        .ok_or_else(|| "no system speech found: install speech-dispatcher (spd-say)".to_string())?;
    let mut raw = text.filter(|t| !t.trim().is_empty()).unwrap_or_default();
    if raw.is_empty()
        && let Some(id) = id
    {
        with_terminal(&state, id, |s| raw = s.since_enter())?;
    }
    let words = speech::say::speakable(&raw);
    if words.is_empty() {
        return Err("nothing to read aloud".to_string());
    }
    stop_reading(&app, &state.speaking);
    let child = engine
        .speak(&words)
        .map_err(|e| format!("reading aloud: {e}"))?;
    let n = state.readings.fetch_add(1, Ordering::AcqRel) + 1;
    *state
        .speaking
        .lock()
        .map_err(|_| "restart riteui".to_string())? = Some(Speaking { engine, child, n });
    let speaking = Arc::clone(&state.speaking);
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let Ok(mut slot) = speaking.lock() else {
                return;
            };
            match slot.as_mut() {
                Some(reading) if reading.n == n => {
                    if !matches!(reading.child.try_wait(), Ok(None)) {
                        *slot = None;
                        let _ = app.emit("read-aloud-done", ());
                        return;
                    }
                }
                // Stopped, or another reading took over: it says so.
                _ => return,
            }
        }
    });
    Ok(())
}

/// Stops reading aloud.
#[tauri::command]
fn read_aloud_stop(app: tauri::AppHandle, state: Tauri<'_, AppState>) -> Result<()> {
    stop_reading(&app, &state.speaking);
    Ok(())
}

/// Read aloud's switch in Preferences.
#[tauri::command]
fn set_read_aloud_enabled(
    app: tauri::AppHandle,
    state: Tauri<'_, AppState>,
    on: bool,
) -> Result<()> {
    let mut config = Config::load();
    if on {
        config.remove("ui.read-aloud");
    } else {
        config.set("ui.read-aloud", "off");
        stop_reading(&app, &state.speaking);
    }
    config.save().map_err(err)
}

/// Where the Piper voice stands (ticket 287).
#[tauri::command]
async fn piper_status(state: Tauri<'_, AppState>) -> Result<rite_ui_model::terminal::PiperStatus> {
    use std::sync::atomic::Ordering;
    let busy = state.piper_busy.load(Ordering::Acquire);
    blocking(move || {
        use speech::piper;
        Ok(rite_ui_model::terminal::PiperStatus {
            supported: piper::supported(),
            installer: piper::Installer::find().map(|i| i.name().to_string()),
            voices: piper::VOICES
                .iter()
                .map(|v| rite_ui_model::terminal::PiperVoice {
                    id: v.id.to_string(),
                    label: v.label.to_string(),
                })
                .collect(),
            installed: piper::installed(),
            busy,
        })
    })
    .await
}

/// Sets up (or switches to) a Piper voice, saying how it's going through
/// `piper-progress` events; returns at once.
#[tauri::command]
fn piper_install(app: tauri::AppHandle, state: Tauri<'_, AppState>, voice: String) -> Result<()> {
    use std::sync::atomic::Ordering;
    use tauri::Emitter;
    let which = speech::piper::voice(&voice).ok_or_else(|| format!("no voice called {voice}"))?;
    if state.piper_busy.swap(true, Ordering::AcqRel) {
        return Err("a Piper set-up is already running".to_string());
    }
    let busy = Arc::clone(&state.piper_busy);
    std::thread::spawn(move || {
        let say = |stage: &str, error: Option<String>| {
            let _ = app.emit(
                "piper-progress",
                rite_ui_model::terminal::PiperProgress {
                    stage: stage.to_string(),
                    error,
                },
            );
        };
        let outcome = speech::piper::install(which, &mut |stage| {
            say(
                match stage {
                    speech::piper::Stage::Installing => "installing",
                    speech::piper::Stage::Voice => "voice",
                    speech::piper::Stage::Configuring => "configuring",
                },
                None,
            )
        });
        busy.store(false, Ordering::Release);
        say("done", outcome.err().map(|e| format!("{e:#}")));
    });
    Ok(())
}

/// Takes the Piper voice away again: speech-dispatcher goes back to what it
/// had.
#[tauri::command]
async fn piper_remove(app: tauri::AppHandle, state: Tauri<'_, AppState>) -> Result<()> {
    use std::sync::atomic::Ordering;
    if state.piper_busy.swap(true, Ordering::AcqRel) {
        return Err("a Piper set-up is running".to_string());
    }
    stop_reading(&app, &state.speaking);
    let busy = Arc::clone(&state.piper_busy);
    let outcome = blocking(|| speech::piper::remove().map_err(|e| format!("{e:#}"))).await;
    busy.store(false, Ordering::Release);
    outcome
}

/// The page panicked: its message, on riteui's error output (the page has
/// stopped by then, and its console isn't visible).
#[tauri::command]
fn page_panic(message: String) {
    eprintln!("riteui: the page panicked: {message}");
}

/// Frames and endings from the panel's terminals, as Tauri events.
struct TauriSink(tauri::AppHandle);

impl terminal::Sink for TauriSink {
    fn frame(&self, frame: rite_ui_model::terminal::TermFrame) {
        use tauri::Emitter;
        let _ = self.0.emit("terminal-frame", frame);
    }
    fn exited(&self, id: u32) {
        use tauri::Emitter;
        let _ = self.0.emit("terminal-exit", id);
    }
}

/// What a bottom-panel tab runs: the user's login shell, or with `ai` that
/// command through it, interactive so PATH is a terminal's. Windows has no
/// `$SHELL` or login shell (ticket 280): Windows PowerShell, which every
/// Windows 10 and 11 has, without its banner.
fn tab_program(ai: Option<String>, shell: Option<&str>, windows: bool) -> (String, Vec<String>) {
    if windows {
        let mut args = vec!["-NoLogo".to_string()];
        if let Some(command) = ai {
            args.extend(["-Command".to_string(), command]);
        }
        return ("powershell.exe".to_string(), args);
    }
    let shell = shell
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("/bin/bash")
        .to_string();
    match ai {
        None => (shell, vec!["-l".to_string()]),
        Some(command) => (shell, vec!["-lic".to_string(), command]),
    }
}

/// Starts a terminal for the bottom panel (ticket 185): the user's shell,
/// or the AI command from Preferences, in the project folder (the folder
/// holding its repositories), on a `cols` × `rows` screen. Returns its id.
#[tauri::command]
async fn terminal_open(
    app: tauri::AppHandle,
    state: Tauri<'_, AppState>,
    kind: rite_ui_model::terminal::TermKind,
    cols: u16,
    rows: u16,
) -> Result<u32> {
    let terminals = Arc::clone(&state.terminals);
    let next = Arc::clone(&state.next_terminal);
    blocking(move || {
        use rite_ui_model::terminal::TermKind;
        let cwd = Project::open()
            .ok()
            .map(|p| rite_lib::registry::project_dir_of(p.repos[0].1.repo_root()));
        let ai = match kind {
            TermKind::Shell => None,
            TermKind::Ai => Some(ai_command_now()),
        };
        let shell = std::env::var("SHELL").ok();
        let (program, args) = tab_program(ai, shell.as_deref(), cfg!(windows));
        let id = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let session = terminal::Session::open(
            id,
            program.clone(),
            args,
            cwd,
            cols,
            rows,
            Arc::new(TauriSink(app)),
        )
        .map_err(|e| format!("starting {program}: {e}"))?;
        terminals
            .lock()
            .map_err(|_| "restart riteui".to_string())?
            .insert(id, session);
        Ok(id)
    })
    .await
}

/// Runs `f` on terminal `id`, if it's still open.
fn with_terminal(state: &AppState, id: u32, f: impl FnOnce(&terminal::Session)) -> Result<()> {
    let terminals = state
        .terminals
        .lock()
        .map_err(|_| "restart riteui".to_string())?;
    if let Some(session) = terminals.get(&id) {
        f(session);
    }
    Ok(())
}

/// Keystrokes or pasted text for terminal `id`.
#[tauri::command]
fn terminal_input(state: Tauri<'_, AppState>, id: u32, text: String) -> Result<()> {
    with_terminal(&state, id, |s| s.input(&text))
}

/// Terminal `id`'s screen is now `cols` × `rows`.
#[tauri::command]
fn terminal_resize(state: Tauri<'_, AppState>, id: u32, cols: u16, rows: u16) -> Result<()> {
    with_terminal(&state, id, |s| s.resize(cols, rows))
}

/// Scrolls terminal `id` back (positive) or forward through its history.
#[tauri::command]
fn terminal_scroll(state: Tauri<'_, AppState>, id: u32, lines: i32) -> Result<()> {
    with_terminal(&state, id, |s| s.scroll(lines))
}

/// Ends terminal `id` and its program.
#[tauri::command]
fn terminal_close(state: Tauri<'_, AppState>, id: u32) -> Result<()> {
    let session = state
        .terminals
        .lock()
        .map_err(|_| "restart riteui".to_string())?
        .remove(&id);
    // Dropped outside the lock: ending the program can take a moment.
    drop(session);
    Ok(())
}

fn ai_command_now() -> String {
    Config::load()
        .get("ui.ai-command")
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .unwrap_or(DEFAULT_AI_COMMAND)
        .to_string()
}

/// Notes what the window shows (ticket 186), for an AI assistant's
/// `screen` tool (`rite mcp`): the project's note, keyed by its own board
/// repository. A project switch takes the old project's note away.
#[tauri::command]
fn set_screen(state: Tauri<'_, AppState>, mut view: rite_lib::screen::View) -> Result<()> {
    let found = rite_lib::model::find_repos().map_err(err)?;
    let (_, root, own) = &found.repos[0];
    let project = rite_lib::current_project::project_name(root);
    // The page knows an idea by its path; ideas live in the project's own
    // documents repository.
    if let Some(idea) = view.idea.as_mut()
        && idea.repo.is_empty()
    {
        idea.repo = rite_lib::ideas::source(own).name;
    }
    let mut last = state
        .screen
        .lock()
        .map_err(|_| "restart riteui".to_string())?;
    if let Some(before) = last.as_ref()
        && before != root
    {
        rite_lib::screen::clear(before);
    }
    *last = Some(root.clone());
    rite_lib::screen::write(root, &project, view).map_err(err)
}

/// The command the AI tab runs (ticket 185), `claude` unless Preferences
/// says otherwise.
#[tauri::command]
fn ai_command() -> String {
    ai_command_now()
}

/// Sets the AI tab's command in your own config; empty goes back to
/// `claude`.
#[tauri::command]
fn set_ai_command(command: String) -> Result<String> {
    let mut config = Config::load();
    let command = command.trim();
    if command.is_empty() || command == DEFAULT_AI_COMMAND {
        config.remove("ui.ai-command");
    } else {
        config.set("ui.ai-command", command);
    }
    config.save().map_err(err)?;
    Ok(ai_command_now())
}

/// The editor command from Preferences (ticket 188), empty when none is set.
fn editor_command_now() -> String {
    Config::load()
        .get("ui.editor-command")
        .map(str::trim)
        .unwrap_or_default()
        .to_string()
}

/// The command the board's Editor button runs; empty means no button.
#[tauri::command]
fn editor_command() -> String {
    editor_command_now()
}

/// Sets the editor command in your own config; empty removes it.
#[tauri::command]
fn set_editor_command(command: String) -> Result<String> {
    let mut config = Config::load();
    let command = command.trim();
    if command.is_empty() {
        config.remove("ui.editor-command");
    } else {
        config.set("ui.editor-command", command);
    }
    config.save().map_err(err)?;
    Ok(editor_command_now())
}

/// Opens the project's folder in the editor from Preferences (ticket 188).
/// The editor starts in a session of its own with no terminal, so it isn't
/// riteui's child: closing riteui leaves it running. It goes through the
/// user's shell, interactive like the AI tab's, so PATH is the one a
/// terminal has.
#[tauri::command]
async fn open_in_editor() -> Result<()> {
    blocking(move || {
        let command = editor_command_now();
        if command.is_empty() {
            return Err("set an editor in Preferences first".to_string());
        }
        let dir = Project::open()
            .map(|p| rite_lib::registry::project_dir_of(p.repos[0].1.repo_root()))?;
        spawn_detached(&command, &dir).map_err(|e| format!("starting the editor ({command}): {e}"))
    })
    .await
}

/// Runs `command dir` through the user's login shell, detached from this
/// process: its own session, no stdin, stdout or stderr.
#[cfg(unix)]
fn spawn_detached(command: &str, dir: &std::path::Path) -> std::io::Result<()> {
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "/bin/bash".to_string());
    let mut cmd = std::process::Command::new(shell);
    // `$1` is the folder, so no quoting of its path is needed.
    cmd.args(["-lic", &format!("{command} \"$1\""), "rite"])
        .arg(dir)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: `setsid` is async-signal-safe and touches no memory.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    let mut child = cmd.spawn()?;
    // Reaped on a thread so it doesn't linger as a zombie while riteui runs.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// The same on Windows (ticket 279), which has no login shell: `cmd /C`,
/// so an editor installed as a `.cmd` launcher (VS Code's `code`) runs, with
/// no console window and in a process group of its own. The line is given
/// to `cmd` as written, since it parses quotes its own way; a Windows path
/// can't hold a `"`.
#[cfg(windows)]
fn spawn_detached(command: &str, dir: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut child = std::process::Command::new("cmd")
        .raw_arg(format!("/C {command} \"{}\"", dir.display()))
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Clones a repository the project declares but this machine hasn't got
/// (ticket 108), beside the tickets repository, and registers it — what
/// `rite clone <name>` does. Returns a warning for each other clone of the
/// same remote (ticket 095).
#[tauri::command]
async fn clone_repository(state: Tauri<'_, AppState>, name: String) -> Result<Vec<String>> {
    let writing = Arc::clone(&state.writing);
    blocking(move || {
        let _guard = writing
            .lock()
            .map_err(|_| "the board is in a bad state — restart riteui".to_string())?;
        clone_into(&Project::open()?, &name)
    })
    .await
}

fn clone_into(project: &Project, name: &str) -> Result<Vec<String>> {
    let cloned =
        rite_lib::clone::clone_repo(&project.repos[0].1, name).map_err(|e| format!("{e:#}"))?;
    Ok(cloned
        .also_at
        .iter()
        .map(|other| {
            format!(
                "{name} is also cloned at {} — two copies of one repository drift apart; prefer one",
                other.display()
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_tabs_run_the_login_shell_or_powershell_on_windows() {
        let s = |v: &[&str]| v.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        assert_eq!(
            super::tab_program(None, Some("/bin/zsh"), false),
            ("/bin/zsh".to_string(), s(&["-l"]))
        );
        assert_eq!(
            super::tab_program(Some("claude".into()), None, false),
            ("/bin/bash".to_string(), s(&["-lic", "claude"]))
        );
        assert_eq!(
            super::tab_program(None, Some("/bin/zsh"), true),
            ("powershell.exe".to_string(), s(&["-NoLogo"]))
        );
        assert_eq!(
            super::tab_program(Some("claude".into()), None, true),
            (
                "powershell.exe".to_string(),
                s(&["-NoLogo", "-Command", "claude"])
            )
        );
    }

    #[test]
    fn the_pages_link_kinds_are_amd_libs() {
        let lib: Vec<(&str, &str)> = rite_lib::links::LinkType::ALL
            .iter()
            .map(|kind| (kind.slug(), kind.label()))
            .collect();
        assert_eq!(lib, rite_ui_model::LINK_KINDS);
        // ...and turn round the same way (ticket 121).
        for kind in rite_lib::links::LinkType::ALL {
            assert_eq!(
                rite_ui_model::link_inverse(kind.slug()),
                kind.inverse().slug()
            );
        }
    }

    /// The page says why a repository and branch type don't go together
    /// exactly as rite-lib does (ticket 066), and agrees on `none`.
    #[test]
    fn the_pages_pairing_rule_is_amd_libs() {
        assert_eq!(
            rite_ui_model::ticket::NO_BRANCH,
            rite_lib::branch::NO_BRANCH
        );
        for (repository, kind) in [
            ("acme-api", ""),
            ("", "feature"),
            ("acme-api", "feature"),
            ("acme-api", "none"),
            ("", "none"),
            ("", ""),
        ] {
            assert_eq!(
                rite_ui_model::ticket::pairing_mismatch(repository, kind),
                rite_lib::pairing::mismatch(repository, kind),
                "{repository:?} {kind:?}"
            );
        }
    }

    /// The tree rite-lib builds reads back as the page's own copy (ticket
    /// 214), field for field.
    #[test]
    fn the_pages_tree_is_amd_libs() {
        use rite_lib::requires::{Edge, TreeNode};
        let node = |id: &str, edge: Edge, children: Vec<TreeNode>| TreeNode {
            id: id.into(),
            title: format!("Ticket {id}"),
            file: format!("{id}-t.md"),
            column: "backlog".into(),
            epic: Some("s13".into()),
            place: "sprint s13".into(),
            done: false,
            edge,
            unmet: vec!["002".into()],
            stub: Some("s12".into()),
            seen: true,
            current: true,
            children,
        };
        let lib = node(
            "001",
            Edge::Root,
            vec![
                node("002", Edge::Related, Vec::new()),
                // The hierarchy's edge (ticket 261).
                node("003", Edge::Child, Vec::new()),
            ],
        );
        let json = serde_json::to_value(&lib).unwrap();
        let page: rite_ui_model::tree::TreeNode = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(page.children[0].edge, rite_ui_model::tree::Edge::Related);
        assert_eq!(page.children[1].edge, rite_ui_model::tree::Edge::Child);
        assert_eq!(serde_json::to_value(&page).unwrap(), json);
    }

    use super::*;
    use rite_ui_model::Column as UiColumn;

    /// Ticket 188: the editor command gets the project folder as its
    /// argument and runs, in a session of its own.
    #[cfg(unix)]
    #[test]
    fn the_editor_is_started_detached_with_the_folder() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("fake editor.sh");
        // Not `/proc`, which macOS has none of (ticket 345): the test is
        // only that it ran, in the folder it was given.
        std::fs::write(&script, "#!/bin/sh\necho \"$$\" > \"$1/ran\"\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let dir = tmp.path().join("project dir");
        std::fs::create_dir(&dir).unwrap();
        spawn_detached(&format!("'{}'", script.display()), &dir).unwrap();
        let ran = dir.join("ran");
        for _ in 0..100 {
            if ran.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(ran.exists(), "the editor never ran");
    }

    /// Ticket 117: the status line after a change matches its commit, with
    /// no empty `""` when there is no detail.
    #[test]
    fn the_status_line_says_what_the_commit_says() {
        let (_dir, project) = project();
        let board = &project.repos[0].1;
        let started = committed(board, "group-start", "Sprint 1", "", || Ok(())).unwrap();
        assert_eq!(started.message, "group-start: Sprint 1");
        let renamed = committed(board, "group-rename", "Old", "New", || Ok(())).unwrap();
        assert_eq!(renamed.message, "group-rename: Old \"New\"");
    }

    /// Ticket 122: the board carries the project's tags, listed and in
    /// use, with counts and the "only listed" switch, for Admin and the
    /// editor.
    #[test]
    fn the_board_carries_the_project_tags_with_counts() {
        let (_dir, project) = project();
        let board = &project.repos[0].1;
        let mut settings = board_settings::load(board);
        settings.add_tag("future").unwrap();
        settings.add_tag("old").unwrap();
        settings.set_only_listed_tags(true);
        settings.save(board).unwrap();
        let file = new_ticket(&project, "Tagged");
        let path = board.root.join("backlog").join(&file);
        let text = rite_lib::eol::read(&path).unwrap();
        std::fs::write(&path, text.replace("tags: []", "tags: [future,misc]")).unwrap();

        let summary = project.summary().unwrap();
        let uses: Vec<(String, usize, bool, bool)> = summary
            .tags
            .iter()
            .map(|t| (t.name.clone(), t.count, t.listed, t.reserved))
            .collect();
        // DRAFT and TRIAGE are built in (tickets 114, 158): always there,
        // never listed.
        assert_eq!(
            uses,
            [
                ("DRAFT".to_string(), 0, false, true),
                ("FUTURE".to_string(), 1, true, false),
                ("MISC".to_string(), 1, false, false),
                ("OLD".to_string(), 0, true, false),
                ("TRIAGE".to_string(), 0, false, true),
            ]
        );
        assert!(summary.tags_only);
    }

    /// Tickets 120, 195: the Repository filter and fields hold the project's
    /// declared repositories and nothing else, so one that `rite deregister`
    /// takes off the project leaves them, and a repository a ticket merely
    /// names (one renamed or retired since) never joins them.
    #[test]
    fn the_repository_lists_are_the_declared_repositories_only() {
        let (_dir, project) = project();
        let settings = project.repos[0].1.repo_root().join(".settings");
        let listed = |p: &Project| -> Vec<String> {
            p.summary()
                .unwrap()
                .ticket_repos
                .into_iter()
                .map(|r| r.name)
                .collect()
        };

        std::fs::write(&settings, "commit = off\nrepos = site\n").unwrap();
        assert_eq!(listed(&project), ["site"]);

        std::fs::write(&settings, "commit = off\n").unwrap();
        assert!(listed(&project).is_empty(), "undeclared: gone");

        let file = new_ticket(&project, "About the old site");
        let path = project.repos[0].1.root.join("backlog").join(&file);
        let text = rite_lib::eol::read(&path).unwrap();
        assert!(text.contains("repository: \"\""), "{text}");
        std::fs::write(
            &path,
            text.replace("repository: \"\"", "repository: \"site\""),
        )
        .unwrap();
        assert!(
            listed(&project).is_empty(),
            "a ticket naming it doesn't bring it back"
        );

        std::fs::write(&settings, "commit = off\nrepos = site\n").unwrap();
        assert_eq!(listed(&project), ["site"], "declared: listed once");
    }

    #[test]
    fn the_window_is_titled_after_its_project() {
        assert_eq!(window_title("fantastic-tools"), "fantastic-tools — riteui");
        assert_eq!(window_title(""), "riteui");
        assert_eq!(window_title("  "), "riteui");
    }

    /// The page's "clone…" goes through rite-lib's own checks (ticket 108).
    /// A successful clone writes the machine's registry, so it's covered in
    /// tests/test.sh, which points that at a temp directory.
    #[test]
    fn cloning_refuses_a_repository_the_project_doesnt_declare() {
        let (_dir, project) = project();
        let err = clone_into(&project, "nosuch").unwrap_err();
        assert!(err.contains("isn't declared for this project"), "{err}");
    }

    /// A project of one git repository with a board, auto-commit off.
    fn project() -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("demo-tickets");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t.co"]);
        git(&["config", "user.name", "t"]);
        let board = Board::in_repo(&repo);
        board.create().unwrap();
        std::fs::write(repo.join(".settings"), "commit = off\n").unwrap();
        let project = Project {
            repos: vec![("demo-tickets".into(), board)],
            unregistered: vec!["demo-site".into()],
            identity: Some("t@t.co".into()),
        };
        (dir, project)
    }

    fn new_ticket(project: &Project, title: &str) -> String {
        let fields = TicketFields {
            title: title.into(),
            assignees: Vec::new(),
            repository: None,
            points: String::new(),
            epic: None,
            tags: Vec::new(),
            branch_type: String::new(),
            parent: String::new(),
            related: Vec::new(),
            clone_of: String::new(),
            body: String::new(),
            priority: String::new(),
            requires: Vec::new(),
            required_by: Vec::new(),
        };
        let dest = transition::create_ticket(project.board("").unwrap(), &fields)
            .unwrap()
            .dest
            .unwrap();
        dest.file_name().unwrap().to_string_lossy().into_owned()
    }

    fn column(c: UiColumn) -> Place {
        Place::column(c)
    }

    /// A ticket with the fields the counts look at (ticket 349).
    fn new_ticket_with(
        project: &Project,
        title: &str,
        priority: &str,
        tags: &[&str],
        epic: Option<&str>,
    ) -> String {
        let fields = TicketFields {
            title: title.into(),
            assignees: Vec::new(),
            repository: None,
            points: String::new(),
            epic: epic.map(str::to_string),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            branch_type: String::new(),
            parent: String::new(),
            related: Vec::new(),
            clone_of: String::new(),
            body: String::new(),
            priority: priority.into(),
            requires: Vec::new(),
            required_by: Vec::new(),
        };
        let dest = transition::create_ticket(project.board("").unwrap(), &fields)
            .unwrap()
            .dest
            .unwrap();
        dest.file_name().unwrap().to_string_lossy().into_owned()
    }

    #[test]
    fn the_open_count_is_the_same_on_the_page_and_in_the_library() {
        // Ticket 349: open tickets are every one not in a done column,
        // including those in an epic, which the page keeps apart from its
        // columns; top and TRIAGE are counted among them.
        let (_dir, project) = project();
        let board = project.board("").unwrap();
        board
            .create_group(&Group {
                dir: board.dir(Column::Backlog).join("e1"),
                name: "e1".into(),
                kind: Kind::Epic,
                description: String::new(),
                days: 0,
                state: State::Pending,
            })
            .unwrap();
        new_ticket_with(&project, "plain", "low", &[], None);
        new_ticket_with(&project, "urgent", "top", &[], None);
        new_ticket_with(&project, "needs triage", "", &["TRIAGE"], None);
        new_ticket_with(&project, "in an epic", "top", &[], Some("e1"));
        let finished = new_ticket_with(&project, "finished", "top", &[], None);
        std::fs::rename(
            board.dir(Column::Backlog).join(&finished),
            board.dir(Column::Done).join(&finished),
        )
        .unwrap();

        let page = project.summary().unwrap().counts();
        let library = rite_lib::my_work::counts(&[board]).unwrap();
        // Done is left out; the epic's ticket is in; a TRIAGE ticket is top.
        assert_eq!((page.open, page.top, page.triage), (4, 3, 1));
        assert_eq!(
            (library.open, library.top, library.triage),
            (page.open, page.top, page.triage)
        );
    }

    #[test]
    fn the_board_shows_what_amd_lib_reads() {
        let (_dir, project) = project();
        let file = new_ticket(&project, "First");
        let board = project.summary().unwrap();
        let card = &board.columns[0].tickets[0];
        assert_eq!(
            (card.file.as_str(), card.title.as_str()),
            (file.as_str(), "First")
        );
        assert_eq!(card.priority, "low");
        assert_eq!(board.repos[0].name, "demo-tickets");
        assert_eq!(board.repos[0].wip.doing, 2);
        assert_eq!(board.repos[0].lease_hours, 8);
        assert!(!board.repos[1].available, "a declared repo not cloned here");
        // No declared project: the repository's name.
        assert_eq!(board.project, "demo-tickets");
    }

    #[test]
    fn the_header_shows_the_projects_own_name() {
        let (_dir, project) = project();
        let settings = rite_lib::board_settings::path(project.board("").unwrap());
        std::fs::write(&settings, "commit = off\nproject = grimartz\n").unwrap();
        assert_eq!(project.summary().unwrap().project, "grimartz");
    }

    #[test]
    fn the_board_carries_the_project_users_for_the_assignee_picker() {
        let (_dir, project) = project();
        let settings = rite_lib::board_settings::path(project.board("").unwrap());
        std::fs::write(
            &settings,
            "commit = off\nusers = oldippy <o@t.co>, Ann <a@t.co>\n",
        )
        .unwrap();
        let board = project.summary().unwrap();
        assert_eq!(board.users, ["Ann", "oldippy"]);
    }

    #[test]
    fn a_lapsed_lease_is_shown_as_lapsed_not_as_a_lock() {
        let (_dir, project) = project();
        let file = new_ticket(&project, "Lapsed");
        let path = project.board("").unwrap().root.join("backlog").join(&file);
        let text = rite_lib::eol::read(&path).unwrap();
        let text = text
            .replace("locked-by: \"\"", "locked-by: \"t@t.co\"")
            .replace(
                "locked-until: \"\"",
                "locked-until: \"2000-01-01T00:00:00Z\"",
            );
        std::fs::write(&path, text).unwrap();

        let ticket = project.summary().unwrap().columns[0].tickets[0].clone();
        assert!(ticket.lock.is_none(), "a lapsed lease holds nothing");
        let lapsed = ticket.lapsed.unwrap();
        assert!(lapsed.mine && lapsed.by == "t@t.co", "{lapsed:?}");
    }

    #[test]
    fn drags_move_tickets_the_way_amd_does() {
        let (_dir, project) = project();
        let a = new_ticket(&project, "A");
        let backlog = column(UiColumn::Backlog);
        let todo = column(UiColumn::Todo);
        let board = project.board("").unwrap();
        new_group(board, "ui", Kind::Epic, DEFAULT_DAYS).unwrap();

        // A column can't be skipped.
        let err = move_in(&project, &a, &backlog, &column(UiColumn::Done), false).unwrap_err();
        assert!(err.contains("one column at a time"), "{err}");

        // Into todo takes no lease (ticket 156): todo only means it's next.
        move_in(&project, &a, &backlog, &todo, false).unwrap();
        let summary = project.summary().unwrap();
        assert!(summary.columns[1].tickets[0].lock.is_none());

        // Into doing takes it for the mover; back to todo gives it back.
        let doing = column(UiColumn::Doing);
        move_in(&project, &a, &todo, &doing, false).unwrap();
        let lock = project.summary().unwrap().columns[2].tickets[0]
            .lock
            .clone()
            .unwrap();
        assert!(lock.mine && lock.by == "t@t.co", "{lock:?}");
        move_in(&project, &a, &doing, &todo, false).unwrap();
        assert!(
            project.summary().unwrap().columns[1].tickets[0]
                .lock
                .is_none()
        );

        // From todo onto an epic: back to the backlog, filed in it.
        move_in(&project, &a, &todo, &Place::epic("ui"), false).unwrap();
        let summary = project.summary().unwrap();
        assert_eq!(summary.epics[0].tickets[0].file, a);

        // Out of the epic straight to todo, keeping its label; and back.
        move_in(&project, &a, &Place::epic("ui"), &todo, false).unwrap();
        let card = project.summary().unwrap().columns[1].tickets[0].clone();
        assert_eq!(card.epic_label, "ui");
        move_in(&project, &a, &todo, &backlog, false).unwrap();
        assert_eq!(project.summary().unwrap().epics[0].tickets.len(), 1);

        // Out of the epic into the loose backlog.
        move_in(&project, &a, &Place::epic("ui"), &backlog, false).unwrap();
        let summary = project.summary().unwrap();
        assert!(summary.epics[0].tickets.is_empty());
        assert_eq!(summary.columns[0].tickets[0].epic_label, "");
    }

    #[test]
    fn doing_takes_two_unless_forced() {
        let (_dir, project) = project();
        let files: Vec<String> = ["A", "B", "C"]
            .iter()
            .map(|t| new_ticket(&project, t))
            .collect();
        let (backlog, todo, doing) = (
            column(UiColumn::Backlog),
            column(UiColumn::Todo),
            column(UiColumn::Doing),
        );
        for f in &files {
            move_in(&project, f, &backlog, &todo, false).unwrap();
        }
        move_in(&project, &files[0], &todo, &doing, false).unwrap();
        move_in(&project, &files[1], &todo, &doing, false).unwrap();
        let err = move_in(&project, &files[2], &todo, &doing, false).unwrap_err();
        assert!(err.contains("2 of 2"), "{err}");
        move_in(&project, &files[2], &todo, &doing, true).unwrap();
        assert_eq!(project.summary().unwrap().columns[2].tickets.len(), 3);
    }

    #[test]
    fn the_editor_saves_through_amd_lib_and_keeps_locks() {
        let (_dir, project) = project();
        let file = new_ticket(&project, "Edit me");
        let board = project.board("").unwrap();
        let place = column(UiColumn::Backlog);
        let task = task_at(board, &place, &file).unwrap();
        let opened = task.hash().unwrap();
        let text = rite_lib::eol::read(&task.path).unwrap();

        // Locked after the editor opened: the save keeps the lock, but is
        // refused as a conflict because the file changed underneath it.
        task.take_lease(opened, 8.0).unwrap();
        let edited = text.replace("## Notes", "## Notes\n\nMore");
        let err = transition::save_text(board, &task, opened, &edited).unwrap_err();
        assert!(format!("{err:#}").contains("changed"), "{err:#}");

        // Reopened, the save lands and the lock stays.
        let task = task_at(board, &place, &file).unwrap();
        transition::save_text(board, &task, task.hash().unwrap(), &edited).unwrap();
        let saved = rite_lib::eol::read(&task.path).unwrap();
        assert!(
            saved.contains("More") && saved.contains("locked-by: \"t@t.co\""),
            "{saved}"
        );

        // An invalid priority or size is refused.
        let task = task_at(board, &place, &file).unwrap();
        let bad = saved.replace("priority: \"low\"", "priority: \"urgent\"");
        assert!(transition::save_text(board, &task, task.hash().unwrap(), &bad).is_err());
    }

    #[test]
    fn templates_preview_and_save_through_amd_lib() {
        let (_dir, project) = project();
        let board = project.board("").unwrap();
        let built_in = template_of(board).unwrap();
        assert!(!built_in.customised);
        let rendered = preview(board, &built_in.source).unwrap();
        assert!(
            rendered.contains("title: \"An example ticket\""),
            "{rendered}"
        );
        assert!(preview(board, "{{ nope").is_err());
    }

    #[test]
    fn the_editor_writes_and_reads_several_people_as_amd_does() {
        let ticket = "---\nid: \"001\"\ntitle: \"T\"\nassignee: \"\"\n---\n\nbody\n";
        let people = vec!["Alex \"Al\" Morgan".to_string(), "Jo, Jr".to_string()];
        let mut fields =
            rite_ui_model::ticket::Fields::from(&rite_ui_model::parse_frontmatter(ticket));
        fields.assignees = people.clone();
        assert_eq!(
            rite_ui_model::ticket::apply_fields(ticket, &fields),
            rite_lib::task::with_assignees(ticket, &people).unwrap()
        );
        for raw in [r#"["Alex \"Al\" Morgan","Jo, Jr"]"#, "[a, 'b, c']", "[]"] {
            assert_eq!(
                rite_ui_model::parse_names(raw),
                rite_lib::task::parse_names(raw),
                "{raw}"
            );
        }
    }

    #[test]
    fn the_page_compares_project_names_as_rite_does() {
        for (a, b) in [
            ("KanRite", "kanrite"),
            ("kanrite", "kanrite"),
            ("KanRite-tickets", "kanrite-Tickets"),
            ("kanrite", "kanrite2"),
            ("kan-rite", "kanrite"),
            ("\u{c4}rger", "\u{e4}rger"),
        ] {
            assert_eq!(
                rite_ui_model::same_name(a, b),
                rite_lib::names::same(a, b),
                "{a} / {b}"
            );
        }
    }

    #[test]
    fn the_editor_derives_branch_names_as_amd_does() {
        for title in [
            "Crash on save",
            "Ünïcödé — títlé!",
            "a very long title that runs well past the fifty character limit rite sets",
        ] {
            assert_eq!(
                rite_ui_model::slugify(title),
                rite_lib::task::slugify(title),
                "{title}"
            );
        }
    }
}
