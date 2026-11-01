//! Top-level state and layout.

use std::collections::BTreeSet;

use leptos::prelude::*;
use leptos::task::spawn_local;
use rite_ui_model::appearance::Appearance;
use rite_ui_model::epic::Place;
use rite_ui_model::ticket::SortOrder;
use rite_ui_model::{Assigned, Board, Column, SyncReport, TicketDocument, TicketSummary};
use wasm_bindgen::JsCast;

use crate::admin::AdminPanel;
use crate::api;
use crate::board::BoardView;
use crate::dialog::{
    CloneRepoDialog, NewEpicDialog, NewSprintDialog, NewTicketDialog, OverLimitDialog,
};
use crate::editor::Editor;
use crate::preferences::PreferencesPanel;

/// What fills the window: the landing page or a project's board (ticket
/// 020). riteui always opens on the landing page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Home,
    Board,
    /// The project's documents (ticket 129).
    Documents,
    /// The project's ideas, best first (ticket 131).
    Ideas,
    /// The board's roadmaps and their milestones (ticket 206).
    Roadmaps,
}

/// Which overlay, if any, is open over the board.
#[derive(Clone, Debug, PartialEq)]
pub enum Overlay {
    Editor(TicketDocument),
    /// The requires tree around a ticket (ticket 214): where its card is,
    /// and its file.
    Tree {
        place: Place,
        file: String,
        /// Parent and child instead (ticket 261).
        children: bool,
    },
    /// New ticket; from the editor's Save and New, linked back to the
    /// ticket it came from (ticket 227).
    NewTicket(Option<LinkBack>),
    NewEpic,
    NewSprint,
    Admin,
    Preferences,
    /// A "(not cloned)" repository chosen in the filter: clone it? (108)
    CloneRepo(String),
    /// A new repository for the project, from a ticket (ticket 228).
    NewRepo(NewRepoFor),
    /// The release notes of each newer KanRite (ticket 336), from the update
    /// bar's What's new; the newest release's page on GitHub to fall back on.
    WhatsNew(String),
    /// A ticket dropped on a column at its WIP limit: move it anyway?
    OverLimit {
        file: String,
        from: Place,
        to: Place,
        reason: String,
    },
}

/// The ticket a new one is made from with Save and New (ticket 227) or Add
/// child (ticket 259): on its board, by id and title, and the link the
/// dialog starts on.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkBack {
    pub repo: String,
    pub id: String,
    pub title: String,
    pub kind: LinkKind,
}

/// The ticket a new repository is made for (ticket 228).
#[derive(Clone, Debug, PartialEq)]
pub struct NewRepoFor {
    pub place: Place,
    pub file: String,
    pub id: String,
    pub title: String,
}

/// How a ticket made with Save and New is tied to the one it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    /// That ticket requires the new one: it was found to depend on it.
    Requires,
    Related,
    /// The new ticket is that one's child (ticket 259): a big ticket
    /// broken into smaller ones.
    Child,
    None,
}

/// State shared by every component, provided as context. All fields are
/// arena signals, so the struct is `Copy` and closures capture it freely.
#[derive(Clone, Copy)]
pub struct Ctx {
    pub board: RwSignal<Option<Board>>,
    /// Origin out of reach at the last catch-up: changes waiting (197).
    pub offline: RwSignal<Option<u32>>,
    /// The newer release the update bar offers (ticket 192), set by its own
    /// checks and by a check asked for by hand (ticket 304).
    pub update_offer: RwSignal<Option<rite_ui_model::UpdateInfo>>,
    /// Landing page or board (ticket 020).
    pub view: RwSignal<View>,
    /// The landing page's data, loaded on arrival there.
    pub home: RwSignal<Option<Result<rite_ui_model::Home, String>>>,
    pub error: RwSignal<Option<String>>,
    pub overlay: RwSignal<Option<Overlay>>,
    pub collapsed: RwSignal<BTreeSet<Column>>,
    /// The Epics column folds away on its own, separately from the backlog.
    pub epics_collapsed: RwSignal<bool>,
    /// The epic or sprint opened from the Backlogs column, as (repository,
    /// name), shown as a column of its own beside it (ticket 109). One at
    /// a time.
    pub open_epic: RwSignal<Option<(String, String)>>,
    /// The ticket last opened, as (repository, file name) (ticket 163): its
    /// card is outlined and scrolled into view, so after jumping to it from
    /// My work you can see where it is. Kept until another ticket opens.
    pub focus: RwSignal<Option<(String, String)>>,
    /// Whether the focused card still has to be scrolled to: once per
    /// opening, so a reload doesn't keep pulling the board back to it.
    pub focus_scroll: RwSignal<bool>,
    /// The ticket being dragged: its file name and where it came from.
    pub dragging: RwSignal<Option<(String, Place)>>,
    /// How every column and epic is ordered (the Sort dropdown).
    pub sort: RwSignal<SortOrder>,
    /// Whether the sort was picked in the dropdown (ticket 230); until it
    /// is, it follows the board's default.
    pub sort_chosen: RwSignal<bool>,
    /// The search box: only matching cards are shown while it has text.
    pub search: RwSignal<String>,
    /// Which repository's *board* new tickets, epics and WIP limits go to;
    /// `None` is the project's own. No longer set by the filter (ticket 101).
    pub repo: RwSignal<Option<String>>,
    /// The repository filter (ticket 101): only tickets *about* this
    /// repository (their `repository` field), or all (`None`).
    pub about: RwSignal<Option<String>>,
    /// The user filter: whose tickets to show.
    pub user: RwSignal<Assigned>,
    /// The user's colour scheme (Preferences → Appearance).
    pub appearance: RwSignal<Appearance>,
    /// The board's zoom, in percent (ticket 024).
    pub zoom: RwSignal<u32>,
    /// Whether the desktop is in dark mode, for "follow the system".
    pub system_dark: RwSignal<bool>,
    /// What git did with the last ticket change.
    pub sync: RwSignal<Option<SyncReport>>,
    /// Bumped whenever the board is refreshed because the files changed (or
    /// the user asked): an open editor checks its ticket then (ticket 091).
    pub disk_tick: RwSignal<u64>,
    /// The document open in Documents (ticket 129): repository and path.
    pub document: RwSignal<crate::documents::Open>,
    /// What the Documents view shows (ticket 168): one folder of the
    /// project's own or the shared repository, set by the landing page's
    /// buttons; `None` is everything.
    pub doc_scope: RwSignal<Option<rite_ui_model::DocScope>>,
    /// The idea open in Ideas, by its path (ticket 186: in `Ctx` so the
    /// screen note can say which).
    pub idea: RwSignal<Option<String>>,
}

impl Ctx {
    pub fn get() -> Ctx {
        expect_context::<Ctx>()
    }

    /// Re-reads the board from disk.
    pub fn reload(self) {
        spawn_local(async move {
            match api::load_board().await {
                Ok(board) => self.board.set(Some(board)),
                Err(e) => self.error.set(Some(e)),
            }
        });
    }

    /// Opens a ticket in the editor.
    pub fn open_ticket(self, place: Place, file: String) {
        self.show_card(&place, &file);
        spawn_local(async move {
            match api::open_ticket(&place, &file).await {
                Ok(doc) => self.overlay.set(Some(Overlay::Editor(doc))),
                Err(e) => self.error.set(Some(e)),
            }
        });
    }

    /// Goes to a ticket's card without opening it (ticket 214: a node in
    /// the tree): the board is shown, the card outlined and scrolled to.
    pub fn show_card(self, place: &Place, file: &str) {
        self.view.set(View::Board);
        // Where it is on the board (ticket 163): its card is outlined, and
        // one filed in an epic or sprint has that group's column opened.
        // A folded column is unfolded, so the card is there to see.
        self.focus.set(Some((place.repo.clone(), file.to_string())));
        self.focus_scroll.set(true);
        if place.column == Column::Backlog
            && let Some(group) = &place.epic
        {
            self.open_epic
                .set(Some((place.repo.clone(), group.clone())));
        } else {
            self.collapsed.update(|c| {
                c.remove(&place.column);
            });
        }
    }

    /// The cards to show from a list: those matching the search, in the
    /// order chosen in the Sort dropdown.
    pub fn shown(self, mut list: Vec<TicketSummary>) -> Vec<TicketSummary> {
        list.retain(|t| self.in_view(t));
        self.sort.get().sort(&mut list);
        list
    }

    /// The board's own sort (ticket 230): Automatic where auto priority is
    /// on, else priority then oldest.
    pub fn default_sort(self) -> SortOrder {
        if self
            .board
            .with(|b| b.as_ref().is_some_and(|b| b.auto_priority))
        {
            SortOrder::Automatic
        } else {
            SortOrder::Default
        }
    }

    /// Whether a card passes the repository and user filters and the search.
    pub fn in_view(self, t: &TicketSummary) -> bool {
        let repo_ok = self
            .about
            .with(|r| r.as_ref().is_none_or(|r| *r == t.repository));
        repo_ok && self.user.with(|u| u.matches(t)) && self.search.with(|q| t.matches(q))
    }

    /// Whether any filter differs from its default: repository, user, sort
    /// or search.
    pub fn filters_changed(self) -> bool {
        self.about.with(Option::is_some)
            || self.user.with(|u| *u != Assigned::Anyone)
            || self.sort.get() != self.default_sort()
            || self.searching()
    }

    /// Puts every filter back to its default: all repositories, all users,
    /// the default order, and no search.
    pub fn reset_filters(self) {
        self.about.set(None);
        self.user.set(Assigned::Anyone);
        self.sort_chosen.set(false);
        self.sort.set(self.default_sort());
        self.search.set(String::new());
    }

    /// Whether a search or the user filter is hiding some tickets, so epics
    /// left with nothing to show can step aside.
    pub fn narrowed(self) -> bool {
        self.searching() || self.user.with(|u| *u != Assigned::Anyone)
    }

    /// Whether the board spans more than one available repository.
    pub fn multi_repo(self) -> bool {
        self.board.with(|b| {
            b.as_ref()
                .is_some_and(|b| b.repos.iter().filter(|r| r.available).count() > 1)
        })
    }

    /// Whether a search is narrowing the board.
    pub fn searching(self) -> bool {
        self.search.with(|q| !q.trim().is_empty())
    }

    /// Re-reads the board because something outside the page may have
    /// changed it, and lets an open editor check its own ticket.
    pub fn refresh(self) {
        self.reload();
        self.disk_tick.update(|tick| *tick += 1);
    }

    /// Shows what happened when a change was committed and pushed.
    pub fn report(self, report: SyncReport) {
        self.sync.set(Some(report));
    }

    pub fn close_overlay(self) {
        self.overlay.set(None);
    }
}

#[component]
pub fn App() -> impl IntoView {
    let ctx = Ctx {
        board: RwSignal::new(None),
        view: RwSignal::new(View::Home),
        offline: RwSignal::new(None),
        update_offer: RwSignal::new(None),
        home: RwSignal::new(None),
        error: RwSignal::new(None),
        overlay: RwSignal::new(None),
        collapsed: RwSignal::new(BTreeSet::new()),
        epics_collapsed: RwSignal::new(false),
        open_epic: RwSignal::new(None),
        focus: RwSignal::new(None),
        focus_scroll: RwSignal::new(false),
        dragging: RwSignal::new(None),
        sort: RwSignal::new(SortOrder::default()),
        sort_chosen: RwSignal::new(false),
        search: RwSignal::new(String::new()),
        repo: RwSignal::new(None),
        about: RwSignal::new(None),
        user: RwSignal::new(Assigned::Anyone),
        sync: RwSignal::new(None),
        appearance: RwSignal::new(Appearance::default()),
        zoom: RwSignal::new(rite_ui_model::zoom::DEFAULT),
        system_dark: RwSignal::new(false),
        disk_tick: RwSignal::new(0),
        document: RwSignal::new(None),
        doc_scope: RwSignal::new(None),
        idea: RwSignal::new(None),
    };
    provide_context(ctx);
    // The sort follows the board's default (ticket 230: Automatic with auto
    // priority on) until one is picked in the dropdown.
    Effect::new(move |_| {
        let wanted = ctx.default_sort();
        if !ctx.sort_chosen.get_untracked() && ctx.sort.get_untracked() != wanted {
            ctx.sort.set(wanted);
        }
    });
    // A document belongs to the project it was opened in (ticket 175):
    // whatever switches project (the landing page's picker or buttons, the
    // header's switcher, a Projects row), the open document goes with it,
    // so the reader never re-reads another project's document.
    let shown = Memo::new(move |_| ctx.board.with(|b| b.as_ref().map(|b| b.tasks_dir.clone())));
    Effect::new(move |before: Option<Option<String>>| {
        let now = shown.get();
        if before.is_some_and(|b| b != now) {
            ctx.document.set(None);
            ctx.idea.set(None);
        }
        now
    });
    report_screen(ctx);
    ctx.reload();
    use_colour_scheme(ctx);
    watch_disk(ctx);
    refresh_keys(ctx);
    zoom_keys(ctx);
    crate::home::load_home(ctx.home);
    let on_home = move || ctx.view.get() == View::Home;
    let go_home = move |_| {
        ctx.overlay.set(None);
        ctx.view.set(View::Home);
        crate::home::load_home(ctx.home);
    };

    let tasks_dir = move || ctx.board.with(|b| b.as_ref().map(|b| b.tasks_dir.clone()));
    // Changes only when another project opens, not on every reload.
    let project_dir = Memo::new(move |_| tasks_dir());
    // The project's own name (ticket 087), until a board is open.
    let project = move || {
        ctx.board.with(|b| {
            b.as_ref()
                .map(|b| rite_ui_model::shown_name(&b.project, &b.shown).to_string())
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| "KanRite".to_string())
        })
    };

    view! {
        <div class="flex h-screen flex-col bg-white text-slate-900 dark:bg-slate-900 dark:text-slate-100">
            <crate::update_bar::UpdateBar />
            <OfflineBar />
            <header class="flex items-center gap-4 border-b border-slate-200 bg-white px-4 py-2 dark:border-slate-700 dark:bg-slate-800">
                // The landing page has no project of its own; a board offers
                // the way back to it (ticket 020).
                <Show
                    when=on_home
                    fallback=move || {
                        view! {
                            <button
                                class="whitespace-nowrap rounded border border-slate-300 px-2.5 py-1.5 text-sm hover:bg-slate-100 dark:border-slate-600 dark:hover:bg-slate-700"
                                title="Back to My work and your projects"
                                on:click=go_home
                            >
                                "⌂ Home"
                            </button>
                            <ProjectSwitcher project=project />
                            <ViewTabs />
                            <ActiveSprints />
                        }
                    }
                >
                    // The version (ticket 240): the page is built with riteui,
                    // so its own is riteui's.
                    <span class="flex items-baseline gap-1.5 whitespace-nowrap">
                        <span class="text-lg font-semibold">"KanRite"</span>
                        <crate::update_bar::VersionCheck />
                    </span>
                </Show>
                <span class="min-w-0 flex-1 truncate text-xs text-slate-500" title=tasks_dir>
                    {move || {
                        if on_home() {
                            String::new()
                        } else {
                            tasks_dir().unwrap_or_else(|| "No tasks directory open".into())
                        }
                    }}
                </span>
                <SyncStatus />
                <button
                    class="rounded border border-slate-300 px-2.5 py-1.5 text-sm hover:bg-slate-100 dark:border-slate-600 dark:hover:bg-slate-700"
                    title="Refresh from disk (F5)"
                    aria-label="Refresh"
                    on:click=move |_| {
                        if on_home() {
                            crate::home::load_home(ctx.home);
                        } else {
                            ctx.refresh();
                        }
                    }
                >
                    "↻"
                </button>
                // The current project and its documents, beside Preferences
                // (ticket 168).
                <Show when=on_home>
                    <crate::home::HomeBar />
                </Show>
                // New ticket, Preferences and Admin, with the open-ticket count
                // underneath (ticket 349): the count is of the whole project, so
                // it sits with the project's own buttons.
                <div class="flex shrink-0 flex-col items-end gap-1">
                    <div class="flex items-center gap-4">
                    // Tickets are made on the board; documents have their own
                    // New document (ticket 178).
                    <Show when=move || ctx.view.get() == View::Board>
                        <button
                            class="whitespace-nowrap rounded bg-accent-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-accent-500 disabled:opacity-50"
                            disabled=move || ctx.board.with(|b| b.as_ref().is_none_or(|b| b.archived))
                            on:click=move |_| ctx.overlay.set(Some(Overlay::NewTicket(None)))
                        >
                            "New ticket"
                        </button>
                    </Show>
                    <button
                        class="rounded border border-slate-300 px-3 py-1.5 text-sm hover:bg-slate-100 dark:border-slate-600 dark:hover:bg-slate-700"
                        title="Your own settings on this machine: colours, zoom, the board to open"
                        on:click=move |_| ctx.overlay.set(Some(Overlay::Preferences))
                    >
                        "Preferences"
                    </button>
                    // Only for the project's admins (ticket 149): its admins list,
                    // or admin access on GitHub, or open when rite can't tell.
                    <Show when=move || {
                        !on_home() && ctx.board.with(|b| b.as_ref().is_none_or(|b| b.admin.allowed))
                    }>
                        <button
                            class="rounded border border-slate-300 px-3 py-1.5 text-sm hover:bg-slate-100 dark:border-slate-600 dark:hover:bg-slate-700"
                            title="The project's settings, shared with the team"
                            on:click=move |_| ctx.overlay.set(Some(Overlay::Admin))
                        >
                            "Admin"
                        </button>
                    </Show>
                    </div>
                    <crate::counts::BoardCounts />
                </div>
            </header>

            <Show when=move || ctx.view.get() == View::Board>
                <FilterBar />
            </Show>

            <Show when=move || ctx.error.with(Option::is_some)>
                <div
                    role="alert"
                    class="flex items-center gap-3 bg-red-100 px-4 py-2 text-sm text-red-800 dark:bg-red-950 dark:text-red-200"
                >
                    <span class="flex-1">{move || ctx.error.get().unwrap_or_default()}</span>
                    <button class="underline" on:click=move |_| ctx.error.set(None)>
                        "Dismiss"
                    </button>
                </div>
            </Show>

            // An archived project only reads (ticket 157).
            <Show when=move || {
                !on_home() && ctx.board.with(|b| b.as_ref().is_some_and(|b| b.archived))
            }>
                <div
                    role="status"
                    class="flex items-center gap-3 bg-slate-200 px-4 py-2 text-sm text-slate-800 dark:bg-slate-700 dark:text-slate-100"
                >
                    <span class="flex-1">
                        "This project is archived: you can read it, but nothing can be changed until it's unarchived."
                    </span>
                    <Show when=move || ctx.board.with(|b| b.as_ref().is_some_and(|b| b.admin.allowed))>
                        <button class="underline" on:click=move |_| ctx.overlay.set(Some(Overlay::Admin))>
                            "Unarchive in Admin"
                        </button>
                    </Show>
                </div>
            </Show>

            <Show when=move || ctx.sync.with(|s| s.as_ref().is_some_and(|s| s.warning.is_some()))>
                <div
                    role="alert"
                    class="flex items-center gap-3 bg-amber-100 px-4 py-2 text-sm text-amber-900 dark:bg-amber-950 dark:text-amber-100"
                >
                    <span class="flex-1">
                        {move || ctx.sync.with(|s| s.as_ref().and_then(|s| s.warning.clone()).unwrap_or_default())}
                    </span>
                    <button class="underline" on:click=move |_| ctx.sync.set(None)>
                        "Dismiss"
                    </button>
                </div>
            </Show>

            <main class="min-h-0 flex-1">
                {move || match ctx.view.get() {
                    View::Home => view! { <crate::home::HomeView home=ctx.home /> }.into_any(),
                    View::Board => view! { <BoardView /> }.into_any(),
                    // Started afresh for each project (ticket 129).
                    View::Documents => {
                        project_dir.track();
                        view! { <crate::documents::DocumentsView /> }.into_any()
                    }
                    View::Ideas => {
                        project_dir.track();
                        view! { <crate::ideas::IdeasView /> }.into_any()
                    }
                    View::Roadmaps => {
                        project_dir.track();
                        view! { <crate::roadmap::RoadmapsView /> }.into_any()
                    }
                }}
            </main>

            // The terminal and AI panel, on the board (ticket 185).
            <crate::panel::BottomPanel project=project_dir />

            {move || match ctx.overlay.get() {
                Some(Overlay::Editor(doc)) => view! { <Editor doc /> }.into_any(),
                Some(Overlay::NewTicket(link)) => view! { <NewTicketDialog link /> }.into_any(),
                Some(Overlay::NewEpic) => view! { <NewEpicDialog /> }.into_any(),
                Some(Overlay::NewSprint) => view! { <NewSprintDialog /> }.into_any(),
                Some(Overlay::Admin) => view! { <AdminPanel /> }.into_any(),
                Some(Overlay::Preferences) => view! { <PreferencesPanel /> }.into_any(),
                Some(Overlay::CloneRepo(name)) => view! { <CloneRepoDialog name /> }.into_any(),
                Some(Overlay::NewRepo(ticket)) => {
                    view! { <crate::dialog::NewRepoDialog ticket /> }.into_any()
                }
                Some(Overlay::Tree { place, file, children }) => {
                    view! { <crate::tree::TreePanel place file children /> }.into_any()
                }
                Some(Overlay::WhatsNew(url)) => {
                    view! { <crate::update_bar::WhatsNewDialog url /> }.into_any()
                }
                Some(Overlay::OverLimit { file, from, to, reason }) => {
                    view! { <OverLimitDialog file from to reason /> }.into_any()
                }
                None => ().into_any(),
            }}
        </div>
    }
}

/// The sprints running now (ticket 165), beside the tabs: each a badge
/// with its name and length that opens its column on the board. Nothing
/// when no sprint is started.
#[component]
fn ActiveSprints() -> impl IntoView {
    use rite_ui_model::epic::GroupKind;
    let ctx = Ctx::get();
    // The open project's running sprints, only while its board is on
    // screen (ticket 183): never on Home, Documents or Ideas, and keyed by
    // project so another project's sprint can't linger.
    let running = move || {
        if ctx.view.get() != View::Board {
            return Vec::new();
        }
        ctx.board.with(|b| {
            b.as_ref()
                .map(|b| {
                    b.epics
                        .iter()
                        .filter(|e| e.kind == GroupKind::Sprint && e.started)
                        .map(|e| (b.tasks_dir.clone(), e.repo.clone(), e.name.clone(), e.days))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    };
    // In a container of its own, so the badges go with it when the header
    // changes, rather than staying behind as loose siblings.
    view! {
        <div class="flex shrink-0 items-center gap-2">
        <For each=running key=|s| s.clone() let:sprint>
            {
                let (_, repo, name, days) = sprint;
                let open = (repo.clone(), name.clone());
                let length = if days == 0 { String::new() } else { format!(" · {days} days") };
                view! {
                    <button
                        type="button"
                        class="flex shrink-0 items-center gap-1.5 whitespace-nowrap rounded border border-lime-600 bg-lime-50/70 px-2 py-1 text-sm hover:bg-lime-100 dark:bg-lime-950/40 dark:hover:bg-lime-900/60"
                        title=format!("Sprint {name} is running{length}. Open it on the board")
                        on:click=move |_| {
                            ctx.view.set(View::Board);
                            ctx.open_epic.set(Some(open.clone()));
                        }
                    >
                        <span class="rounded bg-lime-700 px-1.5 text-[10px] font-semibold uppercase tracking-wide text-white">
                            "Sprint"
                        </span>
                        <span class="font-medium">{name.clone()}</span>
                    </button>
                }
            }
        </For>
        </div>
    }
}

/// The way back to the board from Documents (tickets 129, 168).
#[component]
fn ViewTabs() -> impl IntoView {
    // Documents is reached from the landing page's buttons (ticket 168), so
    // the board has no tabs: only the way back while documents are open.
    let ctx = Ctx::get();
    view! {
        <Show when=move || matches!(ctx.view.get(), View::Documents | View::Ideas | View::Roadmaps)>
            <button
                type="button"
                class="rounded border border-slate-300 px-2.5 py-1.5 text-sm hover:bg-slate-100 dark:border-slate-600 dark:hover:bg-slate-700"
                title="Back to this project's board"
                on:click=move |_| ctx.view.set(View::Board)
            >
                "Board"
            </button>
        </Show>
        // Back to the ideas from one opened in Documents (ticket 131).
        <Show when=move || ctx.view.get() == View::Documents>
            <button
                type="button"
                class="rounded border border-slate-300 px-2.5 py-1.5 text-sm hover:bg-slate-100 dark:border-slate-600 dark:hover:bg-slate-700"
                title="This project's ideas"
                on:click=move |_| ctx.view.set(View::Ideas)
            >
                "Ideas"
            </button>
        </Show>
    }
}

/// A centred modal over a dimmed board. Escape closes it; a click on the
/// backdrop deliberately does not, so a stray click cannot discard an edit.
/// Follows the files (ticket 091): asks for the board's fingerprint every
/// second and refreshes when it changes, so changes made anywhere else — a
/// terminal, an editor, a pull or a checkout — show up without a click. A
/// change seen mid-drag waits for the drop. The first answer, and the first
/// after switching project, is just the new baseline.
/// Keeps rite-ui told what's on screen (ticket 186), so an AI assistant
/// using `rite mcp` knows what "this ticket" is: the screen, the open
/// ticket, document, idea or epic column, and the board's filters. Sent
/// only when it changes.
fn report_screen(ctx: Ctx) {
    use rite_ui_model::screen::{ScreenPlace, ScreenView};
    let view = Memo::new(move |_| {
        let screen = ctx.view.get();
        // Re-sent when the project changes, so the new project gets a note.
        let _ = ctx.board.with(|b| b.as_ref().map(|b| b.tasks_dir.clone()));
        let on_board = screen == View::Board;
        ScreenView {
            screen: match screen {
                View::Home => "home",
                View::Board => "board",
                View::Documents => "documents",
                View::Ideas => "ideas",
                View::Roadmaps => "roadmaps",
            }
            .to_string(),
            ticket: ctx.overlay.with(|o| match o {
                Some(Overlay::Editor(t)) => Some(ScreenPlace::new(&t.repo, &t.file)),
                _ => None,
            }),
            document: (screen == View::Documents)
                .then(|| ctx.document.get())
                .flatten()
                .map(|(repo, path)| ScreenPlace::new(&repo, &path)),
            idea: (screen == View::Ideas)
                .then(|| ctx.idea.get())
                .flatten()
                .map(|path| ScreenPlace::new("", &path)),
            epic: on_board
                .then(|| ctx.open_epic.get())
                .flatten()
                .map(|(repo, name)| ScreenPlace::new(&repo, &name)),
            search: if on_board {
                ctx.search.get().trim().to_string()
            } else {
                String::new()
            },
            about: on_board.then(|| ctx.about.get()).flatten(),
            assignee: match on_board.then(|| ctx.user.get()) {
                Some(Assigned::User(name)) => name,
                Some(Assigned::Nobody) => "(nobody)".to_string(),
                _ => String::new(),
            },
        }
    });
    Effect::new(move |_| {
        let view = view.get();
        spawn_local(async move {
            // Best effort: a missing note only means the assistant asks.
            let _ = api::set_screen(&view).await;
        });
    });
}

fn watch_disk(ctx: Ctx) {
    let last = StoredValue::new(None::<String>);
    let pending = StoredValue::new(false);
    let poll = move || {
        spawn_local(async move {
            if let Ok(now) = api::board_fingerprint().await {
                let before = last.get_value();
                if before.as_ref().is_some_and(|before| *before != now) {
                    pending.set_value(true);
                }
                last.set_value(Some(now));
            }
            if pending.get_value() && ctx.dragging.with_untracked(Option::is_none) {
                pending.set_value(false);
                ctx.refresh();
            }
        });
    };
    if let Err(e) = set_interval_with_handle(poll, std::time::Duration::from_secs(1)) {
        ctx.error
            .set(Some(format!("can't watch the board for changes: {e:?}")));
    }
    // Teammates' work (ticket 102): fetch and fast-forward every 30s; the
    // fingerprint poll above then sees the files change. Warnings are
    // left out of the way — every 30s would only nag — but origin being out
    // of reach shows as a bar (ticket 197).
    let sync = move || {
        spawn_local(async move {
            if let Ok(status) = api::sync_board().await {
                ctx.offline.set(status.offline);
            }
        });
    };
    let _ = set_interval_with_handle(sync, std::time::Duration::from_secs(30));
}

/// F5 and Ctrl+R refresh the board rather than reloading the whole page,
/// which would throw away the filters, the sort and anything being typed.
fn refresh_keys(ctx: Ctx) {
    let _keys = window_event_listener(leptos::ev::keydown, move |ev| {
        // Ctrl+R is a shell's reverse search (ticket 185).
        if in_terminal(&ev) {
            return;
        }
        let reload_key = ev.key() == "F5"
            || ((ev.ctrl_key() || ev.meta_key()) && ev.key().eq_ignore_ascii_case("r"));
        if reload_key {
            ev.prevent_default();
            ctx.refresh();
        }
    });
}

/// Whether a key was pressed in the bottom panel's terminal, which keeps
/// keys like Ctrl+R and Ctrl+0 for its program (ticket 185).
fn in_terminal(ev: &web_sys::KeyboardEvent) -> bool {
    use wasm_bindgen::JsCast;
    ev.target()
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        .and_then(|el| el.closest(".term").ok().flatten())
        .is_some()
}

/// Ctrl + and Ctrl - zoom the board a step, Ctrl 0 back to 100% (ticket 024),
/// remembered on this machine. The saved size is applied at start.
fn zoom_keys(ctx: Ctx) {
    spawn_local(async move {
        if let Ok(saved) = api::zoom().await {
            ctx.zoom.set(saved);
        }
    });
    let _keys = window_event_listener(leptos::ev::keydown, move |ev| {
        if !(ev.ctrl_key() || ev.meta_key()) || in_terminal(&ev) {
            return;
        }
        use rite_ui_model::zoom;
        let now = ctx.zoom.get_untracked();
        let to = match ev.key().as_str() {
            "+" | "=" => zoom::step(now, true),
            "-" | "_" => zoom::step(now, false),
            "0" => zoom::DEFAULT,
            _ => return,
        };
        ev.prevent_default();
        zoom_to(ctx, to);
    });
}

/// Zooms the board to `to` percent and remembers it (ticket 024): the keys
/// and Preferences both come here.
pub fn zoom_to(ctx: Ctx, to: u32) {
    if to == ctx.zoom.get_untracked() {
        return;
    }
    // Set before the call returns, so a quick second press steps on from
    // here rather than from the old size.
    ctx.zoom.set(to);
    spawn_local(async move {
        if let Err(e) = api::set_zoom(to).await {
            ctx.error.set(Some(e));
        }
    });
}

#[component]
pub fn Modal(
    title: String,
    #[prop(default = "max-w-3xl")] width: &'static str,
    children: Children,
) -> impl IntoView {
    let ctx = Ctx::get();
    let label = title.clone();
    // Listen on the window: focus is not reliably inside the dialog (a newly
    // inserted `autofocus` field is not focused by WebKit), so a handler on
    // the dialog itself would miss the key.
    let escape = window_event_listener(leptos::ev::keydown, move |ev| {
        if ev.key() == "Escape" {
            ctx.close_overlay();
        }
    });
    on_cleanup(move || escape.remove());
    view! {
        <div class="fixed inset-0 z-10 flex items-center justify-center bg-black/40 p-6">
            <section
                role="dialog"
                aria-modal="true"
                aria-label=label
                class=format!(
                    "flex max-h-full w-full {width} flex-col gap-3 rounded-lg bg-white p-4 shadow-xl dark:bg-slate-800",
                )
            >
                <h2 class="text-base font-semibold">{title}</h2>
                {children()}
            </section>
        </div>
    }
}

/// A Close button at the foot of a dialog that has no action of its own to
/// end on (ticket 159): Preferences and Admin save as you go, and Escape
/// alone left a mouse user with no visible way out.
#[component]
pub fn ModalClose() -> impl IntoView {
    let ctx = Ctx::get();
    view! {
        <div class="flex justify-end">
            <button
                class="rounded border border-slate-300 px-3 py-1.5 text-sm hover:bg-slate-100 dark:border-slate-600 dark:hover:bg-slate-700"
                on:click=move |_| ctx.close_overlay()
            >
                "Close"
            </button>
        </div>
    }
}

/// The last commit the board made, in the header: "Pushed …" once it is on
/// origin, "Committed …" if it only got as far as the local repository.
#[component]
fn SyncStatus() -> impl IntoView {
    let ctx = Ctx::get();
    move || {
        ctx.sync.with(|s| {
            s.as_ref().filter(|s| s.committed).map(|s| {
                let (icon, tone, verb) = if s.pushed {
                    ("✓", "text-emerald-700 dark:text-emerald-300", "Pushed")
                } else {
                    ("!", "text-amber-700 dark:text-amber-300", "Committed")
                };
                view! {
                    <span role="status" class=format!("max-w-md truncate text-xs {tone}") title=s.message.clone()>
                        {format!("{icon} {verb} {}", s.message)}
                    </span>
                }
            })
        })
    }
}

/// Searches the tickets on the board (archived ones are not loaded). Escape
/// or the × clears it. `word` and `-word` search the text; `#TAG` and
/// `-#TAG` search the tags (ticket 123), and typing one offers the tags that
/// start with what's been typed: arrows and Enter (or a click) finish it.
#[component]
fn SearchBox() -> impl IntoView {
    let ctx = Ctx::get();
    let open = RwSignal::new(false);
    let cursor = RwSignal::new(0usize);
    // The tags starting with the tag term being typed, sorted, with how
    // many tickets carry each.
    let options = move || {
        let Some((_, typed)) = rite_ui_model::tag_being_typed(&ctx.search.get()) else {
            return Vec::new();
        };
        let mut found = ctx.board.with(|b| {
            b.as_ref()
                .map(|b| {
                    b.tags
                        .iter()
                        .filter(|t| t.name.to_uppercase().starts_with(&typed))
                        .map(|t| (t.name.to_uppercase(), t.count))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        });
        found.sort();
        found.dedup_by(|a, b| a.0 == b.0);
        found
    };
    let choose = move |tag: String| {
        ctx.search
            .update(|q| *q = rite_ui_model::complete_tag(q, &tag));
        cursor.set(0);
    };
    view! {
        <div class="relative flex items-center">
            <span aria-hidden="true" class="pointer-events-none absolute left-2.5 text-sm text-slate-400">
                "⌕"
            </span>
            <input
                type="search"
                placeholder="Search tickets  -word  #TAG  -#TAG"
                aria-label="Search tickets. Start a word with minus to leave it out; start with hash to search tags."
                role="combobox"
                aria-expanded=move || (open.get() && !options().is_empty()).to_string()
                autocomplete="off"
                title="word: in the text.  -word: leave out.  #TAG: tagged (starts with).  -#TAG: not tagged."
                class="w-72 rounded border border-slate-300 bg-white py-1.5 pr-7 pl-7 text-sm text-slate-900 placeholder:text-slate-400 focus:outline-2 focus:outline-accent-500 dark:border-slate-600 dark:bg-slate-900 dark:text-slate-100 [&::-webkit-search-cancel-button]:hidden"
                prop:value=move || ctx.search.get()
                on:focus=move |_| open.set(true)
                on:blur=move |_| open.set(false)
                on:input=move |ev| {
                    ctx.search.set(event_target_value(&ev));
                    cursor.set(0);
                    open.set(true);
                }
                on:keydown=move |ev| {
                    let shown = options();
                    let listing = open.get_untracked() && !shown.is_empty();
                    match ev.key().as_str() {
                        "ArrowDown" if listing => {
                            ev.prevent_default();
                            cursor.update(|c| *c = (*c + 1).min(shown.len() - 1));
                        }
                        "ArrowUp" if listing => {
                            ev.prevent_default();
                            cursor.update(|c| *c = c.saturating_sub(1));
                        }
                        "Enter" | "Tab" if listing => {
                            if let Some((tag, _)) = shown.get(cursor.get_untracked()) {
                                ev.prevent_default();
                                choose(tag.clone());
                            }
                        }
                        "Escape" if listing => {
                            ev.prevent_default();
                            ev.stop_propagation();
                            open.set(false);
                        }
                        "Escape" => ctx.search.set(String::new()),
                        _ => {}
                    }
                }
            />
            <Show when=move || ctx.searching()>
                <button
                    type="button"
                    title="Clear the search"
                    aria-label="Clear the search"
                    class="absolute right-1.5 rounded px-1 text-sm text-slate-400 hover:text-slate-700 dark:hover:text-slate-200"
                    on:click=move |_| ctx.search.set(String::new())
                >
                    "×"
                </button>
            </Show>
            <Show when=move || open.get() && !options().is_empty()>
                <ul
                    role="listbox"
                    aria-label="Tags"
                    class="absolute top-full z-20 mt-1 max-h-56 w-full overflow-y-auto rounded border border-slate-300 bg-white py-1 text-sm shadow-lg dark:border-slate-600 dark:bg-slate-800"
                >
                    {move || {
                        options()
                            .into_iter()
                            .enumerate()
                            .map(|(i, (tag, count))| {
                                let chosen = tag.clone();
                                view! {
                                    <li
                                        role="option"
                                        aria-selected=move || (cursor.get() == i).to_string()
                                        class=move || {
                                            let base = "flex cursor-pointer justify-between gap-2 px-2 py-1";
                                            if cursor.get() == i {
                                                format!("{base} bg-accent-600 text-white")
                                            } else {
                                                format!("{base} hover:bg-slate-100 dark:hover:bg-slate-700")
                                            }
                                        }
                                        // Before the input's blur, so the click lands.
                                        on:mousedown=move |ev| {
                                            ev.prevent_default();
                                            choose(chosen.clone());
                                        }
                                    >
                                        <span>{tag}</span>
                                        <span class="opacity-60">{count}</span>
                                    </li>
                                }
                            })
                            .collect_view()
                    }}
                </ul>
            </Show>
        </div>
    }
}

/// The repository filter, at the far left: all repositories the project
/// spans, or one. Same accent-coloured pill as Sort (and, like it, painted itself:
/// WebKit's native select box would hide white text).
#[component]
fn RepoFilter() -> impl IntoView {
    let ctx = Ctx::get();
    let repos = move || {
        ctx.board.with(|b| {
            b.as_ref()
                .map(|b| b.ticket_repos.clone())
                .unwrap_or_default()
        })
    };
    let shown = move || ctx.about.get().unwrap_or_else(|| "All".to_string());
    // A fixed width (tickets 096/100): a long repository name is cut short
    // here, in full on hover, and in full in the opened list, so it never
    // widens the filter or pushes the others onto another row.
    view! {
        <label
            class="group relative flex w-64 shrink-0 cursor-pointer items-center gap-1 whitespace-nowrap rounded bg-accent-600 py-1.5 pr-2 pl-3 text-sm font-medium text-white hover:bg-accent-500 focus-within:outline-2 focus-within:outline-offset-2 focus-within:outline-accent-500"
            title=move || format!("Repository: {} — show only the tickets about one repository", shown())
        >
            "Repository:"
            <select
                class="min-w-0 flex-1 cursor-pointer appearance-none truncate border-0 bg-accent-600 pr-5 font-medium text-white group-hover:bg-accent-500 focus:outline-none"
                prop:value=move || ctx.about.get().unwrap_or_default()
                on:change=move |ev| {
                    let value = event_target_value(&ev);
                    // One that isn't cloned here offers to clone it (ticket
                    // 108); the filter stays as it was until that's done.
                    let missing = repos().iter().any(|r| r.name == value && !r.available);
                    if missing {
                        let select: web_sys::HtmlSelectElement = event_target(&ev);
                        select.set_value(&ctx.about.get_untracked().unwrap_or_default());
                        ctx.overlay.set(Some(Overlay::CloneRepo(value)));
                    } else {
                        ctx.about.set((!value.is_empty()).then_some(value));
                    }
                }
            >
                <option class="bg-accent-600 text-white" value="">
                    "All"
                </option>
                {move || {
                    repos()
                        .into_iter()
                        .map(|r| {
                            let label = if r.available {
                                r.name.clone()
                            } else {
                                format!("{} (not cloned — clone…)", r.name)
                            };
                            view! {
                                <option class="bg-accent-600 text-white" value=r.name.clone()>
                                    {label}
                                </option>
                            }
                        })
                        .collect_view()
                }}
            </select>
            <span aria-hidden="true" class="pointer-events-none absolute right-2.5 text-xs text-white">
                "▾"
            </span>
        </label>
    }
}

/// The user filter, right of the repository filter: everyone's tickets, one
/// user's, or the unassigned ones. Users are whoever has tickets assigned.
#[component]
fn UserFilter() -> impl IntoView {
    let ctx = Ctx::get();
    let users = move || {
        ctx.board
            .with(|b| b.as_ref().map(Board::assignees).unwrap_or_default())
    };
    view! {
        <label
            class="group relative flex cursor-pointer items-center gap-1 whitespace-nowrap rounded bg-accent-600 py-1.5 pr-2 pl-3 text-sm font-medium text-white hover:bg-accent-500 focus-within:outline-2 focus-within:outline-offset-2 focus-within:outline-accent-500"
            title="Show the tickets assigned to one user, or everyone's"
        >
            "User:"
            <select
                class="cursor-pointer appearance-none border-0 bg-accent-600 pr-5 font-medium text-white group-hover:bg-accent-500 focus:outline-none"
                prop:value=move || ctx.user.with(Assigned::key)
                on:change=move |ev| ctx.user.set(Assigned::from_key(&event_target_value(&ev)))
            >
                <option class="bg-accent-600 text-white" value=Assigned::Anyone.key()>
                    "All"
                </option>
                {move || {
                    users()
                        .into_iter()
                        .map(|name| {
                            view! {
                                <option class="bg-accent-600 text-white" value=name.clone()>
                                    {name.clone()}
                                </option>
                            }
                        })
                        .collect_view()
                }}
                <option class="bg-accent-600 text-white" value=Assigned::Nobody.key()>
                    "Unassigned"
                </option>
            </select>
            <span aria-hidden="true" class="pointer-events-none absolute right-2.5 text-xs text-white">
                "▾"
            </span>
        </label>
    }
}

/// The row of filters across the top of the board: which repository and
/// whose work, a search, the order, and a reset back to the defaults.
#[component]
fn FilterBar() -> impl IntoView {
    let ctx = Ctx::get();
    view! {
        <nav
            aria-label="Filters"
            class="flex flex-wrap items-center gap-3 border-b border-slate-200 bg-slate-50 px-4 py-2 dark:border-slate-700 dark:bg-slate-800/60"
        >
            <Marked active=Signal::derive(move || ctx.repo.with(Option::is_some))>
                <RepoFilter />
            </Marked>
            <Marked active=Signal::derive(move || ctx.user.with(|u| *u != Assigned::Anyone))>
                <UserFilter />
            </Marked>
            <Marked active=Signal::derive(move || ctx.searching())>
                <SearchBox />
            </Marked>
            <Marked active=Signal::derive(move || ctx.sort.get() != ctx.default_sort())>
            // WebKit draws a native (white) box for a <select> unless its
            // appearance is turned off, so the select is unstyled and
            // painted in the accent itself, with its own arrow.
            <label
                class="group relative flex cursor-pointer items-center gap-1 whitespace-nowrap rounded bg-accent-600 py-1.5 pr-2 pl-3 text-sm font-medium text-white hover:bg-accent-500 focus-within:outline-2 focus-within:outline-offset-2 focus-within:outline-accent-500"
                title="Order of the tickets in every column"
            >
                "Sort:"
                <select
                    class="cursor-pointer appearance-none border-0 bg-accent-600 pr-5 font-medium text-white group-hover:bg-accent-500 focus:outline-none"
                    prop:value=move || ctx.sort.get().key()
                    on:change=move |ev| {
                        ctx.sort_chosen.set(true);
                        ctx.sort.set(SortOrder::from_key(&event_target_value(&ev)));
                    }
                >
                    // The open list is in the accent too, with white text.
                    {SortOrder::ALL
                        .map(|o| {
                            view! {
                                <option class="bg-accent-600 text-white" value=o.key()>
                                    {o.label()}
                                </option>
                            }
                        })
                        .collect_view()}
                </select>
                <span aria-hidden="true" class="pointer-events-none absolute right-2.5 text-xs text-white">
                    "▾"
                </span>
            </label>
            </Marked>
            <button
                class="mt-2 whitespace-nowrap rounded border border-slate-300 bg-white px-3 py-1.5 text-sm hover:bg-slate-100 disabled:opacity-40 dark:border-slate-600 dark:bg-slate-900 dark:hover:bg-slate-700"
                title="Put Repository, User, Sort and the search back to their defaults"
                disabled=move || !ctx.filters_changed()
                on:click=move |_| ctx.reset_filters()
            >
                "Reset filters"
            </button>
        </nav>
    }
}

/// A filter in the filter row, with a coloured line above it while the
/// filter is active (differs from its default). The line's space is always
/// kept, so controls do not shift as filters change.
#[component]
fn Marked(active: Signal<bool>, children: Children) -> impl IntoView {
    view! {
        <div class="flex flex-col gap-1" data-active=move || active.get().to_string()>
            <span
                aria-hidden="true"
                class=move || {
                    format!(
                        "h-1 rounded-full transition-colors {}",
                        if active.get() { "bg-amber-400" } else { "bg-transparent" },
                    )
                }
            ></span>
            {children()}
            <span class="sr-only">{move || if active.get() { "Filter active" } else { "" }}</span>
        </div>
    }
}

/// Applies the user's colour scheme to the page: their saved choice is
/// loaded once, the desktop's light/dark setting is watched for "follow the
/// system", and `<html>` gets `class="dark"` and `data-accent` to match.
fn use_colour_scheme(ctx: Ctx) {
    spawn_local(async move {
        if let Ok(saved) = api::appearance().await {
            ctx.appearance.set(saved);
        }
    });
    if let Some(query) =
        web_sys::window().and_then(|w| w.match_media("(prefers-color-scheme: dark)").ok().flatten())
    {
        ctx.system_dark.set(query.matches());
        let watch = wasm_bindgen::closure::Closure::<dyn Fn()>::new({
            let query = query.clone();
            move || ctx.system_dark.set(query.matches())
        });
        let _ = query.add_event_listener_with_callback("change", watch.as_ref().unchecked_ref());
        // The listener lives as long as the page does.
        watch.forget();
    }
    Effect::new(move |_| {
        let Appearance { theme, accent } = ctx.appearance.get();
        let dark = theme.is_dark(ctx.system_dark.get());
        if let Some(root) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.document_element())
        {
            let _ = root.class_list().toggle_with_force("dark", dark);
            let _ = root.set_attribute("data-accent", accent.key());
        }
    });
}

/// The project's name, at the left of the header; a select when the folder
/// it lives in holds more than one project (ticket 093), switching to the
/// one picked.
#[component]
fn ProjectSwitcher(project: impl Fn() -> String + Send + Sync + Copy + 'static) -> impl IntoView {
    let ctx = Ctx::get();
    let choices = move || {
        ctx.board
            .with(|b| b.as_ref().map(|b| b.projects.clone()).unwrap_or_default())
    };
    let switch = move |name: String| {
        spawn_local(async move {
            match api::use_project(&name).await {
                Ok(board) => {
                    ctx.board.set(Some(board));
                    ctx.document.set(None);
                    ctx.error.set(None);
                }
                Err(e) => ctx.error.set(Some(e)),
            }
        });
    };
    view! {
        <Show
            when=move || { choices().len() > 1 }
            fallback=move || {
                view! { <h1 class="shrink-0 whitespace-nowrap text-lg font-semibold">{project}</h1> }
            }
        >
            <label
                class="relative flex shrink-0 cursor-pointer items-center rounded border border-slate-300 hover:bg-slate-100 focus-within:outline-2 focus-within:outline-offset-2 focus-within:outline-accent-500 dark:border-slate-600 dark:hover:bg-slate-700"
                title="Switch to another project in this folder"
            >
                <select
                    aria-label="Project"
                    class="cursor-pointer appearance-none border-0 bg-transparent py-1 pr-7 pl-2 text-lg font-semibold focus:outline-none"
                    on:change=move |ev| switch(event_target_value(&ev))
                >
                    {move || {
                        choices()
                            .into_iter()
                            .map(|choice| {
                                view! {
                                    <option
                                        class="bg-white text-slate-900 dark:bg-slate-800 dark:text-slate-100"
                                        value=choice.name.clone()
                                        selected=choice.current
                                    >
                                        {rite_ui_model::shown_name(&choice.name, &choice.shown).to_string()}
                                    </option>
                                }
                            })
                            .collect_view()
                    }}
                </select>
                <span aria-hidden="true" class="pointer-events-none absolute right-2.5 text-xs text-slate-500">
                    "▾"
                </span>
            </label>
        </Show>
    }
}

/// Origin couldn't be reached at the last catch-up (ticket 197): changes
/// are committed here and go when it's back. Slate, not orange: nothing to
/// click, only to know.
#[component]
fn OfflineBar() -> impl IntoView {
    let ctx = Ctx::get();
    move || {
        ctx.offline.get().map(|waiting| {
            let text = match waiting {
                0 => "Can't reach origin: working from your copy of the board".to_string(),
                1 => "Can't reach origin: 1 change waiting to push, sent as soon as it's back"
                    .to_string(),
                n => format!(
                    "Can't reach origin: {n} changes waiting to push, sent as soon as it's back"
                ),
            };
            view! {
                <div
                    role="status"
                    class="bg-slate-700 px-4 py-1.5 text-sm text-white dark:bg-slate-600"
                >
                    {text}
                </div>
            }
        })
    }
}
