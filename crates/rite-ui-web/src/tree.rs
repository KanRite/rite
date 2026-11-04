//! The requires tree around a ticket (ticket 214), as `rite tree <ref>`
//! draws it, centred on the ticket (ticket 373): what waits on it above,
//! the ticket, then what it needs below, with where each is and what it
//! still waits on. Related links aren't drawn. A click on a ticket goes to
//! its card. Done tickets are left out until "Include done" is ticked
//! (ticket 217). With `children` it's parent and child instead (ticket
//! 261): the ticket's ancestors and everything under it.

use leptos::prelude::*;
use leptos::task::spawn_local;
use rite_ui_model::epic::Place;
use rite_ui_model::tree::{Edge, TreeNode};

use crate::api;
use crate::app::{Ctx, Modal, ModalClose};

#[component]
pub fn TreePanel(place: Place, file: String, children: bool) -> impl IntoView {
    let trees = RwSignal::new(None::<Result<Vec<TreeNode>, String>>);
    let include_done = RwSignal::new(false);
    let repo = place.repo.clone();
    Effect::new(move |_| {
        let done = include_done.get();
        let (place, file) = (place.clone(), file.clone());
        spawn_local(async move {
            let got = if children {
                api::ticket_hierarchy(&place, &file, done).await
            } else {
                api::ticket_family(&place, &file, done).await
            };
            // A later tick may have answered first: keep only the current one.
            if include_done.get_untracked() == done {
                trees.set(Some(got));
            }
        });
    });
    view! {
        <Modal
            title=if children { "Parent and children" } else { "Requires" }.to_string()
            width="max-w-4xl"
        >
            <p class="text-xs text-slate-500">
                <span>"Click a ticket to go to its card."</span>
                <label class="ml-4 inline-flex items-center gap-1">
                    <input
                        type="checkbox"
                        prop:checked=move || include_done.get()
                        on:change=move |ev| include_done.set(event_target_checked(&ev))
                    />
                    "Include done"
                </label>
            </p>
            <div class="min-h-0 overflow-y-auto">
                {move || match trees.get() {
                    None => view! { <p class="text-sm text-slate-500">"Reading the board…"</p> }.into_any(),
                    Some(Err(e)) => view! { <p class="text-sm text-red-700 dark:text-red-300">{e}</p> }.into_any(),
                    Some(Ok(list)) if list.is_empty() => {
                        let says = match (children, include_done.get_untracked()) {
                            (true, _) => "This ticket has no parent and no children.",
                            (false, true) => "This ticket requires nothing, and nothing requires it.",
                            (false, false) => {
                                "Everything this ticket requires or is required by is done. Tick Include done to see it."
                            }
                        };
                        view! { <p class="text-sm text-slate-500">{says}</p> }.into_any()
                    }
                    Some(Ok(mut list)) if !children && list.len() == 1 && list[0].current => {
                        around(list.remove(0), repo.clone())
                    }
                    Some(Ok(list)) => {
                        let repo = repo.clone();
                        view! {
                            <ul class="flex flex-col gap-3">
                                {list.into_iter().map(|n| node(n, repo.clone(), false)).collect_view()}
                            </ul>
                        }
                            .into_any()
                    }
                }}
            </div>
            <ModalClose />
        </Modal>
    }
}

/// The tree centred on the ticket (ticket 373): "Waits on" and what
/// requires it, going up; the ticket; then "Needs" and what it requires,
/// going down.
fn around(mut ticket: TreeNode, repo: String) -> AnyView {
    let (up, down): (Vec<TreeNode>, Vec<TreeNode>) = std::mem::take(&mut ticket.children)
        .into_iter()
        .partition(|c| c.edge == Edge::WaitsOn);
    let id = ticket.id.clone();
    let section = move |heading: String, nodes: Vec<TreeNode>, repo: String| {
        let count = nodes.len();
        (count > 0).then(|| {
            view! {
                <section class="flex flex-col">
                    <h3 class="px-2 text-xs font-semibold uppercase text-slate-500">{heading}</h3>
                    <ul class="mt-0.5 flex flex-col">
                        {nodes
                            .into_iter()
                            .enumerate()
                            .map(|(i, n)| node(n, repo.clone(), i + 1 == count))
                            .collect_view()}
                    </ul>
                </section>
            }
        })
    };
    view! {
        <div class="flex flex-col gap-3">
            {section(format!("Waits on {id}"), up, repo.clone())}
            <ul>{node(ticket, repo.clone(), false)}</ul>
            {section(format!("{id} needs"), down, repo)}
        </div>
    }
    .into_any()
}

/// One ticket and, below it, what hangs from it. `last` stops its line
/// halfway down, as the last of its siblings.
fn node(n: TreeNode, repo: String, last: bool) -> AnyView {
    let ctx = Ctx::get();
    let is_child = n.edge != Edge::Root;
    let target = ctx.board.with(|b| {
        let workflow = b.as_ref().map(|b| b.workflow.clone()).unwrap_or_default();
        n.place_in(&repo, &workflow)
    });
    let file = n.file.clone();
    let muted = n.seen || n.stub.is_some();
    let label = if n.seen {
        format!("↑ {} {} (above)", n.id, n.title)
    } else {
        format!("{} {}", n.id, n.title)
    };
    let place = match &n.stub {
        Some(sprint) => format!("in started sprint {sprint}"),
        None => n.place.clone(),
    };
    let row_tone = if n.current {
        "bg-accent-100 ring-2 ring-accent-500 dark:bg-accent-950"
    } else {
        "hover:bg-slate-100 dark:hover:bg-slate-700"
    };
    let children = n.children;
    let count = children.len();
    view! {
        <li class=if is_child { "relative pl-7" } else { "relative" }>
            {is_child
                .then(|| {
                    view! {
                        // The line down from the parent, and across to this row.
                        <span
                            aria-hidden="true"
                            class=format!("absolute left-2 top-0 w-0.5 bg-slate-400 {}", if last { "h-3.5" } else { "h-full" })
                        ></span>
                        <span
                            aria-hidden="true"
                            class="absolute left-2 top-3.5 h-0.5 w-4 bg-slate-400"
                        ></span>
                    }
                })}
            <button
                type="button"
                class=format!(
                    "flex w-full flex-wrap items-baseline gap-x-2 rounded px-2 py-1 text-left text-sm {row_tone} {}",
                    if muted { "text-slate-400 dark:text-slate-500" } else { "" },
                )
                title=if n.seen { "Drawn above: go to its card" } else { "Go to its card" }
                aria-current=n.current.then_some("true")
                on:click=move |_| {
                    ctx.close_overlay();
                    ctx.show_card(&target, &file);
                }
            >
                <span class=if muted { "" } else { "font-medium" }>{label}</span>
                <span class="text-xs text-slate-500">{place}</span>
                {n.done.then(|| view! { <span class="text-xs text-emerald-700 dark:text-emerald-300" title="Done">"✓"</span> })}
                {(!n.unmet.is_empty() && !n.seen)
                    .then(|| {
                        view! {
                            <span class="rounded border border-orange-400 px-1 text-xs text-orange-700 dark:border-orange-500 dark:text-orange-300">
                                {format!("waits on {}", n.unmet.join(", "))}
                            </span>
                        }
                    })}
            </button>
            {(count > 0)
                .then(|| {
                    view! {
                        <ul class="mt-0.5 flex flex-col">
                            {children
                                .into_iter()
                                .enumerate()
                                .map(|(i, c)| node(c, repo.clone(), i + 1 == count))
                                .collect_view()}
                        </ul>
                    }
                })}
        </li>
    }
    .into_any()
}
