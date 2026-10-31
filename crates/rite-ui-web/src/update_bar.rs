//! The update bar (ticket 192): across the top of every screen when a newer
//! KanRite is out, in orange, a colour no accent uses. **Update** installs
//! it and restarts riteui (ticket 191), after asking for an open ticket to be
//! closed so nothing typed is lost; an install riteui doesn't replace says
//! why and how instead. **Later** hides that version for a day, across
//! restarts; a still newer release shows at once.
//!
//! When the project's team requires a newer KanRite (ticket 193), the bar
//! is the **required** kind: the board is read-only here, and there's
//! Update but no Later.
//!
//! When riteui's own program is replaced while it runs (`rite update` in a
//! terminal, the installer, a package manager: ticket 208), the bar says so
//! instead, with **Restart**. Nothing restarts without the click.

use leptos::prelude::*;
use leptos::task::spawn_local;
use rite_ui_model::{ManualCheck, UpdateResult};

use crate::api;
use crate::app::{Ctx, Overlay};

/// A check for updates asked for by hand (ticket 304), shared by the top
/// bar's version and Preferences: what it found goes in `note`, and a newer
/// release goes to the update bar.
pub fn check_by_hand(note: RwSignal<Option<String>>) {
    let ctx = Ctx::get();
    note.set(Some("Checking for updates…".to_string()));
    spawn_local(async move {
        let said = match api::check_for_update().await {
            Ok(ManualCheck::Newer { info }) => {
                let said = format!(
                    "KanRite {} is out: Update is in the bar above.",
                    info.version
                );
                ctx.update_offer.set(Some(info));
                said
            }
            Ok(ManualCheck::Current { current }) => {
                format!("You're up to date (KanRite {current}).")
            }
            Ok(ManualCheck::Failed { why }) => why,
            Err(why) => why,
        };
        note.set(Some(said));
    });
}

/// The version in the top bar (ticket 240), which checks for updates when
/// clicked (ticket 304); what it found shows beside it for a few seconds.
#[component]
pub fn VersionCheck() -> impl IntoView {
    let note = RwSignal::new(None::<String>);
    // Each check's note clears itself, unless a newer click replaced it.
    let shown = StoredValue::new(0_u32);
    let click = move |_| {
        shown.update_value(|n| *n = n.wrapping_add(1));
        let this = shown.get_value();
        check_by_hand(note);
        set_timeout(
            move || {
                if shown.get_value() == this {
                    note.set(None);
                }
            },
            std::time::Duration::from_secs(8),
        );
    };
    // The answer floats under the version, so the header doesn't move.
    view! {
        <span class="relative">
            <button
                type="button"
                class="rounded text-xs font-medium text-slate-500 hover:text-slate-300 hover:underline"
                title="The version of KanRite running. Click to check for updates."
                on:click=click
            >
                {concat!("v", env!("CARGO_PKG_VERSION"))}
            </button>
            {move || note.get().map(|n| view! {
                <span
                    role="status"
                    class="absolute left-0 top-full z-50 mt-1 whitespace-nowrap rounded border border-slate-300 bg-white px-2 py-1 text-xs text-slate-700 shadow dark:border-slate-600 dark:bg-slate-800 dark:text-slate-200"
                >
                    {n}
                </span>
            })}
        </span>
    }
}

/// Where Update has got to.
#[derive(Clone, Debug, PartialEq)]
enum Phase {
    Idle,
    /// A ticket is open in the editor: close it first.
    EditorOpen,
    Working,
    Updated(String),
    /// Not an install riteui replaces: why, and the installer's command when
    /// that's the way.
    Unmanaged(String, Option<String>),
    Failed {
        why: String,
        now: String,
    },
}

/// How often the page asks again. Asking riteui is local; it asks GitHub only
/// every `update::CHECK_EVERY` (three hours), so this just keeps the bar from
/// lagging that by hours more (ticket 285).
const RECHECK: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// How often riteui's own program is looked at: one `stat` (ticket 208).
const BINARY_CHECK: std::time::Duration = std::time::Duration::from_secs(5);

#[component]
pub fn UpdateBar() -> impl IntoView {
    let ctx = Ctx::get();
    let offer = ctx.update_offer;
    let phase = RwSignal::new(Phase::Idle);
    let update = move || {
        if ctx
            .overlay
            .with_untracked(|o| matches!(o, Some(Overlay::Editor(_))))
        {
            phase.set(Phase::EditorOpen);
            return;
        }
        phase.set(Phase::Working);
        spawn_local(async move {
            match api::apply_update().await {
                Ok(UpdateResult::Updated { version }) => {
                    phase.set(Phase::Updated(version));
                    let _ = api::restart_app().await;
                }
                Ok(UpdateResult::Unmanaged { why, install }) => {
                    phase.set(Phase::Unmanaged(why, install))
                }
                Ok(UpdateResult::Failed { why, now }) => phase.set(Phase::Failed { why, now }),
                Err(why) => phase.set(Phase::Failed {
                    why,
                    now: String::new(),
                }),
            }
        });
    };
    let ask = move || {
        spawn_local(async move {
            // Silent on failure: no network is nothing to report.
            if let Ok(found) = api::update_status().await {
                offer.set(found);
            }
        });
    };
    ask();
    let _ = set_interval_with_handle(ask, RECHECK);
    // riteui's program replaced under it (ticket 208).
    let replaced = RwSignal::new(None::<String>);
    let restart_note = RwSignal::new(None::<String>);
    let _ = set_interval_with_handle(
        move || {
            if replaced.with_untracked(Option::is_some) {
                return;
            }
            spawn_local(async move {
                if let Ok(Some(version)) = api::binary_changed().await {
                    replaced.set(Some(version));
                }
            });
        },
        BINARY_CHECK,
    );
    let restart = move |_| {
        if ctx
            .overlay
            .with_untracked(|o| matches!(o, Some(Overlay::Editor(_))))
        {
            restart_note.set(Some(
                "Close the ticket you're editing first, so nothing you've typed is lost, then Restart.".into(),
            ));
            return;
        }
        restart_note.set(Some("Restarting…".into()));
        spawn_local(async move {
            if let Err(e) = api::restart_app().await {
                restart_note.set(Some(e));
            }
        });
    };
    let later = move |_| {
        let Some(version) = offer.with_untracked(|o| o.as_ref().map(|o| o.version.clone())) else {
            return;
        };
        offer.set(None);
        phase.set(Phase::Idle);
        spawn_local(async move {
            let _ = api::snooze_update(&version).await;
        });
    };
    let notes = move |_| {
        if let Some(url) = offer.with_untracked(|o| o.as_ref().map(|o| o.notes_url.clone())) {
            spawn_local(async move {
                let _ = api::open_external(&url).await;
            });
        }
    };
    // The team requires a newer KanRite (ticket 193).
    let required = move || {
        ctx.board
            .with(|b| b.as_ref().and_then(|b| b.needs_version.clone()))
    };
    let button = "rounded px-2.5 py-1 text-sm font-medium focus:outline-2 focus:outline-offset-1 focus:outline-orange-700";
    view! {
        <Show when=move || replaced.with(Option::is_some)>
            <section
                role="status"
                aria-live="polite"
                aria-label="riteui was updated"
                class="flex flex-col gap-1 border-b border-orange-300 bg-orange-100 px-4 py-2 text-orange-950 dark:border-orange-700 dark:bg-orange-900 dark:text-orange-50"
            >
                <div class="flex flex-wrap items-center gap-3">
                    <p class="flex-1 text-sm">
                        {move || format!(
                            "riteui was updated to {}. Restart to use it; open Terminal and AI sessions close.",
                            replaced.get().unwrap_or_default()
                        )}
                    </p>
                    <button class=format!("{button} bg-orange-600 text-white hover:bg-orange-500") on:click=restart>
                        "Restart"
                    </button>
                </div>
                {move || restart_note.get().map(|note| view! { <p class="text-sm">{note}</p> })}
            </section>
        </Show>
        <Show when=move || required().is_some() && replaced.with(Option::is_none)>
            <section
                role="alert"
                aria-label="Update required"
                class="flex flex-col gap-1 border-b border-orange-300 bg-orange-100 px-4 py-2 text-orange-950 dark:border-orange-700 dark:bg-orange-900 dark:text-orange-50"
            >
                <div class="flex flex-wrap items-center gap-3">
                    <p class="flex-1 text-sm">
                        {move || format!(
                            "This project needs KanRite {} or newer (its team set that), so it's read-only here until you update.",
                            required().unwrap_or_default()
                        )}
                    </p>
                    <button
                        class=format!("{button} bg-orange-600 text-white hover:bg-orange-500 disabled:opacity-60")
                        disabled=move || phase.with(|p| matches!(p, Phase::Working | Phase::Updated(_)))
                        on:click=move |_| update()
                    >
                        {move || match phase.get() {
                            Phase::Working => "Updating…",
                            Phase::Failed { .. } => "Retry",
                            _ => "Update",
                        }}
                    </button>
                </div>
                {move || status_line(phase.get())}
                <UnmanagedHelp phase />
            </section>
        </Show>
        <Show when=move || offer.with(Option::is_some) && replaced.with(Option::is_none) && required().is_none()>
            <section
                role="status"
                aria-live="polite"
                aria-label="Update available"
                class="flex flex-col gap-2 border-b border-orange-300 bg-orange-100 px-4 py-2 text-orange-950 dark:border-orange-700 dark:bg-orange-900 dark:text-orange-50"
            >
                <div class="flex flex-wrap items-center gap-3">
                    <p class="flex-1 text-sm">
                        {move || {
                            offer
                                .with(|o| {
                                    o.as_ref().map(|o| {
                                        let behind = if o.behind > 1 {
                                            format!(", {} releases behind", o.behind)
                                        } else {
                                            String::new()
                                        };
                                        format!(
                                            "KanRite {} is available. You have {}{behind}.",
                                            o.version, o.current
                                        )
                                    })
                                })
                                .unwrap_or_default()
                        }}
                        " "
                        <button class="underline" on:click=notes>"What's new"</button>
                    </p>
                    <button
                        class=format!("{button} bg-orange-600 text-white hover:bg-orange-500 disabled:opacity-60")
                        disabled=move || phase.with(|p| matches!(p, Phase::Working | Phase::Updated(_)))
                        title="Install it and restart riteui; your boards, documents and settings aren't touched"
                        on:click=move |_| update()
                    >
                        {move || match phase.get() {
                            Phase::Working => "Updating…",
                            Phase::Failed { .. } => "Retry",
                            _ => "Update",
                        }}
                    </button>
                    <button
                        class=format!("{button} border border-orange-400 hover:bg-orange-200 disabled:opacity-60 dark:border-orange-500 dark:hover:bg-orange-800")
                        disabled=move || phase.with(|p| matches!(p, Phase::Working | Phase::Updated(_)))
                        title="Hide this version for a day"
                        on:click=later
                    >
                        "Later"
                    </button>
                </div>
                {move || status_line(phase.get())}
                <UnmanagedHelp phase />
            </section>
        </Show>
    }
}

/// The line under the bar saying where Update has got to.
fn status_line(phase: Phase) -> Option<impl IntoView> {
    let line = match phase {
        Phase::Idle | Phase::Unmanaged(..) => return None,
        Phase::EditorOpen => {
            "Close the ticket you're editing first, so nothing you've typed is lost, then Update."
                .to_string()
        }
        Phase::Working => {
            "Downloading and checking the release; riteui restarts when it's in. Your boards, documents and settings aren't touched.".to_string()
        }
        Phase::Updated(v) => format!("Updated to {v}. Restarting…"),
        Phase::Failed { why, now } => {
            if now.is_empty() {
                format!("The update didn't finish: {why}")
            } else {
                format!("The update didn't finish: {why}. riteui is {now}, working as before.")
            }
        }
    };
    Some(view! { <p class="text-sm">{line}</p> })
}

/// Why riteui can't update this install, and the installer's command when
/// that's the way (both forms of the bar).
#[component]
fn UnmanagedHelp(phase: RwSignal<Phase>) -> impl IntoView {
    view! {
        <Show when=move || phase.with(|p| matches!(p, Phase::Unmanaged(..)))>
            <p class="text-sm">{move || match phase.get() { Phase::Unmanaged(why, _) => format!("{why}."), _ => String::new() }}</p>
        </Show>
        <Show when=move || phase.with(|p| matches!(p, Phase::Unmanaged(_, Some(_))))>
            <div class="flex flex-col gap-1 text-sm">
                <label for="update-command">
                    "The installer, to run in a terminal, then restart riteui. Your boards, documents and settings aren't touched."
                </label>
                <input
                    id="update-command"
                    readonly
                    class="w-full rounded border border-orange-400 bg-white px-2 py-1 font-mono text-xs text-slate-900 dark:bg-slate-900 dark:text-slate-100"
                    prop:value=move || match phase.get() {
                        Phase::Unmanaged(_, Some(command)) => command,
                        _ => String::new(),
                    }
                    on:focus=move |ev| {
                        use wasm_bindgen::JsCast;
                        if let Some(input) = ev
                            .target()
                            .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
                        {
                            input.select();
                        }
                    }
                />
            </div>
        </Show>
    }
}
