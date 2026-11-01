//! Preferences (ticket 148): settings that belong to you on this machine,
//! not to the project. Saved in KanRite's own config
//! (`~/.config/kanrite/config`), never committed: the colour scheme, the
//! zoom, and which board this machine opens when started from anywhere.

use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsCast;

use crate::api;
use crate::app::{Ctx, Modal, ModalClose, zoom_to};

#[component]
pub fn PreferencesPanel() -> impl IntoView {
    view! {
        <Modal title="Preferences".to_string() width="max-w-2xl">
            <div class="flex min-h-0 flex-col gap-4 overflow-y-auto">
                <AppearanceSection />
                <ZoomSection />
                <BoardSection />
                <AiSection />
                <SpeechSection />
                <ReadAloudSection />
                <EditorSection />
                <UpdatesSection />
            </div>
            <ModalClose />
        </Modal>
    }
}

#[component]
fn AppearanceSection() -> impl IntoView {
    use rite_ui_model::appearance::{Accent, Appearance, Theme};
    let ctx = Ctx::get();
    let problem = RwSignal::new(None::<String>);
    let choose = move |change: Appearance| {
        ctx.appearance.set(change);
        spawn_local(async move {
            match api::set_appearance(change).await {
                Ok(_) => problem.set(None),
                Err(e) => problem.set(Some(e)),
            }
        });
    };
    // Written out in full so Tailwind generates them.
    let swatch = |accent: Accent| match accent {
        Accent::Sky => "bg-sky-700",
        Accent::Indigo => "bg-indigo-600",
        Accent::Teal => "bg-teal-600",
        Accent::Slate => "bg-slate-600",
    };
    view! {
        <section class="flex flex-col gap-2">
            <h3 class="text-sm font-semibold">"Appearance"</h3>
            <div role="radiogroup" aria-label="Theme" class="flex flex-wrap gap-2">
                {Theme::ALL
                    .map(|theme| {
                        let on = move || ctx.appearance.with(|a| a.theme == theme);
                        view! {
                            <button
                                type="button"
                                role="radio"
                                aria-checked=move || on().to_string()
                                class=move || {
                                    if on() {
                                        "rounded bg-accent-600 px-3 py-1.5 text-sm font-medium text-white"
                                    } else {
                                        "rounded border border-slate-300 px-3 py-1.5 text-sm hover:bg-slate-100 dark:border-slate-600 dark:hover:bg-slate-700"
                                    }
                                }
                                on:click=move |_| {
                                    choose(Appearance { theme, ..ctx.appearance.get_untracked() })
                                }
                            >
                                {theme.label()}
                            </button>
                        }
                    })
                    .collect_view()}
            </div>
            <div role="radiogroup" aria-label="Accent colour" class="flex flex-wrap items-center gap-3">
                <span class="text-xs text-slate-500">"Accent"</span>
                {Accent::ALL
                    .map(|accent| {
                        let on = move || ctx.appearance.with(|a| a.accent == accent);
                        view! {
                            <button
                                type="button"
                                role="radio"
                                aria-checked=move || on().to_string()
                                aria-label=accent.label()
                                title=accent.label()
                                class=move || {
                                    format!(
                                        "h-7 w-7 rounded-full {} {}",
                                        swatch(accent),
                                        if on() {
                                            "ring-2 ring-offset-2 ring-slate-900 ring-offset-white dark:ring-white dark:ring-offset-slate-800"
                                        } else {
                                            "opacity-80 hover:opacity-100"
                                        },
                                    )
                                }
                                on:click=move |_| {
                                    choose(Appearance { accent, ..ctx.appearance.get_untracked() })
                                }
                            ></button>
                        }
                    })
                    .collect_view()}
            </div>
            <p class="text-xs text-slate-500">
                "Saved in your own settings on this machine, and used again next time."
            </p>
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
        </section>
    }
}

/// The board's zoom (ticket 024): the same steps as Ctrl + and Ctrl -.
#[component]
fn ZoomSection() -> impl IntoView {
    use rite_ui_model::zoom;
    let ctx = Ctx::get();
    // Worked out here, not in the markup: `view!` would read a `>` there as
    // the end of the tag.
    let smallest = move || ctx.zoom.get() <= zoom::STEPS[0];
    let largest = move || ctx.zoom.get() >= zoom::STEPS[zoom::STEPS.len() - 1];
    let button = "rounded border border-slate-300 px-3 py-1.5 text-sm hover:bg-slate-100 disabled:opacity-50 dark:border-slate-600 dark:hover:bg-slate-700";
    view! {
        <section class="flex flex-col gap-2">
            <h3 class="text-sm font-semibold">"Zoom"</h3>
            <div class="flex items-center gap-2">
                <button
                    class=button
                    aria-label="Zoom out"
                    disabled=smallest
                    on:click=move |_| zoom_to(ctx, zoom::step(ctx.zoom.get_untracked(), false))
                >
                    "−"
                </button>
                <span class="w-14 text-center font-mono text-sm" aria-live="polite">
                    {move || format!("{}%", ctx.zoom.get())}
                </span>
                <button
                    class=button
                    aria-label="Zoom in"
                    disabled=largest
                    on:click=move |_| zoom_to(ctx, zoom::step(ctx.zoom.get_untracked(), true))
                >
                    "+"
                </button>
                <button
                    class=button
                    disabled=move || ctx.zoom.get() == zoom::DEFAULT
                    on:click=move |_| zoom_to(ctx, zoom::DEFAULT)
                >
                    "Reset"
                </button>
            </div>
            <p class="text-xs text-slate-500">
                "Also Ctrl + and Ctrl −, and Ctrl 0 to reset. Remembered on this machine."
            </p>
        </section>
    }
}

/// Which board this machine opens when riteui starts from outside any
/// project (a desktop launcher starts in your home directory).
#[component]
fn BoardSection() -> impl IntoView {
    let ctx = Ctx::get();
    let dir = RwSignal::new(String::new());
    let status = RwSignal::new(None::<String>);
    spawn_local(async move {
        if let Ok(Some(d)) = api::tasks_dir().await {
            dir.set(d);
        }
    });
    let open_dir = move |_| {
        let path = dir.get_untracked();
        spawn_local(async move {
            match api::set_tasks_dir(&path).await {
                Ok(board) => {
                    dir.set(board.tasks_dir.clone());
                    ctx.board.set(Some(board));
                    ctx.error.set(None);
                    status.set(Some("Board opened.".into()));
                }
                Err(e) => status.set(Some(e)),
            }
        });
    };
    let field = "rounded border border-slate-300 bg-slate-50 px-2 py-1.5 dark:border-slate-600 dark:bg-slate-900";
    let secondary = "rounded border border-slate-300 px-3 py-1.5 text-sm hover:bg-slate-100 disabled:opacity-50 dark:border-slate-600 dark:hover:bg-slate-700";
    view! {
        <section class="flex flex-col gap-2">
            <h3 class="text-sm font-semibold">"Board to open"</h3>
            <div class="flex gap-2">
                <input
                    type="text"
                    aria-label="Tasks directory"
                    placeholder="/path/to/project-tickets/tasks"
                    class=format!("{field} flex-1 font-mono text-sm")
                    prop:value=move || dir.get()
                    on:input=move |ev| dir.set(event_target_value(&ev))
                />
                <button class=secondary on:click=open_dir>
                    "Open"
                </button>
            </div>
            <p class="text-xs text-slate-500">
                "A directory holding backlog/, todo/, doing/ and done/. Opened when riteui starts outside a project, and remembered for next time."
            </p>
            {move || status.get().map(|s| view! { <p class="text-xs">{s}</p> })}
        </section>
    }
}

/// The command the bottom panel's AI tab runs (ticket 185): `claude` unless
/// set here. A tab already running keeps its command until it's started
/// again.
#[component]
fn AiSection() -> impl IntoView {
    let command = RwSignal::new(String::new());
    let status = RwSignal::new(None::<String>);
    spawn_local(async move {
        if let Ok(c) = api::ai_command().await {
            command.set(c);
        }
    });
    let save = move |_| {
        let value = command.get_untracked();
        spawn_local(async move {
            match api::set_ai_command(&value).await {
                Ok(saved) => {
                    command.set(saved.clone());
                    status.set(Some(format!(
                        "The AI tab runs {saved} the next time it starts."
                    )));
                }
                Err(e) => status.set(Some(e)),
            }
        });
    };
    let field = "rounded border border-slate-300 bg-slate-50 px-2 py-1.5 dark:border-slate-600 dark:bg-slate-900";
    let secondary = "rounded border border-slate-300 px-3 py-1.5 text-sm hover:bg-slate-100 disabled:opacity-50 dark:border-slate-600 dark:hover:bg-slate-700";
    view! {
        <section class="flex flex-col gap-2">
            <h3 class="text-sm font-semibold">"AI command"</h3>
            <div class="flex gap-2">
                <input
                    type="text"
                    aria-label="AI command"
                    placeholder="claude"
                    class=format!("{field} flex-1 font-mono text-sm")
                    prop:value=move || command.get()
                    on:input=move |ev| command.set(event_target_value(&ev))
                />
                <button class=secondary on:click=save>
                    "Save"
                </button>
            </div>
            <p class="text-xs text-slate-500">
                "What the board's AI tab runs, in the project folder, through your shell (so it's found as in a terminal). Leave it empty for claude."
            </p>
            {move || status.get().map(|s| view! { <p class="text-xs">{s}</p> })}
            <h4 class="mt-2 text-sm font-semibold">"Let your AI see the board"</h4>
            <p class="text-xs text-slate-500">
                "Connected once, your AI tool can read the board, tickets, documents and ideas, see what's on screen here, and create, move and edit tickets (each committed, as from the board). Nothing runs until you connect one. For Claude Code, run this once:"
            </p>
            <input
                type="text"
                readonly=true
                aria-label="Command that connects Claude Code to the board"
                class=format!("{field} font-mono text-xs")
                prop:value=CONNECT_CLAUDE
                on:focus=move |ev| {
                    if let Some(input) = ev
                        .target()
                        .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
                    {
                        input.select();
                    }
                }
            />
            <p class="text-xs text-slate-500">
                "Copilot in VS Code, Cursor, Codex, Gemini CLI and others: "
                <code class="font-mono">"rite mcp setup"</code>
                " prints what to add."
            </p>
        </section>
    }
}

/// Connects Claude Code to `rite mcp` (ticket 186), as `rite mcp setup
/// claude` prints it.
const CONNECT_CLAUDE: &str = "claude mcp add --scope user kanrite -- rite mcp";

/// The editor the board's Editor button opens the project folder in (ticket
/// 188). Empty means no button.
#[component]
fn EditorSection() -> impl IntoView {
    let command = RwSignal::new(String::new());
    let status = RwSignal::new(None::<String>);
    spawn_local(async move {
        if let Ok(c) = api::editor_command().await {
            command.set(c);
        }
    });
    let save = move |_| {
        let value = command.get_untracked();
        spawn_local(async move {
            match api::set_editor_command(&value).await {
                Ok(saved) => {
                    command.set(saved.clone());
                    status.set(Some(if saved.is_empty() {
                        "No editor: the board has no Editor button.".to_string()
                    } else {
                        format!("The board's Editor button opens the project in {saved}.")
                    }));
                }
                Err(e) => status.set(Some(e)),
            }
        });
    };
    let field = "rounded border border-slate-300 bg-slate-50 px-2 py-1.5 dark:border-slate-600 dark:bg-slate-900";
    let secondary = "rounded border border-slate-300 px-3 py-1.5 text-sm hover:bg-slate-100 disabled:opacity-50 dark:border-slate-600 dark:hover:bg-slate-700";
    view! {
        <section class="flex flex-col gap-2">
            <h3 class="text-sm font-semibold">"Editor command"</h3>
            <div class="flex gap-2">
                <input
                    type="text"
                    aria-label="Editor command"
                    placeholder="code"
                    class=format!("{field} flex-1 font-mono text-sm")
                    prop:value=move || command.get()
                    on:input=move |ev| command.set(event_target_value(&ev))
                />
                <button class=secondary on:click=save>
                    "Save"
                </button>
            </div>
            <p class="text-xs text-slate-500">
                "The command that opens a folder in your editor, such as code, zed or idea. The board then shows an Editor button after AI, which opens the project folder in it. Empty: no button."
            </p>
            {move || status.get().map(|s| view! { <p class="text-xs">{s}</p> })}
        </section>
    }
}

/// Speech to text (ticket 187): nothing until a model is downloaded here;
/// a switch to turn it off (the model stays), and Remove to uninstall.
#[component]
fn SpeechSection() -> impl IntoView {
    use rite_ui_model::terminal::{SpeechProgress, SpeechStatus};
    let status = RwSignal::new(None::<SpeechStatus>);
    let progress = RwSignal::new(None::<SpeechProgress>);
    let note = RwSignal::new(None::<String>);
    let reload = move || {
        spawn_local(async move {
            match api::speech_status().await {
                Ok(s) => status.set(Some(s)),
                Err(e) => note.set(Some(e)),
            }
        });
    };
    reload();
    // Downloads report here; Preferences can be closed and opened meanwhile.
    api::on_event::<SpeechProgress>("speech-progress", move |p| {
        if p.finished {
            let _ = progress.try_set(None);
            if let Some(e) = &p.error {
                let _ = note.try_set(Some(format!("Couldn't download {}: {e}", p.model)));
            } else {
                let _ = note.try_set(Some(format!("{} is installed.", p.model)));
            }
            reload();
        } else {
            let _ = progress.try_set(Some(p));
        }
    });
    let download = move |model: String| {
        note.set(None);
        progress.set(Some(SpeechProgress {
            model: model.clone(),
            ..Default::default()
        }));
        spawn_local(async move {
            if let Err(e) = api::speech_download(&model).await {
                progress.set(None);
                note.set(Some(e));
            }
        });
    };
    let remove = move |model: String| {
        spawn_local(async move {
            match api::speech_remove(&model).await {
                Ok(()) => note.set(Some(format!("{model} is removed."))),
                Err(e) => note.set(Some(e)),
            }
            reload();
        });
    };
    let choose = move |model: String| {
        spawn_local(async move {
            if let Err(e) = api::set_speech_model(&model).await {
                note.set(Some(e));
            }
            reload();
        });
    };
    let switch = move |on: bool| {
        spawn_local(async move {
            if let Err(e) = api::set_speech_enabled(on).await {
                note.set(Some(e));
            }
            reload();
        });
    };
    let small = "rounded border border-slate-300 px-2 py-1 text-xs hover:bg-slate-100 disabled:opacity-50 dark:border-slate-600 dark:hover:bg-slate-700";
    view! {
        <section class="flex flex-col gap-2">
            <h3 class="text-sm font-semibold">"Speech to text"</h3>
            <p class="text-xs text-slate-500">
                "Talk to the terminal and the AI tab: hold the 🎤 in the panel, or Ctrl+Shift+Space. It runs a Whisper model on this machine; nothing is sent anywhere. Download a model to start; Remove takes it off again."
            </p>
            {move || {
                status
                    .get()
                    .map(|st| {
                        let installed_any = st.models.iter().any(|m| m.installed);
                        let enabled = st.enabled;
                        let mic = st
                            .microphone
                            .clone()
                            .map(|m| format!("Microphone: {m}"))
                            .unwrap_or_else(|| "No microphone found.".to_string());
                        let chosen = st.model.clone();
                        view! {
                            <Show when=move || installed_any>
                                <label class="flex items-center gap-2 text-sm">
                                    <input
                                        type="checkbox"
                                        prop:checked=enabled
                                        on:change=move |ev| switch(event_target_checked(&ev))
                                    />
                                    "Use speech to text"
                                    <span class="text-xs text-slate-500">"(off hides the 🎤 and frees its memory; the model stays)"</span>
                                </label>
                            </Show>
                            <ul class="flex flex-col gap-1">
                                {st
                                    .models
                                    .into_iter()
                                    .map(|m| {
                                        let id = m.id.clone();
                                        let (get, rm, use_it) = (id.clone(), id.clone(), id.clone());
                                        let in_use = chosen.as_deref() == Some(id.as_str());
                                        let busy = move || progress.with(Option::is_some);
                                        let this_progress = {
                                            let id = id.clone();
                                            move || {
                                                progress.with(|p| {
                                                    p.as_ref().filter(|p| p.model == id).map(|p| {
                                                        if p.total > 0 {
                                                            format!(
                                                                "{:.0} of {:.0} MB",
                                                                p.done as f64 / 1e6,
                                                                p.total as f64 / 1e6
                                                            )
                                                        } else {
                                                            "starting…".to_string()
                                                        }
                                                    })
                                                })
                                            }
                                        };
                                        view! {
                                            <li class="flex items-center gap-2 text-sm">
                                                <span class="w-20 font-mono">{id.clone()}</span>
                                                <span class="w-16 text-xs text-slate-500">{format!("{} MB", m.size_mb)}</span>
                                                {if m.installed {
                                                    view! {
                                                        <span class="text-xs text-green-700 dark:text-green-400">
                                                            {if in_use { "installed, in use" } else { "installed" }}
                                                        </span>
                                                        <span class="flex-1"></span>
                                                        <Show when=move || !in_use>
                                                            <button type="button" class=small on:click={
                                                                let id = use_it.clone();
                                                                move |_| choose(id.clone())
                                                            }>"Use"</button>
                                                        </Show>
                                                        <button type="button" class=small on:click={
                                                            let id = rm.clone();
                                                            move |_| remove(id.clone())
                                                        }>"Remove"</button>
                                                    }
                                                        .into_any()
                                                } else {
                                                    view! {
                                                        <span class="flex-1 text-xs text-slate-500">{this_progress.clone()}</span>
                                                        <button type="button" class=small disabled=busy on:click={
                                                            let id = get.clone();
                                                            move |_| download(id.clone())
                                                        }>"Download"</button>
                                                    }
                                                        .into_any()
                                                }}
                                            </li>
                                        }
                                    })
                                    .collect_view()}
                            </ul>
                            <p class="text-xs text-slate-500">
                                {mic}
                                ". tiny.en is quick; base.en is more accurate. Both are English only."
                            </p>
                        }
                    })
            }}
            {move || note.get().map(|n| view! { <p class="text-xs">{n}</p> })}
        </section>
    }
}

/// Read aloud (ticket 286): the system's own speech, so there's nothing to
/// download; the switch hides the 🔊.
#[component]
fn ReadAloudSection() -> impl IntoView {
    use rite_ui_model::terminal::ReadAloudStatus;
    let status = RwSignal::new(None::<ReadAloudStatus>);
    let note = RwSignal::new(None::<String>);
    let reload = move || {
        spawn_local(async move {
            match api::read_aloud_status().await {
                Ok(s) => status.set(Some(s)),
                Err(e) => note.set(Some(e)),
            }
        });
    };
    reload();
    let switch = move |on: bool| {
        spawn_local(async move {
            if let Err(e) = api::set_read_aloud_enabled(on).await {
                note.set(Some(e));
            }
            reload();
        });
    };
    let test = move |_| {
        note.set(None);
        spawn_local(async move {
            if let Err(e) = api::read_aloud(None, Some("Read aloud is working.")).await {
                note.set(Some(e));
            }
        });
    };
    let small = "rounded border border-slate-300 px-2 py-1 text-xs hover:bg-slate-100 disabled:opacity-50 dark:border-slate-600 dark:hover:bg-slate-700";
    view! {
        <section class="flex flex-col gap-2">
            <h3 class="text-sm font-semibold">"Read aloud"</h3>
            <p class="text-xs text-slate-500">
                "The 🔊 in the panel reads the text you've selected in the terminal, or what came out since you last pressed Enter (an AI tool's reply, without its tool calls). It uses this computer's own speech; nothing is downloaded or sent anywhere."
            </p>
            {move || {
                status
                    .get()
                    .map(|st| {
                        let enabled = st.enabled;
                        let found = st.engine.is_some();
                        let engine = match st.engine {
                            Some(name) => format!("Speech: {name}."),
                            None => "No system speech found. On Linux, install speech-dispatcher (it provides spd-say).".to_string(),
                        };
                        view! {
                            <label class="flex items-center gap-2 text-sm">
                                <input
                                    type="checkbox"
                                    prop:checked=enabled
                                    on:change=move |ev| switch(event_target_checked(&ev))
                                />
                                "Use read aloud"
                                <span class="text-xs text-slate-500">"(off hides the 🔊)"</span>
                            </label>
                            <div class="flex items-center gap-2">
                                <p class="flex-1 text-xs text-slate-500">{engine}</p>
                                <button type="button" class=small disabled=move || !(enabled && found) on:click=test>
                                    "Test"
                                </button>
                            </div>
                        }
                    })
            }}
            {move || note.get().map(|n| view! { <p class="text-xs">{n}</p> })}
            <PiperSection />
        </section>
    }
}

/// A natural voice (ticket 287): Piper set up behind speech-dispatcher, on
/// Linux only (macOS's own voices are already natural).
#[component]
fn PiperSection() -> impl IntoView {
    use rite_ui_model::terminal::{PiperProgress, PiperStatus};
    let status = RwSignal::new(None::<PiperStatus>);
    let stage = RwSignal::new(None::<String>);
    let note = RwSignal::new(None::<String>);
    let chosen = RwSignal::new(String::new());
    let reload = move || {
        spawn_local(async move {
            match api::piper_status().await {
                Ok(s) => {
                    if chosen.get_untracked().is_empty() {
                        let first = s
                            .installed
                            .clone()
                            .or_else(|| s.voices.first().map(|v| v.id.clone()))
                            .unwrap_or_default();
                        chosen.set(first);
                    }
                    if s.busy {
                        stage.set(Some("working…".to_string()));
                    }
                    status.set(Some(s));
                }
                Err(e) => note.set(Some(e)),
            }
        });
    };
    reload();
    api::on_event::<PiperProgress>("piper-progress", move |p| {
        if p.stage == "done" {
            stage.set(None);
            note.set(Some(match p.error {
                Some(e) => e,
                None => "Set up: read aloud now uses Piper.".to_string(),
            }));
            reload();
        } else {
            stage.set(Some(
                match p.stage.as_str() {
                    "installing" => "Installing Piper (about 190 MB, a minute or two)…",
                    "voice" => "Downloading the voice (about 60 MB)…",
                    _ => "Setting up speech-dispatcher…",
                }
                .to_string(),
            ));
        }
    });
    let install = move |_| {
        let voice = chosen.get_untracked();
        note.set(None);
        stage.set(Some("Starting…".to_string()));
        spawn_local(async move {
            if let Err(e) = api::piper_install(&voice).await {
                stage.set(None);
                note.set(Some(e));
            }
        });
    };
    let remove = move |_| {
        note.set(None);
        stage.set(Some("Removing…".to_string()));
        spawn_local(async move {
            let outcome = api::piper_remove().await;
            stage.set(None);
            note.set(Some(match outcome {
                Ok(()) => "Removed: read aloud is back to the system voice.".to_string(),
                Err(e) => e,
            }));
            reload();
        });
    };
    let busy = move || stage.with(Option::is_some);
    let small = "rounded border border-slate-300 px-2 py-1 text-xs hover:bg-slate-100 disabled:opacity-50 dark:border-slate-600 dark:hover:bg-slate-700";
    view! {
        {move || {
            status
                .get()
                .filter(|st| st.supported)
                .map(|st| {
                    let installed = st.installed.clone();
                    let has = installed.is_some();
                    let can_install = st.installer.is_some() || has;
                    let label_of = {
                        let voices = st.voices.clone();
                        move |id: &str| {
                            voices
                                .iter()
                                .find(|v| v.id == id)
                                .map_or_else(|| id.to_string(), |v| v.label.clone())
                        }
                    };
                    let state_line = match &installed {
                        Some(id) => format!("Using Piper: {}.", label_of(id)),
                        None => match &st.installer {
                            Some(how) => format!(
                                "Not set up. Set up installs Piper with {how} into riteui's own folder, downloads the voice, and makes it speech-dispatcher's voice (for other programs too). Remove undoes all of it."
                            ),
                            None => "Setting up Piper needs uv or Python 3 with venv (on Ubuntu: sudo apt install python3-venv).".to_string(),
                        },
                    };
                    let unchanged = {
                        let installed = installed.clone();
                        move || installed.as_deref() == Some(chosen.get().as_str())
                    };
                    view! {
                        <div class="flex flex-col gap-2 rounded border border-slate-200 p-2 dark:border-slate-700">
                            <h4 class="text-xs font-semibold">"A natural voice (Piper)"</h4>
                            <p class="text-xs text-slate-500">{state_line}</p>
                            <div class="flex flex-wrap items-center gap-2">
                                <select
                                    class="rounded border border-slate-300 bg-white px-2 py-1 text-xs dark:border-slate-600 dark:bg-slate-800"
                                    prop:value=move || chosen.get()
                                    disabled=busy
                                    on:change=move |ev| chosen.set(event_target_value(&ev))
                                >
                                    {st
                                        .voices
                                        .iter()
                                        .map(|v| view! { <option value=v.id.clone()>{v.label.clone()}</option> })
                                        .collect_view()}
                                </select>
                                <button
                                    type="button"
                                    class=small
                                    disabled=move || busy() || !can_install || unchanged()
                                    on:click=install
                                >
                                    {if has { "Use this voice" } else { "Set up" }}
                                </button>
                                <Show when=move || has>
                                    <button type="button" class=small disabled=busy on:click=remove>
                                        "Remove"
                                    </button>
                                </Show>
                            </div>
                        </div>
                    }
                })
        }}
        {move || stage.get().map(|s| view! { <p class="text-xs text-slate-500">{s}</p> })}
        {move || note.get().map(|n| view! { <p class="text-xs">{n}</p> })}
    }
}

/// Updates (ticket 304): the version running, when GitHub last answered a
/// check, and Check now.
#[component]
fn UpdatesSection() -> impl IntoView {
    let checked = RwSignal::new(None::<i64>);
    let note = RwSignal::new(None::<String>);
    let reload = move || {
        spawn_local(async move {
            if let Ok(at) = api::update_checked().await {
                checked.set(at);
            }
        });
    };
    reload();
    // Read again once a check has said what it found.
    Effect::new(move |_| {
        if note.with(|n| n.as_deref().is_some_and(|n| !n.starts_with("Checking"))) {
            reload();
        }
    });
    let last = move || match checked.get() {
        Some(at) => {
            let now = (js_sys::Date::now() / 1000.0) as i64;
            format!("Last checked {}.", rite_ui_model::ago(now - at))
        }
        None => "Not checked yet.".to_string(),
    };
    let small = "rounded border border-slate-300 px-2 py-1 text-xs hover:bg-slate-100 disabled:opacity-50 dark:border-slate-600 dark:hover:bg-slate-700";
    view! {
        <section class="flex flex-col gap-2">
            <h3 class="text-sm font-semibold">"Updates"</h3>
            <div class="flex items-center gap-2">
                <p class="flex-1 text-sm">
                    {concat!("KanRite ", env!("CARGO_PKG_VERSION"), ". ")}
                    <span class="text-xs text-slate-500">{last}</span>
                </p>
                <button
                    type="button"
                    class=small
                    disabled=move || note.with(|n| n.as_deref().is_some_and(|n| n.starts_with("Checking")))
                    on:click=move |_| crate::update_bar::check_by_hand(note)
                >
                    "Check now"
                </button>
            </div>
            <p class="text-xs text-slate-500">
                "riteui also checks by itself every three hours (unless update-check = off in your config). Clicking the version in the top bar checks too."
            </p>
            {move || note.get().map(|n| view! { <p role="status" class="text-xs">{n}</p> })}
        </section>
    }
}
