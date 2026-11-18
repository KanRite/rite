//! The landing page (ticket 020): where riteui opens. Your work across every
//! project on this machine, highest priority first, a click away from its
//! ticket (or, with nothing to do, backlog tickets to pick, ticket 160);
//! and the projects, each a click away from its board. It's the home
//! that documents, ideas and collaboration will join.

use leptos::prelude::*;
use leptos::task::spawn_local;
use rite_ui_model::epic::Place;
use rite_ui_model::{Home, HomeProject, ProjectStats, SprintBurn, WorkItem};

use crate::api;
use crate::app::{Ctx, View};

/// Loads the landing page's data into `home`, reporting a failure on the
/// page rather than leaving it blank.
pub fn load_home(home: RwSignal<Option<Result<Home, String>>>) {
    spawn_local(async move { home.set(Some(api::home().await)) });
}

#[component]
pub fn HomeView(home: RwSignal<Option<Result<Home, String>>>) -> impl IntoView {
    // GitHub issues come in by themselves when the landing page opens
    // (ticket 173): once it has shown, in the background, each project at
    // most every five minutes (riteui keeps count). What came in is said in
    // the header, and the page reloads to show it.
    let ctx = Ctx::get();
    let asked = StoredValue::new(false);
    Effect::new(move |_| {
        let loaded = home.with(|h| matches!(h, Some(Ok(_))));
        if loaded && !asked.get_value() {
            asked.set_value(true);
            spawn_local(async move {
                if let Ok(reports) = api::pull_issues().await
                    && !reports.is_empty()
                {
                    for report in reports {
                        ctx.report(report);
                    }
                    load_home(home);
                }
            });
        }
    });
    view! {
        <div class="mx-auto flex h-full max-w-5xl flex-col gap-6 overflow-y-auto p-6">
            <HomeSummary />
            {move || match home.get() {
                None => view! { <p class="text-sm text-slate-500">"Loading your work…"</p> }.into_any(),
                Some(Err(e)) => {
                    view! {
                        <p role="alert" class="text-sm text-red-700 dark:text-red-300">
                            {format!("Couldn't read your projects: {e}")}
                        </p>
                    }
                        .into_any()
                }
                Some(Ok(h)) => {
                    view! {
                        <MyWork
                            work=h.work.clone()
                            to_pick=h.to_pick.clone()
                            me=h.me.clone()
                            include_unassigned=h.include_unassigned
                            home=home
                        />
                        <Projects projects=h.projects.clone() home=home stats_days=h.stats_days />
                    }
                        .into_any()
                }
            }}
        </div>
    }
}

/// The landing page's current project: the project whose board was on
/// screen, when there was one (ticket 175: going Home from a board keeps it
/// current, whatever the folder's `.current-project` says), else what riteui
/// has open.
fn current_project(ctx: Ctx) -> Option<rite_ui_model::HomeProject> {
    let on_screen = ctx.board.with(|b| {
        // Not `Path::parent`: this is wasm, which only splits on `/`, and a
        // Windows path has `\` (ticket 329).
        b.as_ref()
            .and_then(|b| rite_ui_model::parent_path(&b.tasks_dir))
    });
    ctx.home.with(|h| {
        let h = h.as_ref().and_then(|h| h.as_ref().ok())?;
        // Git says `G:/a/b` where the registry has `G:\a\b` on Windows
        // (ticket 329), so compare the places, not the spellings.
        let pick = |root: &str| {
            h.projects
                .iter()
                .find(|p| rite_ui_model::same_path(&p.root, root))
                .cloned()
        };
        on_screen
            .as_deref()
            .and_then(pick)
            .or_else(|| pick(&h.current))
    })
}

/// The current project's short summary, under the landing page's buttons
/// (ticket 398). Nothing at all when it has none.
#[component]
fn HomeSummary() -> impl IntoView {
    let ctx = Ctx::get();
    move || {
        current_project(ctx)
            .map(|p| p.summary)
            .filter(|s| !s.is_empty())
            .map(|s| view! { <p class="max-w-3xl text-sm text-slate-600 dark:text-slate-300">{s}</p> })
    }
}

/// The landing page's top bar (ticket 168): the current project, a picker
/// to change it, its Board (172), and its Documents, ADRs, Ideas and Shared, each greyed out
/// when that repository isn't cloned here. Choosing another project makes
/// it current (and remembered) without leaving the landing page.
#[component]
pub fn HomeBar() -> impl IntoView {
    let ctx = Ctx::get();
    let home = ctx.home;
    let data = move || home.with(|h| h.as_ref().and_then(|h| h.as_ref().ok()).cloned());
    let current = move || current_project(ctx);
    let choose = move |root: String| {
        spawn_local(async move {
            match api::set_tasks_dir(&root).await {
                Ok(board) => {
                    ctx.board.set(Some(board));
                    crate::home::load_home(home);
                }
                Err(e) => ctx.error.set(Some(e)),
            }
        });
    };
    let small = "rounded border border-slate-300 px-2.5 py-1.5 text-sm hover:bg-slate-100 disabled:cursor-default disabled:opacity-40 disabled:hover:bg-transparent dark:border-slate-600 dark:hover:bg-slate-700";
    let doc_button = move |label: &'static str, folder: &'static str, help: &'static str| {
        let available = move || current().is_some_and(|p| p.documents);
        let title = move || match current() {
            None => "Choose a project first".to_string(),
            Some(p) if !p.documents => format!("{}-documents isn't cloned on this machine", p.name),
            Some(_) => help.to_string(),
        };
        view! {
            <button
                type="button"
                class=small
                title=title
                disabled=move || !available()
                on:click=move |_| {
                    if let Some(p) = current() {
                        open_documents(ctx, p.root, false, folder);
                    }
                }
            >
                {label}
            </button>
        }
    };
    let shared_title = move || match current() {
        None => "Choose a project first".to_string(),
        Some(p) => match (p.shared, p.shared_cloned) {
            (Some(name), true) => format!("Read {name}, the documents every project shares"),
            (Some(name), false) => format!("{name} isn't cloned on this machine"),
            (None, _) => {
                "This project names no shared documents (Admin → Shared documents)".to_string()
            }
        },
    };
    view! {
        <select
            class="rounded border border-slate-300 bg-white px-2 py-1.5 text-sm font-medium appearance-none dark:border-slate-600 dark:bg-slate-800"
            aria-label="Current project"
            title="The current project: its documents are a click away, and it's the board riteui opens"
            on:change=move |ev| choose(event_target_value(&ev))
        >
            <Show when=move || current().is_none()>
                <option value="" selected=true>"Choose a project"</option>
            </Show>
            <For
                each=move || data().map(|h| h.projects).unwrap_or_default()
                // Archiving changes the label, so it's part of the key.
                key=|p| (p.root.clone(), p.archived)
                let:p
            >
                {
                    let root = p.root.clone();
                    let selected = move || current().is_some_and(|c| rite_ui_model::same_path(&c.root, &root));
                    let name = rite_ui_model::shown_name(&p.name, &p.shown).to_string();
                    let label = if p.archived {
                        format!("{name} (archived)")
                    } else if p.broken.is_some() {
                        format!("{name} (not a git repository)")
                    } else {
                        name
                    };
                    view! { <option value=p.root.clone() selected=selected disabled=p.broken.is_some()>{label}</option> }
                }
            </For>
        </select>
        // Its board, as clicking its name under Projects does (ticket 172).
        <button
            type="button"
            class=small
            title=move || match current() {
                None => "Choose a project first".to_string(),
                Some(p) => format!("Open {}'s board", rite_ui_model::shown_name(&p.name, &p.shown)),
            }
            disabled=move || current().is_none()
            on:click=move |_| {
                if let Some(p) = current() {
                    open(ctx, p.root, View::Board, None);
                }
            }
        >
            "Board"
        </button>
        // Roadmaps: a plan per board, its milestones becoming epics (ticket 206).
        <button
            type="button"
            class=small
            title=move || match current() {
                None => "Choose a project first".to_string(),
                Some(p) => format!("{}'s roadmaps and milestones", rite_ui_model::shown_name(&p.name, &p.shown)),
            }
            disabled=move || current().is_none()
            on:click=move |_| {
                if let Some(p) = current() {
                    open(ctx, p.root, View::Roadmaps, None);
                }
            }
        >
            "Roadmap"
        </button>
        {doc_button("Documents", "docs", "Read this project's documents (docs/)")}
        {doc_button("ADRs", "adr", "Read this project's decisions and proposals (adr/)")}
        // Ideas have a view of their own (ticket 131).
        <button
            type="button"
            class=small
            title=move || match current() {
                None => "Choose a project first".to_string(),
                Some(p) if !p.documents => format!("{}-documents isn't cloned on this machine", p.name),
                Some(_) => "This project's ideas: vote, add notes, make tickets".to_string(),
            }
            disabled=move || !current().is_some_and(|p| p.documents)
            on:click=move |_| {
                if let Some(p) = current() {
                    open(ctx, p.root, View::Ideas, None);
                }
            }
        >
            "Ideas"
        </button>
        <button
            type="button"
            class=small
            title=shared_title
            disabled=move || !current().is_some_and(|p| p.shared_cloned)
            on:click=move |_| {
                if let Some(p) = current() {
                    open_documents(ctx, p.root, true, "");
                }
            }
        >
            "Shared"
        </button>
    }
}

/// Opens `root`'s documents at one folder (ticket 168): `docs`, `adr` or
/// `ideas` of its own repository, or the shared repository.
fn open_documents(ctx: Ctx, root: String, shared: bool, folder: &str) {
    ctx.doc_scope.set(Some(rite_ui_model::DocScope {
        shared,
        folder: folder.to_string(),
    }));
    open(ctx, root, View::Documents, None);
}

/// Opens `root`'s project at `view` (its board or its documents), then,
/// if given, one of its tickets.
fn open(
    ctx: Ctx,
    root: String,
    view: View,
    ticket: Option<(String, rite_ui_model::Column, Option<String>, String)>,
) {
    spawn_local(async move {
        match api::set_tasks_dir(&root).await {
            Ok(board) => {
                ctx.board.set(Some(board));
                ctx.document.set(None);
                ctx.error.set(None);
                ctx.view.set(view);
                if let Some((repo, column, epic, file)) = ticket {
                    ctx.open_ticket(Place { repo, column, epic }, file);
                }
            }
            Err(e) => ctx.error.set(Some(e)),
        }
    });
}

/// How many tickets to pick are listed before "Show all" (ticket 171).
const PICK_SHOWN: usize = 5;

#[component]
fn MyWork(
    work: Vec<WorkItem>,
    to_pick: Vec<WorkItem>,
    me: String,
    include_unassigned: bool,
    home: RwSignal<Option<Result<Home, String>>>,
) -> impl IntoView {
    let ctx = Ctx::get();
    let set_unassigned = move |on: bool| {
        spawn_local(async move {
            match api::set_include_unassigned(on).await {
                Ok(()) => load_home(home),
                Err(e) => ctx.error.set(Some(e)),
            }
        });
    };
    let empty = work.is_empty();
    let picking = empty && !to_pick.is_empty();
    let note = if me.is_empty() {
        "git has no user.email, so rite can't tell which tickets are yours.".to_string()
    } else if picking {
        "Nothing in todo or doing is assigned to you. Work to pick:".to_string()
    } else {
        "Nothing in todo or doing is assigned to you.".to_string()
    };
    let rows = if empty { to_pick } else { work };
    // Work to pick can run long (every unassigned ticket at the top
    // priority), pushing Projects off the page: the first few, then Show
    // all (ticket 171). Your own work is always listed in full.
    let total = rows.len();
    let long = picking && total > PICK_SHOWN;
    let show_all = RwSignal::new(false);
    let rows = StoredValue::new(rows);
    view! {
        <section aria-labelledby="my-work" class="flex flex-col gap-2">
            <div class="flex items-baseline justify-between gap-3">
                <h2 id="my-work" class="text-lg font-semibold">"My work"</h2>
                <label
                    class="flex items-center gap-2 text-sm text-slate-600 dark:text-slate-300"
                    title="With nothing assigned to you: unassigned todo and doing tickets first, then unassigned backlog tickets by priority"
                >
                    <input
                        type="checkbox"
                        prop:checked=include_unassigned
                        on:change=move |ev| set_unassigned(event_target_checked(&ev))
                    />
                    "Include unassigned tickets"
                </label>
            </div>
            <Show when=move || empty>
                <p class="text-sm text-slate-500">{note.clone()}</p>
            </Show>
            <ul class="flex flex-col gap-1.5">
                {move || {
                    let limit = if long && !show_all.get() { PICK_SHOWN } else { total };
                    rows.get_value().into_iter().take(limit).map(work_row).collect_view()
                }}
            </ul>
            <Show when=move || long>
                <button
                    type="button"
                    class="self-start text-sm text-slate-500 hover:underline"
                    aria-expanded=move || show_all.get().to_string()
                    on:click=move |_| show_all.update(|s| *s = !*s)
                >
                    {move || {
                        if show_all.get() {
                            format!("Show the first {PICK_SHOWN}")
                        } else {
                            format!("Show all ({total})")
                        }
                    }}
                </button>
            </Show>
        </section>
    }
}

/// One ticket on the landing page: a click opens it on its board.
fn work_row(w: WorkItem) -> impl IntoView {
    let ctx = Ctx::get();
    let target = (w.repo.clone(), w.column, w.folder.clone(), w.file.clone());
    let root = w.project_root.clone();
    let priority = w.priority.clone();
    let tone = match priority.as_str() {
        "top" => "bg-red-600 text-white dark:bg-red-700",
        "high" => "bg-red-100 text-red-800 dark:bg-red-950 dark:text-red-200",
        "medium" => "bg-amber-100 text-amber-800 dark:bg-amber-950 dark:text-amber-200",
        _ => "bg-slate-200 text-slate-700 dark:bg-slate-700 dark:text-slate-200",
    };
    view! {
        <li>
            <button
                type="button"
                class="flex w-full items-baseline gap-3 rounded-md border border-slate-200 bg-white px-3 py-2 text-left text-sm hover:border-accent-400 dark:border-slate-700 dark:bg-slate-800"
                title="Open this ticket on its board"
                on:click=move |_| open(ctx, root.clone(), View::Board, Some(target.clone()))
            >
                <span class=format!("rounded px-1.5 text-xs font-medium {tone}")>{priority.clone()}</span>
                <span class="w-16 text-xs uppercase text-slate-500">{w.column.dir_name()}</span>
                <span class="font-mono text-xs text-slate-500">{w.id.clone()}</span>
                // Waiting to be triaged (ticket 248): first, for everyone.
                {w.triage
                    .then(|| view! {
                        <span class="rounded border border-dashed border-amber-500 px-1.5 text-xs font-semibold text-amber-700 dark:text-amber-300" title="Waiting to be triaged: look at it, then take TRIAGE off">
                            "TRIAGE"
                        </span>
                    })}
                <span class="flex-1 font-medium">{w.title.clone()}</span>
                {(!w.points.is_empty())
                    .then(|| view! { <span class="font-mono text-xs text-slate-500">{format!("{} pts", w.points)}</span> })}
                <span class="text-xs text-slate-500">{rite_ui_model::shown_name(&w.project, &w.project_shown).to_string()}</span>
            </button>
        </li>
    }
}

#[component]
fn Projects(
    projects: Vec<HomeProject>,
    home: RwSignal<Option<Result<Home, String>>>,
    stats_days: u32,
) -> impl IntoView {
    let ctx = Ctx::get();
    // The statistics' period (ticket 233), remembered on this machine.
    let days = RwSignal::new(stats_days);
    let pick_days = move |d: u32| {
        days.set(d);
        spawn_local(async move {
            if let Err(e) = api::set_stats_days(d).await {
                ctx.error.set(Some(e));
            }
        });
    };
    // The same name in two folders: say which folder, so they can be told
    // apart (ticket 095).
    let twice = |name: &str| {
        projects
            .iter()
            .filter(|p| rite_ui_model::same_name(&p.name, name))
            .count()
            > 1
    };
    let rows: Vec<(HomeProject, bool)> = projects
        .iter()
        .map(|p| (p.clone(), twice(&p.name)))
        .collect();
    // An archived project is no longer maintained (ticket 263): not shown.
    let rows: Vec<_> = rows.into_iter().filter(|(p, _)| !p.archived).collect();
    let (shown, hidden): (Vec<_>, Vec<_>) = rows.into_iter().partition(|(p, _)| !p.hidden);
    let show_hidden = RwSignal::new(false);
    let hidden_count = hidden.len();
    let toggle = move |root: String, hide: bool| {
        spawn_local(async move {
            match api::set_project_hidden(&root, hide).await {
                Ok(()) => crate::home::load_home(home),
                Err(e) => ctx.error.set(Some(e)),
            }
        });
    };
    let row = move |(p, show_dir): (HomeProject, bool)| {
        let root = p.root.clone();
        let flip = p.root.clone();
        let hide = !p.hidden;
        let stats_root = p.root.clone();
        let counts_root = p.root.clone();
        let folded = p.graphs_folded;
        let broken = p.broken.clone();
        view! {
            <li class="flex flex-col gap-1 rounded-md border border-slate-200 bg-white px-3 py-2 text-sm dark:border-slate-700 dark:bg-slate-800">
                <div class="flex items-center gap-3">
                {match broken.clone() {
                    // Its repository isn't one (ticket 341): say why, not a
                    // button that fails.
                    Some(why) => view! {
                        <div class="flex-1">
                            <span class="font-medium">{rite_ui_model::shown_name(&p.name, &p.shown).to_string()}</span>
                            {show_dir.then(|| view! { <span class="ml-2 text-xs font-normal text-slate-500">{p.dir.clone()}</span> })}
                            <p class="text-xs text-red-700 dark:text-red-400" role="alert">{format!("Can't be opened: {why}.")}</p>
                        </div>
                    }.into_any(),
                    None => view! {
                <button
                    type="button"
                    class="flex-1 text-left font-medium hover:underline"
                    title="Open this project's board"
                    on:click=move |_| open(ctx, root.clone(), View::Board, None)
                >
                    {rite_ui_model::shown_name(&p.name, &p.shown).to_string()}
                    {show_dir.then(|| view! { <span class="ml-2 text-xs font-normal text-slate-500">{p.dir.clone()}</span> })}
                </button>
                <crate::counts::ProjectCounts root=counts_root.clone() />
                    }.into_any(),
                }}
                <button
                    type="button"
                    class="rounded border border-slate-300 px-2 py-0.5 text-xs hover:bg-slate-100 dark:border-slate-600 dark:hover:bg-slate-700"
                    title=if hide { "Leave this project out of My work (on this machine only)" } else { "Put this project back in My work" }
                    on:click=move |_| toggle(flip.clone(), hide)
                >
                    {if hide { "Hide" } else { "Show" }}
                </button>
                </div>
                {broken.is_none().then(|| view! { <ProjectStatsView root=stats_root.clone() days folded /> })}
            </li>
        }
    };
    view! {
        <section aria-labelledby="projects" class="flex flex-col gap-2">
            <div class="flex items-center gap-3">
                <h2 id="projects" class="flex-1 text-lg font-semibold">"Projects"</h2>
                <div role="group" aria-label="Statistics period" class="flex overflow-hidden rounded border border-slate-300 text-xs dark:border-slate-600">
                    {[7u32, 30, 90]
                        .into_iter()
                        .map(|d| {
                            view! {
                                <button
                                    type="button"
                                    class=move || {
                                        if days.get() == d {
                                            "bg-accent-600 px-2 py-0.5 text-white"
                                        } else {
                                            "px-2 py-0.5 hover:bg-slate-100 dark:hover:bg-slate-700"
                                        }
                                    }
                                    aria-pressed=move || (days.get() == d).to_string()
                                    title=format!("Statistics for the last {d} days")
                                    on:click=move |_| pick_days(d)
                                >
                                    {format!("{d} days")}
                                </button>
                            }
                        })
                        .collect_view()}
                </div>
            </div>
            <ul class="flex flex-col gap-1.5">{shown.into_iter().map(row).collect_view()}</ul>
            <Show when=move || { hidden_count > 0 }>
                <button
                    type="button"
                    class="self-start text-sm text-slate-500 hover:underline"
                    aria-expanded=move || show_hidden.get().to_string()
                    on:click=move |_| show_hidden.update(|s| *s = !*s)
                >
                    {move || format!("{} Hidden ({hidden_count})", if show_hidden.get() { "▾" } else { "▸" })}
                </button>
            </Show>
            <Show when=move || show_hidden.get()>
                <ul class="flex flex-col gap-1.5 opacity-80">{hidden.clone().into_iter().map(row).collect_view()}</ul>
            </Show>
        </section>
    }
}

/// A project's statistics under its row (ticket 233): raised, done and
/// closed over the period with a bar a week, and each running sprint's
/// burn-up and burn-down. Read from git history when shown.
#[component]
fn ProjectStatsView(root: String, days: RwSignal<u32>, folded: bool) -> impl IntoView {
    let stats = RwSignal::new(None::<Result<ProjectStats, String>>);
    let root = StoredValue::new(root);
    // The sprint graphs, folded or open (ticket 305), remembered on this
    // machine.
    let folded = RwSignal::new(folded);
    let fold = move |_| {
        let now = !folded.get_untracked();
        folded.set(now);
        spawn_local(async move {
            let _ = api::set_project_graphs_folded(&root.get_value(), now).await;
        });
    };
    Effect::new(move |_| {
        let d = days.get();
        spawn_local(async move {
            let got = api::project_stats(&root.get_value(), d).await;
            // Only the answer for the period still chosen.
            if days.get_untracked() == d {
                stats.set(Some(got));
            }
        });
    });
    move || {
        match stats.get() {
        None => view! { <p class="text-xs text-slate-400">"Reading the history…"</p> }.into_any(),
        Some(Err(e)) => view! { <p class="text-xs text-slate-500">{format!("No statistics: {e}")}</p> }.into_any(),
        Some(Ok(s)) => view! {
            <div class="flex flex-col gap-2">
                <div class="flex flex-wrap items-center gap-3 text-xs text-slate-500">
                    <span>
                        {format!("{} raised · ", s.raised)}
                        <span class="text-accent-700 dark:text-accent-300">{format!("{} done", s.done)}</span>
                        {format!(" · {} closed in the last {} days", s.closed, s.days)}
                    </span>
                    <WeekBars stats=s.clone() />
                    {(!s.sprints.is_empty()).then(|| {
                        let n = s.sprints.len();
                        view! {
                            <button
                                type="button"
                                class="ml-auto rounded px-1.5 py-0.5 hover:bg-slate-100 hover:underline dark:hover:bg-slate-700"
                                aria-expanded=move || (!folded.get()).to_string()
                                title=move || if folded.get() { "Show the sprint graphs" } else { "Fold the sprint graphs away" }
                                on:click=fold
                            >
                                {move || {
                                    let graphs = if n == 1 { "Sprint graphs".to_string() } else { format!("Sprint graphs ({n} sprints)") };
                                    format!("{} {graphs}", if folded.get() { "▸" } else { "▾" })
                                }}
                            </button>
                        }
                    })}
                </div>
                {
                    let sprints = s.sprints.clone();
                    move || (!folded.get()).then(|| {
                        sprints.iter().map(|b| view! { <BurnCharts burn=b.clone() /> }).collect_view()
                    })
                }
            </div>
        }.into_any(),
    }
    }
}

/// A pair of bars a week, oldest first: raised (grey) and done (accent).
#[component]
fn WeekBars(stats: ProjectStats) -> impl IntoView {
    let top = stats
        .weeks
        .iter()
        .map(|w| w.raised.max(w.done))
        .max()
        .unwrap_or(0)
        .max(1);
    let (w, h) = (8.0_f64, 18.0_f64);
    let width = stats.weeks.len() as f64 * w;
    let bars = stats
        .weeks
        .iter()
        .enumerate()
        .map(|(i, week)| {
            let x = i as f64 * w;
            let raised = week.raised as f64 / top as f64 * h;
            let done = week.done as f64 / top as f64 * h;
            view! {
                <rect x=x y=h - raised width="3" height=raised class="fill-slate-400 dark:fill-slate-500">
                    <title>{format!("raised {}, done {}, closed {}", week.raised, week.done, week.closed)}</title>
                </rect>
                <rect x=x + 3.5 y=h - done width="3" height=done class="fill-accent-500" />
            }
        })
        .collect_view();
    view! {
        <svg
            role="img"
            aria-label="Raised and done, a week at a time"
            viewBox=format!("0 0 {width} {h}")
            width=width
            height=h
        >
            {bars}
        </svg>
    }
}

/// A running sprint's burn-up (scope and done) and burn-down (points left
/// against the ideal straight line), over its days.
#[component]
fn BurnCharts(burn: SprintBurn) -> impl IntoView {
    let today = burn.done_by_day.len();
    let done = burn.done_by_day.last().copied().unwrap_or(0);
    let heading = format!(
        "Sprint {} · day {today} of {} · {done} of {} points done",
        burn.sprint, burn.days, burn.total
    );
    let up = burn.clone();
    let down = burn;
    view! {
        <div class="flex flex-col gap-1 rounded bg-slate-50 p-2 dark:bg-slate-900">
            <p class="text-xs font-medium">{heading}</p>
            <div class="flex flex-wrap gap-4">
                <Chart burn=up down=false />
                <Chart burn=down down=true />
            </div>
        </div>
    }
}

#[component]
fn Chart(burn: SprintBurn, down: bool) -> impl IntoView {
    let (w, h) = (240.0_f64, 90.0_f64);
    let days = f64::from(burn.days.max(1));
    let total = f64::from(burn.total.max(1));
    let x = move |d: f64| d / days * w;
    // Room above the top line for the points label.
    let y = move |v: f64| h - v / total * (h - 12.0);
    // From the start (day 0), then the end of each day so far.
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
    // Burn-up's guide is the scope; burn-down's the ideal line to zero.
    let guide = if down {
        format!("0,{:.1} {w:.1},{h:.1}", y(f64::from(burn.total)))
    } else {
        format!(
            "0,{:.1} {w:.1},{:.1}",
            y(f64::from(burn.total)),
            y(f64::from(burn.total))
        )
    };
    let label = if down { "Burn-down" } else { "Burn-up" };
    let guide_name = if down { "ideal" } else { "scope" };
    view! {
        <figure class="flex flex-col gap-0.5">
            <svg
                role="img"
                aria-label=format!("{label} of sprint {}", burn.sprint)
                viewBox=format!("-2 -2 {} {}", w + 4.0, h + 14.0)
                width=w + 4.0
                height=h + 16.0
                class="overflow-visible"
            >
                <line x1="0" y1=h x2=w y2=h class="stroke-slate-300 dark:stroke-slate-600" stroke-width="1" />
                <polyline points=guide fill="none" stroke-width="1" stroke-dasharray="4 3" class="stroke-slate-400" />
                <polyline points=line fill="none" stroke-width="2" class="stroke-accent-500" />
                <text x="0" y="7" font-size="9" class="fill-slate-500">{format!("{} pts", burn.total)}</text>
                <text x="0" y=h + 10.0 font-size="9" class="fill-slate-500">"day 1"</text>
                <text x=w y=h + 10.0 font-size="9" text-anchor="end" class="fill-slate-500">{format!("day {}", burn.days)}</text>
            </svg>
            <figcaption class="text-[11px] text-slate-500">
                {format!("{label}: points {}, dashed {guide_name}", if down { "left" } else { "done" })}
            </figcaption>
        </figure>
    }
}
