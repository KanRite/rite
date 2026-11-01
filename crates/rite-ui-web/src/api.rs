//! Calls into the Tauri backend.
//!
//! `withGlobalTauri` exposes `window.__TAURI__.core.invoke`; this binds it
//! with `wasm-bindgen` and wraps each command in a typed function, so the
//! rest of the UI never touches `JsValue`.

use rite_ui_model::appearance::Appearance;
use rite_ui_model::epic::Place;
use rite_ui_model::wip::WipLimits;
use rite_ui_model::{Board, Created, SyncReport, TicketDocument, TicketTemplate};
use serde::Serialize;
use serde::de::DeserializeOwned;
use wasm_bindgen::prelude::*;

/// The editor command (empty when Preferences has none).
pub async fn editor_command() -> Result<String, String> {
    call("editor_command", &NoArgs {}).await
}

pub async fn set_editor_command(command: &str) -> Result<String, String> {
    call("set_editor_command", &AiCommandArgs { command }).await
}

/// Opens the project's folder in the editor, detached from riteui.
pub async fn open_in_editor() -> Result<(), String> {
    call("open_in_editor", &NoArgs {}).await
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"], catch)]
    async fn invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;
}

async fn call<A: Serialize, R: DeserializeOwned>(cmd: &str, args: &A) -> Result<R, String> {
    let args = serde_wasm_bindgen::to_value(args).map_err(|e| e.to_string())?;
    let out = invoke(cmd, args)
        .await
        .map_err(|e| e.as_string().unwrap_or_else(|| format!("{e:?}")))?;
    serde_wasm_bindgen::from_value(out).map_err(|e| e.to_string())
}

#[derive(Serialize)]
struct NoArgs {}

#[derive(Serialize)]
struct SourceArgs<'a> {
    source: &'a str,
}

#[derive(Serialize)]
struct PathArgs<'a> {
    path: &'a str,
}

pub async fn load_board() -> Result<Board, String> {
    call("load_board", &NoArgs {}).await
}

#[derive(Serialize)]
struct OnArgs {
    on: bool,
}

pub async fn auto_commit() -> Result<bool, String> {
    call("auto_commit", &NoArgs {}).await
}

pub async fn set_auto_commit(on: bool) -> Result<bool, String> {
    call("set_auto_commit", &OnArgs { on }).await
}

pub async fn get_template() -> Result<TicketTemplate, String> {
    call("get_template", &NoArgs {}).await
}

pub async fn preview_template(source: &str) -> Result<String, String> {
    call("preview_template", &SourceArgs { source }).await
}

pub async fn save_template(source: &str) -> Result<TicketTemplate, String> {
    call("save_template", &SourceArgs { source }).await
}

pub async fn reset_template() -> Result<TicketTemplate, String> {
    call("reset_template", &NoArgs {}).await
}

pub async fn tasks_dir() -> Result<Option<String>, String> {
    call("tasks_dir", &NoArgs {}).await
}

pub async fn set_tasks_dir(path: &str) -> Result<Board, String> {
    call("set_tasks_dir", &PathArgs { path }).await
}

#[derive(Serialize)]
struct ProjectArgs<'a> {
    name: &'a str,
}

/// Switches to another project in the same folder (ticket 093).
pub async fn use_project(name: &str) -> Result<Board, String> {
    call("use_project", &ProjectArgs { name }).await
}

/// Clones a declared repository that isn't on this machine (ticket 108);
/// returns any duplicate-clone warnings.
pub async fn clone_repository(name: &str) -> Result<Vec<String>, String> {
    call("clone_repository", &ProjectArgs { name }).await
}

#[derive(Serialize)]
struct MoveArgs<'a> {
    file: &'a str,
    from: &'a Place,
    to: &'a Place,
    force: bool,
}

/// Moves a ticket; `force` overrides a full column's WIP limit.
pub async fn move_ticket(
    file: &str,
    from: &Place,
    to: &Place,
    force: bool,
) -> Result<SyncReport, String> {
    call(
        "move_ticket",
        &MoveArgs {
            file,
            from,
            to,
            force,
        },
    )
    .await
}

#[derive(Serialize)]
struct TicketRef<'a> {
    place: &'a Place,
    file: &'a str,
}

#[derive(Serialize)]
struct SaveArgs<'a> {
    place: &'a Place,
    file: &'a str,
    content: &'a str,
    version: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TitleArgs<'a> {
    title: &'a str,
    repo: Option<&'a str>,
    repository: Option<&'a str>,
    draft: bool,
    required_by: Option<&'a str>,
    related_to: Option<&'a str>,
    parent: Option<&'a str>,
}

#[derive(Serialize)]
struct NameArgs<'a> {
    name: &'a str,
    repo: Option<&'a str>,
}

#[derive(Serialize)]
struct RenameArgs<'a> {
    repo: Option<&'a str>,
    from: &'a str,
    to: &'a str,
}

pub async fn open_ticket(place: &Place, file: &str) -> Result<TicketDocument, String> {
    call("open_ticket", &TicketRef { place, file }).await
}

/// A summary of what's on disk; it changes whenever the board's files do
/// (ticket 091).
pub async fn board_fingerprint() -> Result<String, String> {
    call("board_fingerprint", &NoArgs {}).await
}

/// Brings the board up to date with its remote (ticket 102); returns any
/// warnings, and whether origin was out of reach (ticket 197).
pub async fn sync_board() -> Result<rite_ui_model::SyncStatus, String> {
    call("sync_board", &NoArgs {}).await
}

/// The ticket's current version on disk, or `None` once it has moved or
/// gone.
pub async fn ticket_version(place: &Place, file: &str) -> Result<Option<String>, String> {
    call("ticket_version", &TicketRef { place, file }).await
}

/// Saves a ticket's text; `version` is the fingerprint it was opened with,
/// so a ticket changed since is refused rather than overwritten.
pub async fn save_ticket(
    place: &Place,
    file: &str,
    content: &str,
    version: &str,
) -> Result<SyncReport, String> {
    call(
        "save_ticket",
        &SaveArgs {
            place,
            file,
            content,
            version,
        },
    )
    .await
}

pub async fn archive_ticket(place: &Place, file: &str) -> Result<SyncReport, String> {
    call("archive_ticket", &TicketRef { place, file }).await
}

/// Duplicates a ticket into its board's backlog, linked as a clone.
pub async fn clone_ticket(place: &Place, file: &str) -> Result<Created, String> {
    call("clone_ticket", &TicketRef { place, file }).await
}

#[derive(Serialize)]
struct LinkArgs<'a> {
    place: &'a Place,
    file: &'a str,
    other: &'a str,
    kind: &'a str,
    remove: bool,
}

/// Links a ticket to another as `kind` (`requires`, `child-of`, …), or unlinks
/// them; the other ticket records the opposite (ticket 207).
pub async fn link_ticket(
    place: &Place,
    file: &str,
    other: &str,
    kind: &str,
    remove: bool,
) -> Result<SyncReport, String> {
    call(
        "link_ticket",
        &LinkArgs {
            place,
            file,
            other,
            kind,
            remove,
        },
    )
    .await
}

#[derive(Serialize)]
struct RetypeArgs<'a> {
    place: &'a Place,
    file: &'a str,
    other: &'a str,
    old: &'a str,
    new: &'a str,
}

/// Changes the type of the link between a ticket and `other` from `old` to
/// `new`, on both tickets (ticket 213).
pub async fn retype_link(
    place: &Place,
    file: &str,
    other: &str,
    old: &str,
    new: &str,
) -> Result<SyncReport, String> {
    call(
        "retype_link",
        &RetypeArgs {
            place,
            file,
            other,
            old,
            new,
        },
    )
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FamilyArgs<'a> {
    place: &'a Place,
    file: &'a str,
    include_done: bool,
}

/// Parent and child around a ticket (ticket 261), done tickets only with
/// `include_done`.
pub async fn ticket_hierarchy(
    place: &Place,
    file: &str,
    include_done: bool,
) -> Result<Vec<rite_ui_model::tree::TreeNode>, String> {
    call(
        "ticket_hierarchy",
        &FamilyArgs {
            place,
            file,
            include_done,
        },
    )
    .await
}

/// The requires tree around a ticket (ticket 214), done tickets only with
/// `include_done` (ticket 217).
pub async fn ticket_family(
    place: &Place,
    file: &str,
    include_done: bool,
) -> Result<Vec<rite_ui_model::tree::TreeNode>, String> {
    call(
        "ticket_family",
        &FamilyArgs {
            place,
            file,
            include_done,
        },
    )
    .await
}

/// Creates a ticket in `repo`'s backlog (the board's own when `None`),
/// as a draft when `draft` (ticket 114).
/// `link` ties it back to the ticket it was made from (ticket 227): that
/// ticket's id, and whether it requires the new one, relates to it or is
/// its parent (ticket 259).
pub async fn create_ticket(
    title: &str,
    repo: Option<&str>,
    repository: Option<&str>,
    draft: bool,
    link: Option<(&str, crate::app::LinkKind)>,
) -> Result<Created, String> {
    use crate::app::LinkKind;
    call(
        "create_ticket",
        &TitleArgs {
            title,
            repo,
            repository,
            draft,
            required_by: link.and_then(|(id, k)| (k == LinkKind::Requires).then_some(id)),
            related_to: link.and_then(|(id, k)| (k == LinkKind::Related).then_some(id)),
            parent: link.and_then(|(id, k)| (k == LinkKind::Child).then_some(id)),
        },
    )
    .await
}

pub async fn create_epic(name: &str, repo: Option<&str>) -> Result<SyncReport, String> {
    call("create_epic", &NameArgs { name, repo }).await
}

pub async fn rename_epic(repo: Option<&str>, from: &str, to: &str) -> Result<SyncReport, String> {
    call("rename_epic", &RenameArgs { repo, from, to }).await
}

#[derive(Serialize)]
struct DeleteEpicArgs<'a> {
    repo: Option<&'a str>,
    name: &'a str,
}

/// Deletes an epic or sprint; refused while any ticket in it isn't done.
pub async fn delete_epic(repo: Option<&str>, name: &str) -> Result<SyncReport, String> {
    call("delete_epic", &DeleteEpicArgs { repo, name }).await
}

/// A newer KanRite to offer (ticket 192), if any.
pub async fn update_status() -> Result<Option<rite_ui_model::UpdateInfo>, String> {
    call("update_status", &NoArgs {}).await
}

/// The update bar's Update: installs the newest release (ticket 191).
pub async fn apply_update() -> Result<rite_ui_model::UpdateResult, String> {
    call("apply_update", &NoArgs {}).await
}

/// The version riteui's program on disk is now, when it was replaced while
/// running (ticket 208).
pub async fn binary_changed() -> Result<Option<String>, String> {
    call("binary_changed", &NoArgs {}).await
}

/// Starts riteui again, into the version just installed.
pub async fn restart_app() -> Result<(), String> {
    call("restart_app", &NoArgs {}).await
}

#[derive(Serialize)]
struct SnoozeArgs<'a> {
    version: &'a str,
}

/// The update bar's Later: this version, hidden for a day.
pub async fn snooze_update(version: &str) -> Result<(), String> {
    call("snooze_update", &SnoozeArgs { version }).await
}

/// Check for updates now (ticket 304).
pub async fn check_for_update() -> Result<rite_ui_model::ManualCheck, String> {
    call("check_for_update", &NoArgs {}).await
}

/// What's new: the notes of each release newer than this one, newest first
/// (ticket 336).
pub async fn whats_new() -> Result<Vec<rite_ui_model::ReleaseNotes>, String> {
    call("whats_new", &NoArgs {}).await
}

/// When GitHub last answered a check for updates (unix seconds).
pub async fn update_checked() -> Result<Option<i64>, String> {
    call("update_checked", &NoArgs {}).await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MoveEpicArgs<'a> {
    repo: Option<&'a str>,
    name: &'a str,
    to_todo: bool,
}

/// Moves every ticket of an epic or sprint still in the backlog to the loose
/// backlog, or to todo (ticket 256).
pub async fn move_epic_tickets(
    repo: Option<&str>,
    name: &str,
    to_todo: bool,
) -> Result<SyncReport, String> {
    call(
        "move_epic_tickets",
        &MoveEpicArgs {
            repo,
            name,
            to_todo,
        },
    )
    .await
}

#[derive(Serialize)]
struct SprintArgs<'a> {
    name: &'a str,
    days: u32,
    repo: Option<&'a str>,
}

#[derive(Serialize)]
struct StartArgs<'a> {
    repo: Option<&'a str>,
    name: &'a str,
}

pub async fn create_sprint(
    name: &str,
    days: u32,
    repo: Option<&str>,
) -> Result<SyncReport, String> {
    call("create_sprint", &SprintArgs { name, days, repo }).await
}

pub async fn start_sprint(repo: Option<&str>, name: &str) -> Result<SyncReport, String> {
    call("start_sprint", &StartArgs { repo, name }).await
}

/// Closes a started sprint and files its report (ticket 052).
pub async fn close_sprint(repo: Option<&str>, name: &str) -> Result<SyncReport, String> {
    call("close_sprint", &StartArgs { repo, name }).await
}

#[derive(Serialize)]
struct BackfillArgs<'a> {
    repo: Option<&'a str>,
    name: &'a str,
    points: u32,
}

/// Backfills a running sprint with up to `points` from the loose backlog.
pub async fn backfill_sprint(
    repo: Option<&str>,
    name: &str,
    points: u32,
) -> Result<SyncReport, String> {
    call("backfill_sprint", &BackfillArgs { repo, name, points }).await
}

#[derive(Serialize)]
struct AppearanceArgs {
    appearance: Appearance,
}

pub async fn appearance() -> Result<Appearance, String> {
    call("appearance", &NoArgs {}).await
}

/// Applies the board's saved zoom and returns it, in percent (ticket 024).
pub async fn zoom() -> Result<u32, String> {
    call("zoom", &NoArgs {}).await
}

#[derive(Serialize)]
struct ZoomArgs {
    percent: u32,
}

/// Zooms the board and remembers the size on this machine.
pub async fn set_zoom(percent: u32) -> Result<u32, String> {
    call("set_zoom", &ZoomArgs { percent }).await
}

pub async fn set_appearance(appearance: Appearance) -> Result<Appearance, String> {
    call("set_appearance", &AppearanceArgs { appearance }).await
}

#[derive(Serialize)]
struct WipArgs<'a> {
    limits: WipLimits,
    repo: Option<&'a str>,
}

pub async fn set_wip_limits(limits: WipLimits, repo: Option<&str>) -> Result<SyncReport, String> {
    call("set_wip_limits", &WipArgs { limits, repo }).await
}

#[derive(Serialize)]
struct AddColumnArgs<'a> {
    label: &'a str,
    kind: &'a str,
    after: Option<rite_ui_model::Column>,
    before: Option<rite_ui_model::Column>,
}

/// Adds a column of `kind` to the project's board (ticket 224).
pub async fn add_column(
    label: &str,
    kind: &str,
    after: Option<rite_ui_model::Column>,
    before: Option<rite_ui_model::Column>,
) -> Result<SyncReport, String> {
    call(
        "add_column",
        &AddColumnArgs {
            label,
            kind,
            after,
            before,
        },
    )
    .await
}

#[derive(Serialize)]
struct RenameColumnArgs<'a> {
    column: rite_ui_model::Column,
    label: &'a str,
}

/// Renames one of the project's columns; its folder stays.
pub async fn rename_column(
    column: rite_ui_model::Column,
    label: &str,
) -> Result<SyncReport, String> {
    call("rename_column", &RenameColumnArgs { column, label }).await
}

#[derive(Serialize)]
struct MoveColumnArgs {
    column: rite_ui_model::Column,
    after: Option<rite_ui_model::Column>,
    before: Option<rite_ui_model::Column>,
}

/// Moves one of the project's columns along the board.
pub async fn move_column(
    column: rite_ui_model::Column,
    after: Option<rite_ui_model::Column>,
    before: Option<rite_ui_model::Column>,
) -> Result<SyncReport, String> {
    call(
        "move_column",
        &MoveColumnArgs {
            column,
            after,
            before,
        },
    )
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoveColumnArgs {
    column: rite_ui_model::Column,
    move_to: Option<rite_ui_model::Column>,
}

/// Removes an added column, its tickets moved to `move_to` first.
pub async fn remove_column(
    column: rite_ui_model::Column,
    move_to: Option<rite_ui_model::Column>,
) -> Result<SyncReport, String> {
    call("remove_column", &RemoveColumnArgs { column, move_to }).await
}

#[derive(Serialize)]
struct ColumnWipArgs {
    column: rite_ui_model::Column,
    limit: Option<u32>,
}

/// Sets or clears a doing-kind column's own WIP limit.
pub async fn set_column_wip(
    column: rite_ui_model::Column,
    limit: Option<u32>,
) -> Result<SyncReport, String> {
    call("set_column_wip", &ColumnWipArgs { column, limit }).await
}

/// Where a new repository would be made, and its defaults (ticket 228).
pub async fn repo_defaults() -> Result<rite_ui_model::RepoDefaults, String> {
    call("repo_defaults", &NoArgs {}).await
}

#[derive(Serialize)]
struct NewRepoArgs<'a> {
    name: &'a str,
    description: &'a str,
    private: bool,
    readme: bool,
    place: Option<&'a Place>,
    file: Option<&'a str>,
}

/// Makes (or connects) a repository for the project, for the ticket at
/// `place`/`file` when given.
pub async fn create_repository(
    name: &str,
    description: &str,
    private: bool,
    readme: bool,
    ticket: Option<(&Place, &str)>,
) -> Result<SyncReport, String> {
    call(
        "create_repository",
        &NewRepoArgs {
            name,
            description,
            private,
            readme,
            place: ticket.map(|(p, _)| p),
            file: ticket.map(|(_, f)| f),
        },
    )
    .await
}

#[derive(Serialize)]
struct FlowArgs {
    any: bool,
}

/// Any column to any other, or the next column plus the extra moves.
pub async fn set_flow(any: bool) -> Result<SyncReport, String> {
    call("set_flow", &FlowArgs { any }).await
}

#[derive(Serialize)]
struct AllowMoveArgs {
    from: rite_ui_model::Column,
    to: rite_ui_model::Column,
    on: bool,
}

/// Allows an extra one-way move, or stops allowing it.
pub async fn allow_move(
    from: rite_ui_model::Column,
    to: rite_ui_model::Column,
    on: bool,
) -> Result<SyncReport, String> {
    call("allow_move", &AllowMoveArgs { from, to, on }).await
}

#[derive(Serialize)]
struct LandingArgs {
    column: rite_ui_model::Column,
}

/// Where new tickets land.
pub async fn set_landing(column: rite_ui_model::Column) -> Result<SyncReport, String> {
    call("set_landing", &LandingArgs { column }).await
}

/// Back to the standard four columns.
pub async fn reset_columns() -> Result<SyncReport, String> {
    call("reset_columns", &NoArgs {}).await
}

#[derive(Serialize)]
struct TagArgs<'a> {
    tag: &'a str,
}

/// Adds a tag to the project's list (ticket 122); saved in UPPERCASE.
pub async fn add_tag(tag: &str) -> Result<SyncReport, String> {
    call("add_tag", &TagArgs { tag }).await
}

/// Takes an unused tag off the project's list (ticket 122).
pub async fn remove_tag(tag: &str) -> Result<SyncReport, String> {
    call("remove_tag", &TagArgs { tag }).await
}

/// Whether only listed tags may be put on a ticket (ticket 122).
pub async fn set_tags_only(on: bool) -> Result<SyncReport, String> {
    call("set_tags_only", &OnArgs { on }).await
}

#[derive(Serialize)]
struct UnlockArgs<'a> {
    place: &'a Place,
    file: &'a str,
    force: bool,
}

/// Locks a ticket for the user, or renews their own lock.
pub async fn lock_ticket(place: &Place, file: &str) -> Result<SyncReport, String> {
    call("lock_ticket", &TicketRef { place, file }).await
}

/// Releases the user's own lock; `force` breaks someone else's.
pub async fn unlock_ticket(place: &Place, file: &str, force: bool) -> Result<SyncReport, String> {
    call("unlock_ticket", &UnlockArgs { place, file, force }).await
}

#[derive(Serialize)]
struct HoursArgs<'a> {
    hours: u32,
    repo: Option<&'a str>,
}

pub async fn set_lease_hours(hours: u32, repo: Option<&str>) -> Result<SyncReport, String> {
    call("set_lease_hours", &HoursArgs { hours, repo }).await
}

/// The landing page: your work across every project, and the projects
/// (ticket 020).
pub async fn home() -> Result<rite_ui_model::Home, String> {
    call("home", &NoArgs {}).await
}

#[derive(Serialize)]
struct HiddenArgs<'a> {
    root: &'a str,
    hidden: bool,
}

/// Hides a project from My work, or shows it again.
pub async fn set_project_hidden(root: &str, hidden: bool) -> Result<(), String> {
    call("set_project_hidden", &HiddenArgs { root, hidden }).await
}

#[derive(Serialize)]
struct FoldArgs<'a> {
    root: &'a str,
    folded: bool,
}

/// Folds or opens a project's sprint graphs on the landing page (ticket 305).
pub async fn set_project_graphs_folded(root: &str, folded: bool) -> Result<(), String> {
    call("set_project_graphs_folded", &FoldArgs { root, folded }).await
}

#[derive(Serialize)]
struct ProjectArchiveArgs<'a> {
    root: &'a str,
    on: bool,
}

/// Archives a project or brings it back (ticket 157); `root` is its
/// repository or its board, `github` its repositories on GitHub too.
pub async fn set_project_archived(root: &str, on: bool) -> Result<SyncReport, String> {
    call("set_project_archived", &ProjectArchiveArgs { root, on }).await
}

/// My work's "Include unassigned tickets" (ticket 160).
pub async fn set_include_unassigned(on: bool) -> Result<(), String> {
    call("set_include_unassigned", &OnArgs { on }).await
}

/// The project's documents repositories and their documents (ticket 129).
pub async fn list_documents() -> Result<Vec<rite_ui_model::DocSource>, String> {
    call("list_documents", &NoArgs {}).await
}

#[derive(Serialize)]
struct DocArgs<'a> {
    source: &'a str,
    path: &'a str,
}

/// A document, rendered for reading.
pub async fn read_document(source: &str, path: &str) -> Result<rite_ui_model::RenderedDoc, String> {
    call("read_document", &DocArgs { source, path }).await
}

#[derive(Serialize)]
struct VersionArgs<'a> {
    source: &'a str,
    path: &'a str,
    commit: &'a str,
}

/// A document's last versions, newest first (ticket 169).
pub async fn document_history(
    source: &str,
    path: &str,
) -> Result<Vec<rite_ui_model::DocVersion>, String> {
    call("document_history", &DocArgs { source, path }).await
}

/// An earlier version of a document, rendered for reading.
pub async fn read_document_version(
    source: &str,
    path: &str,
    commit: &str,
) -> Result<rite_ui_model::RenderedDoc, String> {
    call(
        "read_document_version",
        &VersionArgs {
            source,
            path,
            commit,
        },
    )
    .await
}

/// Puts a document back as it was at `commit`, as a new commit, pushed.
pub async fn restore_document(
    source: &str,
    path: &str,
    commit: &str,
) -> Result<SyncReport, String> {
    call(
        "restore_document",
        &VersionArgs {
            source,
            path,
            commit,
        },
    )
    .await
}

#[derive(Serialize)]
struct UrlArgs<'a> {
    url: &'a str,
}

/// Opens a web or mail link outside riteui.
pub async fn open_external(url: &str) -> Result<(), String> {
    call("open_external", &UrlArgs { url }).await
}

#[derive(Serialize)]
struct SharedArgs<'a> {
    name: &'a str,
}

/// Sets the project's display name (ticket 326); empty clears it.
pub async fn set_display_name(name: &str) -> Result<SyncReport, String> {
    call("set_display_name", &SharedArgs { name }).await
}

/// Names the documents repository every project shares; empty clears it.
pub async fn set_shared_documents(name: &str) -> Result<SyncReport, String> {
    call("set_shared_documents", &SharedArgs { name }).await
}

/// Pulls in GitHub issues for every project whose switch is on (ticket
/// 173): what the landing page does when it loads.
pub async fn pull_issues() -> Result<Vec<SyncReport>, String> {
    call("pull_issues", &NoArgs {}).await
}

/// Whether the landing page pulls in this project's GitHub issues.
pub async fn set_import_issues(on: bool) -> Result<SyncReport, String> {
    call("set_import_issues", &OnArgs { on }).await
}

/// Whether priorities follow requires links (ticket 215).
pub async fn set_auto_priority(on: bool) -> Result<SyncReport, String> {
    call("set_auto_priority", &OnArgs { on }).await
}

#[derive(Serialize)]
struct TimeLimitsArgs {
    todo: u32,
    doing: u32,
}

/// How long a ticket should take in todo and in doing (ticket 252).
pub async fn set_time_limits(todo: u32, doing: u32) -> Result<SyncReport, String> {
    call("set_time_limits", &TimeLimitsArgs { todo, doing }).await
}

/// How near a due date sends a ticket straight to todo (ticket 253).
pub async fn set_due_to_todo_days(days: u32) -> Result<SyncReport, String> {
    call("set_due_to_todo_days", &DaysArgs { days }).await
}

/// Whether the backlog column can be folded away (ticket 253).
pub async fn set_backlog_always_shown(on: bool) -> Result<SyncReport, String> {
    call("set_backlog_always_shown", &OnArgs { on }).await
}

/// Whether a priority set by hand pins its ticket (ticket 215).
pub async fn set_priority_override(on: bool) -> Result<SyncReport, String> {
    call("set_priority_override", &OnArgs { on }).await
}

#[derive(Serialize)]
struct IssuesArgs<'a> {
    repo: &'a str,
    label: &'a str,
    preview: bool,
}

/// Imports open GitHub issues into the backlog, tagged TRIAGE (ticket 158),
/// or with `preview` only says what it would import.
pub async fn import_issues(
    repo: &str,
    label: &str,
    preview: bool,
) -> Result<rite_ui_model::IssueImport, String> {
    call(
        "import_issues",
        &IssuesArgs {
            repo,
            label,
            preview,
        },
    )
    .await
}

// ---- Changing documents (ticket 176)

#[derive(Serialize)]
struct ArchiveArgs<'a> {
    source: &'a str,
    path: &'a str,
    on: bool,
}

/// Marks a document archived, or not.
pub async fn archive_document(
    source: &str,
    path: &str,
    on: bool,
) -> Result<rite_ui_model::DocChange, String> {
    call("archive_document", &ArchiveArgs { source, path, on }).await
}

#[derive(Serialize)]
struct FolderArgs<'a> {
    source: &'a str,
    parent: &'a str,
    name: &'a str,
}

/// Makes a folder inside `parent` (empty for the top).
pub async fn create_document_folder(
    source: &str,
    parent: &str,
    name: &str,
) -> Result<rite_ui_model::DocChange, String> {
    call(
        "create_document_folder",
        &FolderArgs {
            source,
            parent,
            name,
        },
    )
    .await
}

#[derive(Serialize)]
struct DeleteFolderArgs<'a> {
    source: &'a str,
    folder: &'a str,
}

/// Removes an empty folder.
pub async fn delete_document_folder(
    source: &str,
    folder: &str,
) -> Result<rite_ui_model::DocChange, String> {
    call(
        "delete_document_folder",
        &DeleteFolderArgs { source, folder },
    )
    .await
}

#[derive(Serialize)]
struct MoveDocArgs<'a> {
    source: &'a str,
    path: &'a str,
    folder: &'a str,
}

/// Moves a document into `folder` (empty for the top), links kept working.
pub async fn move_document(
    source: &str,
    path: &str,
    folder: &str,
) -> Result<rite_ui_model::DocChange, String> {
    call(
        "move_document",
        &MoveDocArgs {
            source,
            path,
            folder,
        },
    )
    .await
}

#[derive(Serialize)]
struct NewDocArgs<'a> {
    source: &'a str,
    folder: &'a str,
    title: &'a str,
}

/// Makes a new document titled `title` in `folder`.
pub async fn create_document(
    source: &str,
    folder: &str,
    title: &str,
) -> Result<rite_ui_model::DocChange, String> {
    call(
        "create_document",
        &NewDocArgs {
            source,
            folder,
            title,
        },
    )
    .await
}

/// A document's text for editing.
pub async fn open_document_for_edit(
    source: &str,
    path: &str,
) -> Result<rite_ui_model::DocEdit, String> {
    call("open_document_for_edit", &DocArgs { source, path }).await
}

#[derive(Serialize)]
struct SaveDocArgs<'a> {
    source: &'a str,
    path: &'a str,
    text: &'a str,
    version: &'a str,
}

/// Saves an edited document, unless it changed since it was opened.
pub async fn save_document(
    source: &str,
    path: &str,
    text: &str,
    version: &str,
) -> Result<rite_ui_model::DocChange, String> {
    call(
        "save_document",
        &SaveDocArgs {
            source,
            path,
            text,
            version,
        },
    )
    .await
}

/// The project's roadmaps and the boards without one (ticket 206).
pub async fn list_roadmaps() -> Result<rite_ui_model::RoadmapsPage, String> {
    call("list_roadmaps", &NoArgs {}).await
}

#[derive(Serialize)]
struct CreateRoadmapArgs<'a> {
    board: &'a str,
    alias: &'a str,
}

/// Starts a board's roadmap, called `alias` (else the board's name without
/// `-tickets`).
pub async fn create_roadmap(board: &str, alias: &str) -> Result<SyncReport, String> {
    call("create_roadmap", &CreateRoadmapArgs { board, alias }).await
}

#[derive(Serialize)]
struct AdoptArgs<'a> {
    roadmap: &'a str,
    board: &'a str,
}

/// Gives a roadmap from before roadmaps had boards its board.
pub async fn adopt_roadmap(roadmap: &str, board: &str) -> Result<SyncReport, String> {
    call("adopt_roadmap", &AdoptArgs { roadmap, board }).await
}

#[derive(Serialize)]
struct RoadmapDetailsArgs<'a> {
    roadmap: &'a str,
    goal: &'a str,
    sprints: bool,
}

pub async fn save_roadmap(roadmap: &str, goal: &str, sprints: bool) -> Result<SyncReport, String> {
    call(
        "save_roadmap",
        &RoadmapDetailsArgs {
            roadmap,
            goal,
            sprints,
        },
    )
    .await
}

#[derive(Serialize)]
struct RenameRoadmapArgs<'a> {
    roadmap: &'a str,
    name: &'a str,
}

/// Gives a roadmap a new name; its file moves with it.
pub async fn rename_roadmap(roadmap: &str, name: &str) -> Result<SyncReport, String> {
    call("rename_roadmap", &RenameRoadmapArgs { roadmap, name }).await
}

#[derive(Serialize)]
struct RoadmapArgs<'a> {
    roadmap: &'a str,
}

/// Deletes a roadmap and its milestones' empty epics and sprints (ticket 306).
pub async fn delete_roadmap(roadmap: &str) -> Result<SyncReport, String> {
    call("delete_roadmap", &RoadmapArgs { roadmap }).await
}

#[derive(Serialize)]
struct MilestoneNameArgs<'a> {
    roadmap: &'a str,
    name: &'a str,
}

/// Takes a milestone off a roadmap, and its epic or sprint when empty (306).
pub async fn delete_milestone(roadmap: &str, name: &str) -> Result<SyncReport, String> {
    call("delete_milestone", &MilestoneNameArgs { roadmap, name }).await
}

#[derive(Serialize)]
struct MilestoneArgs<'a> {
    roadmap: &'a str,
    milestone: &'a rite_ui_model::MilestoneCard,
}

/// Adds a milestone, which makes its epic or sprint on the roadmap's board.
pub async fn add_milestone(
    roadmap: &str,
    milestone: &rite_ui_model::MilestoneCard,
) -> Result<SyncReport, String> {
    call("add_milestone", &MilestoneArgs { roadmap, milestone }).await
}

#[derive(Serialize)]
struct MilestoneDatesArgs<'a> {
    roadmap: &'a str,
    milestone: &'a str,
    start: &'a str,
    due: &'a str,
}

/// Changes a milestone's start and due dates (either may be empty).
pub async fn set_milestone_dates(
    roadmap: &str,
    milestone: &str,
    start: &str,
    due: &str,
) -> Result<SyncReport, String> {
    call(
        "set_milestone_dates",
        &MilestoneDatesArgs {
            roadmap,
            milestone,
            start,
            due,
        },
    )
    .await
}

/// The project's ideas, best first (ticket 131).
pub async fn list_ideas() -> Result<rite_ui_model::IdeaList, String> {
    call("list_ideas", &NoArgs {}).await
}

/// One idea: its write-up, votes and notes.
pub async fn read_idea(path: &str) -> Result<rite_ui_model::IdeaDetail, String> {
    call("read_idea", &PathArgs { path }).await
}

#[derive(Serialize)]
struct NewIdeaArgs<'a> {
    title: &'a str,
    body: &'a str,
    named: bool,
}

/// Puts an idea forward, with your name on it when `named`.
pub async fn create_idea(
    title: &str,
    body: &str,
    named: bool,
) -> Result<rite_ui_model::DocChange, String> {
    call("create_idea", &NewIdeaArgs { title, body, named }).await
}

#[derive(Serialize)]
struct VoteArgs<'a> {
    path: &'a str,
    delta: i32,
}

/// One click on an idea's + (1) or − (−1).
pub async fn vote_idea(path: &str, delta: i32) -> Result<rite_ui_model::DocChange, String> {
    call("vote_idea", &VoteArgs { path, delta }).await
}

#[derive(Serialize)]
struct NoteArgs<'a> {
    path: &'a str,
    text: &'a str,
}

/// Adds your note under an idea.
pub async fn note_idea(path: &str, text: &str) -> Result<rite_ui_model::DocChange, String> {
    call("note_idea", &NoteArgs { path, text }).await
}

/// Makes a backlog ticket from an idea.
pub async fn idea_to_ticket(path: &str) -> Result<rite_ui_model::IdeaTicket, String> {
    call("idea_to_ticket", &PathArgs { path }).await
}

#[derive(Serialize)]
struct IdeaGroupArgs<'a> {
    path: &'a str,
    sprint: bool,
    name: &'a str,
    days: u32,
}

/// Makes an epic, or a sprint `days` long, from an idea (admins only).
pub async fn idea_to_group(
    path: &str,
    sprint: bool,
    name: &str,
    days: u32,
) -> Result<SyncReport, String> {
    call(
        "idea_to_group",
        &IdeaGroupArgs {
            path,
            sprint,
            name,
            days,
        },
    )
    .await
}

#[derive(Serialize)]
struct PinArgs<'a> {
    source: &'a str,
    path: Option<&'a str>,
}

/// Your pinned documents (ticket 232).
pub async fn my_pins() -> Result<Vec<rite_ui_model::DocPin>, String> {
    call("my_pins", &NoArgs {}).await
}

#[derive(Serialize)]
struct DocPinArgs<'a> {
    source: &'a str,
    path: &'a str,
    on: bool,
}

/// Pins a document to your own list, or unpins it.
pub async fn set_doc_pin(
    source: &str,
    path: &str,
    on: bool,
) -> Result<rite_ui_model::DocChange, String> {
    call("set_doc_pin", &DocPinArgs { source, path, on }).await
}

/// Pins a document as the start page (ticket 179); `None` goes back to the
/// README.
pub async fn pin_start_page(
    source: &str,
    path: Option<&str>,
) -> Result<rite_ui_model::DocChange, String> {
    call("pin_start_page", &PinArgs { source, path }).await
}

#[derive(Serialize)]
struct QueryArgs<'a> {
    query: &'a str,
}

/// Searches the project's documents and the shared ones (ticket 179).
pub async fn search_documents(query: &str) -> Result<Vec<rite_ui_model::DocHit>, String> {
    call("search_documents", &QueryArgs { query }).await
}

// --- The bottom panel's terminals (ticket 185) ---------------------------

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "event"], js_name = listen, catch)]
    async fn tauri_listen(event: &str, handler: &js_sys::Function) -> Result<JsValue, JsValue>;
}

/// Calls `f` with each `name` event's payload from the backend, for as long
/// as the page lives.
pub fn on_event<T: DeserializeOwned + 'static>(name: &'static str, f: impl Fn(T) + 'static) {
    let handler = Closure::<dyn Fn(JsValue)>::new(move |event: JsValue| {
        let payload = js_sys::Reflect::get(&event, &"payload".into()).unwrap_or(JsValue::NULL);
        if let Ok(value) = serde_wasm_bindgen::from_value::<T>(payload) {
            f(value);
        }
    });
    let function: js_sys::Function = handler.as_ref().unchecked_ref::<js_sys::Function>().clone();
    // Lives as long as the page.
    handler.forget();
    wasm_bindgen_futures::spawn_local(async move {
        let _ = tauri_listen(name, &function).await;
    });
}

#[derive(Serialize)]
struct TermOpenArgs {
    kind: rite_ui_model::terminal::TermKind,
    cols: u16,
    rows: u16,
}

/// Starts a terminal; its id.
pub async fn terminal_open(
    kind: rite_ui_model::terminal::TermKind,
    cols: u16,
    rows: u16,
) -> Result<u32, String> {
    call("terminal_open", &TermOpenArgs { kind, cols, rows }).await
}

#[derive(Serialize)]
struct TermInputArgs<'a> {
    id: u32,
    text: &'a str,
}

pub async fn terminal_input(id: u32, text: &str) -> Result<(), String> {
    call("terminal_input", &TermInputArgs { id, text }).await
}

#[derive(Serialize)]
struct TermSizeArgs {
    id: u32,
    cols: u16,
    rows: u16,
}

pub async fn terminal_resize(id: u32, cols: u16, rows: u16) -> Result<(), String> {
    call("terminal_resize", &TermSizeArgs { id, cols, rows }).await
}

#[derive(Serialize)]
struct TermScrollArgs {
    id: u32,
    lines: i32,
}

pub async fn terminal_scroll(id: u32, lines: i32) -> Result<(), String> {
    call("terminal_scroll", &TermScrollArgs { id, lines }).await
}

#[derive(Serialize)]
struct TermIdArgs {
    id: u32,
}

pub async fn terminal_close(id: u32) -> Result<(), String> {
    call("terminal_close", &TermIdArgs { id }).await
}

/// The AI tab's command (`claude` unless Preferences says otherwise).
pub async fn ai_command() -> Result<String, String> {
    call("ai_command", &NoArgs {}).await
}

#[derive(Serialize)]
struct AiCommandArgs<'a> {
    command: &'a str,
}

pub async fn set_ai_command(command: &str) -> Result<String, String> {
    call("set_ai_command", &AiCommandArgs { command }).await
}

#[derive(Serialize)]
struct ScreenArgs<'a> {
    view: &'a rite_ui_model::screen::ScreenView,
}

/// Tells rite-ui what's on screen, for an AI assistant (ticket 186).
pub async fn set_screen(view: &rite_ui_model::screen::ScreenView) -> Result<(), String> {
    call("set_screen", &ScreenArgs { view }).await
}

#[wasm_bindgen]
extern "C" {
    /// `invoke` called directly, for a panic hook, where nothing awaits.
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"], js_name = invoke)]
    fn invoke_now(cmd: &str, args: JsValue) -> js_sys::Promise;
}

#[derive(Serialize)]
struct PanicArgs<'a> {
    message: &'a str,
}

/// Tells riteui the page panicked, so the message reaches its error output.
pub fn report_panic(message: &str) {
    if let Ok(args) = serde_wasm_bindgen::to_value(&PanicArgs { message }) {
        let _ = invoke_now("page_panic", args);
    }
}

// --- Speech to text (ticket 187) ------------------------------------------

pub async fn speech_status() -> Result<rite_ui_model::terminal::SpeechStatus, String> {
    call("speech_status", &NoArgs {}).await
}

#[derive(Serialize)]
struct SpeechModelArgs<'a> {
    model: &'a str,
}

/// Downloads a model; progress comes as `speech-progress` events.
pub async fn speech_download(model: &str) -> Result<(), String> {
    call("speech_download", &SpeechModelArgs { model }).await
}

pub async fn speech_remove(model: &str) -> Result<(), String> {
    call("speech_remove", &SpeechModelArgs { model }).await
}

pub async fn set_speech_model(model: &str) -> Result<(), String> {
    call("set_speech_model", &SpeechModelArgs { model }).await
}

pub async fn set_speech_enabled(on: bool) -> Result<(), String> {
    call("set_speech_enabled", &OnArgs { on }).await
}

pub async fn speech_start() -> Result<(), String> {
    call("speech_start", &NoArgs {}).await
}

/// Stops recording; what was said.
pub async fn speech_stop() -> Result<String, String> {
    call("speech_stop", &NoArgs {}).await
}

pub async fn speech_cancel() -> Result<(), String> {
    call("speech_cancel", &NoArgs {}).await
}

// --- Read aloud (ticket 286) ----------------------------------------------

pub async fn read_aloud_status() -> Result<rite_ui_model::terminal::ReadAloudStatus, String> {
    call("read_aloud_status", &NoArgs {}).await
}

#[derive(Serialize)]
struct ReadAloudArgs<'a> {
    id: Option<u32>,
    text: Option<&'a str>,
}

/// Reads `text`, else terminal `id`'s output since the last Enter; the end
/// comes as a `read-aloud-done` event.
pub async fn read_aloud(id: Option<u32>, text: Option<&str>) -> Result<(), String> {
    call("read_aloud", &ReadAloudArgs { id, text }).await
}

pub async fn read_aloud_stop() -> Result<(), String> {
    call("read_aloud_stop", &NoArgs {}).await
}

pub async fn set_read_aloud_enabled(on: bool) -> Result<(), String> {
    call("set_read_aloud_enabled", &OnArgs { on }).await
}

// --- A Piper voice (ticket 287) -------------------------------------------

pub async fn piper_status() -> Result<rite_ui_model::terminal::PiperStatus, String> {
    call("piper_status", &NoArgs {}).await
}

#[derive(Serialize)]
struct VoiceArgs<'a> {
    voice: &'a str,
}

/// Starts setting up `voice`; progress comes as `piper-progress` events.
pub async fn piper_install(voice: &str) -> Result<(), String> {
    call("piper_install", &VoiceArgs { voice }).await
}

pub async fn piper_remove() -> Result<(), String> {
    call("piper_remove", &NoArgs {}).await
}

#[derive(Serialize)]
struct StatsArgs<'a> {
    root: &'a str,
    days: u32,
}

/// A project's statistics over `days` (ticket 233).
pub async fn project_stats(root: &str, days: u32) -> Result<rite_ui_model::ProjectStats, String> {
    call("project_stats", &StatsArgs { root, days }).await
}

#[derive(Serialize)]
struct RootArgs<'a> {
    root: &'a str,
}

/// A project's open tickets, and how many are top and TRIAGE (ticket 349).
pub async fn project_counts(root: &str) -> Result<rite_ui_model::TicketCounts, String> {
    call("project_counts", &RootArgs { root }).await
}

#[derive(Serialize)]
struct DaysArgs {
    days: u32,
}

/// Remembers the statistics period on this machine.
pub async fn set_stats_days(days: u32) -> Result<(), String> {
    call("set_stats_days", &DaysArgs { days }).await
}
