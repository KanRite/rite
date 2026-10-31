//! Types and rules shared by the Tauri backend and the Leptos frontend.
//!
//! Everything here is plain data plus pure functions, so it compiles both
//! natively and to `wasm32-unknown-unknown`.

use serde::{Deserialize, Serialize};

pub mod appearance;
pub mod epic;
pub mod rich;
pub mod screen;
pub mod terminal;
pub mod ticket;
pub mod tree;
pub mod waiting;
pub mod wip;
pub mod zoom;

/// What a column is for (ticket 223): the behaviour its rules key on, as
/// rite-lib's `board::Kind`.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Backlog,
    Todo,
    Doing,
    Done,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Backlog, Kind::Todo, Kind::Doing, Kind::Done];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Backlog => "backlog",
            Kind::Todo => "todo",
            Kind::Doing => "doing",
            Kind::Done => "done",
        }
    }

    pub fn parse(name: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == name)
    }
}

/// A board column (tickets 223, 224): its folder under `tasks/` and its
/// kind, as rite-lib's `board::Column`. `Copy`: folder names are interned.
/// The four standard columns keep their old names. On the wire a standard
/// column is its folder (`"backlog"`, as it always was) and another is
/// `"folder:kind"` (`"review:doing"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Column {
    folder: &'static str,
    kind: Kind,
}

#[allow(non_upper_case_globals)]
impl Column {
    pub const Backlog: Column = Column {
        folder: "backlog",
        kind: Kind::Backlog,
    };
    pub const Todo: Column = Column {
        folder: "todo",
        kind: Kind::Todo,
    };
    pub const Doing: Column = Column {
        folder: "doing",
        kind: Kind::Doing,
    };
    pub const Done: Column = Column {
        folder: "done",
        kind: Kind::Done,
    };
    /// The four every board has, in their standard order.
    pub const STANDARD: [Column; 4] = [Column::Backlog, Column::Todo, Column::Doing, Column::Done];

    pub fn new(folder: &str, kind: Kind) -> Column {
        Column {
            folder: intern(folder),
            kind,
        }
    }

    /// The directory name under `tasks/`.
    pub fn dir_name(self) -> &'static str {
        self.folder
    }

    pub fn kind(self) -> Kind {
        self.kind
    }

    pub fn is(self, kind: Kind) -> bool {
        self.kind == kind
    }

    /// The standard column whose folder is `name`.
    pub fn from_dir_name(name: &str) -> Option<Column> {
        Column::STANDARD.into_iter().find(|c| c.folder == name)
    }

    fn is_standard(self) -> bool {
        Column::STANDARD.contains(&self)
    }
}

impl Default for Column {
    fn default() -> Column {
        Column::Backlog
    }
}

impl Serialize for Column {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if self.is_standard() {
            s.serialize_str(self.folder)
        } else {
            s.serialize_str(&format!("{}:{}", self.folder, self.kind.as_str()))
        }
    }
}

impl<'de> Deserialize<'de> for Column {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Column, D::Error> {
        let text = String::deserialize(d)?;
        match text.split_once(':') {
            Some((folder, kind)) => {
                let kind = Kind::parse(kind).ok_or_else(|| {
                    serde::de::Error::custom(format!("unknown column kind {kind:?}"))
                })?;
                Ok(Column::new(folder, kind))
            }
            None => Column::from_dir_name(&text)
                .ok_or_else(|| serde::de::Error::custom(format!("unknown column {text:?}"))),
        }
    }
}

/// `name` as a `&'static str`, leaked once per distinct name.
fn intern(name: &str) -> &'static str {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static NAMES: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    if let Some(standard) = Column::from_dir_name(name) {
        return standard.folder;
    }
    let mut names = NAMES
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(&known) = names.get(name) {
        return known;
    }
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    names.insert(leaked);
    leaked
}

/// One column as the board shows it: what it's called, and a doing-kind
/// column's own WIP limit (the standard doing's is the repository's
/// `wip`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub column: Column,
    pub label: String,
    #[serde(default)]
    pub wip: Option<u32>,
}

/// The board's columns, in order (tickets 223, 224): what the page draws
/// and every drag rule's sense of "next to".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workflow {
    pub columns: Vec<ColumnInfo>,
    /// Any column to any other (ticket 225); else the next column, plus
    /// `moves`.
    #[serde(default)]
    pub any: bool,
    /// Extra one-way moves the team allows.
    #[serde(default)]
    pub moves: Vec<(Column, Column)>,
    /// Where new tickets land.
    #[serde(default)]
    pub landing: Column,
}

impl Default for Workflow {
    fn default() -> Workflow {
        Workflow::standard()
    }
}

impl Workflow {
    pub fn standard() -> Workflow {
        Workflow {
            columns: Column::STANDARD
                .into_iter()
                .map(|column| ColumnInfo {
                    column,
                    label: standard_label(column).to_string(),
                    wip: None,
                })
                .collect(),
            any: false,
            moves: Vec::new(),
            landing: Column::Backlog,
        }
    }

    /// Whether a ticket may move straight from `from` to `to` (ticket 225),
    /// as rite-lib's `Workflow::allows`.
    pub fn allows(&self, from: Column, to: Column) -> bool {
        if from == to || self.position(from).is_none() || self.position(to).is_none() {
            return false;
        }
        self.any || self.next_to(from, to) || self.moves.contains(&(from, to))
    }

    pub fn columns(&self) -> impl Iterator<Item = Column> + '_ {
        self.columns.iter().map(|c| c.column)
    }

    /// What `column` is called; its folder when it isn't one of these.
    pub fn label(&self, column: Column) -> String {
        self.columns
            .iter()
            .find(|c| c.column == column)
            .map(|c| c.label.clone())
            .unwrap_or_else(|| standard_label(column).to_string())
    }

    pub fn position(&self, column: Column) -> Option<usize> {
        self.columns.iter().position(|c| c.column == column)
    }

    /// One column at a time, forwards or back.
    pub fn next_to(&self, from: Column, to: Column) -> bool {
        match (self.position(from), self.position(to)) {
            (Some(a), Some(b)) => a.abs_diff(b) == 1,
            _ => false,
        }
    }

    /// The first column of `kind`.
    pub fn first(&self, kind: Kind) -> Column {
        self.columns()
            .find(|c| c.is(kind))
            .unwrap_or(Column::STANDARD[kind as usize])
    }

    /// Only the backlog and done kinds fold out of the way.
    pub fn is_collapsible(&self, column: Column) -> bool {
        column.is(Kind::Backlog) || column.is(Kind::Done)
    }

    /// A doing-kind column's own limit (not the standard doing's).
    pub fn wip(&self, column: Column) -> Option<u32> {
        self.columns
            .iter()
            .find(|c| c.column == column)
            .and_then(|c| c.wip)
    }
}

/// The standard columns' names, a folder's own otherwise.
fn standard_label(column: Column) -> &'static str {
    match column.dir_name() {
        "backlog" => "Backlog",
        "todo" => "To do",
        "doing" => "Doing",
        "done" => "Done",
        other => other,
    }
}

/// What a card on the board shows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TicketSummary {
    /// The repository whose board it is on.
    #[serde(default)]
    pub repo: String,
    /// The repository it's about (tickets 101/107), a bare name; empty if
    /// it doesn't say. What the repository filter matches.
    #[serde(default)]
    pub repository: String,
    /// File name within its column directory, e.g. `001-some-title.md`.
    pub file: String,
    pub column: Column,
    pub id: String,
    pub title: String,
    /// The main assignee: the first of `assignees`.
    pub assignee: String,
    /// Everyone it's assigned to, in order (ticket 254); empty for nobody.
    #[serde(default)]
    pub assignees: Vec<String>,
    pub tags: Vec<String>,
    /// Empty for tickets saved before priorities existed.
    pub priority: String,
    /// `YYYY-MM-DD`; empty if the ticket does not say.
    pub created: String,
    /// The epic it is filed under (backlog only), by directory.
    #[serde(default)]
    pub epic: Option<String>,
    /// The ticket's body (everything after the frontmatter), for searching.
    #[serde(default)]
    pub text: String,
    /// The epic it belongs to by its `epic:` label, kept after it leaves the
    /// epic's folder (where `epic` above says it is filed). Empty if none.
    #[serde(default)]
    pub epic_label: String,
    /// Its size estimate (`rite`'s `points:`), empty if unsized.
    #[serde(default)]
    pub points: String,
    /// Its lock, while one is in force.
    #[serde(default)]
    pub lock: Option<Lock>,
    /// A lease still recorded but past its time (ticket 110). It blocks
    /// nothing; it's shown so a lapsed lock isn't mistaken for no lock.
    #[serde(default)]
    pub lapsed: Option<Lock>,
    /// The ticket it sits under (`parent:`), as written; empty if none.
    #[serde(default)]
    pub parent: String,
    /// The tickets it's related to (`related:`), as written.
    #[serde(default)]
    pub related: Vec<String>,
    /// The ticket this one is a copy of (`clone-of:`), as written; empty if none.
    #[serde(default)]
    pub clone_of: String,
    /// The tickets that are copies of it (`clones:`), as written.
    #[serde(default)]
    pub clones: Vec<String>,
    /// Its typed links (`links:`), each `kind:id` as written (ticket 207).
    #[serde(default)]
    pub links: Vec<String>,
    /// The ticket auto priority raised it from (ticket 215), or empty.
    #[serde(default)]
    pub priority_from: String,
    /// When it's due, `YYYY-MM-DD`, or empty (ticket 143).
    #[serde(default)]
    pub due: String,
    /// How it repeats (`monthly`…), or empty.
    #[serde(default)]
    pub recur: String,
}

/// The kinds of link a ticket can have (ticket 207), as `(slug, label)`:
/// the slug is what `links:` and `rite link --type` use. The same list, in
/// the same order, as `rite_lib::links::LinkType::ALL`, which `rite-ui`'s
/// tests check, since this crate can't depend on rite-lib.
pub const LINK_KINDS: [(&str, &str); 9] = [
    ("relates-to", "Relates to"),
    ("requires", "Requires"),
    ("required-by", "Required by"),
    ("parent-of", "Parent of"),
    ("child-of", "Child of"),
    ("clone-of", "Clone of"),
    ("cloned-by", "Cloned by"),
    ("supersedes", "Supersedes"),
    ("superseded-by", "Superseded by"),
];

/// The kind as the other ticket records it (ticket 121), as
/// `rite_lib::links::LinkType::inverse`: `requires` is `required-by` from
/// the other end. An unknown slug is its own.
pub fn link_inverse(slug: &str) -> &str {
    match slug {
        "requires" => "required-by",
        "required-by" => "requires",
        "parent-of" => "child-of",
        "child-of" => "parent-of",
        "clone-of" => "cloned-by",
        "cloned-by" => "clone-of",
        "supersedes" => "superseded-by",
        "superseded-by" => "supersedes",
        other => other,
    }
}

/// The label for a link kind's slug (the slug itself if it's unknown).
pub fn link_label(slug: &str) -> String {
    LINK_KINDS
        .iter()
        .find(|(known, _)| *known == slug)
        .map_or_else(|| slug.to_string(), |(_, label)| (*label).to_string())
}

/// A `links:` entry split into its kind slug and the other ticket's id.
pub fn split_link(entry: &str) -> Option<(&str, &str)> {
    let (kind, id) = entry.split_once(':')?;
    (!id.trim().is_empty()).then_some((kind, id.trim()))
}

/// The tickets a person may be after when they type `query` into the link
/// search (ticket 207), best first, at most `limit`. A number (`12`, `#12`)
/// finds the ticket numbered 12 first and then those whose number starts
/// with it; words must all appear in the title (ignoring case), or in the
/// file name, so a pasted slug works. `me` is left out, and nothing is
/// offered for an empty query.
pub fn link_candidates(
    all: &[TicketSummary],
    me: Option<u32>,
    query: &str,
    limit: usize,
) -> Vec<TicketSummary> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let digits = query.trim_start_matches('#');
    let number = (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
        .then(|| digits.trim_start_matches('0'));
    let words: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .map(String::from)
        .collect();
    let mut hits: Vec<(u8, u32, &TicketSummary)> = all
        .iter()
        .filter(|t| me.is_none() || ticket_number(&t.id) != me)
        .filter_map(|t| {
            let n = ticket_number(&t.id).unwrap_or(0);
            if let Some(typed) = number {
                let shown = n.to_string();
                if typed.is_empty() || shown == typed {
                    return Some((0, n, t));
                }
                if shown.starts_with(typed) {
                    return Some((1, n, t));
                }
            }
            let haystack = format!("{} {}", t.title, t.file).to_lowercase();
            words
                .iter()
                .all(|w| haystack.contains(w.as_str()))
                .then_some((2, n, t))
        })
        .collect();
    hits.sort_by_key(|(rank, n, _)| (*rank, *n));
    hits.into_iter()
        .take(limit)
        .map(|(_, _, t)| t.clone())
        .collect()
}

/// The ticket number a `parent`/`related` entry names: `042`, `"042"` or
/// `[[042-some-title]]` all give 42.
pub fn ticket_number(reference: &str) -> Option<u32> {
    let digits: String = reference
        .trim()
        .trim_matches(|c| c == '"' || c == '[')
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// A lock (lease) on a ticket that is in force: who holds it and until when.
/// While it holds, nobody else can edit, move or archive the ticket.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Lock {
    /// The holder's git identity (`user.email`).
    pub by: String,
    /// When it lapses, as RFC 3339 UTC (`locked-until`).
    pub until: String,
    /// The same moment in local time, for display.
    pub until_local: String,
    /// Whether the person using the board holds it.
    pub mine: bool,
}

impl TicketSummary {
    /// Everyone it's assigned to: `assignees`, or `assignee` alone from an
    /// older riteui that didn't send the list.
    pub fn people(&self) -> Vec<String> {
        if !self.assignees.is_empty() {
            return self.assignees.clone();
        }
        Some(self.assignee.trim().to_string())
            .filter(|who| !who.is_empty())
            .into_iter()
            .collect()
    }

    /// The card's names (ticket 254): `@Alex`, or `@Alex +1` for two.
    pub fn people_label(&self) -> Option<String> {
        let people = self.people();
        let first = people.first()?;
        Some(match people.len() {
            1 => format!("@{first}"),
            n => format!("@{first} +{}", n - 1),
        })
    }

    /// Whether the ticket matches a search (ticket 123), ignoring case. The
    /// query is split on spaces into terms:
    ///
    /// - `word` must appear, and `-word` must not, in the ticket's id,
    ///   title, file name, body, assignee, priority or epic. **Tags are not
    ///   searched as text**, so `triage` never finds the tag `TRIAGE`.
    /// - `#TAG` matches a ticket carrying a tag that starts with `TAG`, and
    ///   `-#TAG` one that carries none. With several `#TAG` terms a ticket
    ///   needs just one of them; every `-#TAG` still rules tickets out.
    /// - A lone `-`, `#` or `-#` is ignored. An empty query matches all.
    ///
    /// Tags are checked first: they are short, and a ticket they rule out
    /// never has its text lowercased.
    pub fn matches(&self, query: &str) -> bool {
        let mut words: Vec<String> = Vec::new();
        let mut not_words: Vec<String> = Vec::new();
        let mut tags: Vec<String> = Vec::new();
        let mut not_tags: Vec<String> = Vec::new();
        for term in query.split_whitespace() {
            let (excluded, rest) = match term.strip_prefix('-') {
                Some(rest) => (true, rest),
                None => (false, term),
            };
            let (is_tag, rest) = match rest.strip_prefix('#') {
                Some(rest) => (true, rest),
                None => (false, rest),
            };
            if rest.is_empty() {
                continue;
            }
            let list = match (is_tag, excluded) {
                (true, true) => &mut not_tags,
                (true, false) => &mut tags,
                (false, true) => &mut not_words,
                (false, false) => &mut words,
            };
            list.push(if is_tag {
                rest.to_uppercase()
            } else {
                rest.to_lowercase()
            });
        }
        let has = |wanted: &String| {
            self.tags
                .iter()
                .any(|t| t.trim().to_uppercase().starts_with(wanted.as_str()))
        };
        if !tags.is_empty() && !tags.iter().any(has) {
            return false;
        }
        if not_tags.iter().any(has) {
            return false;
        }
        if words.is_empty() && not_words.is_empty() {
            return true;
        }
        let haystack = [
            self.id.as_str(),
            self.title.as_str(),
            self.file.as_str(),
            self.text.as_str(),
            self.assignee.as_str(),
            &self.assignees.join("\n"),
            self.priority.as_str(),
            self.epic.as_deref().unwrap_or(""),
            self.epic_label.as_str(),
        ]
        .join("\n")
        .to_lowercase();
        words.iter().all(|w| haystack.contains(w.as_str()))
            && !not_words.iter().any(|w| haystack.contains(w.as_str()))
    }
}

/// The tag term being typed at the end of a search (ticket 123): the last
/// word, when it starts `#` or `-#` and the query doesn't end in a space.
/// `#T` gives `Some(("", "T"))`; `x -#tr` gives `Some(("-", "TR"))`. The
/// search box offers the tags starting with the typed part.
pub fn tag_being_typed(query: &str) -> Option<(&'static str, String)> {
    if query.ends_with(char::is_whitespace) {
        return None;
    }
    let word = query.split_whitespace().next_back()?;
    if let Some(typed) = word.strip_prefix("-#") {
        Some(("-", typed.to_uppercase()))
    } else {
        word.strip_prefix('#')
            .map(|typed| ("", typed.to_uppercase()))
    }
}

/// The query with the tag term being typed finished as `choice`, followed by
/// a space so the next term can be typed straight away. A query that isn't
/// ending in a tag term is returned unchanged.
pub fn complete_tag(query: &str, choice: &str) -> String {
    let Some((dash, _)) = tag_being_typed(query) else {
        return query.to_string();
    };
    let start = query.rfind(char::is_whitespace).map_or(0, |i| {
        i + query[i..].chars().next().map_or(1, char::len_utf8)
    });
    format!("{}{dash}#{} ", &query[..start], choice.to_uppercase())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardColumn {
    pub column: Column,
    pub tickets: Vec<TicketSummary>,
}

impl Board {
    /// The workflow `repo`'s tickets move by: that board's own, else the
    /// one drawn.
    /// Whether `repo`'s board pairs repository and branch type (ticket 066).
    pub fn tickets_only(&self, repo: &str) -> bool {
        self.repos
            .iter()
            .find(|r| r.name == repo && r.available)
            .is_some_and(|r| r.tickets_only)
    }

    pub fn workflow_of(&self, repo: &str) -> &Workflow {
        self.repos
            .iter()
            .find(|r| r.name == repo && r.available)
            .map_or(&self.workflow, |r| &r.workflow)
    }

    /// What `column` is called on this board.
    pub fn label(&self, column: Column) -> String {
        self.workflow.label(column)
    }

    /// The loose tickets in `column` (those in an epic are under `epics`).
    pub fn tickets(&self, column: Column) -> &[TicketSummary] {
        self.columns
            .iter()
            .find(|c| c.column == column)
            .map_or(&[], |c| c.tickets.as_slice())
    }
}

/// `shown` when there is one, else `name` (ticket 326): a page sent by an
/// older riteui has no `shown`.
pub fn shown_name<'a>(name: &'a str, shown: &'a str) -> &'a str {
    if shown.trim().is_empty() { name } else { shown }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Board {
    /// The project's name (`project` in its `.settings`), shown in the
    /// header; the repository's name when the board doesn't declare one.
    #[serde(default)]
    pub project: String,
    /// What people read for the project (ticket 326): its `display-name`,
    /// else `project`. The window title and the header show it.
    #[serde(default)]
    pub shown: String,
    /// Absolute path of the `tasks/` directory being shown.
    pub tasks_dir: String,
    /// Every repository the project spans, the board's own first. Tickets
    /// and epics from all available ones are merged into the lists below.
    #[serde(default)]
    pub repos: Vec<RepoInfo>,
    /// The board's columns in order, with their names and own WIP limits
    /// (tickets 223, 224): what the page draws. `columns` follows it.
    #[serde(default)]
    pub workflow: Workflow,
    /// The columns' loose tickets; tickets inside an epic are under `epics`.
    pub columns: Vec<BoardColumn>,
    /// The backlog's epics, by name.
    #[serde(default)]
    pub epics: Vec<Epic>,
    /// Every project in the folder this one sits in (ticket 093), for the
    /// header's switcher. Just this one when it's alone there.
    #[serde(default)]
    pub projects: Vec<ProjectChoice>,
    /// The repositories tickets can be about, for the filter and the
    /// Repository fields: the project's declared ones plus any a ticket
    /// already names, sorted.
    #[serde(default)]
    pub ticket_repos: Vec<RepoChoice>,
    /// The project's users' names, for the assignee picker (ticket 089).
    #[serde(default)]
    pub users: Vec<String>,
    /// Who's using the board, by git `user.name`: the picker's "Me".
    #[serde(default)]
    pub me: Option<String>,
    /// The project's tags (ticket 122): every listed tag, and every tag in
    /// use on its boards, each with how many tickets carry it. Sorted;
    /// always UPPERCASE.
    #[serde(default)]
    pub tags: Vec<TagUse>,
    /// Only listed tags may be put on a ticket (the project's `prevent-arbitrary-tags`, formerly `tags-only`).
    #[serde(default)]
    pub tags_only: bool,
    /// Whether this person gets Admin (ticket 149), and why.
    #[serde(default)]
    pub admin: AdminAccess,
    /// The documents repository every project shares (ticket 129); empty
    /// when the project names none.
    #[serde(default)]
    pub shared_documents: String,
    /// Whether the landing page pulls in this project's GitHub issues
    /// (ticket 173).
    #[serde(default)]
    pub import_issues: bool,
    /// Priorities follow requires links (ticket 215) and due dates (252).
    #[serde(default)]
    pub auto_priority: bool,
    /// Days a ticket should take in todo, then doing (ticket 252); 0 is no
    /// limit.
    #[serde(default)]
    pub time_limits: (u32, u32),
    /// A ticket due within this many days goes straight to todo (ticket
    /// 253); 0 never.
    #[serde(default)]
    pub due_to_todo_days: u32,
    /// The backlog column can't be folded away (ticket 253).
    #[serde(default)]
    pub backlog_always_shown: bool,
    /// A priority set by hand pins its ticket against auto priority.
    #[serde(default)]
    pub priority_override: bool,
    /// The KanRite version the project's team requires, when it's newer
    /// than this build (ticket 193): the board is read-only here until
    /// updated, and the update bar has no Later.
    #[serde(default)]
    pub needs_version: Option<String>,
    /// The project is archived (ticket 157): read-only until unarchived.
    #[serde(default)]
    pub archived: bool,
}

/// A newer KanRite release for the update bar (ticket 192): its version,
/// its release notes, how many releases this build is behind, and the
/// version running now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateInfo {
    pub version: String,
    pub notes_url: String,
    pub behind: usize,
    pub current: String,
}

/// A folder path in one comparable form (ticket 329): `\` as `/`, a
/// Windows `\\?\` prefix and a trailing slash dropped, and the case folded
/// when it starts with a drive letter. On Windows git reports `G:/a/b`
/// where the registry holds `G:\a\b`, and the page compares what each gave.
pub fn path_key(path: &str) -> String {
    let path = path.replace('\\', "/");
    let path = path
        .strip_prefix("//?/")
        .unwrap_or(&path)
        .trim_end_matches('/');
    if path.as_bytes().get(1) == Some(&b':') {
        path.to_lowercase()
    } else {
        path.to_string()
    }
}

/// Whether two project or repository names are the same name, whatever their
/// case (ticket 338). The page can't use rite-lib's `names::same`, which
/// rite-ui's tests check this agrees with.
pub fn same_name(a: &str, b: &str) -> bool {
    a == b || a.to_lowercase() == b.to_lowercase()
}

/// Whether two folder paths name the same place, whichever slashes they use.
pub fn same_path(a: &str, b: &str) -> bool {
    path_key(a) == path_key(b)
}

/// The folder holding `path`, split on `/` or `\` alike: the page runs as
/// wasm, where `std::path::Path` only knows `/`.
pub fn parent_path(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches(['/', '\\']);
    let cut = trimmed.rfind(['/', '\\'])?;
    Some(trimmed[..cut].to_string())
}

/// How long ago something was, from seconds: "just now", "5 minutes ago",
/// "2 hours ago", "3 days ago" (Preferences' last update check, ticket 304).
pub fn ago(seconds: i64) -> String {
    let (n, unit) = match seconds.max(0) {
        s if s < 60 => return "just now".to_string(),
        s if s < 3_600 => (s / 60, "minute"),
        s if s < 86_400 => (s / 3_600, "hour"),
        s => (s / 86_400, "day"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

/// What a check for updates asked for by hand found (ticket 304).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "lowercase")]
pub enum ManualCheck {
    /// A newer release: the update bar offers it.
    Newer { info: UpdateInfo },
    /// This build is the newest.
    Current { current: String },
    /// GitHub couldn't be asked.
    Failed { why: String },
}

/// What the update bar's Update did (ticket 191).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "lowercase")]
pub enum UpdateResult {
    /// Updated to this version; riteui restarts into it.
    Updated { version: String },
    /// Not an install riteui replaces (a source build, a package manager's),
    /// and why; `install` is the installer's command when running it is the
    /// way, for the bar to show. riteui sends it because the page can't tell
    /// which system it's on: on Windows it's `install.ps1` (ticket 282).
    Unmanaged {
        why: String,
        install: Option<String>,
    },
    /// It stopped: why, and the version rite and riteui are now.
    Failed { why: String, now: String },
}

/// Whether to show Admin, and the note that says why (ticket 149).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminAccess {
    pub allowed: bool,
    pub note: String,
}

impl Default for AdminAccess {
    /// Open, as before ticket 149: never lock someone out on a guess.
    fn default() -> Self {
        AdminAccess {
            allowed: true,
            note: String::new(),
        }
    }
}

/// One of a project's tags and how much it's used.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagUse {
    pub name: String,
    /// Tickets carrying it, across the project's boards (not the archive).
    pub count: usize,
    /// On the project's tag list (Admin), as opposed to only in use.
    pub listed: bool,
    /// Built into the tool (DRAFT, ticket 114; TRIAGE, 158): always offered and allowed,
    /// never on the project's list, never removable.
    #[serde(default)]
    pub reserved: bool,
}

/// A repository tickets can be about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoChoice {
    pub name: String,
    /// Cloned on this machine. One that isn't can't be chosen for a ticket
    /// (tickets 101/107) and is shown greyed out.
    pub available: bool,
}

/// A `repository` value as a bare name: `kanrite/rite`, `amd.git` and `amd`
/// all name `amd`. Must match `rite_lib::task::repo_name`.
pub fn repo_name(value: &str) -> String {
    let value = value.trim().trim_end_matches('/');
    let value = value.strip_suffix(".git").unwrap_or(value);
    value
        .rsplit(['/', ':'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// A project the header can switch to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectChoice {
    pub name: String,
    /// Its display name, else `name` (ticket 326); `name` is what's chosen.
    #[serde(default)]
    pub shown: String,
    /// Whether it's the one on screen.
    pub current: bool,
}

/// The user filter: whose work to show.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum Assigned {
    /// Everyone's tickets ("All").
    #[default]
    Anyone,
    /// Only tickets assigned to this user.
    User(String),
    /// Only tickets nobody is assigned to.
    Nobody,
}

impl Assigned {
    pub fn matches(&self, ticket: &TicketSummary) -> bool {
        let people = ticket.people();
        match self {
            Assigned::Anyone => true,
            // Any of its people (ticket 254).
            Assigned::User(name) => people.iter().any(|who| who.trim() == name),
            Assigned::Nobody => people.is_empty(),
        }
    }

    /// The dropdown's `value` for this choice. `""` is All; a user's own
    /// name; a value no assignee can have (it starts with a space) for
    /// unassigned.
    pub fn key(&self) -> String {
        match self {
            Assigned::Anyone => String::new(),
            Assigned::User(name) => name.clone(),
            Assigned::Nobody => " unassigned".into(),
        }
    }

    pub fn from_key(key: &str) -> Assigned {
        match key {
            "" => Assigned::Anyone,
            " unassigned" => Assigned::Nobody,
            name => Assigned::User(name.to_string()),
        }
    }
}

impl Board {
    /// Everyone with a ticket assigned anywhere on the board, in alphabetical
    /// order (ignoring case), for the user filter.
    pub fn assignees(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .columns
            .iter()
            .flat_map(|c| c.tickets.iter())
            .chain(self.epics.iter().flat_map(|e| e.tickets.iter()))
            .flat_map(|t| t.people())
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())
            .collect();
        names.sort_by_key(|n| n.to_lowercase());
        names.dedup();
        names
    }
}

/// A repository the project spans (`rite register`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoInfo {
    pub name: String,
    /// Cloned on this machine with a `tasks/` board; otherwise only declared.
    pub available: bool,
    /// Its board's WIP limit (on doing).
    #[serde(default)]
    pub wip: wip::WipLimits,
    /// How long a lock lasts on its board, in hours.
    #[serde(default = "default_lease_hours")]
    pub lease_hours: u32,
    /// Its board's own workflow, which its tickets' moves follow (ticket
    /// 225): each board reads its own `.settings`, as rite does.
    #[serde(default)]
    pub workflow: Workflow,
    /// Its board is a dedicated `*-tickets` repository, where a ticket's
    /// repository and branch type go together (ticket 066).
    #[serde(default)]
    pub tickets_only: bool,
}

/// How long a lock lasts unless a board says otherwise: a working day.
pub const DEFAULT_LEASE_HOURS: u32 = 8;

fn default_lease_hours() -> u32 {
    DEFAULT_LEASE_HOURS
}

/// A backlog container — an epic or a sprint — and the tickets filed in it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Epic {
    /// The repository whose backlog it is in.
    #[serde(default)]
    pub repo: String,
    pub name: String,
    #[serde(default)]
    pub kind: epic::GroupKind,
    /// A sprint's length; 0 for an epic.
    #[serde(default)]
    pub days: u32,
    /// Whether a sprint has been started (epics never are).
    #[serde(default)]
    pub started: bool,
    pub tickets: Vec<TicketSummary>,
    /// A closed sprint (ticket 052): read-only, like a started one, and
    /// no longer running.
    #[serde(default)]
    pub closed: bool,
    /// Tickets in it, in any column, that aren't done (ticket 204): it can
    /// be deleted when there are none.
    #[serde(default)]
    pub unfinished: usize,
}

/// A ticket's full markdown, as opened in the editor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketDocument {
    #[serde(default)]
    pub repo: String,
    pub file: String,
    pub column: Column,
    /// Set when the ticket lives inside an epic in the backlog.
    #[serde(default)]
    pub epic: Option<String>,
    pub content: String,
    /// Its lock, while one is in force.
    #[serde(default)]
    pub lock: Option<Lock>,
    /// A lease still recorded but past its time (ticket 110). It blocks
    /// nothing; it's shown so a lapsed lock isn't mistaken for no lock.
    #[serde(default)]
    pub lapsed: Option<Lock>,
    /// A fingerprint of `content` as it was opened. Saving hands it back,
    /// and the save is refused if the ticket has changed since.
    #[serde(default)]
    pub version: String,
    /// "created by … on …; last edited by … on …" (ticket 155), or empty.
    #[serde(default)]
    pub authorship: String,
    /// Why its priority is what it is, when auto priority raised or a hand
    /// pinned it (ticket 215), or empty.
    #[serde(default)]
    pub priority_note: String,
}

/// What happened in git after a ticket change was written to disk.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReport {
    /// The commit message used, e.g. `edit: 004 "title"`.
    pub message: String,
    pub committed: bool,
    pub pushed: bool,
    /// Why it was not committed or pushed, when it should have been.
    pub warning: Option<String>,
}

/// What a background catch-up found (tickets 102, 197).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncStatus {
    pub warnings: Vec<String>,
    /// Origin couldn't be reached at the last try: how many changes are
    /// waiting to push. `None` when it could.
    pub offline: Option<u32>,
}

/// What importing GitHub issues did, or would do (ticket 158).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueImport {
    /// One line per issue: `#12 -> 160 "Title" (owner/repo) [TRIAGE, BUG]`.
    pub lines: Vec<String>,
    /// `imported 2 issues, 1 already imported`, or the preview's version.
    pub summary: String,
    /// The commit, after a real import that brought something in.
    pub sync: Option<SyncReport>,
}

/// A project's documents repository (ticket 129): its own, or the one every
/// project shares.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocSource {
    /// The repository's name.
    pub name: String,
    /// The one every project shares, not the project's own.
    pub shared: bool,
    /// Whether it's cloned on this machine; when not, `documents` is empty.
    pub cloned: bool,
    pub documents: Vec<DocEntry>,
    /// Every folder, empty ones included (ticket 176).
    #[serde(default)]
    pub folders: Vec<String>,
    /// The page Documents opens on (ticket 179): the pinned one, else the
    /// README. Only the project's own repository has one.
    #[serde(default)]
    pub start: Option<String>,
    /// Whether `start` was pinned, rather than the README by default.
    #[serde(default)]
    pub pinned: bool,
}

/// One of your pinned documents (ticket 232).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocPin {
    /// The documents repository it's in.
    pub source: String,
    pub path: String,
    /// Its first heading, else its file name.
    pub title: String,
    /// Still there (and that repository cloned here); a missing one is
    /// listed so it can be unpinned.
    pub exists: bool,
}

/// A line of a document that matched a search (ticket 179).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocHit {
    pub source: String,
    pub path: String,
    pub title: String,
    /// 1-based; 0 when only the title matched.
    pub line: usize,
    pub text: String,
}

/// One document in a [`DocSource`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocEntry {
    /// Relative to the repository, `/` between parts.
    pub path: String,
    pub title: String,
    /// Marked `status: archived` (ticket 176).
    #[serde(default)]
    pub archived: bool,
}

/// A document, rendered for reading.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedDoc {
    /// The repository it's in.
    pub source: String,
    pub path: String,
    /// HTML made by riteui from the markdown: raw HTML in the document is
    /// shown as text, a link to another document is `#doc:<path>`, a link
    /// rite won't follow is `#`, and images are inlined as data.
    pub html: String,
    /// Set when this is an earlier version of the document, not the
    /// current one (ticket 169).
    #[serde(default)]
    pub version: Option<DocVersion>,
    /// Marked `status: archived` (ticket 176).
    #[serde(default)]
    pub archived: bool,
}

/// A document opened for editing (ticket 176).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocEdit {
    pub source: String,
    pub path: String,
    /// Its frontmatter block, fences included, kept as it is; empty for
    /// none.
    pub frontmatter: String,
    /// The markdown below it: what's edited.
    pub body: String,
    /// A fingerprint of the whole text as opened; saving hands it back.
    pub version: String,
    /// What the rich editor can't keep (tables, images…): when not empty,
    /// it's edited as Markdown only.
    pub unsupported: Vec<String>,
}

/// A change to a documents repository, and where its document now is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocChange {
    pub sync: SyncReport,
    /// The document's path after the change (moved or created), if any.
    pub path: Option<String>,
}

/// An idea in the list (ticket 131): best first, with your own vote.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdeaCard {
    /// `ideas/<file>.md` in the project's documents repository.
    pub path: String,
    pub title: String,
    /// Who put it forward (a name), if they said.
    pub author: Option<String>,
    pub created: String,
    /// The sum of everyone's votes.
    pub score: i32,
    pub voters: usize,
    /// Your vote, −3 to +3.
    pub mine: i32,
    pub notes: usize,
    /// The tickets made from it, as `<tickets repository>#<id>`.
    pub tickets: Vec<String>,
    /// The epics and sprints made from it (ticket 231), as
    /// `<tickets repository>#<name>`.
    #[serde(default)]
    pub epics: Vec<String>,
    #[serde(default)]
    pub sprints: Vec<String>,
    pub archived: bool,
}

/// The project's ideas.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdeaList {
    /// The documents repository they're in.
    pub source: String,
    /// Whether it's cloned here; nothing can be shown or added otherwise.
    pub cloned: bool,
    pub ideas: Vec<IdeaCard>,
}

/// A milestone of a roadmap (ticket 206). Adding one made an epic — or a
/// sprint, if the roadmap uses sprints — of the same name on its board.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MilestoneCard {
    pub name: String,
    pub description: String,
    /// `epic` or `sprint`; set from the roadmap when one is added.
    pub kind: String,
    /// `YYYY-MM-DD`, or empty.
    pub start: String,
    /// The due-by date, `YYYY-MM-DD`, or empty.
    pub due: String,
    /// Working days a week, 1 to 7; 0 for the usual five.
    pub days_per_week: u32,
    /// Working days it takes; 0 when none was given.
    pub duration: u32,
    /// Working days it takes, given or worked out from its dates.
    pub total: Option<u32>,
    /// Working days left to its due date; negative once past.
    pub remaining: Option<i64>,
    /// Taking longer than calculated.
    pub late: bool,
}

/// A roadmap for one board (ticket 206).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoadmapCard {
    /// Names it in the commands.
    pub slug: String,
    pub name: String,
    /// The board (repository) it plans; empty for a roadmap from before
    /// roadmaps had boards, which is adopted by one.
    pub board: String,
    /// What it is for.
    pub goal: String,
    /// Milestones are sprints rather than epics.
    pub sprints: bool,
    pub milestones: Vec<MilestoneCard>,
    /// Total working days of the milestones that have a length.
    pub duration: Option<u32>,
    /// Working days left to the last due date; negative once past.
    pub remaining: Option<i64>,
    /// Taking longer than calculated.
    pub late: bool,
}

/// The Roadmaps page: the roadmaps, and the boards still without one.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoadmapsPage {
    pub roadmaps: Vec<RoadmapCard>,
    /// Boards (repositories) of the project that have no roadmap.
    pub free_boards: Vec<String>,
}

/// "12 days", "1 day".
pub fn days_label(days: i64) -> String {
    let n = days.abs();
    format!("{n} day{}", if n == 1 { "" } else { "s" })
}

/// One person's vote, as the idea shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdeaVote {
    pub name: String,
    pub value: i32,
}

/// A note under an idea.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdeaNote {
    pub name: String,
    pub date: String,
    pub text: String,
}

/// An idea opened: its write-up rendered, who voted, and the notes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdeaDetail {
    pub card: IdeaCard,
    pub source: String,
    pub html: String,
    pub votes: Vec<IdeaVote>,
    pub notes: Vec<IdeaNote>,
}

/// A ticket made from an idea, and where to find it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdeaTicket {
    pub sync: SyncReport,
    pub id: String,
    /// The tickets repository's name.
    pub repo: String,
    pub file: String,
}

/// One version of a document, from git (ticket 169).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocVersion {
    pub commit: String,
    pub short: String,
    pub author: String,
    /// `YYYY-MM-DD HH:MM`.
    pub date: String,
    pub subject: String,
    /// The document's path at that commit.
    pub path: String,
}

/// The landing page (ticket 020): your work across every project on this
/// machine, and the projects themselves.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Home {
    /// Your todo and doing tickets, highest priority first, then oldest.
    pub work: Vec<WorkItem>,
    /// When `work` is empty, backlog tickets you could pick (ticket 160):
    /// the highest priority there is any of, oldest first.
    #[serde(default)]
    pub to_pick: Vec<WorkItem>,
    /// Whether work to pick includes unassigned tickets (My work's
    /// checkbox, remembered on this machine).
    #[serde(default)]
    pub include_unassigned: bool,
    /// Every project on this machine, by name; hidden ones marked.
    pub projects: Vec<HomeProject>,
    /// Who "you" is (git's `user.email`); empty when git doesn't say.
    pub me: String,
    /// The period, in days, the projects' statistics count over (7, 30 or
    /// 90; ticket 233), remembered on this machine.
    #[serde(default = "default_stats_days")]
    pub stats_days: u32,
    /// The project riteui has open, and remembers for next time, by its
    /// board repository's path (`HomeProject.root`); empty when none
    /// (ticket 168: the top bar's picker and documents buttons act on it).
    #[serde(default)]
    pub current: String,
}

fn default_stats_days() -> u32 {
    30
}

/// How many tickets are open, and how many need looking at first (ticket
/// 349): shown by the board's name and by each project on the landing page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketCounts {
    /// Tickets not in a done-kind column: the backlog (loose, and in epics and
    /// sprints, drafts included), todo, doing, and a project's own columns
    /// that aren't done.
    pub open: usize,
    /// Of those, priority top.
    pub top: usize,
    /// Of those, tagged TRIAGE. Always top as well, so each is in both.
    pub triage: usize,
}

impl TicketCounts {
    fn add(&mut self, ticket: &TicketSummary) {
        self.open += 1;
        if ticket.priority.eq_ignore_ascii_case("top") {
            self.top += 1;
        }
        if ticket
            .tags
            .iter()
            .any(|t| t.trim().eq_ignore_ascii_case("TRIAGE"))
        {
            self.triage += 1;
        }
    }
}

impl Board {
    /// The open tickets this board draws, and how many are top and TRIAGE
    /// (ticket 349): every column that isn't done-kind, and the tickets in
    /// epics and sprints, which the page keeps apart from the columns. Of the
    /// whole project, not of what a search or filter shows. rite-lib's
    /// `my_work::counts` is the same count; rite-ui's tests check they agree.
    pub fn counts(&self) -> TicketCounts {
        let mut counts = TicketCounts::default();
        for column in &self.columns {
            if column.column.kind() != Kind::Done {
                column.tickets.iter().for_each(|t| counts.add(t));
            }
        }
        for epic in &self.epics {
            epic.tickets.iter().for_each(|t| counts.add(t));
        }
        counts
    }
}

/// A project's statistics (ticket 233), read from git history.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectStats {
    pub days: u32,
    pub raised: usize,
    pub done: usize,
    /// Abandoned or deleted.
    pub closed: usize,
    /// Oldest first; the last ends today.
    pub weeks: Vec<StatWeek>,
    /// Each running sprint on its boards.
    pub sprints: Vec<SprintBurn>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatWeek {
    pub raised: usize,
    pub done: usize,
    pub closed: usize,
}

/// A started sprint's burn, in points.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SprintBurn {
    pub repo: String,
    pub sprint: String,
    /// `YYYY-MM-DD`.
    pub start: String,
    pub days: u32,
    pub total: u32,
    /// Points done by the end of each day so far.
    pub done_by_day: Vec<u32>,
}

/// A project on the landing page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeProject {
    pub name: String,
    /// Its display name, else `name` (ticket 326).
    #[serde(default)]
    pub shown: String,
    /// Its board's repository: what opening it opens.
    pub root: String,
    /// The folder holding its repositories, to tell two clones apart.
    pub dir: String,
    /// Left out of My work by you.
    pub hidden: bool,
    /// Whether its own documents repository is cloned here (ticket 168):
    /// Documents, ADRs and Ideas are greyed out when not.
    #[serde(default)]
    pub documents: bool,
    /// The shared documents repository it names, if any.
    #[serde(default)]
    pub shared: Option<String>,
    /// Whether that shared repository is cloned here.
    #[serde(default)]
    pub shared_cloned: bool,
    /// Archived for everyone (ticket 157): listed apart, with Unarchive.
    #[serde(default)]
    pub archived: bool,
    /// Its sprint graphs folded on the landing page, by you (ticket 305).
    #[serde(default)]
    pub graphs_folded: bool,
    /// Why its repository isn't one, so it can't be opened (ticket 341).
    #[serde(default)]
    pub broken: Option<String>,
}

/// Which documents the Documents view shows (ticket 168): the project's
/// own repository or the shared one, and a folder in it (`docs`, `adr`,
/// `ideas`), or everything when empty.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocScope {
    pub shared: bool,
    pub folder: String,
}

impl DocScope {
    /// Whether `path` (relative to the repository) is inside the folder.
    pub fn holds(&self, path: &str) -> bool {
        self.folder.is_empty()
            || path
                .strip_prefix(self.folder.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    }
}

/// One ticket in My work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItem {
    pub project: String,
    /// The project's display name, else `project` (ticket 326).
    #[serde(default)]
    pub project_shown: String,
    /// The project's board repository, opened to show the ticket.
    pub project_root: String,
    /// The repository whose board the ticket is on.
    pub repo: String,
    pub column: Column,
    pub id: String,
    /// File name, `NNN-slug.md`.
    pub file: String,
    pub title: String,
    pub priority: String,
    pub points: String,
    pub created: String,
    /// The backlog folder (epic or sprint) it's filed in, to find it there.
    #[serde(default)]
    pub folder: Option<String>,
    /// Tagged TRIAGE (ticket 248): first in My work, for everyone.
    #[serde(default)]
    pub triage: bool,
}

/// A newly created ticket and what happened when it was recorded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Created {
    pub doc: TicketDocument,
    pub sync: SyncReport,
}

/// Where a new repository would be made, for the dialog (ticket 228).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoDefaults {
    /// In words: "on GitHub, under acme-org", "beside the tickets
    /// repository's remote", "on this machine only".
    pub place: String,
    /// The same as the tickets repository's.
    pub private: bool,
    /// What a name starts with, the project's: "acme-".
    pub prefix: String,
    /// Why it can't be done here, if it can't (not an admin, another forge).
    #[serde(default)]
    pub refused: Option<String>,
}

/// The template new tickets are rendered from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketTemplate {
    /// Where an overriding template lives (or would live) on disk.
    pub path: String,
    pub source: String,
    /// `true` when `source` comes from the board's own override file rather
    /// than the built-in default.
    pub customised: bool,
}

/// The frontmatter fields the board needs from a ticket.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frontmatter {
    pub id: String,
    pub title: String,
    pub assignee: String,
    /// Everyone it's assigned to (ticket 254): the `assignees` list while it
    /// starts with `assignee`, else `assignee` alone; empty for nobody.
    pub assignees: Vec<String>,
    /// The repository it's about, as a bare name.
    pub repository: String,
    pub tags: Vec<String>,
    pub branch_type: String,
    pub priority: String,
    /// `YYYY-MM-DD`, as `rite` writes it.
    pub created: String,
    /// The epic the ticket belongs to, if any (`rite`'s `epic:` field).
    pub epic: String,
    /// Its size estimate (`rite`'s `points:` field).
    pub points: String,
    /// Who holds its lock (`locked-by`), and until when (`locked-until`).
    pub locked_by: String,
    pub locked_until: String,
    /// When it's due and how it repeats (ticket 143).
    pub due: String,
    pub recur: String,
}

/// Reads the handful of fields the board displays from a ticket's
/// `---`-delimited frontmatter.
///
/// This is deliberately not a YAML parser: tickets are edited as raw text, so
/// the board only needs to *read* a few flat `key: value` lines, and anything
/// it cannot read simply shows as empty.
pub fn parse_frontmatter(content: &str) -> Frontmatter {
    let mut fm = Frontmatter::default();
    let mut lines = content.lines();
    if lines.next().map(str::trim) != Some("---") {
        return fm;
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "id" => fm.id = unquote(value),
            "title" => fm.title = unquote(value),
            "assignee" => fm.assignee = unquote(value),
            "repository" => fm.repository = repo_name(&unquote(value)),
            "tags" => fm.tags = parse_list(value),
            "branch-type" => fm.branch_type = unquote(value),
            "priority" => fm.priority = unquote(value),
            "created" => fm.created = unquote(value),
            "epic" => fm.epic = unquote(value),
            "points" => fm.points = unquote(value),
            "locked-by" => fm.locked_by = unquote(value),
            "locked-until" => fm.locked_until = unquote(value),
            "due" => fm.due = unquote(value),
            "recur" => fm.recur = unquote(value),
            "assignees" => fm.assignees = parse_names(value),
            _ => {}
        }
    }
    fm.assignees = if fm.assignee.trim().is_empty() {
        Vec::new()
    } else if fm.assignees.first() == Some(&fm.assignee) {
        std::mem::take(&mut fm.assignees)
    } else {
        vec![fm.assignee.clone()]
    };
    fm
}

/// A flow list of names, `["Alex Morgan","Jo, Jr"]`, as rite writes
/// `assignees`: a quoted name may hold a comma. Mirrors
/// `rite_lib::task::parse_names`.
pub fn parse_names(raw: &str) -> Vec<String> {
    let inner = raw.trim().trim_start_matches('[').trim_end_matches(']');
    let mut names = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for ch in inner.chars() {
        match (quote, ch) {
            (Some(_), '\\') if !escaped => escaped = true,
            (Some(q), c) if c == q && !escaped => quote = None,
            (Some(_), c) => {
                current.push(c);
                escaped = false;
            }
            (None, '"' | '\'') => quote = Some(ch),
            (None, ',') => names.push(std::mem::take(&mut current)),
            (None, c) => current.push(c),
        }
    }
    names.push(current);
    names
        .into_iter()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .collect()
}

fn unquote(value: &str) -> String {
    let v = value.trim();
    let quoted = v.len() >= 2
        && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')));
    if quoted {
        v[1..v.len() - 1].replace("\\\"", "\"")
    } else {
        v.to_string()
    }
}

fn parse_list(value: &str) -> Vec<String> {
    value
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(unquote)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Turns a title into the file-name slug `rite` uses — the same rules as
/// `rite_lib::task::slugify` (letters from other scripts transliterated,
/// symbols as separators, at most 50 characters), so a branch name the
/// editor derives matches the one `rite` would. `rite-ui` tests the two agree.
pub fn slugify(title: &str) -> String {
    const MAX_SLUG: usize = 50;
    let mut slug = String::with_capacity(title.len());
    let mut pending_dash = false;
    let push = |ch: char, slug: &mut String, pending_dash: &mut bool| {
        if *pending_dash && !slug.is_empty() {
            slug.push('-');
        }
        slug.push(ch.to_ascii_lowercase());
        *pending_dash = false;
    };
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            push(ch, &mut slug, &mut pending_dash);
        } else if ch.is_alphanumeric() {
            for ch in deunicode::deunicode_char(ch).unwrap_or_default().chars() {
                if ch.is_ascii_alphanumeric() {
                    push(ch, &mut slug, &mut pending_dash);
                } else {
                    pending_dash = true;
                }
            }
        } else {
            pending_dash = true;
        }
    }
    slug.truncate(MAX_SLUG);
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

#[cfg(test)]
mod tests {
    #[test]
    fn paths_match_across_slashes_and_drive_case() {
        use super::same_path;
        assert!(same_path(
            r"G:\github\KanRite\kanrite-tickets",
            "g:/github/KanRite/kanrite-tickets/"
        ));
        assert!(same_path(r"\\?\C:\a\b", "C:/a/b"));
        assert!(!same_path("/home/a/Proj", "/home/a/proj"));
        assert!(!same_path("G:/a/b", "G:/a/c"));
    }

    #[test]
    fn parent_path_splits_on_either_slash() {
        use super::parent_path;
        assert_eq!(parent_path(r"G:\x\y\tasks").as_deref(), Some(r"G:\x\y"));
        assert_eq!(parent_path("/home/x/tasks/").as_deref(), Some("/home/x"));
        assert_eq!(parent_path("tasks"), None);
    }

    #[test]
    fn ago_says_how_long_in_plain_words() {
        use super::ago;
        assert_eq!(ago(-5), "just now", "a clock set back");
        assert_eq!(ago(59), "just now");
        assert_eq!(ago(60), "1 minute ago");
        assert_eq!(ago(150), "2 minutes ago");
        assert_eq!(ago(3_600), "1 hour ago");
        assert_eq!(ago(3 * 86_400 + 5), "3 days ago");
    }

    use super::*;

    #[test]
    fn moves_only_to_adjacent_columns() {
        let w = Workflow::standard();
        assert!(w.next_to(Column::Backlog, Column::Todo));
        assert!(w.next_to(Column::Todo, Column::Backlog));
        assert!(w.next_to(Column::Doing, Column::Done));
        assert!(!w.next_to(Column::Backlog, Column::Doing));
        assert!(!w.next_to(Column::Backlog, Column::Done));
        assert!(!w.next_to(Column::Todo, Column::Todo));
        let mut team = Workflow::standard();
        let review = Column::new("review", Kind::Doing);
        team.columns.insert(
            3,
            ColumnInfo {
                column: review,
                label: "Review".into(),
                wip: Some(1),
            },
        );
        assert!(!team.allows(review, Column::Doing) || team.next_to(review, Column::Doing));
        team.moves.push((Column::Done, review));
        assert!(team.allows(Column::Done, review) && !team.allows(review, review));
        team.any = true;
        assert!(team.allows(Column::Backlog, Column::Done));
        assert!(team.next_to(Column::Doing, review) && team.next_to(review, Column::Done));
        assert!(!team.next_to(Column::Doing, Column::Done));
        assert_eq!(team.wip(review), Some(1));
        assert_eq!(team.label(review), "Review");
    }

    #[test]
    fn only_backlog_and_done_kinds_collapse() {
        let w = Workflow::standard();
        let collapsible: Vec<_> = w.columns().filter(|c| w.is_collapsible(*c)).collect();
        assert_eq!(collapsible, [Column::Backlog, Column::Done]);
    }

    #[test]
    fn a_column_travels_as_its_folder_or_folder_and_kind() {
        let json =
            serde_json::to_string(&[Column::Todo, Column::new("review", Kind::Doing)]).unwrap();
        assert_eq!(json, r#"["todo","review:doing"]"#);
        let back: Vec<Column> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, [Column::Todo, Column::new("review", Kind::Doing)]);
        assert!(serde_json::from_str::<Column>(r#""archive""#).is_err());
        assert_eq!(Column::from_dir_name("archive"), None);
    }

    #[test]
    fn reads_amd_frontmatter() {
        let ticket = "---\nid: \"001\"\ntitle: \"create a Tauri application board\"\n\
                      assignee: \"claude\"\nrelated: []\ntags: [board, ui]\n---\n\n## Notes\n";
        let fm = parse_frontmatter(ticket);
        assert_eq!(fm.id, "001");
        assert_eq!(fm.title, "create a Tauri application board");
        assert_eq!(fm.assignee, "claude");
        assert_eq!(fm.tags, ["board", "ui"]);
    }

    #[test]
    fn title_may_contain_a_colon() {
        let fm = parse_frontmatter("---\ntitle: \"Hello world: test\"\n---\n");
        assert_eq!(fm.title, "Hello world: test");
    }

    #[test]
    fn missing_frontmatter_is_empty() {
        assert_eq!(
            parse_frontmatter("# just a heading\n"),
            Frontmatter::default()
        );
        assert_eq!(
            parse_frontmatter("---\ntags: []\n---\n").tags,
            Vec::<String>::new()
        );
    }

    #[test]
    fn slugs_match_amd() {
        assert_eq!(slugify("Hello world: test"), "hello-world-test");
        assert_eq!(
            slugify("create a Tauri application board"),
            "create-a-tauri-application-board"
        );
        assert_eq!(slugify("  --Trim me!!  "), "trim-me");
    }
}

#[cfg(test)]
mod assigned_tests {
    use super::*;

    fn ticket(assignee: &str) -> TicketSummary {
        TicketSummary {
            repo: String::new(),
            file: "001-t.md".into(),
            column: Column::Todo,
            id: "001".into(),
            title: "T".into(),
            assignee: assignee.into(),
            assignees: Vec::new(),
            tags: Vec::new(),
            priority: String::new(),
            created: String::new(),
            epic: None,
            text: String::new(),
            epic_label: String::new(),
            points: String::new(),
            repository: String::new(),
            lock: None,
            lapsed: None,
            parent: String::new(),
            related: Vec::new(),
            clone_of: String::new(),
            clones: Vec::new(),
            links: Vec::new(),
            priority_from: String::new(),
            due: String::new(),
            recur: String::new(),
        }
    }

    #[test]
    fn a_user_sees_only_their_tickets_and_all_sees_everyone() {
        let mine = ticket("mrdoodles");
        let theirs = ticket("claude");
        let nobody = ticket("");
        let me = Assigned::User("mrdoodles".into());
        assert!(me.matches(&mine) && !me.matches(&theirs) && !me.matches(&nobody));
        assert!(
            [&mine, &theirs, &nobody]
                .iter()
                .all(|t| Assigned::Anyone.matches(t))
        );
        assert!(Assigned::Nobody.matches(&nobody) && !Assigned::Nobody.matches(&mine));
    }

    #[test]
    fn filter_keys_round_trip() {
        for a in [
            Assigned::Anyone,
            Assigned::Nobody,
            Assigned::User("claude".into()),
        ] {
            assert_eq!(Assigned::from_key(&a.key()), a);
        }
    }

    #[test]
    fn relationship_entries_name_a_ticket_number() {
        assert_eq!(ticket_number("042"), Some(42));
        assert_eq!(ticket_number("\"150\""), Some(150));
        assert_eq!(ticket_number("[[007-some-title]]"), Some(7));
        assert_eq!(ticket_number(""), None);
        assert_eq!(ticket_number("abc"), None);
    }

    #[test]
    fn assignees_are_listed_once_in_order() {
        let mut board = Board {
            project: String::new(),
            shown: String::new(),
            tasks_dir: String::new(),
            repos: Vec::new(),
            workflow: Workflow::standard(),
            columns: vec![BoardColumn {
                column: Column::Todo,
                tickets: vec![
                    ticket("mrdoodles"),
                    ticket(""),
                    ticket("claude"),
                    ticket("mrdoodles"),
                ],
            }],
            epics: Vec::new(),
            projects: Vec::new(),
            ticket_repos: Vec::new(),
            users: Vec::new(),
            me: None,
            tags: Vec::new(),
            tags_only: false,
            admin: AdminAccess::default(),
            shared_documents: String::new(),
            import_issues: true,
            auto_priority: false,
            time_limits: (0, 0),
            due_to_todo_days: 3,
            backlog_always_shown: false,
            needs_version: None,
            priority_override: true,
            archived: false,
        };
        board.epics.push(Epic {
            repo: String::new(),
            name: "E".into(),
            kind: epic::GroupKind::Epic,
            days: 0,
            started: false,
            tickets: vec![ticket("Alice")],
            closed: false,
            unfinished: 1,
        });
        assert_eq!(board.assignees(), ["Alice", "claude", "mrdoodles"]);
    }

    #[test]
    fn a_doc_scope_holds_its_folder_only() {
        let adr = DocScope {
            shared: false,
            folder: "adr".into(),
        };
        assert!(adr.holds("adr/adr/0001-x.md"));
        assert!(adr.holds("adr/rfc/0002-y.md"));
        assert!(!adr.holds("adrift.md"));
        assert!(!adr.holds("docs/adr.md"));
        assert!(DocScope::default().holds("anything/at/all.md"));
    }
}

#[cfg(test)]
mod search_tests {
    use super::*;

    fn ticket(title: &str, text: &str, tags: &[&str]) -> TicketSummary {
        TicketSummary {
            repo: String::new(),
            file: "001-t.md".into(),
            column: Column::Todo,
            id: "001".into(),
            title: title.into(),
            assignee: String::new(),
            assignees: Vec::new(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            priority: String::new(),
            created: String::new(),
            epic: None,
            text: text.into(),
            epic_label: String::new(),
            points: String::new(),
            repository: String::new(),
            lock: None,
            lapsed: None,
            parent: String::new(),
            related: Vec::new(),
            clone_of: String::new(),
            clones: Vec::new(),
            links: Vec::new(),
            priority_from: String::new(),
            due: String::new(),
            recur: String::new(),
        }
    }

    #[test]
    fn an_empty_query_and_lone_markers_match_everything() {
        let t = ticket("Sort the board", "", &["UX"]);
        for query in ["", "   ", "-", "#", "-#", "- # -#"] {
            assert!(t.matches(query), "{query:?}");
        }
    }

    #[test]
    fn plain_words_search_text_ignoring_case_and_never_tags() {
        let tagged = ticket("Sort the board", "", &["TRIAGE"]);
        let worded = ticket("Triage the board", "", &[]);
        assert!(!tagged.matches("triage") && !tagged.matches("TRIAGE"));
        assert!(worded.matches("triage") && worded.matches("TRIAGE"));
        assert!(worded.matches("board TRIAGE"));
        assert!(!worded.matches("board nothing"));
    }

    #[test]
    fn a_dash_word_leaves_out_tickets_whose_text_has_it() {
        let a = ticket("Future release", "", &[]);
        let b = ticket("Business case", "a future plan", &[]);
        let c = ticket("Other", "", &[]);
        assert!(!a.matches("-FUTURE") && !b.matches("-future") && c.matches("-future"));
        assert!(b.matches("business -release") && !a.matches("release -future"));
    }

    #[test]
    fn a_hash_tag_matches_tags_that_start_with_it_ignoring_case() {
        let t = ticket("T", "", &["TRIAGE", "AMDUI"]);
        for query in ["#TRIAGE", "#triage", "#TRI", "#t", "#AMD"] {
            assert!(t.matches(query), "{query}");
        }
        assert!(!t.matches("#UI") && !t.matches("#TRIAGED"));
    }

    #[test]
    fn a_tag_term_is_not_found_in_the_text() {
        let t = ticket("Future release", "all about the future", &["CLI"]);
        assert!(!t.matches("#FUTURE") && t.matches("-#FUTURE"));
    }

    #[test]
    fn dash_hash_leaves_out_tagged_tickets() {
        let tagged = ticket("T", "", &["BUSINESS"]);
        let other = ticket("T", "", &["CLI"]);
        let bare = ticket("T", "", &[]);
        assert!(!tagged.matches("-#BUS") && other.matches("-#BUS") && bare.matches("-#BUS"));
    }

    #[test]
    fn several_included_tags_need_just_one_and_exclusions_still_apply() {
        let future = ticket("T", "", &["FUTURE"]);
        let business = ticket("T", "", &["BUSINESS"]);
        let both = ticket("T", "", &["FUTURE", "BUSINESS"]);
        let neither = ticket("T", "", &["CLI"]);
        let query = "#FUTURE #BUSINESS";
        assert!(future.matches(query) && business.matches(query) && both.matches(query));
        assert!(!neither.matches(query));
        let narrowed = "#FUTURE -#BUSINESS";
        assert!(future.matches(narrowed) && !both.matches(narrowed) && !business.matches(narrowed));
    }

    #[test]
    fn tags_and_text_combine() {
        let t = ticket("Search the board", "", &["AMDUI"]);
        assert!(t.matches("#AMDUI search") && t.matches("search #AMDUI"));
        assert!(!t.matches("#CLI search") && !t.matches("#AMDUI -search"));
    }

    #[test]
    fn an_old_lowercase_tag_still_matches() {
        let t = ticket("T", "", &["cli"]);
        assert!(t.matches("#CLI") && !t.matches("-#cli"));
    }

    #[test]
    fn a_tag_term_being_typed_is_the_last_word_starting_with_a_hash() {
        assert_eq!(tag_being_typed("#t"), Some(("", "T".into())));
        assert_eq!(tag_being_typed("#"), Some(("", String::new())));
        assert_eq!(tag_being_typed("board -#tr"), Some(("-", "TR".into())));
        assert_eq!(tag_being_typed("-#"), Some(("-", String::new())));
        for query in ["", "board", "-board", "#TRIAGE ", "#a b", "a#b"] {
            assert_eq!(tag_being_typed(query), None, "{query:?}");
        }
    }

    #[test]
    fn choosing_a_tag_finishes_the_term_and_keeps_the_rest() {
        assert_eq!(complete_tag("#t", "triage"), "#TRIAGE ");
        assert_eq!(complete_tag("search -#am", "AMDUI"), "search -#AMDUI ");
        assert_eq!(complete_tag("search", "AMDUI"), "search");
    }
}

#[cfg(test)]
mod link_tests {
    use super::*;

    fn ticket(id: &str, title: &str) -> TicketSummary {
        TicketSummary {
            repo: String::new(),
            repository: String::new(),
            file: format!("{id}-{}.md", slugify(title)),
            column: Column::Todo,
            id: id.into(),
            title: title.into(),
            assignee: String::new(),
            assignees: Vec::new(),
            tags: Vec::new(),
            priority: String::new(),
            created: String::new(),
            epic: None,
            text: String::new(),
            epic_label: String::new(),
            points: String::new(),
            lock: None,
            lapsed: None,
            parent: String::new(),
            related: Vec::new(),
            clone_of: String::new(),
            clones: Vec::new(),
            links: Vec::new(),
            priority_from: String::new(),
            due: String::new(),
            recur: String::new(),
        }
    }

    fn ids(found: &[TicketSummary]) -> Vec<&str> {
        found.iter().map(|t| t.id.as_str()).collect()
    }

    #[test]
    fn a_number_finds_that_ticket_first_then_the_ones_it_starts() {
        let all = [
            ticket("012", "Twelve"),
            ticket("120", "Hundred twenty"),
            ticket("021", "Other"),
            ticket("001", "One"),
        ];
        assert_eq!(ids(&link_candidates(&all, None, "12", 8)), ["012", "120"]);
        assert_eq!(ids(&link_candidates(&all, None, "#012", 8)), ["012", "120"]);
    }

    #[test]
    fn words_find_a_title_in_any_case_and_all_must_appear() {
        let all = [
            ticket("001", "Enable linking of tickets"),
            ticket("002", "Ticket search bug"),
        ];
        assert_eq!(
            ids(&link_candidates(&all, None, "TICKET", 8)),
            ["001", "002"]
        );
        assert_eq!(
            ids(&link_candidates(&all, None, "search ticket", 8)),
            ["002"]
        );
        assert!(link_candidates(&all, None, "nothing", 8).is_empty());
    }

    #[test]
    fn the_ticket_itself_and_an_empty_query_offer_nothing() {
        let all = [ticket("001", "Alpha"), ticket("002", "Alpha two")];
        assert_eq!(ids(&link_candidates(&all, Some(1), "alpha", 8)), ["002"]);
        assert!(link_candidates(&all, Some(1), "  ", 8).is_empty());
    }

    #[test]
    fn the_list_is_cut_at_the_limit() {
        let all: Vec<_> = (1..=5)
            .map(|n| ticket(&format!("{n:03}"), "Same"))
            .collect();
        assert_eq!(link_candidates(&all, None, "same", 3).len(), 3);
    }

    #[test]
    fn a_links_entry_splits_and_unknown_kinds_keep_their_slug() {
        assert_eq!(split_link("requires:012"), Some(("requires", "012")));
        assert_eq!(split_link("requires:"), None);
        assert_eq!(link_label("required-by"), "Required by");
        assert_eq!(link_label("mystery"), "mystery");
    }
}
