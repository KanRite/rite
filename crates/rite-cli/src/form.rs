//! Interactive prompts, built on `inquire`.
//!
//! Everything here is optional: `rite` stays fully scriptable, so each prompt
//! has a flag or argument that skips it, and nothing prompts unless both stdin
//! and stdout are a terminal. `--no-input` (or `RITE_NO_INPUT=1`) turns the
//! prompts off entirely, so CI fails with a clear message instead of blocking.

use std::ffi::OsStr;
use std::fmt;
use std::io::{self, IsTerminal, Write};
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use inquire::autocompletion::{Autocomplete, Replacement};
use inquire::validator::Validation;
use inquire::{Confirm, CustomUserError, InquireError, Select, Text};

use rite_lib::branch;
use rite_lib::task::Task;

static NO_INPUT: OnceLock<bool> = OnceLock::new();

/// Record the `--no-input` flag. Called once, from `main`.
pub fn set_no_input(no_input: bool) {
    let _ = NO_INPUT.set(no_input);
}

/// Can we prompt? Requires a terminal on both ends and no opt-out.
pub fn available() -> bool {
    if *NO_INPUT.get().unwrap_or(&false) {
        return false;
    }
    if rite_lib::renamed::var("RITE_NO_INPUT").as_deref() == Ok("1") {
        return false;
    }
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// A free-text answer. `default` pre-fills the line for editing.
pub fn text(message: &str, default: Option<&str>, help: Option<&str>) -> Result<String> {
    let mut prompt = Text::new(message);
    if let Some(default) = default {
        prompt = prompt.with_initial_value(default);
    }
    if let Some(help) = help {
        prompt = prompt.with_help_message(help);
    }
    convert(prompt.prompt())
}

/// The task title, asked first. It names the file — and, for a development
/// ticket, the branch — so it's validated as you type: a title with nothing
/// sluggable in it is rejected here rather than becoming a ticket that can
/// never start.
pub fn title(default: Option<&str>) -> Result<String> {
    let mut prompt = Text::new("Title:")
        .with_help_message("names the task file, and its branch")
        .with_validator(|input: &str| {
            Ok(match branch::validate_sluggable(input) {
                Ok(()) => Validation::Valid,
                Err(err) => Validation::Invalid(err.to_string().into()),
            })
        });
    if let Some(default) = default {
        prompt = prompt.with_initial_value(default);
    }
    Ok(convert(prompt.prompt())?.trim().to_string())
}

/// The last step of the form: the ticket itself, opened in `$EDITOR` with the
/// rendered template already in it, so the notes and checklist get filled in
/// while the task is being created rather than in a second pass.
///
/// Modelled on `git commit`: the editor opens straight away (inquire's own
/// editor prompt waits for a keypress first), and nothing is written to the
/// board until it exits cleanly — so abandoning the edit leaves no half-made
/// ticket behind.
pub fn body(rendered: &str, editor: &OsStr) -> Result<String> {
    let mut file = tempfile::Builder::new()
        .prefix("amd-")
        .suffix(".md")
        .tempfile()
        .context("creating a temporary file for the ticket")?;
    file.write_all(rendered.as_bytes())
        .context("writing the ticket to edit")?;
    file.flush().context("writing the ticket to edit")?;

    // $EDITOR can carry arguments ("code --wait", "emacsclient -nw").
    let spec = editor.to_string_lossy();
    let mut parts = spec.split_whitespace();
    let program = parts.next().unwrap_or("vi");
    let status = Command::new(program)
        .args(parts)
        .arg(file.path())
        .status()
        .with_context(|| format!("running {spec}"))?;
    if !status.success() {
        bail!("{spec} exited with {status}; no task created");
    }

    let edited = rite_lib::eol::read(file.path()).context("reading the edited ticket")?;
    if edited.trim().is_empty() {
        bail!("the ticket was left empty; no task created");
    }
    Ok(edited)
}

/// A single label, completing against the ones already on the board. Empty
/// means "no label".
pub fn label(message: &str, default: &str, known: Vec<String>, help: &str) -> Result<String> {
    let answer = convert(
        Text::new(message)
            .with_initial_value(default)
            .with_help_message(help)
            .with_autocomplete(Completer::single(known))
            .prompt(),
    )?;
    Ok(answer.trim().to_string())
}

/// Who a ticket is assigned to (ticket 089): a type-to-filter list of the
/// project's users, with nobody, yourself (`@me`, git's user name) and
/// "someone else" — a name typed in, for someone who hasn't joined yet.
/// Starts on `current`. Returns the name, or "" for nobody.
pub fn assignee(users: Vec<String>, me: Option<String>, current: &str) -> Result<String> {
    const NOBODY: &str = "(nobody)";
    const OTHER: &str = "(someone else…)";
    let me_label = me.as_ref().map(|me| format!("@me ({me})"));
    let mut options = vec![NOBODY.to_string()];
    options.extend(me_label.clone());
    options.extend(users.iter().cloned());
    options.push(OTHER.to_string());
    let start = match current {
        "" => Some(0),
        current => options
            .iter()
            .position(|o| o == current)
            .or_else(|| (me.as_deref() == Some(current)).then_some(1)),
    };
    let mut prompt =
        Select::new("Assignee:", options).with_help_message("type to filter · enter to choose");
    if let Some(start) = start {
        prompt = prompt.with_starting_cursor(start);
    }
    let chosen = convert(prompt.prompt())?;
    Ok(match chosen.as_str() {
        NOBODY => String::new(),
        OTHER => text("Assignee's name:", Some(current), None)?
            .trim()
            .to_string(),
        _ if Some(&chosen) == me_label.as_ref() => me.unwrap_or_default(),
        _ => chosen,
    })
}

/// Pick one of `options`, starting on `default` when it's among them.
pub fn select<T: fmt::Display + Eq>(message: &str, options: Vec<T>, default: &T) -> Result<T> {
    let start = options.iter().position(|option| option == default);
    let mut prompt = Select::new(message, options);
    if let Some(start) = start {
        prompt = prompt.with_starting_cursor(start);
    }
    convert(prompt.prompt())
}

/// A comma-separated tag list, completing against the tags already on the
/// board so a board doesn't accumulate `docs`, `doc` and `documentation`.
pub fn tags(default: &str, known: Vec<String>) -> Result<Vec<String>> {
    let help = if known.is_empty() {
        "comma separated".to_string()
    } else {
        format!("comma separated; tab completes: {}", known.join(", "))
    };

    let answer = convert(
        Text::new("Tags:")
            .with_initial_value(default)
            .with_help_message(&help)
            .with_autocomplete(Completer::multi(known))
            .prompt(),
    )?;
    Ok(answer
        .split(',')
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect())
}

/// The tickets this one relates to, completed against what's on the board.
/// Answers are refs (an id or a slug fragment); the caller resolves them, so a
/// typo is caught here rather than recorded as a dangling link.
pub fn related(default: &str, known: Vec<String>) -> Result<Vec<String>> {
    let help = if known.is_empty() {
        "comma separated ids; blank for none".to_string()
    } else {
        "comma separated ids or slugs, tab completes; blank for none".to_string()
    };
    let answer = convert(
        Text::new("Related:")
            .with_initial_value(default)
            .with_help_message(&help)
            .with_autocomplete(Completer::multi(known))
            .prompt(),
    )?;
    Ok(answer
        .split(',')
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect())
}

/// Completes a label against the ones already on the board. In `multi` mode it
/// completes the value after the last comma, leaving earlier ones alone.
#[derive(Clone)]
struct Completer {
    known: Vec<String>,
    multi: bool,
}

impl Completer {
    fn single(known: Vec<String>) -> Self {
        Self {
            known,
            multi: false,
        }
    }

    fn multi(known: Vec<String>) -> Self {
        Self { known, multi: true }
    }

    fn split(&self, input: &str) -> (String, String) {
        if self.multi {
            split_last_value(input)
        } else {
            (String::new(), input.to_string())
        }
    }

    fn matches(&self, partial: &str) -> Vec<&String> {
        let partial = partial.to_lowercase();
        self.known
            .iter()
            .filter(|value| value.to_lowercase().starts_with(&partial))
            .collect()
    }
}

impl Autocomplete for Completer {
    fn get_suggestions(&mut self, input: &str) -> Result<Vec<String>, CustomUserError> {
        let (prefix, partial) = self.split(input);
        Ok(self
            .matches(&partial)
            .into_iter()
            .map(|value| format!("{prefix}{value}"))
            .collect())
    }

    fn get_completion(
        &mut self,
        input: &str,
        highlighted: Option<String>,
    ) -> Result<Replacement, CustomUserError> {
        if let Some(suggestion) = highlighted {
            return Ok(Replacement::Some(suggestion));
        }
        let (prefix, partial) = self.split(input);
        Ok(self
            .matches(&partial)
            .first()
            .map(|value| format!("{prefix}{value}")))
    }
}

/// `"docs, rel"` -> `("docs, ", "rel")`.
fn split_last_value(input: &str) -> (String, String) {
    match input.rfind(',') {
        Some(comma) => (
            format!("{} ", &input[..=comma]),
            input[comma + 1..].trim_start().to_string(),
        ),
        None => (String::new(), input.to_string()),
    }
}

/// A yes/no question, defaulting to no.
pub fn confirm(message: &str) -> Result<bool> {
    convert(Confirm::new(message).with_default(false).prompt())
}

/// Pick a task from a list, shown the way the board shows them.
pub fn select_task(message: &str, tasks: Vec<Task>) -> Result<Task> {
    if tasks.is_empty() {
        bail!("no tasks to choose from");
    }
    let choices: Vec<Choice> = tasks.into_iter().map(Choice).collect();
    Ok(convert(Select::new(message, choices).prompt())?.0)
}

/// A task rendered as one line of the picker.
struct Choice(Task);

impl fmt::Display for Choice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] {} ({}/)",
            self.0.id_display(),
            self.0.title(),
            self.0.column
        )
    }
}

/// Map inquire's cancellations onto a plain "cancelled" error, so Esc and
/// Ctrl-C read as a decision rather than a crash.
fn convert<T>(result: Result<T, InquireError>) -> Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
            bail!("cancelled")
        }
        Err(InquireError::NotTTY) => bail!("not a terminal — pass the values as arguments"),
        Err(err) => bail!(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::ffi::OsString;

    fn autocomplete() -> Completer {
        Completer::multi(vec!["docs".into(), "release".into(), "regression".into()])
    }

    #[cfg(unix)]
    #[test]
    fn body_returns_what_the_editor_left_behind() {
        // `true` accepts the file and changes nothing.
        let rendered = "---\nid: \"001\"\n---\n\n## Notes\n";
        let edited = body(rendered, &OsString::from("true")).unwrap();
        assert_eq!(edited, rendered);
    }

    #[cfg(unix)]
    #[test]
    fn body_refuses_to_create_a_task_when_the_editor_fails() {
        let err = body("x", &OsString::from("false")).unwrap_err().to_string();
        assert!(err.contains("no task created"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn body_refuses_an_empty_ticket() {
        let err = body("   \n", &OsString::from("true"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("left empty"), "{err}");
    }

    #[test]
    fn split_last_value_keeps_the_tags_already_typed() {
        assert_eq!(
            split_last_value("docs, rel"),
            ("docs, ".to_string(), "rel".to_string())
        );
        assert_eq!(split_last_value("rel"), (String::new(), "rel".to_string()));
        assert_eq!(split_last_value(""), (String::new(), String::new()));
    }

    #[test]
    fn suggestions_complete_only_the_last_tag() {
        let mut completer = autocomplete();
        assert_eq!(
            completer.get_suggestions("docs, re").unwrap(),
            ["docs, release", "docs, regression"]
        );
    }

    #[test]
    fn an_empty_partial_offers_every_known_tag() {
        let mut completer = autocomplete();
        assert_eq!(completer.get_suggestions("").unwrap().len(), 3);
    }

    #[test]
    fn completion_falls_back_to_the_first_match() {
        let mut completer = autocomplete();
        assert_eq!(
            completer.get_completion("reg", None).unwrap(),
            Replacement::Some("regression".to_string())
        );
        assert_eq!(
            completer.get_completion("nope", None).unwrap(),
            Replacement::None
        );
    }
}
