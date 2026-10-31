//! The project's boards as rite-lib reads them, turned into what the window
//! shows ([`rite_ui_model`]'s types), and back again: a [`Place`] and a file
//! name from the window become the [`Task`] rite-lib acts on.
//!
//! Nothing here writes. Every change is an rite-lib call in `lib.rs`.

use std::path::Path;
use std::sync::{Mutex, OnceLock};

use rite_lib::board::{Board, Column, Kind};
use rite_lib::board_settings;
use rite_lib::current_project;
use rite_lib::group::{GROUP_FILE, State};
use rite_lib::model::find_repos;
use rite_lib::task::Task;
use rite_ui_model::epic::{GroupKind, Place};
use rite_ui_model::wip::WipLimits;
use rite_ui_model::{self as ui, BoardColumn, Epic, Lock, RepoInfo, TicketSummary};
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

pub type Result<T> = std::result::Result<T, String>;

/// An rite-lib error as the window shows it: the whole chain, on one line.
pub fn err(error: anyhow::Error) -> String {
    format!("{error:#}")
}

/// This machine's offset from UTC, read once at start-up while the process
/// is still single-threaded — the only time `time` will tell us on Linux.
static LOCAL_OFFSET: OnceLock<UtcOffset> = OnceLock::new();

pub fn init_local_offset() {
    LOCAL_OFFSET.get_or_init(|| UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC));
}

/// The project the board shows: its own repository first, then every other
/// repository it declares that is cloned here (`rite register`).
pub struct Project {
    pub repos: Vec<(String, Board)>,
    /// Declared repositories with no board on this machine.
    pub unregistered: Vec<String>,
    /// The git identity locks are compared against.
    pub identity: Option<String>,
}

impl Project {
    /// The project around the current directory, as `rite` finds it.
    pub fn open() -> Result<Project> {
        // Boards only: their tickets are read by whatever needs them
        // (ticket 184).
        let found = find_repos().map_err(err)?;
        Ok(Project {
            repos: found
                .repos
                .into_iter()
                .map(|(name, _, board)| (name, board))
                .collect(),
            unregistered: found.unregistered,
            identity: found.identity,
        })
    }

    /// The projects in the folder this one's tickets repository sits in,
    /// marking the one on screen (ticket 093).
    pub fn projects(&self) -> Vec<ui::ProjectChoice> {
        let shown = self.repos[0].1.repo_root();
        let Some(dir) = shown.parent() else {
            return Vec::new();
        };
        current_project::projects(dir)
            .into_iter()
            .map(|project| ui::ProjectChoice {
                current: project.tickets == shown,
                shown: current_project::shown_name(&project.tickets),
                name: project.name,
            })
            .collect()
    }

    /// The board of the repository called `repo`; empty means the project's
    /// own.
    pub fn board(&self, repo: &str) -> Result<&Board> {
        if repo.is_empty() {
            return Ok(&self.repos[0].1);
        }
        self.repos
            .iter()
            .find(|(name, _)| name == repo)
            .map(|(_, board)| board)
            .ok_or_else(|| format!("The {repo} repository has no board on this machine."))
    }

    /// Everything the window draws: every repository's tickets and epics,
    /// merged, each labelled with its repository.
    ///
    /// The columns are the project's own (ticket 224): the tickets board's
    /// workflow. A ticket on another of its boards in a folder that
    /// workflow doesn't have shows in the first column of its kind.
    pub fn summary(&self) -> Result<ui::Board> {
        let workflow = ui_workflow(&self.repos[0].1);
        let mut columns: Vec<BoardColumn> = workflow
            .columns()
            .map(|column| BoardColumn {
                column,
                tickets: Vec::new(),
            })
            .collect();
        let slot = |column: Column| {
            let column = from_column(column);
            workflow
                .position(column)
                .or_else(|| workflow.position(workflow.first(column.kind())))
                .unwrap_or(0)
        };
        let mut epics = Vec::new();
        let mut repos = Vec::new();
        for (name, board) in &self.repos {
            let settings = board_settings::load(board);
            repos.push(RepoInfo {
                name: name.clone(),
                available: true,
                wip: WipLimits {
                    doing: settings.wip_limit(Column::Doing).unwrap_or(0),
                },
                lease_hours: settings.lease_hours().round().max(1.0) as u32,
                workflow: ui_workflow(board),
                tickets_only: board.is_tickets_board(),
            });
            for column in board.workflow().columns() {
                for task in board.tasks_in(column).map_err(err)? {
                    let summary = self.summary_of(&task, name);
                    if task.epic.is_none() {
                        columns[slot(column)].tickets.push(summary);
                    }
                }
            }
            for group in board.groups().map_err(err)? {
                let tickets = board
                    .tasks_in(Column::Backlog)
                    .map_err(err)?
                    .into_iter()
                    .filter(|t| t.epic.as_deref() == Some(group.name.as_str()))
                    .map(|t| self.summary_of(&t, name))
                    .collect();
                let unfinished = board
                    .tasks_in_group(&group.name)
                    .map_err(err)?
                    .iter()
                    .filter(|t| !t.column.is(rite_lib::board::Kind::Done))
                    .count();
                epics.push(Epic {
                    unfinished,
                    closed: group.is_sprint() && group.state == State::Closed,
                    repo: name.clone(),
                    kind: if group.is_sprint() {
                        GroupKind::Sprint
                    } else {
                        GroupKind::Epic
                    },
                    days: if group.is_sprint() { group.days } else { 0 },
                    started: group.is_sprint() && group.state == State::Started,
                    name: group.name,
                    tickets,
                });
            }
        }
        repos.extend(self.unregistered.iter().map(|name| RepoInfo {
            name: name.clone(),
            available: false,
            wip: WipLimits::default(),
            lease_hours: ui::DEFAULT_LEASE_HOURS,
            workflow: ui::Workflow::standard(),
            tickets_only: false,
        }));
        for column in &mut columns {
            column.tickets.sort_by(ui::ticket::default_order);
        }
        for epic in &mut epics {
            epic.tickets.sort_by(ui::ticket::default_order);
        }
        let (own_name, own_board) = &self.repos[0];
        let project = board_settings::load(own_board)
            .project()
            .map(str::to_string)
            .filter(|p| !p.trim().is_empty())
            .unwrap_or_else(|| own_name.clone());
        let ticket_repos = self.ticket_repos();
        let settings = board_settings::load(&self.repos[0].1);
        let shown = settings.display_name().unwrap_or_else(|| project.clone());
        let tags = self.tag_uses(&settings)?;
        Ok(ui::Board {
            workflow,
            project,
            shown,
            tasks_dir: self.repos[0].1.root.display().to_string(),
            repos,
            columns,
            epics,
            projects: self.projects(),
            ticket_repos,
            users: board_settings::load(&self.repos[0].1).user_names(),
            me: rite_lib::git::config(self.repos[0].1.repo_root(), "user.name"),
            tags,
            tags_only: settings.only_listed_tags(),
            admin: admin_access(&self.repos[0].1),
            shared_documents: settings.shared_documents().unwrap_or_default(),
            import_issues: settings.import_issues(),
            auto_priority: settings.auto_priority(),
            time_limits: {
                let t = settings.time_limits();
                (t.todo, t.doing)
            },
            due_to_todo_days: settings.due_to_todo_days(),
            backlog_always_shown: settings.backlog_always_shown(),
            needs_version: rite_lib::compat::needed(own_board).map(|v| v.to_string()),
            priority_override: settings.priority_override(),
            archived: settings.archived(),
        })
    }

    /// Every board of the project, the tickets repository's first.
    pub fn boards(&self) -> Vec<&Board> {
        self.repos.iter().map(|(_, board)| board).collect()
    }

    /// The project's listed tags and the tags in use on its boards, with
    /// counts (ticket 122).
    fn tag_uses(&self, settings: &board_settings::BoardSettings) -> Result<Vec<ui::TagUse>> {
        let counts = rite_lib::model::tag_counts(&self.boards()).map_err(err)?;
        let listed = settings.tags();
        let mut names: std::collections::BTreeSet<String> = listed.iter().cloned().collect();
        names.extend(counts.keys().cloned());
        names.extend(rite_lib::task::RESERVED_TAGS.iter().map(|t| t.to_string()));
        Ok(names
            .into_iter()
            .map(|name| ui::TagUse {
                count: counts.get(&name).copied().unwrap_or(0),
                listed: listed.contains(&name),
                reserved: rite_lib::task::is_reserved_tag(&name),
                name,
            })
            .collect())
    }

    /// What the repository filter and fields offer (tickets 101/107, 195):
    /// the project's declared repositories and nothing else, sorted, each
    /// marked available when it's cloned on this machine. A repository a
    /// ticket merely names (one since renamed or retired) is never added.
    fn ticket_repos(&self) -> Vec<ui::RepoChoice> {
        let mut names = board_settings::load(&self.repos[0].1).repos();
        names.sort();
        names.dedup();
        let here = &self.repos[0].1.root;
        names
            .into_iter()
            .map(|name| ui::RepoChoice {
                available: rite_lib::registry::clone_of(here, &name).is_some(),
                name,
            })
            .collect()
    }

    /// A ticket as a card on the board.
    pub fn summary_of(&self, task: &Task, repo: &str) -> TicketSummary {
        TicketSummary {
            repo: repo.to_string(),
            // What it's about; a ticket on a repository's own board (not the
            // project's tickets board) is about that repository by default.
            repository: task.repository().unwrap_or_else(|| {
                if repo == self.repos[0].0 {
                    String::new()
                } else {
                    repo.to_string()
                }
            }),
            file: task.file_name(),
            column: from_column(task.column),
            id: task.id_display(),
            title: task.title(),
            assignee: task.assignee().unwrap_or_default(),
            assignees: task.assignees(),
            tags: task.tags(),
            priority: task.priority(),
            created: task.meta("created").unwrap_or_default(),
            epic: task.epic.clone(),
            text: task.body().unwrap_or_default().trim().to_string(),
            epic_label: task.epic_meta().unwrap_or_default(),
            points: task.points().unwrap_or_default(),
            lock: self.lock_of(task),
            lapsed: self.lapsed_of(task),
            parent: task.parent().unwrap_or_default(),
            related: task.related(),
            clone_of: task.clone_of().unwrap_or_default(),
            clones: task.clones(),
            links: task.links().iter().map(|link| link.entry()).collect(),
            priority_from: task.meta("priority-from").unwrap_or_default(),
            due: task.due().unwrap_or_default(),
            recur: task.recur().unwrap_or_default(),
        }
    }

    /// The lock on a ticket, while one is in force.
    pub fn lock_of(&self, task: &Task) -> Option<Lock> {
        task.lease_active().then(|| self.lease_of(task)).flatten()
    }

    /// A lease recorded on the ticket but past its time (ticket 110): no
    /// lock at all for every check, but worth saying so.
    pub fn lapsed_of(&self, task: &Task) -> Option<Lock> {
        (!task.lease_active())
            .then(|| self.lease_of(task))
            .flatten()
    }

    /// Whatever lease the ticket records, in force or not.
    fn lease_of(&self, task: &Task) -> Option<Lock> {
        let by = task.locked_by()?;
        let until = task.locked_until()?;
        Some(Lock {
            mine: self.identity.as_deref() == Some(by.as_str()),
            until: until.format(&Rfc3339).unwrap_or_default(),
            until_local: local_time(until),
            by,
        })
    }
}

/// When a lock lapses, as the board shows it: "Sun 4 Oct 18:30".
fn local_time(at: OffsetDateTime) -> String {
    let offset = LOCAL_OFFSET.get().copied().unwrap_or(UtcOffset::UTC);
    let format = time::macros::format_description!(
        "[weekday repr:short] [day padding:none] [month repr:short] [hour]:[minute]"
    );
    at.to_offset(offset).format(format).unwrap_or_default()
}

/// The board's column for one of the page's: the same folder and kind.
pub fn to_column(column: ui::Column) -> Column {
    let kind = match column.kind() {
        ui::Kind::Backlog => Kind::Backlog,
        ui::Kind::Todo => Kind::Todo,
        ui::Kind::Doing => Kind::Doing,
        ui::Kind::Done => Kind::Done,
    };
    Column::new(column.dir_name(), kind)
}

/// The board's workflow as the page draws it (ticket 224): columns in
/// order with their names, and a doing-kind column's own WIP limit (the
/// standard doing's is the repository's `wip`).
pub fn ui_workflow(board: &Board) -> ui::Workflow {
    let settings = board_settings::load(board);
    let workflow = board.workflow();
    ui::Workflow {
        any: workflow.flow() == rite_lib::workflow::Flow::Any,
        moves: workflow
            .moves()
            .iter()
            .map(|(f, t)| (from_column(*f), from_column(*t)))
            .collect(),
        landing: from_column(workflow.landing()),
        columns: board
            .workflow()
            .defs()
            .iter()
            .map(|def| ui::ColumnInfo {
                column: from_column(def.column),
                label: def.label.clone(),
                wip: (def.column.is(Kind::Doing) && def.column != Column::Doing)
                    .then(|| settings.wip_limit(def.column))
                    .flatten(),
            })
            .collect(),
    }
}

/// The page's column for one of the board's: the same folder and kind.
pub fn from_column(column: Column) -> ui::Column {
    let kind = match column.kind() {
        Kind::Backlog => ui::Kind::Backlog,
        Kind::Todo => ui::Kind::Todo,
        Kind::Doing => ui::Kind::Doing,
        Kind::Done => ui::Kind::Done,
    };
    ui::Column::new(column.as_str(), kind)
}

/// The ticket at `place` called `file`. Only a plain `*.md` name directly
/// in the place's directory is accepted, and the epic must exist, so the
/// window can never name an arbitrary file.
pub fn task_at(board: &Board, place: &Place, file: &str) -> Result<Task> {
    let plain = file.ends_with(".md")
        && file != GROUP_FILE
        && !file.starts_with('.')
        && Path::new(file).file_name().and_then(|n| n.to_str()) == Some(file);
    if !plain {
        return Err(format!("not a ticket file name: {file:?}"));
    }
    let column = to_column(place.column);
    let mut dir = board.dir(column);
    if let Some(epic) = &place.epic {
        if column != Column::Backlog || board.group(epic).is_err() {
            return Err(format!("there is no epic called {epic:?}"));
        }
        dir = dir.join(epic);
    }
    let path = dir.join(file);
    let mut task = Task::from_path(&path, column)
        .filter(|_| path.is_file())
        .ok_or_else(|| format!("no ticket {file} there any more — reload the board"))?;
    task.epic = place.epic.clone();
    Ok(task)
}

/// Who gets Admin on `board`'s project (ticket 149). GitHub's answer is
/// asked once per repository per session, through `gh`, and kept: a call
/// takes a moment, and the board reloads every time a file changes. The
/// project's `admins` list is read fresh each time, so editing it shows at
/// once.
pub fn admin_access(board: &Board) -> ui::AdminAccess {
    use rite_lib::access::{self, GitHub};
    static ASKED: Mutex<Vec<(std::path::PathBuf, GitHub)>> = Mutex::new(Vec::new());
    let decided = access::admin_access_with(board, |repo| {
        let mut asked = ASKED
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((_, answer)) = asked.iter().find(|(r, _)| r == repo) {
            return *answer;
        }
        let answer = access::github_access(repo);
        asked.push((repo.to_path_buf(), answer));
        answer
    });
    ui::AdminAccess {
        allowed: decided.allowed,
        note: decided.note,
    }
}
