//! The count of open tickets, then how many are active (in todo or doing,
//! ticket 358), then the top and TRIAGE counts in red (ticket 349): in the
//! board's header, and on each project's row on the landing page.
//!
//! Open means not in a done column. A TRIAGE ticket is always top, so each is
//! in both red counts: two readings of the same urgent work. A kind with none
//! shows nothing (no red "0"), and the colour is never the only sign: each
//! number says what it is, and says it again on hover.

use leptos::prelude::*;
use leptos::task::spawn_local;
use rite_ui_model::TicketCounts;

use crate::api;
use crate::app::{Ctx, View};

/// The numbers, as `42 open  5 active  7 top  3 triage`.
pub fn counts_view(counts: TicketCounts) -> impl IntoView {
    view! {
        <span
            class="flex shrink-0 items-baseline gap-2 whitespace-nowrap text-xs"
            title="Open tickets: every ticket not done. Top and TRIAGE are counted among them, and a TRIAGE ticket is always top"
        >
            <span class="text-slate-600 dark:text-slate-300">{format!("{} open", counts.open)}</span>
            <span
                class="text-slate-600 dark:text-slate-300"
                title="Open tickets in a to do or doing column: being worked on, or next"
            >
                {format!("{} active", counts.active)}
            </span>
            {(counts.top > 0)
                .then(|| {
                    view! {
                        <span
                            class="font-semibold text-red-700 dark:text-red-400"
                            title="Open tickets of top priority"
                        >
                            {format!("{} top", counts.top)}
                        </span>
                    }
                })}
            {(counts.triage > 0)
                .then(|| {
                    view! {
                        <span
                            class="font-semibold text-red-700 dark:text-red-400"
                            title="Open tickets waiting to be triaged (tagged TRIAGE)"
                        >
                            {format!("{} triage", counts.triage)}
                        </span>
                    }
                })}
        </span>
    }
}

/// In the board's header, from the board the page already holds: the whole
/// project, so a search or a filter narrowing the cards doesn't change it.
/// Only while a board is on screen.
#[component]
pub fn BoardCounts() -> impl IntoView {
    let ctx = Ctx::get();
    move || {
        if ctx.view.get() != View::Board {
            return None;
        }
        ctx.board
            .with(|b| b.as_ref().map(|b| counts_view(b.counts())))
    }
}

/// On a project's row on the landing page: read for that project when the
/// row is drawn (its boards' ticket files, no git history).
#[component]
pub fn ProjectCounts(root: String) -> impl IntoView {
    let counts = RwSignal::new(None::<TicketCounts>);
    let root = StoredValue::new(root);
    Effect::new(move |_| {
        spawn_local(async move {
            if let Ok(read) = api::project_counts(&root.get_value()).await {
                counts.set(Some(read));
            }
        });
    });
    move || counts.get().map(counts_view)
}
