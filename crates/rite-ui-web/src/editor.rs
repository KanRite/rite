//! The ticket editor. It opens in rich-text mode — frontmatter as a form,
//! the body as formatted text — and a switch flips to the raw markdown.
//!
//! Whatever the user did not touch is saved byte-for-byte as it was: an
//! unedited body keeps its original markdown, and only changed frontmatter
//! lines are rewritten (see `rite_ui_model::ticket::apply_fields`).

use leptos::html::Div;
use leptos::prelude::*;
use leptos::task::spawn_local;
use rite_ui_model::epic::Place;
use rite_ui_model::rich::{parse_markdown, to_html, to_markdown};
use rite_ui_model::ticket::{
    self, BRANCH_TYPES, DEFAULT_PRIORITY, Fields, NO_BRANCH, PRIORITIES, VALID_POINTS,
};
use rite_ui_model::{Kind, TicketDocument, parse_frontmatter};
use wasm_bindgen::JsCast;

use crate::api;
use crate::app::{Ctx, Modal, Overlay};
use crate::rich_dom;

/// The repeats the editor offers (ticket 143); any other rule rite reads
/// (`every 10 days`) is kept as written.
const REPEATS: [&str; 5] = ["weekly", "fortnightly", "monthly", "quarterly", "yearly"];

/// Why the editor's banner is showing: its ticket changed on disk under
/// unsaved edits, or isn't where it was opened from any more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiskNotice {
    Changed,
    Gone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Rich,
    Markdown,
}

/// The Repository select's "＋ New repository…" (ticket 228): not a name
/// a repository can have.
const NEW_REPO: &str = "＋new";

#[component]
pub fn Editor(doc: TicketDocument) -> impl IntoView {
    let ctx = Ctx::get();
    let TicketDocument {
        repo,
        file,
        column,
        epic,
        content,
        lock,
        lapsed,
        version,
        authorship,
        priority_note,
    } = doc;
    let path = match &epic {
        Some(name) => format!("{}/{name}/{file}", column.dir_name()),
        None => format!("{}/{file}", column.dir_name()),
    };
    // Where it is (ticket 163): the repository when the board shows more
    // than one, the column, and its epic or sprint, filed or labelled.
    let label = rite_ui_model::parse_frontmatter(&content).epic;
    let location = {
        let (repo, epic) = (repo.clone(), epic.clone());
        move || {
            ctx.board.with(|b| {
                let b = b.as_ref();
                let many = b.is_some_and(|b| b.repos.len() > 1);
                let name = epic.clone().unwrap_or_else(|| label.trim().to_string());
                let group = (!name.is_empty()).then(|| {
                    let found =
                        b.and_then(|b| b.epics.iter().find(|e| e.repo == repo && e.name == name));
                    (
                        name.clone(),
                        found.map(|e| e.kind).unwrap_or_default(),
                        found.is_some_and(|e| e.started),
                    )
                });
                let label = b.map(|b| b.label(column)).unwrap_or_default();
                rite_ui_model::epic::where_it_is(
                    many.then_some(repo.as_str()),
                    &label,
                    group.as_ref().map(|(n, k, s)| (n.as_str(), *k, *s)),
                    epic.is_some(),
                )
            })
        }
    };
    let place = StoredValue::new(Place { repo, column, epic });
    let lock_file = StoredValue::new(file.clone());
    // The ticket's lock, kept current as it is locked and unlocked here.
    let lock = RwSignal::new(lock);
    // A lease that's still recorded but has run out (ticket 110).
    let lapsed = RwSignal::new(lapsed);
    // The fingerprint of the ticket as it was opened (or last locked or
    // unlocked here); saving hands it back.
    let version = RwSignal::new(version);
    // Someone else holds it: it can be read, but not saved or archived.
    let held = move || lock.with(|l| l.as_ref().is_some_and(|l| !l.mine));
    let original = StoredValue::new(content.clone());

    let mode = RwSignal::new(Mode::Rich);
    // Markdown mode edits this; rich mode was rendered from it.
    let text = RwSignal::new(content.clone());
    let fm = parse_frontmatter(&content);
    let has_frontmatter = RwSignal::new(ticket::split(&content).frontmatter.is_some());
    let fields = RwSignal::new(Fields::from(&fm));
    // Repository and branch type don't go together (ticket 066), on a
    // tickets board: why, as the fields stand.
    let mismatch = move || {
        let paired = place.with_value(|p| {
            ctx.board
                .with(|b| b.as_ref().is_some_and(|b| b.tickets_only(&p.repo)))
        });
        if !paired {
            return None;
        }
        fields.with(|f| ticket::pairing_mismatch(&f.repository, &f.branch_type))
    };
    let id = RwSignal::new(fm.id);
    let body_touched = RwSignal::new(false);
    let dirty = RwSignal::new(false);
    let saving = RwSignal::new(false);
    // Why the last save was refused (an invalid priority, a write error).
    let save_error = RwSignal::new(None::<String>);
    let editor = NodeRef::<Div>::new();

    let render_rich = move || {
        if let Some(el) = editor.get_untracked() {
            let body = text.with_untracked(|t| ticket::split(t).body.to_string());
            el.set_inner_html(&to_html(&parse_markdown(&body)));
        }
    };
    // Fill the rich area once it is in the DOM.
    Effect::new(move |_| {
        if editor.get().is_some() {
            rich_dom::use_paragraphs();
            render_rich();
        }
    });

    // The ticket as it would be saved right now.
    let current = move || -> String {
        let base = text.get_untracked();
        if mode.get_untracked() == Mode::Markdown {
            return base;
        }
        let parts = ticket::split(&base);
        let body = match (body_touched.get_untracked(), editor.get_untracked()) {
            (true, Some(el)) => {
                let md = to_markdown(&rich_dom::read_blocks(&el));
                // Keep the blank line rite puts between frontmatter and body.
                if parts.frontmatter.is_some() {
                    format!("\n{md}")
                } else {
                    md
                }
            }
            _ => parts.body.to_string(),
        };
        let joined = format!("{}{body}", parts.frontmatter.unwrap_or(""));
        fields.with_untracked(|f| ticket::apply_fields(&joined, f))
    };
    let refresh_dirty = move || {
        save_error.set(None);
        dirty.set(original.with_value(|o| *o != current()));
    };
    let touch_body = move || {
        body_touched.set(true);
        refresh_dirty();
    };

    let toggle_mode = move |_| match mode.get_untracked() {
        Mode::Rich => {
            text.set(current());
            mode.set(Mode::Markdown);
        }
        Mode::Markdown => {
            let md = text.get_untracked();
            let fm = parse_frontmatter(&md);
            has_frontmatter.set(ticket::split(&md).frontmatter.is_some());
            id.set(fm.id.clone());
            fields.set(Fields::from(&fm));
            body_touched.set(false);
            mode.set(Mode::Rich);
            render_rich();
        }
    };

    // Archiving asks once more before taking the ticket off the board.
    let confirm_archive = RwSignal::new(false);
    let archive = {
        let file = file.clone();
        move |_| {
            if !confirm_archive.get_untracked() {
                confirm_archive.set(true);
                return;
            }
            let file = file.clone();
            saving.set(true);
            spawn_local(async move {
                match api::archive_ticket(&place.get_value(), &file).await {
                    Ok(report) => {
                        ctx.report(report);
                        ctx.close_overlay();
                        ctx.reload();
                    }
                    Err(e) => save_error.set(Some(e)),
                }
                saving.set(false);
            });
        }
    };

    // Duplicating copies the saved ticket, then opens the copy.
    let duplicate = {
        let file = file.clone();
        move |_| {
            let file = file.clone();
            saving.set(true);
            spawn_local(async move {
                match api::clone_ticket(&place.get_value(), &file).await {
                    Ok(created) => {
                        ctx.report(created.sync);
                        ctx.reload();
                        ctx.overlay.set(Some(Overlay::Editor(created.doc)));
                    }
                    Err(e) => save_error.set(Some(e)),
                }
                saving.set(false);
            });
        }
    };

    let file_for_new = file.clone();
    let file_for_repo = file.clone();
    let save = move |_| {
        let file = file.clone();
        // Same rule the backend applies; checking here keeps the editor open
        // with the reason instead of a round trip.
        let content = match ticket::ensure_priority(&current()) {
            Ok(content) => content,
            Err(e) => return save_error.set(Some(e)),
        };
        saving.set(true);
        spawn_local(async move {
            let opened = version.get_untracked();
            match api::save_ticket(&place.get_value(), &file, &content, &opened).await {
                Ok(report) => {
                    ctx.report(report);
                    ctx.close_overlay();
                    ctx.reload();
                }
                Err(e) => save_error.set(Some(e)),
            }
            saving.set(false);
        });
    };

    // Save and New (ticket 227): saves this ticket if it has edits, then
    // opens New ticket linked back to it, for a ticket found to depend on
    // one that's missing.
    // Add child (ticket 259) is the same, starting on "a child of".
    let save_and_new = {
        let file = file_for_new;
        move |kind: crate::app::LinkKind| {
            let back = crate::app::LinkBack {
                repo: place.get_value().repo,
                id: id.get_untracked(),
                title: fields.with_untracked(|f| f.title.clone()),
                kind,
            };
            let open_new = move || {
                ctx.overlay
                    .set(Some(Overlay::NewTicket(Some(back.clone()))))
            };
            if !dirty.get_untracked() {
                open_new();
                return;
            }
            let content = match ticket::ensure_priority(&current()) {
                Ok(content) => content,
                Err(e) => return save_error.set(Some(e)),
            };
            let file = file.clone();
            saving.set(true);
            spawn_local(async move {
                let opened = version.get_untracked();
                match api::save_ticket(&place.get_value(), &file, &content, &opened).await {
                    Ok(report) => {
                        ctx.report(report);
                        ctx.reload();
                        open_new();
                    }
                    Err(e) => save_error.set(Some(e)),
                }
                saving.set(false);
            });
        }
    };

    // A new repository for this ticket (ticket 228): saved first if it has
    // edits, as Save and New does, then the dialog.
    let new_repo = {
        let file = file_for_repo;
        Callback::new(move |()| {
            let target = crate::app::NewRepoFor {
                place: place.get_value(),
                file: file.clone(),
                id: id.get_untracked(),
                title: fields.with_untracked(|f| f.title.clone()),
            };
            let open = move || ctx.overlay.set(Some(Overlay::NewRepo(target.clone())));
            if !dirty.get_untracked() {
                open();
                return;
            }
            let content = match ticket::ensure_priority(&current()) {
                Ok(content) => content,
                Err(e) => return save_error.set(Some(e)),
            };
            let file = file.clone();
            saving.set(true);
            spawn_local(async move {
                let opened = version.get_untracked();
                match api::save_ticket(&place.get_value(), &file, &content, &opened).await {
                    Ok(report) => {
                        ctx.report(report);
                        ctx.reload();
                        open();
                    }
                    Err(e) => save_error.set(Some(e)),
                }
                saving.set(false);
            });
        })
    };

    let set_field = move |f: fn(&mut Fields, String)| {
        move |ev| {
            let value = event_target_value(&ev);
            fields.update(|fields| f(fields, value));
            refresh_dirty();
        }
    };

    // The ticket changing on disk while it's open here (ticket 091): the
    // board's poll bumps `disk_tick`, and the editor checks its own ticket.
    // Nothing unsaved: reopen it from disk. Unsaved edits: say so, and let
    // the user choose — never overwrite either side silently (saving still
    // goes through the version check).
    let disk_notice = RwSignal::new(None::<DiskNotice>);
    Effect::new(move |seen: Option<u64>| {
        let tick = ctx.disk_tick.get();
        if seen.is_some_and(|seen| seen != tick) {
            spawn_local(async move {
                let (place, file) = (place.get_value(), lock_file.get_value());
                match api::ticket_version(&place, &file).await {
                    Ok(Some(now)) if now == version.get_untracked() => {}
                    Ok(Some(_)) if !dirty.get_untracked() => ctx.open_ticket(place, file),
                    Ok(Some(_)) => disk_notice.set(Some(DiskNotice::Changed)),
                    Ok(None) => disk_notice.set(Some(DiskNotice::Gone)),
                    Err(_) => {}
                }
            });
        }
        tick
    });
    let reload_from_disk = move |_| {
        disk_notice.set(None);
        ctx.open_ticket(place.get_value(), lock_file.get_value());
    };

    let is_rich = move || mode.get() == Mode::Rich;
    let input = "rounded border border-slate-300 bg-slate-50 px-2 py-1 text-sm dark:border-slate-600 dark:bg-slate-900";
    let label = "flex flex-col gap-1 text-xs font-medium text-slate-600 dark:text-slate-300";

    view! {
        <Modal title=format!("Edit {path}") width="max-w-4xl">
            <p class="text-sm font-medium text-accent-800 dark:text-accent-200" aria-label="Where this ticket is">
                {location}
            </p>
            <LockBar lock lapsed version place file=lock_file />
            // Who to ask about it (ticket 155).
            {(!authorship.is_empty())
                .then(|| {
                    view! {
                        <p class="text-xs text-slate-500" aria-label="Who created and last edited this ticket">
                            {authorship.clone()}
                        </p>
                    }
                })}
            {move || {
                disk_notice
                    .get()
                    .map(|notice| {
                        view! {
                            <div
                                role="alert"
                                class="flex items-center gap-3 rounded bg-amber-100 px-3 py-2 text-sm text-amber-900 dark:bg-amber-950 dark:text-amber-200"
                            >
                                <span class="flex-1">
                                    {match notice {
                                        DiskNotice::Changed => {
                                            "This ticket changed on disk while you were editing it."
                                        }
                                        DiskNotice::Gone => {
                                            "This ticket was moved or removed on disk; saving here will be refused."
                                        }
                                    }}
                                </span>
                                <Show when=move || notice == DiskNotice::Changed>
                                    <button class="underline" on:click=reload_from_disk>
                                        "Reload (discard mine)"
                                    </button>
                                </Show>
                                <button class="underline" on:click=move |_| disk_notice.set(None)>
                                    "Keep editing"
                                </button>
                            </div>
                        }
                    })
            }}
            // A draft (ticket 114) says so at the top. Mark ready takes the
            // DRAFT tag off; saving writes it, like any other field.
            <Show when=move || {
                is_rich() && fields.with(|f| rite_ui_model::epic::is_draft(&f.tags))
            }>
                <div
                    role="status"
                    class="flex items-center gap-3 rounded border border-dashed border-amber-500 px-3 py-2 text-sm text-amber-800 dark:text-amber-200"
                >
                    <span class="font-semibold">"Draft"</span>
                    <span class="flex-1">
                        "It stays in the backlog, and out of sprints, until it's marked ready."
                    </span>
                    <button
                        class="rounded bg-amber-600 px-2 py-1 text-xs font-medium text-white hover:bg-amber-500 disabled:opacity-50"
                        disabled=move || held() || mismatch().is_some()
                        title=move || mismatch().map(|why| format!("Not ready yet: {why}"))
                        on:click=move |_| {
                            fields.update(|f| {
                                f.tags.retain(|t| !t.trim().eq_ignore_ascii_case(rite_ui_model::epic::DRAFT_TAG))
                            });
                            refresh_dirty();
                        }
                    >
                        "Mark ready"
                    </button>
                </div>
            </Show>
            <Show when=move || {
                is_rich() && dirty.get() && original.with_value(|o| {
                    rite_ui_model::epic::is_draft(&parse_frontmatter(o).tags)
                }) && !fields.with(|f| rite_ui_model::epic::is_draft(&f.tags))
            }>
                <p role="status" class="text-sm text-amber-800 dark:text-amber-200">
                    "Marked ready: save to take it out of draft."
                </p>
            </Show>
            // Everything above the buttons scrolls as one, so a short window
            // scrolls the dialog instead of squeezing the editor to nothing.
            // It never gets shorter than `min-h-48` (ticket 356): zoomed in,
            // the bars above it took all the room and it vanished; the whole
            // dialog scrolls instead.
            <div class="-mx-1 flex min-h-48 flex-1 flex-col gap-3 overflow-y-auto px-1">
            // ---- Frontmatter as a form (rich mode only)
            <Show when=move || is_rich() && has_frontmatter.get()>
                <div class="grid grid-cols-1 gap-2 sm:grid-cols-[1fr_12rem]">
                    <label class=label>
                        "Title"
                        <input
                            type="text"
                            class=input
                            prop:value=move || fields.with(|f| f.title.clone())
                            on:input=set_field(|f, v| f.title = v)
                        />
                    </label>
                    <label class=label>
                        "Ticket"
                        <input type="text" class=format!("{input} opacity-70") readonly prop:value=move || id.get() />
                    </label>
                    <div class=label>
                        "Assignee"
                        <AssigneePicker fields on_change=refresh_dirty class=input />
                    </div>
                    <label class=label>
                        "Repository"
                        <select
                            class=input
                            title="The repository the work is in; only one cloned on this machine can be chosen"
                            prop:value=move || fields.with(|f| f.repository.clone())
                            on:change=move |ev| {
                                let value = event_target_value(&ev);
                                if value == NEW_REPO {
                                    // Put the select back; the dialog decides.
                                    let current = fields.with_untracked(|f| f.repository.clone());
                                    if let Some(select) = ev
                                        .target()
                                        .and_then(|t| wasm_bindgen::JsCast::dyn_into::<web_sys::HtmlSelectElement>(t).ok())
                                    {
                                        select.set_value(&current);
                                    }
                                    new_repo.run(());
                                } else {
                                    fields.update(|f| f.repository = value);
                                    refresh_dirty();
                                }
                            }
                        >
                            <option value="">"none"</option>
                            {move || {
                                let current = fields.with(|f| f.repository.clone());
                                let mut choices = ctx
                                    .board
                                    .with(|b| b.as_ref().map(|b| b.ticket_repos.clone()).unwrap_or_default());
                                // A repository the ticket already names stays visible
                                // even if it isn't offered, so it's never silently changed.
                                if !current.is_empty() && !choices.iter().any(|r| r.name == current) {
                                    choices.push(rite_ui_model::RepoChoice {
                                        name: current,
                                        available: false,
                                    });
                                }
                                choices
                                    .into_iter()
                                    .map(|r| {
                                        let label = if r.available {
                                            r.name.clone()
                                        } else {
                                            format!("{} (not cloned)", r.name)
                                        };
                                        // Cloning is offered in one place, the
                                        // Repository filter (ticket 108).
                                        let hint = (!r.available)
                                            .then_some("Clone it from the Repository filter first");
                                        view! {
                                            <option value=r.name.clone() disabled=!r.available title=hint>
                                                {label}
                                            </option>
                                        }
                                    })
                                    .collect_view()
                            }}
                            // Ticket 228: a repository the project hasn't got yet.
                            {move || {
                                let admin = ctx
                                    .board
                                    .with(|b| b.as_ref().is_none_or(|b| b.admin.allowed));
                                view! {
                                    <option
                                        value=NEW_REPO
                                        disabled=!admin
                                        title=(!admin).then_some("Only the project's admins can make repositories")
                                    >
                                        {if admin { "＋ New repository…" } else { "＋ New repository… (admins only)" }}
                                    </option>
                                }
                            }}
                        </select>
                    </label>
                    <label class=label>
                        "Branch type"
                        <select
                            class=input
                            prop:value=move || fields.with(|f| f.branch_type.clone())
                            on:change=set_field(|f, v| f.branch_type = v)
                        >
                            <option value="">"not chosen"</option>
                            <option value=NO_BRANCH>"none (no branch)"</option>
                            {BRANCH_TYPES.map(|t| view! { <option value=t>{t}</option> }).collect_view()}
                        </select>
                    </label>
                    // Repository and branch type go together (ticket 066):
                    // saved like this, it's a draft (or refused past the
                    // backlog).
                    {move || {
                        mismatch().map(|why| view! {
                            <p role="status" class="text-xs text-amber-800 sm:col-span-2 dark:text-amber-200">
                                {format!("This ticket {}", why.trim_start_matches("it "))}
                                {if column.kind() == Kind::Backlog {
                                    ". Saved like this, it's a draft."
                                } else {
                                    ". It can't be saved like this past the backlog."
                                }}
                            </p>
                        })
                    }}
                    <div class=label>
                        "Tags"
                        <TagPicker fields on_change=refresh_dirty class=input />
                    </div>
                    <label class=label>
                        "Points"
                        <select
                            class=input
                            title="Size estimate; a ticket needs points to go into a sprint"
                            prop:value=move || fields.with(|f| f.points.clone())
                            on:change=set_field(|f, v| f.points = v)
                        >
                            <option value="">"unsized"</option>
                            {VALID_POINTS.map(|p| view! { <option value=p>{p}</option> }).collect_view()}
                            // A value rite doesn't accept, from markdown mode or an old
                            // ticket, stays visible so the user can see what to correct.
                            {move || {
                                fields.with(|f| {
                                    let p = f.points.trim();
                                    (!p.is_empty() && !VALID_POINTS.contains(&p))
                                        .then(|| {
                                            let bad = p.to_string();
                                            view! {
                                                <option value=bad.clone() disabled>
                                                    {format!("{bad} (not valid)")}
                                                </option>
                                            }
                                        })
                                })
                            }}
                        </select>
                    </label>
                    <label class=label>
                        "Due"
                        <input
                            type="date"
                            class=input
                            title="When it's due; a card past its date says overdue"
                            prop:value=move || fields.with(|f| f.due.clone())
                            on:change=set_field(|f, v| f.due = v)
                        />
                    </label>
                    <label class=label>
                        "Repeats"
                        <select
                            class=input
                            title="Done makes the next one, due one interval on"
                            prop:value=move || fields.with(|f| f.recur.clone())
                            on:change=set_field(|f, v| f.recur = v)
                        >
                            <option value="">"never"</option>
                            {REPEATS.map(|r| view! { <option value=r>{r}</option> }).collect_view()}
                            // A rule written by hand (every 10 days) stays shown.
                            {move || {
                                fields.with(|f| {
                                    let r = f.recur.trim();
                                    (!r.is_empty() && !REPEATS.contains(&r))
                                        .then(|| {
                                            let own = r.to_string();
                                            view! { <option value=own.clone()>{own.clone()}</option> }
                                        })
                                })
                            }}
                        </select>
                    </label>
                    <label class=label>
                        "Priority"
                        <select
                            class=input
                            prop:value=move || {
                                fields.with(|f| {
                                    if f.priority.is_empty() { DEFAULT_PRIORITY.to_string() } else { f.priority.clone() }
                                })
                            }
                            on:change=set_field(|f, v| f.priority = v)
                        >
                            {PRIORITIES.map(|p| view! { <option value=p>{p}</option> }).collect_view()}
                            // Hands the ticket back to auto priority (ticket 215).
                            {move || {
                                ctx.board
                                    .with(|b| b.as_ref().is_some_and(|b| b.auto_priority))
                                    .then(|| view! { <option value="auto">"auto"</option> })
                            }}
                            // An invalid value typed in Markdown mode stays visible, so the
                            // user can see what to correct.
                            {move || {
                                fields.with(|f| {
                                    (!f.priority.is_empty() && !PRIORITIES.contains(&f.priority.as_str())
                                        && f.priority != "auto")
                                        .then(|| {
                                            let bad = f.priority.clone();
                                            view! {
                                                <option value=bad.clone() disabled>
                                                    {format!("{bad} (not valid)")}
                                                </option>
                                            }
                                        })
                                })
                            }}
                        </select>
                        // Why it's this priority (ticket 215).
                        {(!priority_note.is_empty())
                            .then(|| {
                                view! { <span class="text-xs font-normal text-slate-500">{priority_note.clone()}</span> }
                            })}
                        // Ticket 248: TRIAGE keeps it top, whatever is picked.
                        {move || {
                            fields
                                .with(|f| f.tags.iter().any(|t| t.trim().eq_ignore_ascii_case("TRIAGE")))
                                .then(|| view! {
                                    <span class="text-xs font-normal text-amber-700 dark:text-amber-300">
                                        "Waiting to be triaged, so it's top: take TRIAGE off to set another priority."
                                    </span>
                                })
                        }}
                    </label>
                </div>
            </Show>

            <Relationships id=id place=place file=lock_file dirty=dirty />

            // ---- Rich text: toolbar + editable body
            <div class=move || if is_rich() { "flex shrink-0 flex-col gap-2" } else { "hidden" }>
                <crate::rich_editor::Toolbar on_change=touch_body />
                <div
                    node_ref=editor
                    contenteditable="true"
                    role="textbox"
                    aria-multiline="true"
                    aria-label="Ticket text"
                    class="rich-editor h-[45vh] min-h-48 overflow-y-auto rounded border border-slate-300 bg-slate-50 p-3 text-sm focus:outline-2 focus:outline-accent-500 dark:border-slate-600 dark:bg-slate-900"
                    on:input=move |_| touch_body()
                    on:click=move |ev| {
                        let target = ev.target().and_then(|t| t.dyn_into::<web_sys::Element>().ok());
                        if let Some(el) = target {
                            // Links must not navigate the app's window away.
                            if el.closest("a").ok().flatten().is_some() {
                                ev.prevent_default();
                            }
                            // Ticking a checklist box is an edit too.
                            if el.tag_name().eq_ignore_ascii_case("input") {
                                touch_body();
                            }
                        }
                    }
                    on:paste=move |ev| {
                        // Paste as plain text so foreign markup never enters the ticket.
                        ev.prevent_default();
                        let text = ev.clipboard_data().and_then(|d| d.get_data("text/plain").ok());
                        if let Some(text) = text {
                            rich_dom::exec("insertText", Some(&text));
                        }
                    }
                ></div>
            </div>

            // ---- Raw markdown
            <textarea
                aria-label="Ticket markdown"
                class=move || {
                    format!(
                        "h-[60vh] min-h-48 w-full shrink-0 resize-none rounded border border-slate-300 bg-slate-50 p-3 font-mono text-sm dark:border-slate-600 dark:bg-slate-900 {}",
                        if is_rich() { "hidden" } else { "" },
                    )
                }
                spellcheck="false"
                prop:value=move || text.get()
                on:input=move |ev| {
                    text.set(event_target_value(&ev));
                    refresh_dirty();
                }
            ></textarea>
            </div>

            <div class="flex shrink-0 flex-wrap items-center justify-end gap-3">
                <button
                    type="button"
                    title="Take this ticket off the board into tasks/archive/"
                    class=move || {
                        if confirm_archive.get() {
                            "rounded bg-red-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-red-500 disabled:opacity-50"
                        } else {
                            "rounded border border-slate-300 px-3 py-1.5 text-sm text-red-700 hover:bg-red-50 disabled:opacity-50 dark:border-slate-600 dark:text-red-300 dark:hover:bg-red-950"
                        }
                    }
                    disabled=move || saving.get() || held()
                    on:click=archive
                    on:blur=move |_| confirm_archive.set(false)
                >
                    {move || if confirm_archive.get() { "Confirm archive" } else { "Archive" }}
                </button>
                <button
                    type="button"
                    title=move || {
                        if dirty.get() {
                            "Save or discard your edits first: the copy is made from the saved ticket"
                        } else {
                            "Copy this ticket into the backlog, linked to it as a clone"
                        }
                    }
                    class="rounded border border-slate-300 px-3 py-1.5 text-sm hover:bg-slate-100 disabled:opacity-50 dark:border-slate-600 dark:hover:bg-slate-800"
                    disabled=move || saving.get() || dirty.get()
                    on:click=duplicate
                >
                    "Duplicate"
                </button>
                <button
                    type="button"
                    title="Save this ticket, then make a smaller one under it"
                    class="rounded border border-slate-300 px-3 py-1.5 text-sm hover:bg-slate-100 disabled:opacity-50 dark:border-slate-600 dark:hover:bg-slate-800"
                    disabled=move || saving.get() || (dirty.get() && held())
                    on:click={
                        let save_and_new = save_and_new.clone();
                        move |_| save_and_new(crate::app::LinkKind::Child)
                    }
                >
                    "Add child"
                </button>
                <span class="flex-1 text-xs">
                    {move || match save_error.get() {
                        Some(e) => {
                            view! {
                                <span role="alert" class="font-medium text-red-700 dark:text-red-300">
                                    {e}
                                </span>
                            }
                                .into_any()
                        }
                        None => {
                            view! {
                                <span class="text-slate-500">
                                    {if dirty.get() { "Unsaved changes" } else { "" }}
                                </span>
                            }
                                .into_any()
                        }
                    }}
                </span>
                <button
                    type="button"
                    role="switch"
                    aria-checked=move || (!is_rich()).to_string()
                    title=move || {
                        if is_rich() { "Edit the raw markdown" } else { "Back to rich text" }
                    }
                    class="flex items-center gap-2 rounded px-2 py-1.5 text-sm hover:bg-slate-100 dark:hover:bg-slate-700"
                    on:click=toggle_mode
                >
                    <span class=move || {
                        format!(
                            "relative inline-flex h-5 w-9 shrink-0 rounded-full transition {}",
                            if is_rich() { "bg-slate-300 dark:bg-slate-600" } else { "bg-accent-600" },
                        )
                    }>
                        <span class=move || {
                            format!(
                                "absolute top-0.5 h-4 w-4 rounded-full bg-white shadow transition-all {}",
                                if is_rich() { "left-0.5" } else { "left-4.5" },
                            )
                        }></span>
                    </span>
                    {move || if is_rich() { "Markdown" } else { "Rich Text" }}
                </button>
                <button
                    class="rounded border border-slate-300 px-3 py-1.5 text-sm hover:bg-slate-100 dark:border-slate-600 dark:hover:bg-slate-700"
                    on:click=move |_| ctx.close_overlay()
                >
                    "Close"
                </button>
                <button
                    class="rounded border border-accent-600 px-3 py-1.5 text-sm font-medium text-accent-700 hover:bg-accent-50 disabled:opacity-50 dark:text-accent-300 dark:hover:bg-accent-950"
                    disabled=move || saving.get() || (dirty.get() && held())
                    title="Save this ticket, then make a new one linked to it"
                    on:click={
                        let save_and_new = save_and_new.clone();
                        move |_| save_and_new(crate::app::LinkKind::Requires)
                    }
                >
                    "Save and New"
                </button>
                <button
                    class="rounded bg-accent-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-accent-500 disabled:opacity-50"
                    disabled=move || saving.get() || !dirty.get() || held()
                    title=move || held().then_some("Locked by someone else")
                    on:click=save
                >
                    "Save"
                </button>
            </div>
        </Modal>
    }
}

/// The ticket's lock, above the editor: lock it, renew or release your own,
/// or — a separate, confirmed step — break someone else's. A lapsed lease
/// is said as such (ticket 110), not as "Not locked.", which reads as if
/// something unlocked it.
#[component]
fn LockBar(
    lock: RwSignal<Option<rite_ui_model::Lock>>,
    lapsed: RwSignal<Option<rite_ui_model::Lock>>,
    version: RwSignal<String>,
    place: StoredValue<Place>,
    file: StoredValue<String>,
) -> impl IntoView {
    let ctx = Ctx::get();
    let busy = RwSignal::new(false);
    let problem = RwSignal::new(None::<String>);
    let confirm_break = RwSignal::new(false);
    let hours = move || {
        let repo = place.with_value(|p| p.repo.clone());
        ctx.board.with(|b| {
            b.as_ref()
                .and_then(|b| b.repos.iter().find(|r| r.name == repo))
                .map_or(rite_ui_model::DEFAULT_LEASE_HOURS, |r| r.lease_hours)
        })
    };

    // Runs a lock change, then reads the lock back from the ticket.
    let act = move |action: &'static str| {
        busy.set(true);
        problem.set(None);
        confirm_break.set(false);
        spawn_local(async move {
            let (p, f) = (place.get_value(), file.get_value());
            let done = match action {
                "lock" => api::lock_ticket(&p, &f).await,
                "unlock" => api::unlock_ticket(&p, &f, false).await,
                _ => api::unlock_ticket(&p, &f, true).await,
            };
            match done {
                Ok(report) => {
                    ctx.report(report);
                    if let Ok(doc) = api::open_ticket(&p, &f).await {
                        lock.set(doc.lock);
                        lapsed.set(doc.lapsed);
                        version.set(doc.version);
                    }
                    ctx.reload();
                }
                Err(e) => problem.set(Some(e)),
            }
            busy.set(false);
        });
    };
    let button = "rounded border border-slate-300 px-2 py-1 text-xs hover:bg-slate-100 disabled:opacity-50 dark:border-slate-600 dark:hover:bg-slate-700";

    view! {
        <div class=move || {
            let tone = match lock.get() {
                Some(l) if !l.mine => "bg-amber-100 text-amber-900 dark:bg-amber-900/60 dark:text-amber-100",
                _ => "bg-slate-100 text-slate-700 dark:bg-slate-900 dark:text-slate-300",
            };
            format!("flex shrink-0 flex-wrap items-center gap-2 rounded px-3 py-1.5 text-sm {tone}")
        }>
            <span class="flex-1">
                {move || match lock.get() {
                    None => match lapsed.get() {
                        None => "Not locked.".to_string(),
                        Some(l) if l.mine => {
                            format!("Your lock lapsed at {}. Lock it again to keep working.", l.until_local)
                        }
                        Some(l) => {
                            format!(
                                "Locked by {} until {} (lapsed), so anyone can change it now.",
                                l.by,
                                l.until_local,
                            )
                        }
                    },
                    Some(l) if l.mine => format!("Locked by you until {}.", l.until_local),
                    Some(l) => {
                        format!(
                            "Locked by {} until {}. You can read it, but only they can change it.",
                            l.by,
                            l.until_local,
                        )
                    }
                }}
            </span>
            {move || problem.get().map(|p| view! { <span role="alert" class="font-medium text-red-700 dark:text-red-300">{p}</span> })}
            {move || match lock.get() {
                None => {
                    view! {
                        <button class=button disabled=move || busy.get() on:click=move |_| act("lock")>
                            {format!("Lock for {} hours", hours())}
                        </button>
                    }
                        .into_any()
                }
                Some(l) if l.mine => {
                    view! {
                        <button class=button disabled=move || busy.get() on:click=move |_| act("lock")>
                            "Renew"
                        </button>
                        <button class=button disabled=move || busy.get() on:click=move |_| act("unlock")>
                            "Unlock"
                        </button>
                    }
                        .into_any()
                }
                Some(_) => {
                    view! {
                        <button
                            class=move || {
                                if confirm_break.get() {
                                    "rounded bg-red-600 px-2 py-1 text-xs font-medium text-white hover:bg-red-500 disabled:opacity-50"
                                } else {
                                    button
                                }
                            }
                            title="Clear their lock, so anyone can change the ticket"
                            disabled=move || busy.get()
                            on:click=move |_| {
                                if confirm_break.get_untracked() {
                                    act("break");
                                } else {
                                    confirm_break.set(true);
                                }
                            }
                            on:blur=move |_| confirm_break.set(false)
                        >
                            {move || if confirm_break.get() { "Confirm: break their lock" } else { "Break lock" }}
                        </button>
                    }
                        .into_any()
                }
            }}
        </div>
    }
}

/// The tags field (ticket 122): the ticket's tags as chips, each with a ×
/// to take it off, and a box to add one that offers the project's listed
/// tags (or, with no list, every tag in use), sorted and narrowed as you
/// type. Tags are UPPERCASE. With "Prevent arbitrary tags" on (the default), only a listed tag
/// can be added; otherwise a new one can be typed and added with Enter. A
/// tag that isn't on the list is shown dashed.
#[component]
fn TagPicker(
    fields: RwSignal<Fields>,
    on_change: impl Fn() + Copy + Send + Sync + 'static,
    class: &'static str,
) -> impl IntoView {
    let ctx = Ctx::get();
    let open = RwSignal::new(false);
    let cursor = RwSignal::new(0usize);
    let typed = RwSignal::new(String::new());
    let note = RwSignal::new(None::<String>);
    let current = move || fields.with(|f| f.tags.clone());
    // The listed tags, plus the built-in ones (DRAFT, ticket 114), which are
    // always allowed and offered.
    let listed = move || {
        ctx.board.with(|b| {
            b.as_ref()
                .map(|b| {
                    b.tags
                        .iter()
                        .filter(|t| t.listed || t.reserved)
                        .map(|t| t.name.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    };
    // Whether the project has a tag list of its own.
    let has_list = move || {
        ctx.board
            .with(|b| b.as_ref().is_some_and(|b| b.tags.iter().any(|t| t.listed)))
    };
    let tags_only = move || ctx.board.with(|b| b.as_ref().is_some_and(|b| b.tags_only));
    // What's offered: the list (the built-ins at least); only with
    // arbitrary tags allowed and nothing listed, the tags in use (ticket
    // 167).
    let known = move || {
        if has_list() || tags_only() {
            return listed();
        }
        ctx.board.with(|b| {
            b.as_ref()
                .map(|b| b.tags.iter().map(|t| t.name.clone()).collect::<Vec<_>>())
                .unwrap_or_default()
        })
    };
    let only = tags_only;
    let options = move || {
        let query = typed.get().trim().to_uppercase();
        let have: Vec<String> = current().iter().map(|t| t.to_uppercase()).collect();
        known()
            .into_iter()
            .filter(|t| !have.contains(t) && (query.is_empty() || t.contains(&query)))
            .collect::<Vec<_>>()
    };
    let add = move |tag: String| {
        let tag = tag.trim().to_uppercase();
        if tag.is_empty() {
            return;
        }
        if tag.contains(|c: char| c.is_whitespace() || matches!(c, ',' | '[' | ']' | '"' | '\'')) {
            note.set(Some("A tag is one word.".into()));
            return;
        }
        if only() && !listed().contains(&tag) {
            note.set(Some(format!(
                "{tag} isn't on the project's tag list, and only listed tags can be used."
            )));
            return;
        }
        fields.update(|f| {
            if !f.tags.iter().any(|t| t.to_uppercase() == tag) {
                f.tags.push(tag);
            }
        });
        typed.set(String::new());
        note.set(None);
        cursor.set(0);
        on_change();
    };
    let take_off = move |tag: String| {
        fields.update(|f| f.tags.retain(|t| *t != tag));
        on_change();
    };

    view! {
        <div class="flex flex-col gap-1">
            <ul class="flex flex-wrap gap-1" aria-label="This ticket's tags">
                {move || {
                    let list = listed();
                    let has = has_list();
                    current()
                        .into_iter()
                        .map(|tag| {
                            let off_list = has && !list.contains(&tag.to_uppercase());
                            let style = if off_list {
                                "border border-dashed border-accent-400"
                            } else {
                                "bg-accent-100 dark:bg-accent-900"
                            };
                            let title = if off_list { "Not on the project's tag list" } else { "" };
                            let label = format!("Remove tag {}", tag.to_uppercase());
                            let gone = tag.clone();
                            view! {
                                <li
                                    class=format!("flex items-center gap-1 rounded px-1.5 text-sm uppercase text-accent-800 dark:text-accent-200 {style}")
                                    title=title
                                >
                                    {tag}
                                    <button
                                        type="button"
                                        class="text-slate-500 hover:text-red-600"
                                        aria-label=label
                                        on:click=move |_| take_off(gone.clone())
                                    >
                                        "×"
                                    </button>
                                </li>
                            }
                        })
                        .collect_view()
                }}
            </ul>
            <div class="relative">
                <input
                    type="text"
                    class=format!("{class} uppercase")
                    role="combobox"
                    aria-label="Add a tag"
                    aria-expanded=move || open.get().to_string()
                    autocomplete="off"
                    placeholder=move || if only() { "Add a listed tag" } else { "Add a tag" }
                    prop:value=move || typed.get()
                    on:focus=move |_| {
                        cursor.set(0);
                        open.set(true);
                    }
                    on:blur=move |_| open.set(false)
                    on:input=move |ev| {
                        typed.set(event_target_value(&ev));
                        note.set(None);
                        cursor.set(0);
                        open.set(true);
                    }
                    on:keydown=move |ev| {
                        let shown = options();
                        match ev.key().as_str() {
                            "ArrowDown" => {
                                ev.prevent_default();
                                open.set(true);
                                cursor.update(|c| *c = (*c + 1).min(shown.len().saturating_sub(1)));
                            }
                            "ArrowUp" => {
                                ev.prevent_default();
                                cursor.update(|c| *c = c.saturating_sub(1));
                            }
                            "Enter" => {
                                ev.prevent_default();
                                let exact = typed.get_untracked().trim().to_uppercase();
                                match shown.get(cursor.get_untracked()) {
                                    Some(choice) if open.get_untracked() && !shown.contains(&exact) => add(choice.clone()),
                                    _ => add(exact),
                                }
                            }
                            "Escape" if open.get_untracked() => {
                                ev.prevent_default();
                                ev.stop_propagation();
                                open.set(false);
                            }
                            _ => {}
                        }
                    }
                />
                <Show when=move || open.get() && !options().is_empty()>
                    <ul
                        role="listbox"
                        class="absolute z-20 mt-1 max-h-56 w-full overflow-y-auto rounded border border-slate-300 bg-white py-1 text-sm shadow-lg dark:border-slate-600 dark:bg-slate-800"
                    >
                        {move || {
                            options()
                                .into_iter()
                                .enumerate()
                                .map(|(i, tag)| {
                                    let chosen = tag.clone();
                                    view! {
                                        <li
                                            role="option"
                                            aria-selected=move || (cursor.get() == i).to_string()
                                            class=move || {
                                                let base = "cursor-pointer px-2 py-1";
                                                if cursor.get() == i {
                                                    format!("{base} bg-accent-600 text-white")
                                                } else {
                                                    format!("{base} hover:bg-slate-100 dark:hover:bg-slate-700")
                                                }
                                            }
                                            // Before the input's blur, so the click lands.
                                            on:mousedown=move |ev| {
                                                ev.prevent_default();
                                                add(chosen.clone());
                                            }
                                        >
                                            {tag}
                                        </li>
                                    }
                                })
                                .collect_view()
                        }}
                    </ul>
                </Show>
            </div>
            {move || note.get().map(|n| view! { <p class="text-xs text-red-600 dark:text-red-400">{n}</p> })}
        </div>
    }
}

/// The assignee field (tickets 089, 254): the people it's assigned to as
/// chips, the first the main assignee, each with a × to take them off, and
/// a box to add someone that offers you and the project's users, narrowed
/// as you type (arrow keys and Enter, or a click). A name that isn't one of
/// the users is still accepted with Enter — someone may not have joined yet
/// — with a note saying so. No chips is nobody. Not a native `<datalist>`:
/// WebKitGTK's is patchy.
#[component]
fn AssigneePicker(
    fields: RwSignal<Fields>,
    on_change: impl Fn() + Copy + Send + Sync + 'static,
    class: &'static str,
) -> impl IntoView {
    let ctx = Ctx::get();
    let open = RwSignal::new(false);
    let cursor = RwSignal::new(0usize);
    let typed = RwSignal::new(String::new());
    let current = move || fields.with(|f| f.assignees.clone());
    let users = move || {
        ctx.board
            .with(|b| b.as_ref().map(|b| b.users.clone()).unwrap_or_default())
    };
    let me = move || ctx.board.with(|b| b.as_ref().and_then(|b| b.me.clone()));
    let has = move |who: &str| current().iter().any(|c| c.eq_ignore_ascii_case(who));
    // (label, value): Me, then the users, without anyone already on it,
    // narrowed by what's typed.
    let options = move || {
        let query = typed.get().trim().to_lowercase();
        let mut all = Vec::new();
        if let Some(me) = me() {
            all.push((format!("Me ({me})"), me));
        }
        all.extend(
            users()
                .into_iter()
                .filter(|u| Some(u) != me().as_ref())
                .map(|u| (u.clone(), u)),
        );
        all.into_iter()
            .filter(|(label, v)| {
                !has(v) && (query.is_empty() || label.to_lowercase().contains(&query))
            })
            .collect::<Vec<_>>()
    };
    let add = move |who: String| {
        let who = who.trim().to_string();
        if who.is_empty() {
            return;
        }
        if !has(&who) {
            fields.update(|f| f.assignees.push(who));
            on_change();
        }
        typed.set(String::new());
        cursor.set(0);
    };
    let take_off = move |who: String| {
        fields.update(|f| f.assignees.retain(|w| *w != who));
        on_change();
    };
    let strangers = move || {
        let names = users();
        if names.is_empty() {
            return Vec::new();
        }
        current()
            .into_iter()
            .filter(|who| !names.contains(who) && me().as_ref() != Some(who))
            .collect::<Vec<_>>()
    };

    view! {
        <div class="flex flex-col gap-1">
            <ul class="flex flex-wrap gap-1" aria-label="Who it's assigned to">
                {move || {
                    current()
                        .into_iter()
                        .enumerate()
                        .map(|(i, who)| {
                            let title = if i == 0 { "Main assignee" } else { "" };
                            let label = format!("Unassign {who}");
                            let gone = who.clone();
                            view! {
                                <li
                                    class="flex items-center gap-1 rounded bg-accent-100 px-1.5 text-sm font-normal text-accent-800 dark:bg-accent-900 dark:text-accent-200"
                                    title=title
                                >
                                    {who}
                                    <button
                                        type="button"
                                        class="text-slate-500 hover:text-red-600"
                                        aria-label=label
                                        on:click=move |_| take_off(gone.clone())
                                    >
                                        "×"
                                    </button>
                                </li>
                            }
                        })
                        .collect_view()
                }}
            </ul>
            <div class="relative">
                <input
                    type="text"
                    class=class
                    role="combobox"
                    aria-label="Add an assignee"
                    aria-expanded=move || open.get().to_string()
                    autocomplete="off"
                    placeholder=move || if current().is_empty() { "Nobody" } else { "Add someone" }
                    prop:value=move || typed.get()
                    on:focus=move |_| {
                        cursor.set(0);
                        open.set(true);
                    }
                    on:blur=move |_| open.set(false)
                    on:input=move |ev| {
                        typed.set(event_target_value(&ev));
                        cursor.set(0);
                        open.set(true);
                    }
                    on:keydown=move |ev| {
                        let count = options().len();
                        match ev.key().as_str() {
                            "ArrowDown" => {
                                ev.prevent_default();
                                open.set(true);
                                cursor.update(|c| *c = (*c + 1).min(count.saturating_sub(1)));
                            }
                            "ArrowUp" => {
                                ev.prevent_default();
                                cursor.update(|c| *c = c.saturating_sub(1));
                            }
                            "Enter" => {
                                ev.prevent_default();
                                match options().get(cursor.get_untracked()).cloned() {
                                    Some((_, v)) if open.get_untracked() => add(v),
                                    _ => add(typed.get_untracked()),
                                }
                            }
                            "Backspace" if typed.get_untracked().is_empty() => {
                                if let Some(last) = current().pop() {
                                    take_off(last);
                                }
                            }
                            "Escape" if open.get_untracked() => {
                                ev.prevent_default();
                                ev.stop_propagation();
                                open.set(false);
                            }
                            _ => {}
                        }
                    }
                />
                <Show when=move || open.get() && !options().is_empty()>
                    <ul
                        role="listbox"
                        class="absolute z-20 mt-1 max-h-56 w-full overflow-y-auto rounded border border-slate-300 bg-white py-1 text-sm shadow-lg dark:border-slate-600 dark:bg-slate-800"
                    >
                        {move || {
                            options()
                                .into_iter()
                                .enumerate()
                                .map(|(i, (label, v))| {
                                    view! {
                                        <li
                                            role="option"
                                            aria-selected=move || (cursor.get() == i).to_string()
                                            class=move || {
                                                let base = "cursor-pointer px-2 py-1";
                                                if cursor.get() == i {
                                                    format!("{base} bg-accent-600 text-white")
                                                } else {
                                                    format!("{base} hover:bg-slate-100 dark:hover:bg-slate-700")
                                                }
                                            }
                                            // Before the input's blur, so the click lands.
                                            on:mousedown=move |ev| {
                                                ev.prevent_default();
                                                add(v.clone());
                                            }
                                        >
                                            {label}
                                        </li>
                                    }
                                })
                                .collect_view()
                        }}
                    </ul>
                </Show>
            </div>
            {move || {
                let names = strangers();
                (!names.is_empty())
                    .then(|| {
                        view! {
                            <p class="text-[11px] font-normal text-slate-500">
                                {format!(
                                    "{} isn't one of this project's users (rite join adds them)",
                                    names.join(", "),
                                )}
                            </p>
                        }
                    })
            }}
        </div>
    }
}

/// A ticket's relationships (ticket 154): its parent, its children, the
/// tickets it's related to, and those that name it as related without being
/// named back. Each shows its number, title and column, and opens on a
/// click; the links wait while there are unsaved edits, so following one
/// can't lose them.
///
/// Ticket 207 adds the typed links (requires, supersedes, …) and
/// a way to make and break them: **Add link** picks a type, searches the
/// board's tickets by number or title, and a click on a result links it;
/// the other ticket records the opposite. Each link has a × to remove it
/// (the "Linked from" ones don't: they're recorded on the other ticket).
/// Adding and removing write both tickets, so they wait while this one has
/// unsaved edits too.
#[component]
fn Relationships(
    id: RwSignal<String>,
    place: StoredValue<Place>,
    file: StoredValue<String>,
    dirty: RwSignal<bool>,
) -> impl IntoView {
    use rite_ui_model::{
        LINK_KINDS, TicketSummary, link_candidates, link_label, split_link, ticket_number,
    };
    let ctx = Ctx::get();
    // Every ticket on this ticket's board: the columns and the epics.
    let tickets = move || -> Vec<TicketSummary> {
        let repo = place.with_value(|p| p.repo.clone());
        ctx.board.with(|b| {
            b.as_ref()
                .map(|b| {
                    b.columns
                        .iter()
                        .flat_map(|c| c.tickets.iter())
                        .chain(b.epics.iter().flat_map(|e| e.tickets.iter()))
                        .filter(|t| t.repo == repo || repo.is_empty())
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        })
    };
    // Each group: its heading, the kind to unlink it with (none when this
    // ticket doesn't hold the record), and its tickets.
    let groups = move || {
        let me = ticket_number(&id.get());
        let all = tickets();
        let find = |n: u32| {
            all.iter()
                .find(|t| ticket_number(&t.id) == Some(n))
                .cloned()
        };
        let this = me.and_then(find);
        let parent: Vec<TicketSummary> = this
            .as_ref()
            .and_then(|t| ticket_number(&t.parent))
            .and_then(find)
            .into_iter()
            .collect();
        let children: Vec<TicketSummary> = all
            .iter()
            .filter(|t| me.is_some() && ticket_number(&t.parent) == me)
            .cloned()
            .collect();
        let mine: Vec<u32> = this
            .as_ref()
            .map(|t| t.related.iter().filter_map(|r| ticket_number(r)).collect())
            .unwrap_or_default();
        let related: Vec<TicketSummary> = mine.iter().filter_map(|&n| find(n)).collect();
        let linked_from: Vec<TicketSummary> = all
            .iter()
            .filter(|t| {
                me.is_some()
                    && t.related.iter().any(|r| ticket_number(r) == me)
                    && !ticket_number(&t.id).is_some_and(|n| mine.contains(&n))
            })
            .cloned()
            .collect();
        let clone_of: Vec<TicketSummary> = this
            .as_ref()
            .and_then(|t| ticket_number(&t.clone_of))
            .and_then(find)
            .into_iter()
            .collect();
        let clones: Vec<TicketSummary> = this
            .as_ref()
            .map(|t| {
                t.clones
                    .iter()
                    .filter_map(|c| ticket_number(c))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
            .into_iter()
            .filter_map(find)
            .collect();
        let mut out: Vec<(String, Option<&'static str>, Vec<TicketSummary>)> = vec![
            ("Parent".into(), Some("child-of"), parent),
            ("Children".into(), Some("parent-of"), children),
            ("Related".into(), Some("relates-to"), related),
            ("Linked from".into(), None, linked_from),
            ("Clone of".into(), Some("clone-of"), clone_of),
            ("Clones".into(), Some("cloned-by"), clones),
        ];
        // The typed links, a group for each kind, in the picker's order.
        let mut typed: Vec<(&str, u32)> = this
            .as_ref()
            .map(|t| {
                t.links
                    .iter()
                    .filter_map(|entry| split_link(entry))
                    .filter_map(|(kind, other)| Some((kind, ticket_number(other)?)))
                    .collect()
            })
            .unwrap_or_default();
        // A typed link recorded only on the other ticket (ticket 121) shows
        // too, turned round: its `requires` naming this one is "Required by".
        for t in &all {
            let Some(theirs) = ticket_number(&t.id) else {
                continue;
            };
            for (kind, target) in t.links.iter().filter_map(|entry| split_link(entry)) {
                let ours = rite_ui_model::link_inverse(kind);
                if me.is_some()
                    && ticket_number(target) == me
                    && !typed.iter().any(|(k, n)| *k == ours && *n == theirs)
                {
                    typed.push((ours, theirs));
                }
            }
        }
        for (slug, _) in LINK_KINDS {
            let list: Vec<TicketSummary> = typed
                .iter()
                .filter(|(kind, _)| *kind == slug)
                .filter_map(|(_, n)| find(*n))
                .collect();
            out.push((link_label(slug), Some(slug), list));
        }
        out.into_iter()
            .filter(|(_, _, list)| !list.is_empty())
            .collect::<Vec<_>>()
    };

    // Making and breaking links.
    let adding = RwSignal::new(false);
    let kind = RwSignal::new("relates-to".to_string());
    let query = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let failure = RwSignal::new(None::<String>);
    let change_link = move |other: String, kind: String, remove: bool| {
        busy.set(true);
        failure.set(None);
        spawn_local(async move {
            let (place, file) = (place.get_value(), file.get_value());
            match api::link_ticket(&place, &file, &other, &kind, remove).await {
                Ok(report) => {
                    ctx.report(report);
                    query.set(String::new());
                    ctx.reload();
                }
                Err(e) => failure.set(Some(e)),
            }
            busy.set(false);
        });
    };
    // Changing a link's type (ticket 213): both tickets, one commit.
    let retype = move |other: String, old: String, new: String| {
        busy.set(true);
        failure.set(None);
        spawn_local(async move {
            let (place, file) = (place.get_value(), file.get_value());
            match api::retype_link(&place, &file, &other, &old, &new).await {
                Ok(report) => {
                    ctx.report(report);
                    ctx.reload();
                }
                Err(e) => {
                    failure.set(Some(e));
                    ctx.reload();
                }
            }
            busy.set(false);
        });
    };
    let found = move || {
        let me = ticket_number(&id.get());
        link_candidates(&tickets(), me, &query.get(), 8)
    };
    let wait = move || dirty.get() || busy.get();

    let link = move |(t, unlink): (TicketSummary, Option<&'static str>)| {
        let label = format!("{} {}", t.id, t.title);
        let column = t.column.dir_name();
        let target = Place {
            repo: t.repo.clone(),
            column: t.column,
            epic: t.epic.clone(),
        };
        let file = t.file.clone();
        let other = t.id.clone();
        view! {
            <li class="flex items-center gap-1">
                <button
                    type="button"
                    class="text-left text-sm text-accent-700 hover:underline disabled:cursor-default disabled:opacity-60 dark:text-accent-300"
                    disabled=move || dirty.get()
                    title=move || if dirty.get() { "Save or discard your edits first" } else { "Open this ticket" }
                    on:click=move |_| ctx.open_ticket(target.clone(), file.clone())
                >
                    {label}
                </button>
                <span class="ml-1 text-xs text-slate-500">{column}</span>
                {unlink
                    .map(|slug| {
                        let label = format!("Remove the link to {other}");
                        let retype_to = other.clone();
                        let picker = format!("Change how this ticket stands to {other}");
                        view! {
                            <select
                                aria-label=picker.clone()
                                title=move || {
                                    if dirty.get() { "Save or discard your edits first".to_string() } else { picker.clone() }
                                }
                                class="ml-1 rounded border border-slate-300 bg-slate-50 px-1 py-0 text-xs disabled:opacity-50 dark:border-slate-600 dark:bg-slate-900"
                                disabled=wait
                                on:change=move |ev| {
                                    let new = event_target_value(&ev);
                                    if new != slug {
                                        retype(retype_to.clone(), slug.to_string(), new);
                                    }
                                }
                            >
                                {LINK_KINDS
                                    .map(|(s, l)| view! { <option value=s selected=s == slug>{l}</option> })
                                    .collect_view()}
                            </select>
                            <button
                                type="button"
                                aria-label=label.clone()
                                title=move || {
                                    if dirty.get() { "Save or discard your edits first".to_string() } else { label.clone() }
                                }
                                class="ml-1 rounded px-1 text-xs text-slate-500 hover:bg-slate-100 hover:text-red-700 disabled:cursor-default disabled:opacity-50 dark:hover:bg-slate-800"
                                disabled=wait
                                on:click=move |_| change_link(other.clone(), slug.to_string(), true)
                            >
                                "×"
                            </button>
                        }
                    })}
            </li>
        }
    };
    view! {
        <section
            aria-label="Relationships"
            class="flex flex-col gap-1.5 rounded border border-slate-200 px-3 py-2 dark:border-slate-700"
        >
            <div class="flex items-center gap-2">
                <h3 class="text-xs font-semibold uppercase tracking-wide text-slate-600 dark:text-slate-300">
                    "Relationships"
                </h3>
                <button
                    type="button"
                    class="rounded border border-slate-300 px-2 py-0.5 text-xs hover:bg-slate-100 disabled:opacity-50 dark:border-slate-600 dark:hover:bg-slate-800"
                    aria-expanded=move || adding.get().to_string()
                    title=move || {
                        if dirty.get() {
                            "Save or discard your edits first: a link changes this ticket's file"
                        } else {
                            "Link this ticket to another"
                        }
                    }
                    disabled=move || dirty.get()
                    on:click=move |_| adding.update(|open| *open = !*open)
                >
                    {move || if adding.get() { "Done" } else { "Add link" }}
                </button>
            </div>
            <Show when=move || adding.get()>
                <div class="flex flex-col gap-1.5 rounded bg-slate-50 p-2 dark:bg-slate-800/50">
                    <div class="flex flex-wrap items-center gap-2">
                        <select
                            aria-label="Link type"
                            title="How this ticket stands to the one you pick"
                            class="rounded border border-slate-300 bg-slate-50 px-2 py-1 text-sm dark:border-slate-600 dark:bg-slate-900"
                            prop:value=move || kind.get()
                            on:change=move |ev| kind.set(event_target_value(&ev))
                        >
                            {LINK_KINDS
                                .map(|(slug, label)| view! { <option value=slug>{label}</option> })
                                .collect_view()}
                        </select>
                        <input
                            type="search"
                            aria-label="Find a ticket to link, by number or title"
                            placeholder="Find a ticket by number or title"
                            class="min-w-48 flex-1 rounded border border-slate-300 bg-white px-2 py-1 text-sm dark:border-slate-600 dark:bg-slate-900"
                            prop:value=move || query.get()
                            on:input=move |ev| query.set(event_target_value(&ev))
                        />
                    </div>
                    <ul class="flex flex-col gap-0.5" aria-label="Tickets found">
                        {move || {
                            found()
                                .into_iter()
                                .map(|t| {
                                    let (other, label) = (t.id.clone(), format!("{} {}", t.id, t.title));
                                    let column = t.column.dir_name();
                                    view! {
                                        <li class="flex items-center gap-2">
                                            <button
                                                type="button"
                                                class="text-left text-sm hover:underline disabled:cursor-default disabled:opacity-50"
                                                disabled=wait
                                                title=move || format!("{} this ticket", link_label(&kind.get()))
                                                on:click=move |_| change_link(other.clone(), kind.get_untracked(), false)
                                            >
                                                {label}
                                            </button>
                                            <span class="text-xs text-slate-500">{column}</span>
                                        </li>
                                    }
                                })
                                .collect_view()
                        }}
                    </ul>
                    <Show when=move || !query.with(|q| q.trim().is_empty()) && found().is_empty()>
                        <p class="text-xs text-slate-500">"No ticket matches"</p>
                    </Show>
                </div>
            </Show>
            {move || {
                failure
                    .get()
                    .map(|e| view! { <p role="alert" class="text-xs text-red-700 dark:text-red-400">{e}</p> })
            }}
            {move || {
                groups()
                    .into_iter()
                    .map(|(name, unlink, list)| {
                        view! {
                            <div class="flex flex-col gap-0.5">
                                <span class="text-xs font-medium text-slate-500">{name}</span>
                                <ul class="flex flex-col gap-0.5 pl-2">
                                    {list.into_iter().map(|t| link((t, unlink))).collect_view()}
                                </ul>
                            </div>
                        }
                    })
                    .collect_view()
            }}
        </section>
    }
}
