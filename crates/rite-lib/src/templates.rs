//! Task templates, rendered with MiniJinja.
//!
//! Every task file is produced by a template, never by string-building in the
//! CLI: that is what makes the ticket format editable (and consistent) without
//! touching the tool. The built-ins are compiled into the binary, and any
//! `<board>/templates/<name>.md.jinja` overrides or adds to them.
//!
//! Undefined variables are a hard error (`UndefinedBehavior::Strict`) so a typo
//! in a template fails loudly instead of rendering a blank line, and
//! auto-escaping is off because the output is markdown, not HTML.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::error::Error as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use minijinja::{AutoEscape, Environment, UndefinedBehavior, Value, context};
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;

use crate::board::Board;

/// The one built-in ticket. A board can add its own templates, but there is no
/// longer a type of ticket: what varies is the fields you fill in.
pub const DEFAULT_TEMPLATE: &str = "ticket";

/// Suffix for template files in `<board>/templates/`.
pub const TEMPLATE_SUFFIX: &str = ".md.jinja";

/// The template that renders the board's own README on `rite init`.
const BOARD_README: &str = "board-readme";

const BUILTINS: &[(&str, &str)] = &[
    ("ticket", include_str!("../templates/ticket.md.jinja")),
    (
        BOARD_README,
        include_str!("../templates/board-readme.md.jinja"),
    ),
];

/// Where a template came from — shown by `rite templates`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Builtin,
    Board(PathBuf),
}

impl Source {
    pub fn describe(&self) -> String {
        match self {
            Source::Builtin => "built-in".to_string(),
            Source::Board(path) => path.display().to_string(),
        }
    }
}

/// The variables every task template is rendered with.
#[derive(Debug, Serialize)]
pub struct TaskContext {
    /// Zero-padded id, e.g. `007`.
    pub id: String,
    /// The same id as a number, for arithmetic and comparisons.
    pub number: u32,
    pub title: String,
    pub slug: String,
    /// Branch type (`feature`, `bugfix`, …), or empty for a ticket with no
    /// branch.
    pub branch_type: String,
    /// The branch this ticket is worked on, made from the branch type and the
    /// title. Empty when there's no branch type.
    pub branch_name: String,
    /// Who the ticket is assigned to (the first person, when several), or
    /// an empty string — tickets can be created now and assigned later.
    pub assignee: String,
    /// Everyone it's assigned to, in order (ticket 254); empty for nobody.
    /// A built-in ticket writes the `assignees` list itself when there are
    /// two or more.
    pub assignees: Vec<String>,
    /// The repository the ticket is about (a bare name), or empty.
    pub repository: String,
    /// Id of this ticket's parent, or an empty string. One field instead of
    /// epic/story: nesting gives you those, and any depth beyond them.
    pub parent: String,
    /// The parent's `NNN-slug`, for a wikilink an editor can follow.
    pub parent_link: String,
    /// Ids of the tickets this one depends on or relates to. Empty by default.
    pub related: Vec<String>,
    /// Id of the ticket this one is a copy of, or an empty string.
    pub clone_of: String,
    pub tags: Vec<String>,
    /// Local date, `YYYY-MM-DD`.
    pub created: String,
    /// Local timestamp, RFC 3339.
    pub timestamp: String,
    /// Column the task is created in.
    pub column: String,
    /// `git config user.name`, or `unknown`.
    pub author: String,
    /// `git config user.email`, or an empty string.
    pub email: String,
    /// Who's creating it, `Name <email>`: their name from the project's
    /// users list, else git's (ticket 155). Empty without a git email.
    pub created_by: String,
    /// Board directory name (`tasks`).
    pub board: String,
    /// Name of the template being rendered.
    pub template: String,
    /// Anything passed with `--set key=value`.
    pub extra: BTreeMap<String, String>,
}

pub struct Templates {
    env: Environment<'static>,
    sources: BTreeMap<String, Source>,
}

impl Templates {
    /// The compiled-in templates only.
    pub fn builtin() -> Result<Self> {
        let mut env = Environment::new();
        env.set_undefined_behavior(UndefinedBehavior::Strict);
        // Markdown output: HTML escaping would mangle it.
        env.set_auto_escape_callback(|_| AutoEscape::None);
        // Whitespace is part of the syntax since MiniJinja 3.
        let syntax = env
            .syntax()
            .to_builder()
            .keep_trailing_newline(true)
            .trim_blocks(true)
            .lstrip_blocks(true)
            .build()
            .map_err(|err| anyhow!("template syntax: {err}"))?;
        env.set_syntax(syntax);
        // Frontmatter safety: quote and escape a value so a title containing a
        // quote can't corrupt the block.
        env.add_filter("yaml", yaml_filter);

        let mut sources = BTreeMap::new();
        for (name, _) in BUILTINS {
            let source = builtin_source(name).expect("listed in BUILTINS");
            env.add_template_owned(*name, source)
                .with_context(|| format!("built-in template '{name}'"))?;
            sources.insert((*name).to_string(), Source::Builtin);
        }
        Ok(Self { env, sources })
    }

    /// Built-ins plus everything in `<board>/templates/`, which wins on a
    /// name clash.
    pub fn load(board: &Board) -> Result<Self> {
        let mut templates = Templates::builtin()?;
        let dir = board.templates_dir();
        if !dir.is_dir() {
            return Ok(templates);
        }
        let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
            .with_context(|| format!("reading {}", dir.display()))?
            .map(|entry| entry.map(|e| e.path()))
            .collect::<std::io::Result<_>>()
            .with_context(|| format!("reading {}", dir.display()))?;
        entries.sort();
        for path in entries {
            let Some(name) = template_name(&path) else {
                continue;
            };
            let source =
                crate::eol::read(&path).with_context(|| format!("reading {}", path.display()))?;
            templates
                .env
                .add_template_owned(name.clone(), source)
                .map_err(|err| render_error(&path.display().to_string(), err))?;
            templates.sources.insert(name, Source::Board(path));
        }
        Ok(templates)
    }

    /// Template names and where each came from.
    pub fn list(&self) -> impl Iterator<Item = (&str, &Source)> {
        self.sources
            .iter()
            .map(|(name, source)| (name.as_str(), source))
    }

    /// Templates you can create a task from — everything except the internal
    /// board README. These are the choices `rite new` offers.
    pub fn task_templates(&self) -> Vec<String> {
        self.sources
            .keys()
            .filter(|name| name.as_str() != BOARD_README)
            .cloned()
            .collect()
    }

    /// The `extra.*` fields a template needs, in order of first appearance —
    /// the form `rite new` has to ask for.
    pub fn required_extras(&self, name: &str) -> Result<Vec<String>> {
        Ok(required_extras(&self.source_text(name)?))
    }

    /// The template's text, from the board file or the binary.
    pub fn source_text(&self, name: &str) -> Result<String> {
        match self.sources.get(name) {
            Some(Source::Board(path)) => {
                crate::eol::read(path).with_context(|| format!("reading {}", path.display()))
            }
            Some(Source::Builtin) => Ok(builtin_source(name)
                .expect("built-in registered without source")
                .into_owned()),
            None => bail!(unknown_template(name)),
        }
    }

    /// These templates with `source` standing in for `name` — how an editor
    /// previews a change to a template before saving it. A source that
    /// doesn't parse is refused here, with the parser's reason.
    pub fn with_source(mut self, name: &str, source: &str) -> Result<Self> {
        self.env
            .add_template_owned(name.to_string(), source.to_string())
            .map_err(|err| render_error(name, err))?;
        Ok(self)
    }

    pub fn render<S: Serialize>(&self, name: &str, ctx: S) -> Result<String> {
        let template = self
            .env
            .get_template(name)
            .map_err(|_| anyhow!(unknown_template(name)))?;
        template
            .render(Value::from(minijinja::value::Serde(&ctx)))
            .map_err(|err| render_error(name, err))
    }
}

/// Render the board's README (used by `rite init`).
pub fn render_board_readme(board: &Board) -> Result<String> {
    let templates = Templates::load(board)?;
    let columns: Vec<&str> = board.workflow().columns().map(|c| c.as_str()).collect();
    templates.render(
        BOARD_README,
        context! {
            board => board.name(),
            columns => columns,
            types => crate::branch::branch_types(),
            created => today().0,
        },
    )
}

/// Every `extra.<key>` and `extra["<key>"]` a template source references, in
/// order of first appearance and without duplicates.
///
/// This is what makes a template self-describing: adding a field to a template
/// adds a question to the form, with no second place to register it. Undefined
/// variables are a hard error, so a field that isn't collected would otherwise
/// fail the render.
pub fn required_extras(source: &str) -> Vec<String> {
    const MARKER: &str = "extra";
    let bytes = source.as_bytes();
    let mut keys: Vec<String> = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = source[cursor..].find(MARKER) {
        let start = cursor + offset;
        cursor = start + MARKER.len();
        // Skip `my_extra`, `extras` and friends — only the bare name counts.
        if start > 0 {
            let before = bytes[start - 1];
            if before == b'_' || before.is_ascii_alphanumeric() {
                continue;
            }
        }
        let rest = &source[cursor..];
        let key = if let Some(rest) = rest.strip_prefix('.') {
            rest.chars()
                .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
                .collect()
        } else if let Some(rest) = rest.strip_prefix('[') {
            let rest = rest.trim_start();
            match rest.chars().next() {
                Some(quote @ ('"' | '\'')) => rest[1..]
                    .split(quote)
                    .next()
                    .unwrap_or_default()
                    .to_string(),
                _ => String::new(),
            }
        } else {
            String::new()
        };
        if !key.is_empty() && !keys.iter().any(|known| known == &key) {
            keys.push(key);
        }
    }
    keys
}

/// Does a template use `name` as a variable of its own? Word-boundary aware,
/// so `branches` and `my_branch` don't count as `branch`.
pub fn references(source: &str, name: &str) -> bool {
    let bytes = source.as_bytes();
    let mut cursor = 0;
    while let Some(offset) = source[cursor..].find(name) {
        let start = cursor + offset;
        cursor = start + name.len();
        let before_ok = start == 0 || {
            let before = bytes[start - 1];
            !(before == b'_' || before.is_ascii_alphanumeric() || before == b'.')
        };
        let after_ok = match bytes.get(cursor) {
            None => true,
            Some(after) => !(*after == b'_' || after.is_ascii_alphanumeric()),
        };
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

/// `steps_to_reproduce` -> `Steps to reproduce`, for prompt labels.
pub fn field_label(key: &str) -> String {
    let words = key.replace(['_', '-'], " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => words,
    }
}

/// The source of a built-in template, if `name` is one. With LF endings
/// even when this source tree was checked out with CRLF (`eol`), so a
/// Windows build renders the same tickets as any other.
pub fn builtin_source(name: &str) -> Option<Cow<'static, str>> {
    let (_, source) = BUILTINS.iter().find(|(builtin, _)| *builtin == name)?;
    Some(if source.contains("\r\n") {
        Cow::Owned(crate::eol::lf((*source).to_string()))
    } else {
        Cow::Borrowed(*source)
    })
}

/// `<name>.md.jinja` (also `.jinja` / `.j2`) -> `name`; anything else is
/// ignored, so a README or a stray `.swp` in the templates dir is harmless.
pub fn template_name(path: &Path) -> Option<String> {
    let file = path.file_name()?.to_str()?;
    for suffix in [TEMPLATE_SUFFIX, ".jinja", ".j2"] {
        if let Some(stem) = file.strip_suffix(suffix) {
            let stem = stem.strip_suffix(".md").unwrap_or(stem);
            if stem.is_empty() {
                return None;
            }
            return Some(stem.to_string());
        }
    }
    None
}

/// A made-up ticket to render a template against, for a preview: every
/// field filled in, so a template that uses any of them shows something.
pub fn example_context(board: &Board) -> TaskContext {
    let (created, timestamp) = today();
    TaskContext {
        id: "042".to_string(),
        number: 42,
        title: "An example ticket".to_string(),
        slug: "an-example-ticket".to_string(),
        branch_type: "feature".to_string(),
        branch_name: "feature/an-example-ticket".to_string(),
        assignee: String::new(),
        assignees: Vec::new(),
        repository: String::new(),
        parent: String::new(),
        parent_link: String::new(),
        related: Vec::new(),
        clone_of: String::new(),
        tags: Vec::new(),
        created,
        timestamp,
        column: "backlog".to_string(),
        author: "you".to_string(),
        email: String::new(),
        created_by: "You <you@example.com>".to_string(),
        board: board.name(),
        template: DEFAULT_TEMPLATE.to_string(),
        extra: BTreeMap::new(),
    }
}

/// Local date (`YYYY-MM-DD`) and RFC 3339 timestamp, falling back to UTC when
/// the platform won't give us an offset.
pub fn today() -> (String, String) {
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    let date = now
        .format(format_description!("[year]-[month]-[day]"))
        .unwrap_or_default();
    let timestamp = now.format(&Rfc3339).unwrap_or_default();
    (date, timestamp)
}

pub(crate) fn yaml_filter(value: Value) -> String {
    let raw = value.to_string();
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    for ch in raw.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn unknown_template(name: &str) -> String {
    format!("no template '{name}' (try: rite templates)")
}

/// MiniJinja errors carry the template, line and a cause chain — flatten all of
/// it into the message, since only the top line survives otherwise.
fn render_error(name: &str, err: minijinja::Error) -> anyhow::Error {
    let mut message = format!("template '{name}': {err}");
    if let Some(line) = err.line() {
        message.push_str(&format!(" (line {line})"));
    }
    let mut cause = err.source();
    while let Some(err) = cause {
        message.push_str(&format!("\n  caused by: {err}"));
        cause = err.source();
    }
    let debug_info = err.display_debug_info().to_string();
    if !debug_info.trim().is_empty() {
        message.push('\n');
        message.push_str(debug_info.trim_end());
    }
    anyhow!(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> TaskContext {
        TaskContext {
            id: "001".to_string(),
            number: 1,
            title: "First task".to_string(),
            slug: "first-task".to_string(),
            branch_type: "bugfix".to_string(),
            branch_name: "bugfix/first-task".to_string(),
            assignee: "tim".to_string(),
            assignees: vec!["tim".to_string()],
            repository: String::new(),
            parent: "002".to_string(),
            parent_link: "002-checkout".to_string(),
            related: vec!["003".to_string()],
            clone_of: String::new(),
            tags: vec!["x".to_string(), "y".to_string()],
            created: "2026-08-03".to_string(),
            timestamp: "2026-08-03T09:00:00+01:00".to_string(),
            column: "todo".to_string(),
            author: "t".to_string(),
            email: "t@t.co".to_string(),
            created_by: "t <t@t.co>".to_string(),
            board: "tasks".to_string(),
            template: "ticket".to_string(),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn the_ticket_template_renders_the_documented_frontmatter() {
        let rendered = Templates::builtin()
            .unwrap()
            .render("ticket", ctx())
            .unwrap();
        assert!(rendered.starts_with("---\n"), "{rendered}");
        assert!(rendered.contains("\nid: \"001\"\n"), "{rendered}");
        assert!(rendered.contains("\ntitle: \"First task\"\n"), "{rendered}");
        assert!(
            rendered.contains("\nbranch-type: \"bugfix\"\n"),
            "{rendered}"
        );
        assert!(
            rendered.contains("\nbranch-name: \"bugfix/first-task\"\n"),
            "{rendered}"
        );
        assert!(rendered.contains("\nassignee: \"tim\"\n"), "{rendered}");
        assert!(rendered.contains("\nparent: \"002\"\n"), "{rendered}");
        assert!(rendered.contains("[[002-checkout]]"), "{rendered}");
        assert!(rendered.contains("\ntags: [x,y]\n"), "{rendered}");
        assert!(rendered.contains("\nrelated: [003]\n"), "{rendered}");
        assert!(!rendered.contains("clone-of"), "{rendered}");
        assert!(rendered.contains("## Acceptance criteria"), "{rendered}");
        assert!(rendered.ends_with('\n'), "{rendered}");
    }

    #[test]
    fn a_clone_records_what_it_is_a_copy_of() {
        let mut context = ctx();
        context.clone_of = "007".to_string();
        let rendered = Templates::builtin()
            .unwrap()
            .render("ticket", context)
            .unwrap();
        assert!(
            rendered.contains("\nrelated: [003]\nclone-of: \"007\"\ntags"),
            "{rendered}"
        );
    }

    #[test]
    fn every_builtin_renders_with_the_standard_context() {
        let templates = Templates::builtin().unwrap();
        for (name, _) in BUILTINS {
            if *name == BOARD_README {
                continue;
            }
            templates
                .render(name, ctx())
                .unwrap_or_else(|err| panic!("{name}: {err:#}"));
        }
    }

    #[test]
    fn a_title_with_quotes_cannot_break_the_frontmatter() {
        let mut c = ctx();
        c.title = r#"Fix the "quoted" \ thing"#.to_string();
        let rendered = Templates::builtin().unwrap().render("ticket", c).unwrap();
        assert!(
            rendered.contains(r#"title: "Fix the \"quoted\" \\ thing""#),
            "{rendered}"
        );
    }

    #[test]
    fn undefined_variables_are_an_error_not_a_blank() {
        let mut templates = Templates::builtin().unwrap();
        templates
            .env
            .add_template_owned("typo".to_string(), "{{ titel }}".to_string())
            .unwrap();
        let err = templates.render("typo", ctx()).unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("undefined value"), "{message}");
        assert!(message.contains("line 1"), "{message}");
    }

    #[test]
    fn extra_variables_are_available_to_templates() {
        let mut templates = Templates::builtin().unwrap();
        templates
            .env
            .add_template_owned("extra".to_string(), "owner: {{ extra.owner }}".to_string())
            .unwrap();
        let mut c = ctx();
        c.extra.insert("owner".to_string(), "tim".to_string());
        assert_eq!(templates.render("extra", c).unwrap(), "owner: tim");
    }

    #[test]
    fn unknown_templates_point_at_the_templates_command() {
        let err = Templates::builtin()
            .unwrap()
            .render("nope", ctx())
            .unwrap_err();
        assert!(format!("{err:#}").contains("rite templates"), "{err:#}");
    }

    #[test]
    fn required_extras_are_found_in_order_without_duplicates() {
        let source = "{{ extra.owner }} {{ extra.steps_to_reproduce }} {{ extra.owner }}";
        assert_eq!(required_extras(source), ["owner", "steps_to_reproduce"]);
    }

    #[test]
    fn required_extras_handles_subscripts_and_ignores_lookalikes() {
        assert_eq!(required_extras(r#"{{ extra["due date"] }}"#), ["due date"]);
        assert_eq!(required_extras("{{ extra['due'] }}"), ["due"]);
        assert_eq!(
            required_extras("{{ extras.owner }} {{ my_extra.x }}"),
            [] as [String; 0]
        );
        assert_eq!(required_extras("{{ extra }}"), [] as [String; 0]);
    }

    #[test]
    fn built_in_templates_ask_for_nothing_extra() {
        for (name, source) in BUILTINS {
            assert!(
                required_extras(source).is_empty(),
                "{name} would prompt for {:?}",
                required_extras(source)
            );
        }
    }

    #[test]
    fn field_labels_read_like_questions() {
        assert_eq!(field_label("steps_to_reproduce"), "Steps to reproduce");
        assert_eq!(field_label("owner"), "Owner");
        assert_eq!(field_label(""), "");
    }

    #[test]
    fn task_templates_exclude_the_board_readme() {
        let templates = Templates::builtin().unwrap();
        let names = templates.task_templates();
        assert!(names.contains(&"ticket".to_string()));
        assert!(!names.contains(&BOARD_README.to_string()));
    }

    #[test]
    fn references_is_word_boundary_aware() {
        assert!(references("branch: {{ branch | yaml }}", "branch"));
        assert!(!references("{{ branches }} {{ my_branch }}", "branch"));
        assert!(!references("{{ task.branch }}", "branch"));
    }

    #[test]
    fn template_names_come_from_the_filename() {
        assert_eq!(
            template_name(Path::new("/b/templates/story.md.jinja")).as_deref(),
            Some("story")
        );
        assert_eq!(
            template_name(Path::new("/b/templates/story.j2")).as_deref(),
            Some("story")
        );
        assert_eq!(template_name(Path::new("/b/templates/README.md")), None);
    }
}
