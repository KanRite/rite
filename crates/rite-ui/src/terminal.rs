//! The bottom panel's terminals (ticket 185), on `alacritty_terminal`: its
//! `tty` opens the pseudo-terminal and runs the program, its event loop
//! reads the output into a `Term` on a thread of its own, and a frame
//! thread here sends the page the rows that changed, at most every 16 ms,
//! through a [`Sink`] (Tauri events in the app, a channel in tests).
//! All Rust: no terminal code in JavaScript.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config, Term, TermDamage, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
use rite_ui_model::terminal::{TermCursor, TermFrame, TermRow, TermRun};

/// How often, at most, a terminal's changes go to the page.
const FRAME: Duration = Duration::from_millis(16);
/// Lines kept above the screen.
const SCROLLBACK: usize = 5000;

/// `PSModulePath` for Windows PowerShell 5.1 with PowerShell 7's module
/// folders taken out (ticket 346). A shell started from riteui inherits the
/// variable, and where PowerShell 7 is installed it lists 7's folders first, so
/// 5.1 finds 7's PSReadLine (and other modules) before its own and can't load
/// them: "Cannot load PSReadline module". Out go the entries with a folder
/// called `PowerShell` (`Documents\PowerShell\Modules`, `Program Files\PowerShell\Modules`)
/// or `Microsoft.PowerShell_…` (the Store app's); the rest
/// stay in order, `WindowsPowerShell` ones and any a user added. `None` when
/// there is nothing to change, so the variable is left alone.
#[cfg_attr(not(windows), allow(dead_code))]
fn without_powershell_7(path: &str) -> Option<String> {
    let is_7 = |entry: &str| {
        entry.split(['\\', '/']).any(|part| {
            let part = part.to_ascii_lowercase();
            part == "powershell" || part.starts_with("microsoft.powershell_")
        })
    };
    let kept: Vec<&str> = path
        .split(';')
        .filter(|entry| !entry.trim().is_empty() && !is_7(entry))
        .collect();
    let all = path.split(';').filter(|e| !e.trim().is_empty()).count();
    (kept.len() != all && !kept.is_empty()).then(|| kept.join(";"))
}

/// Where frames go.
pub trait Sink: Send + Sync + 'static {
    fn frame(&self, frame: TermFrame);
    fn exited(&self, id: u32);
}

/// The screen size, as `alacritty_terminal` wants it.
struct Size {
    cols: usize,
    lines: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

fn window_size(cols: u16, lines: u16) -> WindowSize {
    WindowSize {
        num_lines: lines,
        num_cols: cols,
        cell_width: 8,
        cell_height: 16,
    }
}

/// Hears the terminal: output arrived (redraw), a reply the program asked
/// for (written back to it), the program ended.
#[derive(Clone)]
struct Listener {
    dirty: Arc<AtomicBool>,
    exited: Arc<AtomicBool>,
    writer: Arc<Mutex<Option<EventLoopSender>>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match event {
            Event::Wakeup => self.dirty.store(true, Ordering::Release),
            Event::PtyWrite(text) => {
                if let Ok(writer) = self.writer.lock()
                    && let Some(writer) = writer.as_ref()
                {
                    let _ = writer.send(Msg::Input(Cow::Owned(text.into_bytes())));
                }
            }
            Event::ChildExit(_) | Event::Exit => {
                self.exited.store(true, Ordering::Release);
                self.dirty.store(true, Ordering::Release);
            }
            _ => {}
        }
    }
}

/// A running terminal.
pub struct Session {
    term: Arc<FairMutex<Term<Listener>>>,
    sender: EventLoopSender,
    dirty: Arc<AtomicBool>,
    full: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    /// Where the input was when Enter was last typed, as a line counted
    /// from the top of the history, so "read aloud" can find the output
    /// since (ticket 286).
    anchor: Mutex<Option<usize>>,
}

impl Session {
    /// Starts `program args` in `cwd` on a `cols` × `lines` screen.
    pub fn open(
        id: u32,
        program: String,
        args: Vec<String>,
        cwd: Option<PathBuf>,
        cols: u16,
        lines: u16,
        sink: Arc<dyn Sink>,
    ) -> std::io::Result<Session> {
        let (cols, lines) = (cols.max(2), lines.max(1));
        let listener = Listener {
            dirty: Arc::new(AtomicBool::new(true)),
            exited: Arc::new(AtomicBool::new(false)),
            writer: Arc::new(Mutex::new(None)),
        };
        let config = Config {
            scrolling_history: SCROLLBACK,
            ..Config::default()
        };
        let size = Size {
            cols: cols as usize,
            lines: lines as usize,
        };
        let term = Arc::new(FairMutex::new(Term::new(config, &size, listener.clone())));
        // Windows PowerShell must not see PowerShell 7's modules (ticket 346).
        #[cfg(windows)]
        let powershell_modules = program
            .eq_ignore_ascii_case("powershell.exe")
            .then(|| std::env::var("PSModulePath").ok())
            .flatten()
            .and_then(|path| without_powershell_7(&path));
        let options = tty::Options {
            shell: Some(tty::Shell::new(program, args)),
            working_directory: cwd,
            drain_on_exit: true,
            // Windows only (ticket 280): quote the arguments by the C
            // runtime's rules, as any program there expects to read them.
            #[cfg(windows)]
            escape_args: true,
            env: [
                ("TERM".to_string(), "xterm-256color".to_string()),
                ("COLORTERM".to_string(), "truecolor".to_string()),
            ]
            .into_iter()
            .collect(),
        };
        #[cfg(windows)]
        let options = {
            let mut options = options;
            if let Some(path) = powershell_modules {
                options.env.insert("PSModulePath".to_string(), path);
            }
            options
        };
        let pty = tty::new(&options, window_size(cols, lines), u64::from(id))?;
        let event_loop = EventLoop::new(Arc::clone(&term), listener.clone(), pty, true, false)?;
        let sender = event_loop.channel();
        if let Ok(mut writer) = listener.writer.lock() {
            *writer = Some(sender.clone());
        }
        event_loop.spawn();

        let session = Session {
            term: Arc::clone(&term),
            sender,
            dirty: Arc::clone(&listener.dirty),
            full: Arc::new(AtomicBool::new(true)),
            stop: Arc::new(AtomicBool::new(false)),
            anchor: Mutex::new(None),
        };
        let (dirty, full, stop, exited) = (
            Arc::clone(&session.dirty),
            Arc::clone(&session.full),
            Arc::clone(&session.stop),
            Arc::clone(&listener.exited),
        );
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(FRAME);
                if stop.load(Ordering::Acquire) {
                    break;
                }
                if dirty.swap(false, Ordering::AcqRel) {
                    let frame =
                        build_frame(&mut term.lock(), id, full.swap(false, Ordering::AcqRel));
                    sink.frame(frame);
                } else if exited.load(Ordering::Acquire) {
                    sink.exited(id);
                    break;
                }
            }
        });
        Ok(session)
    }

    /// Keystrokes or pasted text, as the program should get them. Typing
    /// brings a scrolled-back screen back to the bottom.
    pub fn input(&self, text: &str) {
        {
            let mut term = self.term.lock();
            if term.grid().display_offset() != 0 {
                term.scroll_display(Scroll::Bottom);
                self.redraw();
            }
            // Enter typed, not a paste with line ends in it.
            if text.contains('\r')
                && !text.starts_with("\x1b[200~")
                && let Ok(mut anchor) = self.anchor.lock()
            {
                let (lines, history) = all_lines(&term);
                *anchor = Some(input_top(&lines, history + cursor_line(&term)));
            }
        }
        let _ = self
            .sender
            .send(Msg::Input(Cow::Owned(text.as_bytes().to_vec())));
    }

    /// A new screen size, for the program and the screen alike.
    pub fn resize(&self, cols: u16, lines: u16) {
        let (cols, lines) = (cols.max(2), lines.max(1));
        self.term.lock().resize(Size {
            cols: cols as usize,
            lines: lines as usize,
        });
        let _ = self.sender.send(Msg::Resize(window_size(cols, lines)));
        self.redraw();
    }

    /// Scrolls the screen back (positive) or forward through the history.
    pub fn scroll(&self, lines: i32) {
        self.term.lock().scroll_display(Scroll::Delta(lines));
        self.redraw();
    }

    /// The output since Enter was last typed, without the line it was typed
    /// on or the input waiting now (ticket 286). With no Enter seen, a
    /// full-screen program running, or the Enter scrolled out of the
    /// history, the screen as it is.
    pub fn since_enter(&self) -> String {
        let term = self.term.lock();
        let (lines, history) = all_lines(&term);
        let full_screen = term.mode().contains(TermMode::ALT_SCREEN);
        // Once the history is full, lines fall off the top and an anchor
        // counted from there no longer points where it did.
        let anchor = self
            .anchor
            .lock()
            .ok()
            .and_then(|a| *a)
            .filter(|_| !full_screen && history < SCROLLBACK);
        reply(&lines, anchor, history + cursor_line(&term), history)
    }

    fn redraw(&self) {
        self.full.store(true, Ordering::Release);
        self.dirty.store(true, Ordering::Release);
    }
}

impl Drop for Session {
    /// Ends the program: the event loop stops and drops the pseudo-terminal,
    /// which hangs the program up.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.sender.send(Msg::Shutdown);
    }
}

/// The cursor's line on the screen.
fn cursor_line(term: &Term<Listener>) -> usize {
    term.grid().cursor.point.line.0.max(0) as usize
}

/// Every line, history first, as (text, wraps on to the next), and how many
/// of them are history.
fn all_lines(term: &Term<Listener>) -> (Vec<(String, bool)>, usize) {
    let grid = term.grid();
    let history = grid.history_size();
    let cols = term.columns();
    let lines = (-(history as i32)..term.screen_lines() as i32)
        .map(|y| {
            let row = &grid[Line(y)];
            let mut text = String::new();
            for x in 0..cols {
                let cell = &row[Column(x)];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                text.push(cell.c);
                if let Some(extra) = cell.zerowidth() {
                    text.extend(extra);
                }
            }
            let wraps = row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
            if !wraps {
                text.truncate(text.trim_end().len());
            }
            (text, wraps)
        })
        .collect();
    (lines, history)
}

/// Where the input starts: the cursor's line, or the line above when that's
/// a box's border (an AI tool's prompt box, Claude Code's among them).
fn input_top(lines: &[(String, bool)], cursor: usize) -> usize {
    match cursor.checked_sub(1) {
        Some(above) if lines.get(above).is_some_and(|(t, _)| is_border(t)) => above,
        _ => cursor,
    }
}

/// A line drawn only with box-drawing characters.
fn is_border(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty()
        && text
            .chars()
            .all(|c| matches!(c, '\u{2500}'..='\u{257F}' | ' '))
}

/// Two prompt lines alike but for their digits.
fn same_shape(a: &str, b: &str) -> bool {
    let shape = |s: &str| {
        s.trim_end()
            .chars()
            .map(|c| if c.is_ascii_digit() { '0' } else { c })
            .collect::<String>()
    };
    shape(a) == shape(b)
}

/// The text between the anchor's line (left out: what was typed there) and
/// the input waiting now; the screen from `screen_top` without an anchor.
/// Wrapped rows are joined back into their line.
fn reply(
    lines: &[(String, bool)],
    anchor: Option<usize>,
    cursor: usize,
    screen_top: usize,
) -> String {
    let mut end = input_top(lines, cursor.min(lines.len().saturating_sub(1))).min(lines.len());
    let anchor = anchor.filter(|a| *a < end);
    // A prompt of several lines (a clock, the folder, then `❯`): the lines
    // above the new input that match those above the old one are prompt
    // too. Digits are ignored, since prompts show the time.
    if let Some(a) = anchor {
        let mut k = 0;
        while k < 5
            && k < a
            && end > a + 1
            && !lines[end - 1].0.trim().is_empty()
            && same_shape(&lines[a - 1 - k].0, &lines[end - 1].0)
        {
            k += 1;
            end -= 1;
        }
    }
    let start = anchor.unwrap_or(screen_top.min(end));
    let mut logical: Vec<String> = Vec::new();
    let mut joining = false;
    for (text, wraps) in &lines[start..end] {
        if joining && let Some(last) = logical.last_mut() {
            last.push_str(text);
        } else {
            logical.push(text.clone());
        }
        joining = *wraps;
    }
    if anchor.is_some() && !logical.is_empty() {
        logical.remove(0);
    }
    let text = logical.join("\n");
    text.trim_matches('\n').to_string()
}

/// The rows that changed (all of them with `full`), the cursor and the modes.
fn build_frame(term: &mut Term<Listener>, id: u32, full: bool) -> TermFrame {
    let cols = term.columns();
    let lines = term.screen_lines();
    let changed: Vec<usize> = match term.damage() {
        TermDamage::Full => (0..lines).collect(),
        TermDamage::Partial(_) if full => (0..lines).collect(),
        TermDamage::Partial(damaged) => damaged.map(|d| d.line).collect(),
    };
    let all = full || changed.len() == lines;
    term.reset_damage();
    let offset = term.grid().display_offset() as i32;
    let colors = term.colors();
    let rows = changed
        .into_iter()
        .filter(|y| *y < lines)
        .map(|y| TermRow {
            y: y as u16,
            runs: row_runs(term, Line(y as i32 - offset), cols, colors),
        })
        .collect();
    let content = term.renderable_content();
    let cursor_y = content.cursor.point.line.0 + offset;
    let mode = *term.mode();
    TermFrame {
        id,
        cols: cols as u16,
        lines: lines as u16,
        rows,
        full: all,
        cursor: TermCursor {
            x: content.cursor.point.column.0 as u16,
            y: cursor_y.max(0) as u16,
            visible: mode.contains(TermMode::SHOW_CURSOR)
                && content.cursor.shape != CursorShape::Hidden
                && cursor_y >= 0
                && (cursor_y as usize) < lines,
        },
        app_cursor: mode.contains(TermMode::APP_CURSOR),
        bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
        alt_screen: mode.contains(TermMode::ALT_SCREEN),
    }
}

/// One row as runs of like-drawn text, trailing blank space dropped.
fn row_runs(term: &Term<Listener>, line: Line, cols: usize, colors: &Colors) -> Vec<TermRun> {
    let row = &term.grid()[line];
    let mut runs: Vec<TermRun> = Vec::new();
    for x in 0..cols {
        let cell = &row[Column(x)];
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        let mut fg = css(cell.fg, colors);
        let mut bg = css(cell.bg, colors);
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
            if fg.is_empty() {
                fg = "var(--term-bg)".to_string();
            }
            if bg.is_empty() {
                bg = "var(--term-fg)".to_string();
            }
        }
        let mut text = String::new();
        if cell.flags.contains(Flags::HIDDEN) {
            text.push(' ');
        } else {
            text.push(cell.c);
            if let Some(extra) = cell.zerowidth() {
                text.extend(extra);
            }
        }
        let run = TermRun {
            text,
            fg,
            bg,
            bold: cell.flags.contains(Flags::BOLD),
            italic: cell.flags.contains(Flags::ITALIC),
            underline: cell.flags.intersects(
                Flags::UNDERLINE
                    | Flags::DOUBLE_UNDERLINE
                    | Flags::UNDERCURL
                    | Flags::DOTTED_UNDERLINE
                    | Flags::DASHED_UNDERLINE,
            ),
            dim: cell.flags.contains(Flags::DIM),
            strike: cell.flags.contains(Flags::STRIKEOUT),
        };
        match runs.last_mut() {
            Some(last) if same_look(last, &run) => last.text.push_str(&run.text),
            _ => runs.push(run),
        }
    }
    // Blank space at the end of the row draws nothing.
    if let Some(last) = runs.last_mut()
        && last.bg.is_empty()
        && !last.underline
        && !last.strike
    {
        let kept = last.text.trim_end_matches(' ').len();
        last.text.truncate(kept);
        if last.text.is_empty() {
            runs.pop();
        }
    }
    runs
}

fn same_look(a: &TermRun, b: &TermRun) -> bool {
    a.fg == b.fg
        && a.bg == b.bg
        && a.bold == b.bold
        && a.italic == b.italic
        && a.underline == b.underline
        && a.dim == b.dim
        && a.strike == b.strike
}

/// A cell colour as CSS: empty for the theme's own colours.
fn css(color: Color, colors: &Colors) -> String {
    let index = match color {
        Color::Spec(rgb) => return format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b),
        Color::Indexed(i) => i as usize,
        Color::Named(named) => {
            let n = named as usize;
            match named {
                NamedColor::Foreground
                | NamedColor::Background
                | NamedColor::Cursor
                | NamedColor::BrightForeground
                | NamedColor::DimForeground => return String::new(),
                _ if n < 16 => n,
                // The dim colours draw as their normal ones.
                _ => n.saturating_sub(NamedColor::DimBlack as usize).min(7),
            }
        }
    };
    let (r, g, b) = match colors[index] {
        Some(rgb) => (rgb.r, rgb.g, rgb.b),
        None => palette(index),
    };
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// xterm's 256 colours.
fn palette(index: usize) -> (u8, u8, u8) {
    const BASE: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0xcd, 0x31, 0x31),
        (0x0d, 0xbc, 0x79),
        (0xe5, 0xe5, 0x10),
        (0x24, 0x72, 0xc8),
        (0xbc, 0x3f, 0xbc),
        (0x11, 0xa8, 0xcd),
        (0xe5, 0xe5, 0xe5),
        (0x66, 0x66, 0x66),
        (0xf1, 0x4c, 0x4c),
        (0x23, 0xd1, 0x8b),
        (0xf5, 0xf5, 0x43),
        (0x3b, 0x8e, 0xea),
        (0xd6, 0x70, 0xd6),
        (0x29, 0xb8, 0xdb),
        (0xff, 0xff, 0xff),
    ];
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match index {
        0..=15 => BASE[index],
        16..=231 => {
            let i = index - 16;
            (LEVELS[i / 36], LEVELS[(i / 6) % 6], LEVELS[i % 6])
        }
        232..=255 => {
            let v = (8 + (index - 232) * 10) as u8;
            (v, v, v)
        }
        _ => (0xe5, 0xe5, 0xe5),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::mpsc;

    struct Channel(Mutex<mpsc::Sender<Option<TermFrame>>>);

    impl Sink for Channel {
        fn frame(&self, frame: TermFrame) {
            let _ = self.0.lock().unwrap().send(Some(frame));
        }
        fn exited(&self, _: u32) {
            let _ = self.0.lock().unwrap().send(None);
        }
    }

    fn start(script: &str, cols: u16, lines: u16) -> (Session, mpsc::Receiver<Option<TermFrame>>) {
        let (tx, rx) = mpsc::channel();
        let session = Session::open(
            1,
            "/bin/sh".into(),
            vec!["-c".into(), script.into()],
            None,
            cols,
            lines,
            Arc::new(Channel(Mutex::new(tx))),
        )
        .unwrap();
        (session, rx)
    }

    /// The screen's text, built up from frames until `until` is on it or
    /// the program ends.
    fn screen(
        rx: &mpsc::Receiver<Option<TermFrame>>,
        until: &str,
    ) -> (Vec<Vec<TermRun>>, Option<TermFrame>) {
        let mut rows: Vec<Vec<TermRun>> = Vec::new();
        let mut last = None;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Some(frame)) => {
                    for row in &frame.rows {
                        let y = row.y as usize;
                        if rows.len() <= y {
                            rows.resize(y + 1, Vec::new());
                        }
                        rows[y] = row.runs.clone();
                    }
                    last = Some(frame);
                    let text: String = rows.iter().flatten().map(|r| r.text.as_str()).collect();
                    if text.contains(until) {
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => {}
            }
        }
        (rows, last)
    }

    #[test]
    fn output_comes_through_with_its_colours() {
        let (_session, rx) = start("printf 'hi \\033[31mred\\033[0m done'; sleep 1", 40, 5);
        let (rows, _) = screen(&rx, "done");
        let first = &rows[0];
        let red = first.iter().find(|r| r.text == "red").expect("a red run");
        assert_eq!(red.fg, "#cd3131");
        assert!(first[0].text.starts_with("hi "));
        assert!(first[0].fg.is_empty(), "default colour left to the theme");
    }

    #[test]
    fn input_reaches_the_program() {
        let (session, rx) = start("read line; echo \"got:$line\"; sleep 1", 40, 5);
        std::thread::sleep(Duration::from_millis(200));
        session.input("hello\r");
        let (rows, _) = screen(&rx, "got:hello");
        let text: String = rows.iter().flatten().map(|r| r.text.as_str()).collect();
        assert!(text.contains("got:hello"), "{text}");
    }

    #[test]
    fn a_resize_changes_the_screen() {
        let (session, rx) = start("sleep 5", 40, 5);
        // The first frame, then the resize while the program still runs.
        let _ = rx.recv_timeout(Duration::from_secs(2));
        session.resize(60, 8);
        let mut size = None;
        for _ in 0..20 {
            if let Ok(Some(frame)) = rx.recv_timeout(Duration::from_millis(100)) {
                size = Some((frame.cols, frame.lines));
                break;
            }
        }
        assert_eq!(size, Some((60, 8)));
    }

    #[test]
    fn the_end_of_the_program_is_reported() {
        let (_session, rx) = start("echo bye", 20, 3);
        let mut ended = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if let Ok(None) = rx.recv_timeout(Duration::from_millis(200)) {
                ended = true;
                break;
            }
        }
        assert!(ended);
    }

    #[test]
    fn leaving_the_alternate_screen_brings_the_old_one_back() {
        let (_session, rx) = start(
            "printf 'one\\ntwo\\n'; sleep 0.3; printf '\\033[?1049hFULLSCREEN'; sleep 0.3; printf '\\033[?1049l'; sleep 0.3; printf 'after'; sleep 1",
            30,
            6,
        );
        let (rows, last) = screen(&rx, "after");
        let text: Vec<String> = rows
            .iter()
            .map(|r| r.iter().map(|run| run.text.as_str()).collect())
            .collect();
        assert!(!text.iter().any(|t| t.contains("FULLSCREEN")), "{text:?}");
        assert_eq!(text[0], "one", "{text:?}");
        assert_eq!(text[1], "two", "{text:?}");
        assert!(!last.unwrap().alt_screen);
    }

    fn lines(rows: &[&str]) -> Vec<(String, bool)> {
        rows.iter().map(|r| (r.to_string(), false)).collect()
    }

    #[test]
    fn a_shell_reply_leaves_out_the_command_and_the_new_prompt() {
        let rows = lines(&["$ old", "old out", "$ echo hello", "hello", "world", "$ "]);
        assert_eq!(reply(&rows, Some(2), 5, 0), "hello\nworld");
    }

    #[test]
    fn a_prompt_of_several_lines_is_left_out() {
        let rows = lines(&[
            "┌─ [13:54:21]",
            "│~/demo",
            "└─ me❯ echo hello",
            "hello",
            "",
            "┌─ [13:54:36]",
            "│~/demo",
            "└─ me❯ ",
        ]);
        assert_eq!(reply(&rows, Some(2), 7, 0), "hello");
        // Output that only looks like the line above the old prompt stays
        // when it isn't directly above the new one.
        let rows = lines(&["$ x", "$ echo a", "a", "$ "]);
        assert_eq!(reply(&rows, Some(1), 3, 0), "a");
    }

    #[test]
    fn a_prompt_box_is_left_out_at_both_ends() {
        let rows = lines(&[
            "> what is 6 x 7",
            "",
            "● It's 42.",
            "",
            "╭──────────╮",
            "│ >        │",
            "╰──────────╯",
            "  ? for shortcuts",
        ]);
        // Enter was typed in a box whose top border was line 0; the cursor
        // is now in the new box's text line.
        assert_eq!(reply(&rows, Some(0), 5, 0), "● It's 42.");
        let rule = lines(&["───", "> hi", "───"]);
        assert_eq!(input_top(&rule, 1), 0);
    }

    #[test]
    fn wrapped_rows_join_back_into_one_line() {
        let rows = vec![
            ("$ cmd".to_string(), false),
            ("a long li".to_string(), true),
            ("ne of text".to_string(), false),
            ("$ ".to_string(), false),
        ];
        assert_eq!(reply(&rows, Some(0), 3, 0), "a long line of text");
    }

    #[test]
    fn without_an_anchor_the_screen_is_read() {
        let rows = lines(&["history", "on screen", "more", "$ "]);
        assert_eq!(reply(&rows, None, 3, 1), "on screen\nmore");
        // An anchor past the input (a cleared screen) is no anchor.
        assert_eq!(reply(&rows, Some(9), 3, 1), "on screen\nmore");
    }

    #[test]
    fn since_enter_reads_what_a_command_printed() {
        let (session, rx) = start("read line; printf 'first\\nsecond\\n'; sleep 2", 40, 6);
        std::thread::sleep(Duration::from_millis(200));
        session.input("go\r");
        let _ = screen(&rx, "second");
        assert_eq!(session.since_enter(), "first\nsecond");
    }

    #[test]
    fn palette_corners() {
        assert_eq!(palette(16), (0, 0, 0));
        assert_eq!(palette(231), (255, 255, 255));
        assert_eq!(palette(232), (8, 8, 8));
        assert_eq!(palette(255), (238, 238, 238));
    }
}

#[cfg(test)]
mod module_path_tests {
    use super::*;

    #[test]
    fn powershell_7s_folders_go_and_the_rest_stay_in_order() {
        let path = [
            r"C:\Users\me\Documents\PowerShell\Modules",
            r"C:\Program Files\PowerShell\Modules",
            r"c:\program files\windowsapps\microsoft.powershell_7.5.0.0_x64__8wekyb3d8bbwe\Modules",
            r"C:\Program Files\WindowsPowerShell\Modules",
            r"D:\mine\PowerShellTools",
            r"C:\WINDOWS\system32\WindowsPowerShell\v1.0\Modules",
        ]
        .join(";");
        assert_eq!(
            without_powershell_7(&path).unwrap(),
            [
                r"C:\Program Files\WindowsPowerShell\Modules",
                r"D:\mine\PowerShellTools",
                r"C:\WINDOWS\system32\WindowsPowerShell\v1.0\Modules",
            ]
            .join(";")
        );
    }

    #[test]
    fn nothing_to_change_means_the_variable_is_left_alone() {
        let own = r"C:\Program Files\WindowsPowerShell\Modules;C:\WINDOWS\system32\WindowsPowerShell\v1.0\Modules";
        assert_eq!(without_powershell_7(own), None);
        assert_eq!(without_powershell_7(""), None);
        // Only 7's entries: an empty path would be worse than the inherited one.
        assert_eq!(
            without_powershell_7(r"C:\Program Files\PowerShell\Modules"),
            None
        );
    }
}

/// Needs Windows PowerShell and, to prove anything, PowerShell 7 installed with
/// its module folders in the inherited `PSModulePath` (ticket 346); run it
/// with `cargo test -p rite-ui -- --ignored powershell_panel`.
#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::sync::mpsc;

    struct Channel(Mutex<mpsc::Sender<TermFrame>>);

    impl Sink for Channel {
        fn frame(&self, frame: TermFrame) {
            let _ = self.0.lock().unwrap().send(frame);
        }
        fn exited(&self, _: u32) {}
    }

    #[test]
    #[ignore = "needs PowerShell 7 beside Windows PowerShell"]
    fn powershell_panel_starts_with_psreadline() {
        let (tx, rx) = mpsc::channel();
        let session = Session::open(
            1,
            "powershell.exe".into(),
            vec!["-NoLogo".into()],
            None,
            120,
            20,
            Arc::new(Channel(Mutex::new(tx))),
        )
        .unwrap();
        let mut rows: Vec<String> = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(12);
        while std::time::Instant::now() < deadline {
            if let Ok(frame) = rx.recv_timeout(Duration::from_millis(200)) {
                for row in &frame.rows {
                    let y = row.y as usize;
                    if rows.len() <= y {
                        rows.resize(y + 1, String::new());
                    }
                    rows[y] = row.runs.iter().map(|r| r.text.as_str()).collect();
                }
            }
        }
        drop(session);
        let text = rows.join("\n");
        assert!(text.contains("PS "), "no prompt came up:\n{text}");
        assert!(!text.contains("Cannot load PSReadline"), "{text}");
    }
}
