//! The admin panel: the project's own settings, shared with the team
//! through the tickets repository (auto-commit, tags, the WIP limit, lock
//! length, epics and sprints, the shared documents repository, the ticket
//! template). Personal settings live
//! in Preferences (`preferences.rs`, ticket 148).

use leptos::prelude::*;
use leptos::task::spawn_local;
use rite_ui_model::wip::WipLimits;
use rite_ui_model::{Column, Kind, TicketTemplate};

use crate::api;
use crate::app::{Ctx, Modal, ModalClose};

#[component]
pub fn AdminPanel() -> impl IntoView {
    let template = RwSignal::new(None::<TicketTemplate>);
    let source = RwSignal::new(String::new());
    let preview = RwSignal::new(None::<Result<String, String>>);
    let auto_commit = RwSignal::new(true);
    spawn_local(async move {
        if let Ok(on) = api::auto_commit().await {
            auto_commit.set(on);
        }
    });
    let status = RwSignal::new(None::<String>);

    let show_template = move |t: TicketTemplate| {
        source.set(t.source.clone());
        template.set(Some(t));
        preview.set(None);
    };
    let load_template = move || {
        spawn_local(async move {
            match api::get_template().await {
                Ok(t) => show_template(t),
                Err(e) => status.set(Some(e)),
            }
        });
    };

    load_template();

    let run_preview = move |_| {
        let src = source.get_untracked();
        spawn_local(async move { preview.set(Some(api::preview_template(&src).await)) });
    };
    let save = move |_| {
        let src = source.get_untracked();
        spawn_local(async move {
            match api::save_template(&src).await {
                Ok(t) => {
                    show_template(t);
                    status.set(Some("Template saved.".into()));
                }
                Err(e) => preview.set(Some(Err(e))),
            }
        });
    };
    let reset = move |_| {
        spawn_local(async move {
            match api::reset_template().await {
                Ok(t) => {
                    show_template(t);
                    status.set(Some("Using the built-in template.".into()));
                }
                Err(e) => status.set(Some(e)),
            }
        });
    };

    let dirty =
        move || template.with(|t| t.as_ref().is_some_and(|t| source.with(|s| *s != t.source)));
    let customised = move || template.with(|t| t.as_ref().is_some_and(|t| t.customised));
    let template_note = move || {
        template.with(|t| match t {
            Some(t) if t.customised => format!("Customised for this board: {}", t.path),
            Some(t) => format!("Built-in default. Saving creates {}", t.path),
            None => "Open a board to edit its template.".into(),
        })
    };

    let field = "rounded border border-slate-300 bg-slate-50 px-2 py-1.5 dark:border-slate-600 dark:bg-slate-900";
    let secondary = "rounded border border-slate-300 px-3 py-1.5 text-sm hover:bg-slate-100 disabled:opacity-50 dark:border-slate-600 dark:hover:bg-slate-700";
    let primary = "rounded bg-accent-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-accent-500 disabled:opacity-50";

    view! {
        <Modal title="Admin".to_string() width="max-w-5xl">
            <div class="flex min-h-0 flex-col gap-4 overflow-y-auto">
                // Who gets Admin and why (ticket 149).
                {move || {
                    Ctx::get()
                        .board
                        .with(|b| b.as_ref().map(|b| b.admin.note.clone()))
                        .filter(|note| !note.is_empty())
                        .map(|note| view! { <p class="text-xs text-slate-500">{note}</p> })
                }}
                <section class="flex flex-col gap-1">
                    <label class="flex items-center gap-2 text-sm font-semibold">
                        <input
                            type="checkbox"
                            prop:checked=move || auto_commit.get()
                            on:change=move |ev| {
                                let on = event_target_checked(&ev);
                                spawn_local(async move {
                                    match api::set_auto_commit(on).await {
                                        Ok(on) => auto_commit.set(on),
                                        Err(e) => status.set(Some(e)),
                                    }
                                });
                            }
                        />
                        "Commit and push ticket changes"
                    </label>
                    <p class="text-xs text-slate-500">
                        "Creating, editing, moving or archiving a ticket commits just that change and pushes it to main, as rite does. Boards outside a git repository are never committed."
                    </p>
                </section>

                <TagsAdmin />

                <ColumnsAdmin />
                <WipAdmin />

                <LeaseAdmin />

                <DisplayNameAdmin />
                <DocumentsAdmin />

                <IssuesAdmin />

                <PriorityAdmin />
                <DueDatesAdmin />

                <EpicsAdmin />

                <section class="flex flex-col gap-2">
                    <h3 class="text-sm font-semibold">"Ticket template"</h3>
                    <p class="text-xs text-slate-500">{template_note}</p>
                    <div class="grid min-h-0 grid-cols-1 gap-3 md:grid-cols-2">
                        <textarea
                            aria-label="Ticket template source"
                            spellcheck="false"
                            class=format!("{field} h-[45vh] resize-none font-mono text-xs")
                            prop:value=move || source.get()
                            on:input=move |ev| source.set(event_target_value(&ev))
                        ></textarea>
                        <pre
                            aria-label="Template preview"
                            class=move || {
                                let tone = if matches!(preview.get(), Some(Err(_))) {
                                    "text-red-700 dark:text-red-300"
                                } else {
                                    "text-slate-700 dark:text-slate-300"
                                };
                                format!(
                                    "h-[45vh] overflow-auto whitespace-pre-wrap rounded bg-slate-100 p-2 font-mono text-xs dark:bg-slate-900 {tone}",
                                )
                            }
                        >
                            {move || match preview.get() {
                                Some(Ok(text)) | Some(Err(text)) => text,
                                None => "Preview shows a new ticket rendered from this template.".into(),
                            }}
                        </pre>
                    </div>
                    <div class="flex items-center gap-2">
                        <span class="flex-1 text-xs text-slate-500">
                            {move || status.get().unwrap_or_default()}
                        </span>
                        <button class=secondary disabled=move || !customised() on:click=reset>
                            "Reset to built-in"
                        </button>
                        <button class=secondary disabled=move || template.with(Option::is_none) on:click=run_preview>
                            "Preview"
                        </button>
                        <button class=primary disabled=move || !dirty() on:click=save>
                            "Save template"
                        </button>
                    </div>
                </section>

                // Last, away from the everyday settings (ticket 157).
                <ArchiveAdmin />
            </div>
            <ModalClose />
        </Modal>
    }
}

/// The backlog's epics, each renamable. A rename moves the epic's folder and
/// relabels its tickets in one commit (see `TasksDir::rename_epic`).
#[component]
fn EpicsAdmin() -> impl IntoView {
    let ctx = Ctx::get();
    let epics = move || {
        ctx.board.with(|b| {
            b.as_ref()
                .map(|b| {
                    b.epics
                        .iter()
                        .map(|e| (e.repo.clone(), e.name.clone(), e.tickets.len()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    };
    view! {
        <section class="flex flex-col gap-2">
            <h3 class="text-sm font-semibold">"Sprints and epics"</h3>
            <Show
                when=move || !epics().is_empty()
                fallback=|| {
                    view! {
                        <p class="text-xs text-slate-500">
                            "None yet. Create one with \"+ Sprint\" or \"+ Epic\" in the Backlogs column."
                        </p>
                    }
                }
            >
                <ul class="flex flex-col gap-2">
                    <For
                        each=epics
                        key=|row| row.clone()
                        children=|(repo, name, count)| view! { <EpicRow repo name count /> }
                    />
                </ul>
            </Show>
        </section>
    }
}

#[component]
fn EpicRow(repo: String, name: String, count: usize) -> impl IntoView {
    let repo = StoredValue::new(repo);
    let ctx = Ctx::get();
    let current = StoredValue::new(name.clone());
    let draft = RwSignal::new(name.clone());
    let problem = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let changed = move || current.with_value(|c| draft.with(|d| d.trim() != c));

    let rename = move || {
        if busy.get_untracked() || !changed() {
            return;
        }
        let (from, to, repo) = (current.get_value(), draft.get_untracked(), repo.get_value());
        busy.set(true);
        spawn_local(async move {
            match api::rename_epic(Some(&repo), &from, &to).await {
                Ok(report) => {
                    ctx.report(report);
                    ctx.reload();
                }
                Err(e) => problem.set(Some(e)),
            }
            busy.set(false);
        });
    };

    view! {
        <li class="flex flex-col gap-1">
            <form
                class="flex items-center gap-2"
                on:submit=move |ev| {
                    ev.prevent_default();
                    rename();
                }
            >
                <input
                    type="text"
                    aria-label=format!("Name of the {name} epic")
                    aria-invalid=move || problem.with(Option::is_some).to_string()
                    class="flex-1 rounded border border-slate-300 bg-slate-50 px-2 py-1.5 text-sm dark:border-slate-600 dark:bg-slate-900"
                    prop:value=move || draft.get()
                    on:input=move |ev| {
                        draft.set(event_target_value(&ev));
                        problem.set(None);
                    }
                />
                <Show when=move || ctx.multi_repo()>
                    <span class="max-w-40 shrink-0 truncate text-xs text-slate-500" title="Repository">
                        {repo.get_value()}
                    </span>
                </Show>
                <span class="w-20 shrink-0 text-right text-xs text-slate-500">
                    {format!("{count} ticket{}", if count == 1 { "" } else { "s" })}
                </span>
                <button
                    type="submit"
                    class="rounded border border-slate-300 px-3 py-1.5 text-sm hover:bg-slate-100 disabled:opacity-50 dark:border-slate-600 dark:hover:bg-slate-700"
                    disabled=move || busy.get() || !changed()
                >
                    "Rename"
                </button>
            </form>
            {move || {
                problem
                    .get()
                    .map(|p| {
                        view! {
                            <p role="alert" class="text-xs font-medium text-red-700 dark:text-red-300">
                                {p}
                            </p>
                        }
                    })
            }}
        </li>
    }
}

/// The board's columns (ticket 224): rename, move, set a doing column's
/// WIP limit, remove an added one (its tickets moved first), and add new
/// ones. Saved in the tickets `.settings` and committed, so the whole team
/// gets the same board.
#[component]
fn ColumnsAdmin() -> impl IntoView {
    let ctx = Ctx::get();
    let note = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let rows = Memo::new(move |_| {
        ctx.board.with(|b| {
            b.as_ref()
                .map(|b| {
                    b.workflow
                        .columns
                        .iter()
                        .map(|info| {
                            let in_epics: usize = if info.column == Column::Backlog {
                                b.epics.iter().map(|e| e.tickets.len()).sum()
                            } else {
                                0
                            };
                            (info.clone(), b.tickets(info.column).len() + in_epics)
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    });
    // Runs a change, then says what happened and redraws the board.
    let run = move |work: std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<rite_ui_model::SyncReport, String>>>,
    >,
                    done: &'static str| {
        busy.set(true);
        spawn_local(async move {
            match work.await {
                Ok(report) => {
                    ctx.report(report);
                    note.set(Some(done.into()));
                    ctx.reload();
                }
                Err(e) => note.set(Some(e)),
            }
            busy.set(false);
        });
    };
    let new_label = RwSignal::new(String::new());
    let new_kind = RwSignal::new("doing".to_string());
    let new_after = RwSignal::new(Column::Doing.dir_name().to_string());
    let column_named = move |folder: &str| {
        rows.with_untracked(|r| {
            r.iter()
                .map(|(i, _)| i.column)
                .find(|c| c.dir_name() == folder)
        })
    };
    let add = move |_| {
        let label = new_label.get_untracked();
        if label.trim().is_empty() {
            note.set(Some("Give the column a name.".into()));
            return;
        }
        let kind = new_kind.get_untracked();
        let after = column_named(&new_after.get_untracked());
        new_label.set(String::new());
        run(
            Box::pin(async move { api::add_column(&label, &kind, after, None).await }),
            "Column added.",
        );
    };
    let field = "rounded border border-slate-300 bg-slate-50 px-2 py-1 text-sm dark:border-slate-600 dark:bg-slate-900";
    let small = "rounded border border-slate-300 px-2 py-0.5 text-xs hover:bg-slate-100 disabled:opacity-40 dark:border-slate-600 dark:hover:bg-slate-700";
    view! {
        <section class="flex flex-col gap-2">
            <h3 class="text-sm font-semibold">"Columns"</h3>
            <p class="text-xs text-slate-500">
                "Each column's kind decides what it does: backlog (work waiting), todo (entering it makes the ticket's branch), doing (locked while in it, with a WIP limit) or done (waits for what the ticket requires). Renaming keeps the folder. The four standard columns can be renamed and moved but not removed. Everyone needs amd 4.1 or later to see tickets in an added column."
            </p>
            <ul class="flex flex-col gap-1">
                {move || {
                    let all = rows.get();
                    all.into_iter()
                        .enumerate()
                        .map(|(i, (info, held))| {
                            let column = info.column;
                            let standard = Column::STANDARD.contains(&column);
                            let label = RwSignal::new(info.label.clone());
                            let original = info.label.clone();
                            let wip = RwSignal::new(info.wip.map(|n| n.to_string()).unwrap_or_default());
                            let move_to = RwSignal::new(String::new());
                            let neighbours: Vec<Column> = rows.with_untracked(|r| r.iter().map(|(i, _)| i.column).collect());
                            let (prev, next) = (
                                i.checked_sub(1).map(|p| neighbours[p]),
                                neighbours.get(i + 1).copied(),
                            );
                            let others: Vec<(String, String)> = rows.with_untracked(|r| {
                                r.iter()
                                    .filter(|(o, _)| o.column != column)
                                    .map(|(o, _)| (o.column.dir_name().to_string(), o.label.clone()))
                                    .collect()
                            });
                            view! {
                                <li class="flex flex-wrap items-center gap-2 rounded border border-slate-200 px-2 py-1 dark:border-slate-700">
                                    <input
                                        type="text"
                                        class=format!("{field} w-36")
                                        aria-label=format!("Name of {}/", column.dir_name())
                                        prop:value=move || label.get()
                                        on:input=move |ev| label.set(event_target_value(&ev))
                                    />
                                    <button
                                        class=small
                                        disabled=move || busy.get() || label.with(|l| l.trim() == original || l.trim().is_empty())
                                        on:click=move |_| {
                                            let new = label.get_untracked();
                                            run(Box::pin(async move { api::rename_column(column, &new).await }), "Column renamed.");
                                        }
                                    >
                                        "Rename"
                                    </button>
                                    <span class="font-mono text-xs text-slate-500">{format!("{}/", column.dir_name())}</span>
                                    <span class="text-xs text-slate-500">{column.kind().as_str()}</span>
                                    <span class="text-xs text-slate-500">{format!("{held} ticket(s)")}</span>
                                    {(column.is(Kind::Doing) && column != Column::Doing)
                                        .then(|| {
                                            view! {
                                                <label class="flex items-center gap-1 text-xs">
                                                    "WIP"
                                                    <input
                                                        type="number"
                                                        min="0"
                                                        max="99"
                                                        class=format!("{field} w-16")
                                                        placeholder="none"
                                                        prop:value=move || wip.get()
                                                        on:input=move |ev| wip.set(event_target_value(&ev))
                                                    />
                                                    <button
                                                        class=small
                                                        disabled=move || busy.get()
                                                        on:click=move |_| {
                                                            let limit = wip.get_untracked().trim().parse::<u32>().ok().filter(|n| *n > 0);
                                                            run(Box::pin(async move { api::set_column_wip(column, limit).await }), "WIP limit saved.");
                                                        }
                                                    >
                                                        "Set"
                                                    </button>
                                                </label>
                                            }
                                        })}
                                    <span class="flex-1"></span>
                                    <button
                                        class=small
                                        title="Move left"
                                        aria-label=format!("Move {} left", info.label)
                                        disabled=move || busy.get() || prev.is_none()
                                        on:click=move |_| {
                                            run(Box::pin(async move { api::move_column(column, None, prev).await }), "Column moved.");
                                        }
                                    >
                                        "←"
                                    </button>
                                    <button
                                        class=small
                                        title="Move right"
                                        aria-label=format!("Move {} right", info.label)
                                        disabled=move || busy.get() || next.is_none()
                                        on:click=move |_| {
                                            run(Box::pin(async move { api::move_column(column, next, None).await }), "Column moved.");
                                        }
                                    >
                                        "→"
                                    </button>
                                    {(!standard)
                                        .then(|| {
                                            let others = others.clone();
                                            view! {
                                                {(held > 0)
                                                    .then(|| {
                                                        view! {
                                                            <select
                                                                class=format!("{field} appearance-none text-xs")
                                                                aria-label="Move its tickets to"
                                                                on:change=move |ev| move_to.set(event_target_value(&ev))
                                                            >
                                                                <option value="">"Move its tickets to…"</option>
                                                                {others
                                                                    .into_iter()
                                                                    .map(|(folder, name)| view! { <option value=folder>{name}</option> })
                                                                    .collect_view()}
                                                            </select>
                                                        }
                                                    })}
                                                <button
                                                    class=format!("{small} text-red-700 dark:text-red-300")
                                                    disabled=move || busy.get() || (held > 0 && move_to.with(String::is_empty))
                                                    on:click=move |_| {
                                                        let to = column_named(&move_to.get_untracked());
                                                        run(Box::pin(async move { api::remove_column(column, to).await }), "Column removed.");
                                                    }
                                                >
                                                    "Remove"
                                                </button>
                                            }
                                        })}
                                </li>
                            }
                        })
                        .collect_view()
                }}
            </ul>
            <div class="flex flex-wrap items-center gap-2">
                <input
                    type="text"
                    class=format!("{field} w-40")
                    placeholder="New column, e.g. Review"
                    aria-label="New column's name"
                    prop:value=move || new_label.get()
                    on:input=move |ev| new_label.set(event_target_value(&ev))
                />
                <select
                    class=format!("{field} appearance-none")
                    aria-label="New column's kind"
                    on:change=move |ev| new_kind.set(event_target_value(&ev))
                    prop:value=move || new_kind.get()
                >
                    <option value="backlog">"backlog"</option>
                    <option value="todo">"todo"</option>
                    <option value="doing">"doing"</option>
                    <option value="done">"done"</option>
                </select>
                <label class="flex items-center gap-1 text-sm">
                    "after"
                    <select
                        class=format!("{field} appearance-none")
                        aria-label="Put it after"
                        on:change=move |ev| new_after.set(event_target_value(&ev))
                        prop:value=move || new_after.get()
                    >
                        {move || {
                            rows.get()
                                .into_iter()
                                .map(|(info, _)| {
                                    view! { <option value=info.column.dir_name()>{info.label}</option> }
                                })
                                .collect_view()
                        }}
                    </select>
                </label>
                <button
                    class="rounded bg-accent-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-accent-500 disabled:opacity-50"
                    disabled=move || busy.get()
                    on:click=add
                >
                    "Add column"
                </button>
            </div>
            // Flow rules, landing column and reset (ticket 225).
            {move || {
                let workflow = ctx.board.with(|b| b.as_ref().map(|b| b.workflow.clone()))?;
                let any = workflow.any;
                let label = {
                    let workflow = workflow.clone();
                    move |c: Column| workflow.label(c)
                };
                let backlogs: Vec<(String, String)> = workflow
                    .columns
                    .iter()
                    .filter(|i| i.column.is(Kind::Backlog))
                    .map(|i| (i.column.dir_name().to_string(), i.label.clone()))
                    .collect();
                let all: Vec<(String, String)> = workflow
                    .columns
                    .iter()
                    .map(|i| (i.column.dir_name().to_string(), i.label.clone()))
                    .collect();
                let landing = workflow.landing.dir_name().to_string();
                let move_from = RwSignal::new(String::new());
                let move_to = RwSignal::new(String::new());
                let standard = workflow.columns.len() == 4
                    && workflow.columns.iter().all(|i| Column::STANDARD.contains(&i.column))
                    && !any
                    && workflow.moves.is_empty()
                    && workflow.landing == Column::Backlog;
                let moving: usize = rows.with(|r| {
                    r.iter()
                        .filter(|(i, _)| !Column::STANDARD.contains(&i.column))
                        .map(|(_, held)| held)
                        .sum()
                });
                let confirming = RwSignal::new(false);
                Some(view! {
                    <div class="flex flex-col gap-2 border-t border-slate-200 pt-2 dark:border-slate-700">
                        <label class="flex items-center gap-2 text-sm">
                            "Moves"
                            <select
                                class=format!("{field} appearance-none")
                                aria-label="Which moves are allowed"
                                prop:value=if any { "any" } else { "next" }
                                on:change=move |ev| {
                                    let any = event_target_value(&ev) == "any";
                                    run(Box::pin(async move { api::set_flow(any).await }), "Flow saved.");
                                }
                            >
                                <option value="next">"Next column only, plus the moves below"</option>
                                <option value="any">"Any column to any column"</option>
                            </select>
                        </label>
                        {(!any)
                            .then(|| {
                                let label = label.clone();
                                let all = all.clone();
                                view! {
                                    <ul class="flex flex-wrap gap-2">
                                        {workflow
                                            .moves
                                            .iter()
                                            .map(|(from, to)| {
                                                let (from, to) = (*from, *to);
                                                view! {
                                                    <li class="flex items-center gap-1 rounded bg-slate-100 px-2 py-0.5 text-xs dark:bg-slate-800">
                                                        {format!("{} → {}", label(from), label(to))}
                                                        <button
                                                            class="text-slate-500 hover:text-red-600"
                                                            aria-label=format!("Stop allowing {} to {}", label(from), label(to))
                                                            disabled=move || busy.get()
                                                            on:click=move |_| {
                                                                run(Box::pin(async move { api::allow_move(from, to, false).await }), "Move no longer allowed.");
                                                            }
                                                        >
                                                            "×"
                                                        </button>
                                                    </li>
                                                }
                                            })
                                            .collect_view()}
                                    </ul>
                                    <div class="flex flex-wrap items-center gap-2 text-sm">
                                        "Also allow"
                                        <select class=format!("{field} appearance-none") aria-label="Move from" on:change=move |ev| move_from.set(event_target_value(&ev))>
                                            <option value="">"from…"</option>
                                            {all.iter().map(|(f, n)| view! { <option value=f.clone()>{n.clone()}</option> }).collect_view()}
                                        </select>
                                        "→"
                                        <select class=format!("{field} appearance-none") aria-label="Move to" on:change=move |ev| move_to.set(event_target_value(&ev))>
                                            <option value="">"to…"</option>
                                            {all.iter().map(|(f, n)| view! { <option value=f.clone()>{n.clone()}</option> }).collect_view()}
                                        </select>
                                        <button
                                            class=small
                                            disabled=move || busy.get() || move_from.with(String::is_empty) || move_to.with(String::is_empty)
                                            on:click=move |_| {
                                                let (Some(f), Some(t)) = (column_named(&move_from.get_untracked()), column_named(&move_to.get_untracked())) else {
                                                    return;
                                                };
                                                run(Box::pin(async move { api::allow_move(f, t, true).await }), "Move allowed.");
                                            }
                                        >
                                            "Allow"
                                        </button>
                                    </div>
                                }
                            })}
                        <label class="flex items-center gap-2 text-sm">
                            "New tickets land in"
                            <select
                                class=format!("{field} appearance-none")
                                aria-label="Where new tickets land"
                                prop:value=landing
                                on:change=move |ev| {
                                    if let Some(c) = column_named(&event_target_value(&ev)) {
                                        run(Box::pin(async move { api::set_landing(c).await }), "Landing column saved.");
                                    }
                                }
                            >
                                {backlogs.into_iter().map(|(f, n)| view! { <option value=f>{n}</option> }).collect_view()}
                            </select>
                        </label>
                        {(!standard)
                            .then(|| {
                                view! {
                                    <div class="flex flex-wrap items-center gap-2 text-sm">
                                        <Show
                                            when=move || confirming.get()
                                            fallback=move || {
                                                view! {
                                                    <button class=small disabled=move || busy.get() on:click=move |_| confirming.set(true)>
                                                        "Reset to standard"
                                                    </button>
                                                }
                                            }
                                        >
                                            <span>
                                                {format!(
                                                    "Back to Backlog, To do, Doing and Done? {moving} ticket(s) in added columns move to the standard column of the same kind.",
                                                )}
                                            </span>
                                            <button
                                                class=format!("{small} text-red-700 dark:text-red-300")
                                                disabled=move || busy.get()
                                                on:click=move |_| {
                                                    confirming.set(false);
                                                    run(Box::pin(async move { api::reset_columns().await }), "Columns reset to standard.");
                                                }
                                            >
                                                "Reset"
                                            </button>
                                            <button class=small on:click=move |_| confirming.set(false)>
                                                "Cancel"
                                            </button>
                                        </Show>
                                    </div>
                                }
                            })}
                    </div>
                })
            }}
            {move || note.get().map(|n| view! { <p class="text-xs">{n}</p> })}
        </section>
    }
}

/// Your colour scheme: light, dark or follow the system, and an accent
/// colour. Applied at once and saved in your own settings on this machine.
/// The board's WIP limit on doing: kept with the tickets in `.settings`,
/// so the whole team works to the same one.
#[component]
fn WipAdmin() -> impl IntoView {
    let ctx = Ctx::get();
    // The board's own repository, which a change is saved to.
    let current = move || {
        ctx.board.with(|b| {
            b.as_ref()
                .and_then(|b| b.repos.first())
                .map(|r| (r.name.clone(), r.wip))
        })
    };
    let initial = current().map(|(_, w)| w).unwrap_or_default();
    let doing = RwSignal::new(initial.doing.to_string());
    let note = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);

    let save = move |_| {
        let Some(d) = doing
            .get_untracked()
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|n| *n <= 99)
        else {
            note.set(Some(
                "Give the limit as a whole number from 0 to 99.".into(),
            ));
            return;
        };
        let repo = current().map(|(name, _)| name);
        busy.set(true);
        spawn_local(async move {
            match api::set_wip_limits(WipLimits { doing: d }, repo.as_deref()).await {
                Ok(report) => {
                    ctx.report(report);
                    note.set(Some("Limit saved.".into()));
                    ctx.reload();
                }
                Err(e) => note.set(Some(e)),
            }
            busy.set(false);
        });
    };
    let changed =
        move || current().is_some_and(|(_, w)| doing.with(|d| d.trim() != w.doing.to_string()));

    view! {
        <section class="flex flex-col gap-2">
            <h3 class="text-sm font-semibold">"WIP limit"</h3>
            <div class="flex flex-wrap items-center gap-4">
                <label class="flex items-center gap-2 text-sm">
                    "Doing"
                    <input
                        type="number"
                        min="0"
                        max="99"
                        class="w-20 rounded border border-slate-300 bg-slate-50 px-2 py-1 dark:border-slate-600 dark:bg-slate-900"
                        aria-label="Doing limit"
                        prop:value=move || doing.get()
                        on:input=move |ev| {
                            doing.set(event_target_value(&ev));
                            note.set(None);
                        }
                    />
                </label>
                <button
                    class="rounded bg-accent-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-accent-500 disabled:opacity-50"
                    disabled=move || busy.get() || !changed()
                    on:click=save
                >
                    "Save limit"
                </button>
                {move || note.get().map(|n| view! { <span class="text-sm">{n}</span> })}
            </div>
            <p class="text-xs text-slate-500">
                {format!(
                    "The most tickets Doing may hold at once ({} unless changed); 0 means no limit. To do has no limit, so work never gets stuck. A move into a full Doing asks before going ahead, and moving a ticket out is never blocked. Saved with the tickets, in .settings, and committed and pushed.",
                    rite_ui_model::wip::DEFAULT_LIMIT,
                )}
            </p>
        </section>
    }
}

/// The project's tag list (ticket 122): the listed tags with how many
/// tickets carry each, a form to add one (saved in UPPERCASE), a dropdown of
/// the unused ones to remove, and whether only listed tags may be used.
/// Kept with the tickets in `.settings`, like the WIP limit.
#[component]
fn TagsAdmin() -> impl IntoView {
    let ctx = Ctx::get();
    let tags = move || {
        ctx.board
            .with(|b| b.as_ref().map(|b| b.tags.clone()).unwrap_or_default())
    };
    let only = move || ctx.board.with(|b| b.as_ref().is_some_and(|b| b.tags_only));
    let listed = move || tags().into_iter().filter(|t| t.listed).collect::<Vec<_>>();
    let unused = move || {
        listed()
            .into_iter()
            .filter(|t| t.count == 0)
            .map(|t| t.name)
            .collect::<Vec<_>>()
    };
    let unlisted = move || {
        tags()
            .into_iter()
            .filter(|t| !t.listed && !t.reserved)
            .map(|t| format!("{} ({})", t.name, t.count))
            .collect::<Vec<_>>()
    };
    let new_tag = RwSignal::new(String::new());
    let to_remove = RwSignal::new(String::new());
    let note = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);

    let add = move || {
        let tag = new_tag.get_untracked().trim().to_uppercase();
        if tag.is_empty() {
            note.set(Some("Type a tag first.".into()));
            return;
        }
        busy.set(true);
        spawn_local(async move {
            match api::add_tag(&tag).await {
                Ok(report) => {
                    ctx.report(report);
                    note.set(Some(format!("Added {tag}.")));
                    new_tag.set(String::new());
                    ctx.reload();
                }
                Err(e) => note.set(Some(e)),
            }
            busy.set(false);
        });
    };
    let remove = move |_| {
        let tag = to_remove.get_untracked();
        if tag.is_empty() {
            return;
        }
        busy.set(true);
        spawn_local(async move {
            match api::remove_tag(&tag).await {
                Ok(report) => {
                    ctx.report(report);
                    note.set(Some(format!("Removed {tag}.")));
                    to_remove.set(String::new());
                    ctx.reload();
                }
                Err(e) => note.set(Some(e)),
            }
            busy.set(false);
        });
    };
    let set_only = move |on: bool| {
        busy.set(true);
        spawn_local(async move {
            match api::set_tags_only(on).await {
                Ok(report) => {
                    ctx.report(report);
                    ctx.reload();
                }
                Err(e) => note.set(Some(e)),
            }
            busy.set(false);
        });
    };
    let field = "rounded border border-slate-300 bg-slate-50 px-2 py-1 text-sm dark:border-slate-600 dark:bg-slate-900";
    let button = "rounded bg-accent-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-accent-500 disabled:opacity-50";

    view! {
        <section class="flex flex-col gap-2">
            <h3 class="text-sm font-semibold">"Tags"</h3>
            {move || {
                let all = listed();
                if all.is_empty() {
                    view! {
                        <p class="text-sm text-slate-500">
                            "No tags defined yet, so any tag can be used. Adding one starts the project's list."
                        </p>
                    }
                        .into_any()
                } else {
                    view! {
                        <ul class="flex flex-wrap gap-1.5" aria-label="Listed tags">
                            {all
                                .into_iter()
                                .map(|t| {
                                    let label = format!("{} · {}", t.name, t.count);
                                    view! {
                                        <li class="rounded bg-accent-100 px-1.5 text-sm text-accent-800 dark:bg-accent-900 dark:text-accent-200">
                                            {label}
                                        </li>
                                    }
                                })
                                .collect_view()}
                        </ul>
                    }
                        .into_any()
                }
            }}
            <div class="flex flex-wrap items-center gap-2">
                <input
                    type="text"
                    class=format!("{field} w-48 uppercase")
                    aria-label="New tag"
                    placeholder="New tag"
                    prop:value=move || new_tag.get()
                    on:input=move |ev| {
                        new_tag.set(event_target_value(&ev));
                        note.set(None);
                    }
                    on:keydown=move |ev| {
                        if ev.key() == "Enter" {
                            ev.prevent_default();
                            add();
                        }
                    }
                />
                <button
                    class=button
                    disabled=move || busy.get() || new_tag.with(|t| t.trim().is_empty())
                    on:click=move |_| add()
                >
                    "Add tag"
                </button>
            </div>
            <div class="flex flex-wrap items-center gap-2">
                <select
                    class=format!("{field} w-48 appearance-none")
                    aria-label="Unused tag to remove"
                    disabled=move || unused().is_empty()
                    prop:value=move || to_remove.get()
                    on:change=move |ev| to_remove.set(event_target_value(&ev))
                >
                    <option value="">
                        {move || {
                            if unused().is_empty() { "No unused tags" } else { "Choose an unused tag" }
                        }}
                    </option>
                    {move || {
                        unused()
                            .into_iter()
                            .map(|name| {
                                let value = name.clone();
                                view! { <option value=value>{name}</option> }
                            })
                            .collect_view()
                    }}
                </select>
                <button
                    class=button
                    disabled=move || busy.get() || to_remove.with(|t| t.is_empty())
                    on:click=remove
                >
                    "Remove"
                </button>
            </div>
            <label class="flex items-center gap-2 text-sm">
                <input
                    type="checkbox"
                    prop:checked=only
                    disabled=move || busy.get()
                    on:change=move |ev| set_only(event_target_checked(&ev))
                />
                "Prevent arbitrary tags"
            </label>
            {move || note.get().map(|n| view! { <p class="text-sm">{n}</p> })}
            {move || {
                let others = unlisted();
                (!others.is_empty())
                    .then(|| {
                        view! {
                            <p class="text-xs text-slate-500">
                                "In use but not listed: " {others.join(", ")}
                            </p>
                        }
                    })
            }}
            {move || {
                let built_in = tags()
                    .into_iter()
                    .filter(|t| t.reserved)
                    .map(|t| format!("{} ({})", t.name, t.count))
                    .collect::<Vec<_>>();
                (!built_in.is_empty())
                    .then(|| {
                        view! {
                            <p class="text-xs text-slate-500">
                                "Built in: " {built_in.join(", ")}
                                ". A ticket tagged DRAFT remains in the backlog until the DRAFT tag is removed. TRIAGE marks an imported GitHub issue nobody has sorted yet. Both are always allowed, and neither can be taken off the list."
                            </p>
                        }
                    })
            }}
            <p class="text-xs text-slate-500">
                "Tags are always UPPERCASE. A tag can only be removed once no ticket on the project's boards carries it. With \"Prevent arbitrary tags\" on (the default), only these tags can be used; off, any tag can be typed, and ones not on the list are flagged. DRAFT and TRIAGE are always on it, so with nothing else listed only they can be used. Saved with the tickets, in .settings, and committed and pushed."
            </p>
        </section>
    }
}

/// How long a ticket lock lasts on the board: kept with the tickets in
/// `.settings` (`lease-hours`), a team decision like the WIP limit.
/// The name people read for the project (ticket 326): the window title,
/// the header and the landing page show it; the project keeps its own name
/// for folders and `rite project use`.
#[component]
fn DisplayNameAdmin() -> impl IntoView {
    let ctx = Ctx::get();
    let project = move || {
        ctx.board
            .with(|b| b.as_ref().map(|b| b.project.clone()).unwrap_or_default())
    };
    let saved = move || {
        ctx.board.with(|b| {
            b.as_ref()
                .filter(|b| !b.shown.is_empty() && b.shown != b.project)
                .map(|b| b.shown.clone())
                .unwrap_or_default()
        })
    };
    let name = RwSignal::new(saved());
    let note = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let save = move |_| {
        let value = name.get_untracked().trim().to_string();
        busy.set(true);
        spawn_local(async move {
            match api::set_display_name(&value).await {
                Ok(report) => {
                    ctx.report(report);
                    note.set(Some(if value.is_empty() {
                        format!("The project's own name, {}, is shown.", project())
                    } else {
                        format!("{value} is shown for this project.")
                    }));
                    ctx.reload();
                }
                Err(e) => note.set(Some(e)),
            }
            busy.set(false);
        });
    };
    let field = "flex-1 rounded border border-slate-300 bg-slate-50 px-2 py-1 text-sm dark:border-slate-600 dark:bg-slate-900";
    let button = "rounded bg-accent-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-accent-500 disabled:opacity-50";
    view! {
        <section class="flex flex-col gap-2">
            <h3 class="text-sm font-semibold">"Display name"</h3>
            <p class="text-xs text-slate-500">
                {move || format!(
                    "The name everyone sees for the project: the window title, the header and the landing page. The project is still {} in folder names and `rite project use`. Leave it empty to show that.",
                    project()
                )}
            </p>
            <div class="flex items-center gap-2">
                <input
                    class=field
                    aria-label="Display name"
                    placeholder=move || format!("e.g. {}", project())
                    maxlength="60"
                    prop:value=move || name.get()
                    on:input=move |ev| name.set(event_target_value(&ev))
                />
                <button class=button disabled=move || busy.get() on:click=save>
                    "Save"
                </button>
            </div>
            <Show when=move || note.with(Option::is_some)>
                <p class="text-xs text-slate-600 dark:text-slate-300">{move || note.get().unwrap_or_default()}</p>
            </Show>
        </section>
    }
}

/// The documents repository every project shares (ticket 129), shown in
/// Documents beside the project's own.
#[component]
fn DocumentsAdmin() -> impl IntoView {
    let ctx = Ctx::get();
    let saved = move || {
        ctx.board.with(|b| {
            b.as_ref()
                .map(|b| b.shared_documents.clone())
                .unwrap_or_default()
        })
    };
    let name = RwSignal::new(saved());
    let note = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let save = move |_| {
        let value = name.get_untracked().trim().to_string();
        busy.set(true);
        spawn_local(async move {
            match api::set_shared_documents(&value).await {
                Ok(report) => {
                    ctx.report(report);
                    note.set(Some(if value.is_empty() {
                        "No shared documents repository.".into()
                    } else {
                        format!("Documents shows {value} beside the project's own.")
                    }));
                    ctx.reload();
                }
                Err(e) => note.set(Some(e)),
            }
            busy.set(false);
        });
    };
    let field = "flex-1 rounded border border-slate-300 bg-slate-50 px-2 py-1 text-sm dark:border-slate-600 dark:bg-slate-900";
    let button = "rounded bg-accent-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-accent-500 disabled:opacity-50";
    view! {
        <section class="flex flex-col gap-2">
            <h3 class="text-sm font-semibold">"Shared documents"</h3>
            <p class="text-xs text-slate-500">
                "A documents repository every project reads, such as the organisation's standards, shown in Documents beside this project's own. Its name, as cloned beside the project's repositories. Leave it empty for none."
            </p>
            <div class="flex items-center gap-2">
                <input
                    class=field
                    aria-label="Shared documents repository"
                    placeholder="e.g. acme-documents"
                    prop:value=move || name.get()
                    on:input=move |ev| name.set(event_target_value(&ev))
                />
                <button class=button disabled=move || busy.get() on:click=save>
                    "Save"
                </button>
            </div>
            <Show when=move || note.with(Option::is_some)>
                <p class="text-xs text-slate-600 dark:text-slate-300">{move || note.get().unwrap_or_default()}</p>
            </Show>
        </section>
    }
}

/// Archiving the project (tickets 157, 263): no longer maintained, its
/// repositories archived on GitHub and the project taken off rite on this
/// machine, so the board goes back to the landing page. Asks once more
/// before archiving.
#[component]
fn ArchiveAdmin() -> impl IntoView {
    let ctx = Ctx::get();
    let archived = move || ctx.board.with(|b| b.as_ref().is_some_and(|b| b.archived));
    let confirm = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let note = RwSignal::new(None::<String>);
    let run = move |_| {
        let on = !archived();
        if on && !confirm.get_untracked() {
            confirm.set(true);
            return;
        }
        let Some(root) = ctx
            .board
            .with_untracked(|b| b.as_ref().map(|b| b.tasks_dir.clone()))
        else {
            return;
        };
        busy.set(true);
        spawn_local(async move {
            match api::set_project_archived(&root, on).await {
                Ok(report) => {
                    note.set(report.warning.clone());
                    let forgotten = on && report.warning.is_none();
                    ctx.report(report);
                    confirm.set(false);
                    if forgotten {
                        // Off rite here now: back to the landing page.
                        ctx.overlay.set(None);
                        ctx.view.set(crate::app::View::Home);
                        crate::home::load_home(ctx.home);
                    } else {
                        ctx.reload();
                    }
                }
                Err(e) => note.set(Some(e)),
            }
            busy.set(false);
        });
    };
    let project = move || {
        ctx.board
            .with(|b| b.as_ref().map(|b| b.project.clone()).unwrap_or_default())
    };
    view! {
        <section class="flex flex-col gap-2">
            <h3 class="text-sm font-semibold">
                {move || if archived() { "This project is archived" } else { "Archive this project" }}
            </h3>
            <p class="text-xs text-slate-500">
                {move || if archived() {
                    "No longer maintained: read-only for everyone, and off My work and the landing page. Unarchive brings it back, its repositories on GitHub too.".to_string()
                } else {
                    format!(
                        "For a project that's no longer maintained. {p} becomes read-only for everyone and leaves My work and the landing page; its repositories are archived on GitHub (tickets, documents and declared code repositories), and it's taken off rite on this machine, its folders kept. Committed for the whole team. `rite init {p}` brings it back.",
                        p = project(),
                    )
                }}
            </p>
            <div class="flex items-center gap-2">
                <button
                    class=move || if archived() || !confirm.get() {
                        "rounded border border-slate-300 px-3 py-1.5 text-sm hover:bg-slate-100 disabled:opacity-50 dark:border-slate-600 dark:hover:bg-slate-700"
                    } else {
                        "rounded bg-red-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-red-500 disabled:opacity-50"
                    }
                    disabled=move || busy.get()
                    on:click=run
                >
                    {move || if archived() {
                        "Unarchive"
                    } else if confirm.get() {
                        "Confirm: archive it"
                    } else {
                        "Archive"
                    }}
                </button>
                <Show when=move || confirm.get() && !archived()>
                    <button class="text-sm underline" on:click=move |_| confirm.set(false)>"Cancel"</button>
                </Show>
            </div>
            <Show when=move || note.with(Option::is_some)>
                <p class="text-xs text-amber-700 dark:text-amber-300">{move || note.get().unwrap_or_default()}</p>
            </Show>
        </section>
    }
}

/// Auto priority (ticket 215): priorities set from requires links, and
/// whether a priority set by hand wins.
#[component]
fn PriorityAdmin() -> impl IntoView {
    let ctx = Ctx::get();
    let auto = move || {
        ctx.board
            .with(|b| b.as_ref().is_some_and(|b| b.auto_priority))
    };
    let manual = move || {
        ctx.board
            .with(|b| b.as_ref().is_none_or(|b| b.priority_override))
    };
    // How long a ticket should take in todo and doing (ticket 252).
    let saved_limits = move || {
        ctx.board
            .with(|b| b.as_ref().map_or((0, 0), |b| b.time_limits))
    };
    let todo_days = RwSignal::new(saved_limits().0);
    let doing_days = RwSignal::new(saved_limits().1);
    let note = RwSignal::new(None::<String>);
    let apply = move |result: Result<rite_ui_model::SyncReport, String>| match result {
        Ok(report) => {
            ctx.report(report);
            note.set(None);
            ctx.reload();
        }
        Err(e) => note.set(Some(e)),
    };
    view! {
        <section class="flex flex-col gap-2">
            <h3 class="text-sm font-semibold">"Auto priority"</h3>
            <p class="text-xs text-slate-500">
                "A ticket becomes at least as urgent as the most urgent ticket that requires it, down a chain, so the priorities on your goals flow to the work they wait on; a child is one step below its parent; and it rises as its due date nears. Raised tickets say why, and switching it off puts them back."
            </p>
            <label class="flex items-center gap-2 text-sm font-semibold">
                <input
                    type="checkbox"
                    prop:checked=auto
                    on:change=move |ev| {
                        let on = event_target_checked(&ev);
                        spawn_local(async move { apply(api::set_auto_priority(on).await) });
                    }
                />
                "Set priorities from requires and parent links, due dates and time limits"
            </label>
            <p class="text-xs text-slate-500">
                "A ticket with a due date rises a step each quarter of the time from its creation to its due date: medium from 25%, high from 50%, top from 75%, so top once it's overdue."
            </p>
            <div class="flex flex-wrap items-center gap-2 text-sm">
                <span>"A ticket should take at most"</span>
                <input
                    type="number"
                    min="0"
                    class="w-16 rounded border border-slate-400 bg-transparent px-1"
                    aria-label="Days in todo"
                    prop:value=move || todo_days.get().to_string()
                    on:input=move |ev| todo_days.set(event_target_value(&ev).parse().unwrap_or(0))
                />
                <span>"days in todo and"</span>
                <input
                    type="number"
                    min="0"
                    class="w-16 rounded border border-slate-400 bg-transparent px-1"
                    aria-label="Days in doing"
                    prop:value=move || doing_days.get().to_string()
                    on:input=move |ev| doing_days.set(event_target_value(&ev).parse().unwrap_or(0))
                />
                <span>"days in doing."</span>
                <button
                    class="rounded border border-slate-400 px-2 py-0.5 text-xs disabled:opacity-50"
                    disabled=move || (todo_days.get(), doing_days.get()) == saved_limits()
                    on:click=move |_| {
                        let (t, d) = (todo_days.get_untracked(), doing_days.get_untracked());
                        spawn_local(async move { apply(api::set_time_limits(t, d).await) });
                    }
                >
                    "Save"
                </button>
            </div>
            <p class="text-xs text-slate-500">
                "Each quarter of that a ticket uses raises it a step: medium from 25%, high from 50%, top from 75%. Its time in todo keeps counting once someone picks it up, so an urgent ticket stays urgent. 0 is no limit."
            </p>
            <label class="flex items-center gap-2 text-sm font-semibold">
                <input
                    type="checkbox"
                    prop:checked=manual
                    on:change=move |ev| {
                        let on = event_target_checked(&ev);
                        spawn_local(async move { apply(api::set_priority_override(on).await) });
                    }
                />
                "Allow override: a priority set by hand stays"
            </label>
            <p class="text-xs text-slate-500">
                "On: choosing a priority yourself pins that ticket, and Auto in its priority hands it back. Off: a priority you choose is only a floor; auto priority can still raise it."
            </p>
            <Show when=move || note.with(Option::is_some)>
                <p class="text-xs text-red-700 dark:text-red-300">{move || note.get().unwrap_or_default()}</p>
            </Show>
        </section>
    }
}

/// Due dates on the board (ticket 253): how near a due date sends a ticket
/// straight to todo, and whether the backlog column can be folded away.
#[component]
fn DueDatesAdmin() -> impl IntoView {
    let ctx = Ctx::get();
    let saved = move || {
        ctx.board
            .with(|b| b.as_ref().map_or(3, |b| b.due_to_todo_days))
    };
    let days = RwSignal::new(saved());
    let shown = move || {
        ctx.board
            .with(|b| b.as_ref().is_some_and(|b| b.backlog_always_shown))
    };
    let note = RwSignal::new(None::<String>);
    let apply = move |result: Result<rite_ui_model::SyncReport, String>| match result {
        Ok(report) => {
            ctx.report(report);
            note.set(None);
            ctx.reload();
        }
        Err(e) => note.set(Some(e)),
    };
    view! {
        <section class="flex flex-col gap-2">
            <h3 class="text-sm font-semibold">"Due dates and the backlog"</h3>
            <div class="flex flex-wrap items-center gap-2 text-sm">
                <span>"A ticket due within"</span>
                <input
                    type="number"
                    min="0"
                    class="w-16 rounded border border-slate-400 bg-transparent px-1"
                    aria-label="Days before its due date"
                    prop:value=move || days.get().to_string()
                    on:input=move |ev| days.set(event_target_value(&ev).parse().unwrap_or(0))
                />
                <span>"days goes straight to todo."</span>
                <button
                    class="rounded border border-slate-400 px-2 py-0.5 text-xs disabled:opacity-50"
                    disabled=move || days.get() == saved()
                    on:click=move |_| {
                        let d = days.get_untracked();
                        spawn_local(async move { apply(api::set_due_to_todo_days(d).await) });
                    }
                >
                    "Save"
                </button>
            </div>
            <p class="text-xs text-slate-500">
                "When it's made or given its due date, from the loose backlog, unless it's a draft. Other tickets with due dates sit at the top of the backlog, soonest first. A ticket with a due date can't go in an epic; a sprint holding one shows a warning. 0 never sends one straight to todo."
            </p>
            <label class="flex items-center gap-2 text-sm font-semibold">
                <input
                    type="checkbox"
                    prop:checked=shown
                    on:change=move |ev| {
                        let on = event_target_checked(&ev);
                        spawn_local(async move { apply(api::set_backlog_always_shown(on).await) });
                    }
                />
                "Always show the backlog: it can't be folded away"
            </label>
            <Show when=move || note.with(Option::is_some)>
                <p class="text-xs text-red-700 dark:text-red-300">{move || note.get().unwrap_or_default()}</p>
            </Show>
        </section>
    }
}

/// Open GitHub issues into the backlog, tagged TRIAGE (ticket 158): Preview
/// says what would come in, Import brings it in as one commit.
#[component]
fn IssuesAdmin() -> impl IntoView {
    let ctx = Ctx::get();
    // The landing page pulls issues in by itself while this is on (ticket
    // 173); Preview and Check now are for not waiting.
    let auto = move || {
        ctx.board
            .with(|b| b.as_ref().is_none_or(|b| b.import_issues))
    };
    let auto_note = RwSignal::new(None::<String>);
    let set_auto = move |on: bool| {
        spawn_local(async move {
            match api::set_import_issues(on).await {
                Ok(report) => {
                    ctx.report(report);
                    auto_note.set(None);
                    ctx.reload();
                }
                Err(e) => auto_note.set(Some(e)),
            }
        });
    };
    let result = RwSignal::new(None::<Result<rite_ui_model::IssueImport, String>>);
    let result_node = NodeRef::<leptos::html::Div>::new();
    Effect::new(move |_| {
        if result.with(Option::is_some)
            && let Some(el) = result_node.get()
        {
            el.scroll_into_view_with_bool(false);
        }
    });
    let busy = RwSignal::new(false);
    let run = move |preview: bool| {
        busy.set(true);
        spawn_local(async move {
            let outcome = api::import_issues("", "", preview).await;
            if let Ok(done) = &outcome
                && let Some(sync) = done.sync.clone()
            {
                ctx.report(sync);
                ctx.reload();
            }
            result.set(Some(outcome));
            busy.set(false);
        });
    };
    let secondary = "rounded border border-slate-300 px-3 py-1.5 text-sm hover:bg-slate-100 disabled:opacity-50 dark:border-slate-600 dark:hover:bg-slate-700";
    let primary = "rounded bg-accent-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-accent-500 disabled:opacity-50";
    view! {
        <section class="flex flex-col gap-2">
            <h3 class="text-sm font-semibold">"GitHub issues"</h3>
            <p class="text-xs text-slate-500">
                "Open issues on the project's GitHub repositories come into the backlog tagged TRIAGE, with a link back to the issue. A label becomes a tag when it's on the tag list. An issue is only ever imported once: abandon its ticket to turn it down. Reads GitHub through the gh CLI."
            </p>
            <label class="flex items-center gap-2 text-sm font-semibold">
                <input
                    type="checkbox"
                    prop:checked=auto
                    on:change=move |ev| set_auto(event_target_checked(&ev))
                />
                "Pull in open issues when the landing page opens"
            </label>
            <Show when=move || auto_note.with(Option::is_some)>
                <p class="text-xs text-red-700 dark:text-red-300">{move || auto_note.get().unwrap_or_default()}</p>
            </Show>
            <div class="flex flex-wrap items-center gap-2">
                <button class=secondary disabled=move || busy.get() on:click=move |_| run(true)>
                    "Preview"
                </button>
                <button class=primary disabled=move || busy.get() on:click=move |_| run(false)>
                    "Check now"
                </button>
            </div>
            // What Preview or Import found, right under their buttons
            // (ticket 166), and scrolled to: the panel scrolls, and the
            // buttons can sit at its bottom edge.
            <div node_ref=result_node>
                {move || {
                    result
                        .get()
                        .map(|outcome| match outcome {
                            Ok(done) => {
                                view! {
                                    <div class="flex flex-col gap-1">
                                        <ul class="flex flex-col gap-0.5 font-mono text-xs">
                                            {done.lines.into_iter().map(|line| view! { <li>{line}</li> }).collect_view()}
                                        </ul>
                                        <p class="text-sm text-slate-700 dark:text-slate-200">{done.summary}</p>
                                    </div>
                                }
                                    .into_any()
                            }
                            Err(e) => view! { <p class="text-sm text-red-700 dark:text-red-300">{e}</p> }.into_any(),
                        })
                }}
            </div>
            <p class="text-xs text-slate-500">"They come in at top priority, as every TRIAGE ticket is, and head everyone's My work until they're triaged."</p>
        </section>
    }
}

#[component]
fn LeaseAdmin() -> impl IntoView {
    let ctx = Ctx::get();
    let current = move || {
        ctx.board.with(|b| {
            b.as_ref()
                .and_then(|b| b.repos.first())
                .map(|r| (r.name.clone(), r.lease_hours))
        })
    };
    let initial = current().map_or(rite_ui_model::DEFAULT_LEASE_HOURS, |(_, h)| h);
    let hours = RwSignal::new(initial.to_string());
    let note = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);

    let save = move |_| {
        let Some(h) = hours
            .get_untracked()
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|h| (1..=168).contains(h))
        else {
            note.set(Some("Give a whole number of hours from 1 to 168.".into()));
            return;
        };
        let repo = current().map(|(name, _)| name);
        busy.set(true);
        spawn_local(async move {
            match api::set_lease_hours(h, repo.as_deref()).await {
                Ok(report) => {
                    ctx.report(report);
                    note.set(Some("Saved.".into()));
                    ctx.reload();
                }
                Err(e) => note.set(Some(e)),
            }
            busy.set(false);
        });
    };
    let changed = move || current().is_some_and(|(_, h)| hours.with(|v| v.trim() != h.to_string()));

    view! {
        <section class="flex flex-col gap-2">
            <h3 class="text-sm font-semibold">"Ticket locks"</h3>
            <div class="flex flex-wrap items-center gap-4">
                <label class="flex items-center gap-2 text-sm">
                    "A lock lasts"
                    <input
                        type="number"
                        min="1"
                        max="168"
                        class="w-20 rounded border border-slate-300 bg-slate-50 px-2 py-1 dark:border-slate-600 dark:bg-slate-900"
                        aria-label="Lock length in hours"
                        prop:value=move || hours.get()
                        on:input=move |ev| {
                            hours.set(event_target_value(&ev));
                            note.set(None);
                        }
                    />
                    "hours"
                </label>
                <button
                    class="rounded bg-accent-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-accent-500 disabled:opacity-50"
                    disabled=move || busy.get() || !changed()
                    on:click=save
                >
                    "Save"
                </button>
                {move || note.get().map(|n| view! { <span class="text-sm">{n}</span> })}
            </div>
            <p class="text-xs text-slate-500">
                {format!(
                    "Moving a ticket into To do or Doing locks it for you ({} hours unless changed); lock and unlock from a ticket's editor. While someone else's lock lasts, the board will not change their ticket. Saved with the tickets, in .settings, and committed and pushed.",
                    rite_ui_model::DEFAULT_LEASE_HOURS,
                )}
            </p>
        </section>
    }
}
