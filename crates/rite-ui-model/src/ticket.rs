//! Splitting a ticket into frontmatter and body, and editing frontmatter
//! fields without disturbing anything else in the file.

use crate::{Frontmatter, parse_frontmatter, slugify};

/// The branch types `rite` accepts; empty means the ticket has no branch.
pub const BRANCH_TYPES: [&str; 5] = ["feature", "bugfix", "hotfix", "release", "chore"];

/// Puts tickets with a due date first, soonest first, keeping the rest in
/// the order they came (ticket 253): the backlog's order.
pub fn due_first(list: &mut [crate::TicketSummary]) {
    list.sort_by(|a, b| match (a.due.trim(), b.due.trim()) {
        ("", "") => std::cmp::Ordering::Equal,
        ("", _) => std::cmp::Ordering::Greater,
        (_, "") => std::cmp::Ordering::Less,
        (x, y) => x.cmp(y),
    });
}

/// The branch type that says "no branch, on purpose" (ticket 066), as
/// `rite_lib::branch::NO_BRANCH`.
pub const NO_BRANCH: &str = "none";

/// Why a ticket's repository and branch type don't go together on a
/// tickets board (ticket 066), worded as `rite_lib::pairing::mismatch`
/// (rite-ui's tests check they agree).
pub fn pairing_mismatch(repository: &str, branch_type: &str) -> Option<String> {
    let repository = repository.trim();
    let kind = branch_type.trim();
    if !repository.is_empty() && kind.is_empty() {
        Some(format!(
            "it names a repository ({repository}) but no branch type: choose one, or `{NO_BRANCH}` for no branch"
        ))
    } else if repository.is_empty() && !kind.is_empty() && kind != NO_BRANCH {
        Some(format!(
            "it has a branch type ({kind}) but no repository for its branch to go in"
        ))
    } else {
        None
    }
}

/// Every valid priority, lowest first — the order the editor lists them.
pub const PRIORITIES: [&str; 4] = ["low", "medium", "high", "top"];

/// The sizes `rite` accepts for `points` (empty means unsized).
pub const VALID_POINTS: [&str; 6] = ["1", "2", "3", "5", "8", "13"];

/// What a ticket without a priority gets when it is saved.
pub const DEFAULT_PRIORITY: &str = "low";

/// How urgent a priority is, for ordering: top 3 … low 0. A missing
/// (or unrecognised) priority ranks as low, the value it gets on save.
pub fn priority_rank(priority: &str) -> u8 {
    match priority.trim() {
        "top" => 3,
        "high" => 2,
        "medium" => 1,
        _ => 0,
    }
}

/// The board's default order within a column: highest priority first, then
/// oldest first. Age is the `created` date, then the ticket number — ids
/// are handed out in creation order, so they order tickets created on the
/// same day. A ticket without a date sorts after dated ones.
pub fn default_order(a: &crate::TicketSummary, b: &crate::TicketSummary) -> std::cmp::Ordering {
    let undated = |t: &crate::TicketSummary| t.created.trim().is_empty();
    priority_rank(&b.priority)
        .cmp(&priority_rank(&a.priority))
        .then_with(|| undated(a).cmp(&undated(b)))
        .then_with(|| a.created.trim().cmp(b.created.trim()))
        .then_with(|| id_number(&a.id).cmp(&id_number(&b.id)))
        .then_with(|| a.file.cmp(&b.file))
}

/// The orders a user can pick for the board's columns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SortOrder {
    /// [`default_order`]: highest priority first, then oldest first.
    #[default]
    Default,
    /// Newest first, so older tickets sink to the bottom; priority ignored.
    YoungestFirst,
    /// Lowest priority first, then oldest first within a priority.
    LowestFirst,
    /// What's needed first (ticket 230): a ticket comes before the tickets
    /// in its column that require it; otherwise highest priority, then
    /// oldest. The default on a board with auto priority on.
    Automatic,
}

impl SortOrder {
    pub const ALL: [SortOrder; 4] = [
        SortOrder::Automatic,
        SortOrder::Default,
        SortOrder::YoungestFirst,
        SortOrder::LowestFirst,
    ];

    /// What the dropdown shows.
    pub fn label(self) -> &'static str {
        match self {
            SortOrder::Default => "Priority, then oldest",
            SortOrder::YoungestFirst => "Youngest first",
            SortOrder::LowestFirst => "Lowest first",
            SortOrder::Automatic => "Automatic: what's needed first",
        }
    }

    /// A stable name for the option's `value`.
    pub fn key(self) -> &'static str {
        match self {
            SortOrder::Default => "default",
            SortOrder::YoungestFirst => "youngest",
            SortOrder::LowestFirst => "lowest",
            SortOrder::Automatic => "automatic",
        }
    }

    pub fn from_key(key: &str) -> SortOrder {
        SortOrder::ALL
            .into_iter()
            .find(|o| o.key() == key)
            .unwrap_or_default()
    }

    /// Puts `list` in this order. [`SortOrder::Automatic`] is more than a
    /// comparison: a ticket goes after every ticket in the list it
    /// requires (on its own board), the rest by priority then age; a cycle
    /// is broken at its highest-priority, oldest ticket.
    pub fn sort(self, list: &mut Vec<crate::TicketSummary>) {
        list.sort_by(|a, b| self.compare(a, b));
        if self != SortOrder::Automatic {
            return;
        }
        let needs: Vec<Vec<usize>> = list
            .iter()
            .map(|t| {
                crate::waiting::requires(t, list.iter())
                    .into_iter()
                    .filter(|r| r.repo == t.repo)
                    .filter_map(|r| {
                        list.iter()
                            .position(|o| o.file == r.file && o.repo == r.repo)
                    })
                    .collect()
            })
            .collect();
        let mut placed = vec![false; list.len()];
        let mut order = Vec::with_capacity(list.len());
        while order.len() < list.len() {
            let next = (0..list.len())
                .find(|&i| !placed[i] && needs[i].iter().all(|&j| placed[j] || j == i))
                .or_else(|| (0..list.len()).find(|&i| !placed[i]))
                .unwrap_or(0);
            placed[next] = true;
            order.push(next);
        }
        let mut taken: Vec<Option<crate::TicketSummary>> = list.drain(..).map(Some).collect();
        list.extend(order.into_iter().filter_map(|i| taken[i].take()));
    }

    /// Compares two tickets in this order ([`SortOrder::Automatic`]
    /// compares as the default; its dependency order is [`sort`](Self::sort)).
    pub fn compare(self, a: &crate::TicketSummary, b: &crate::TicketSummary) -> std::cmp::Ordering {
        match self {
            SortOrder::Default | SortOrder::Automatic => default_order(a, b),
            SortOrder::YoungestFirst => age(b, a)
                // A ticket without a date has no age to compare; keep it last.
                .then_with(|| undated(a).cmp(&undated(b))),
            SortOrder::LowestFirst => priority_rank(&a.priority)
                .cmp(&priority_rank(&b.priority))
                .then_with(|| undated(a).cmp(&undated(b)))
                .then_with(|| age(a, b)),
        }
    }
}

fn undated(t: &crate::TicketSummary) -> bool {
    t.created.trim().is_empty()
}

/// Older first: by created date, then ticket number, then file name.
/// (Callers place undated tickets themselves.)
fn age(a: &crate::TicketSummary, b: &crate::TicketSummary) -> std::cmp::Ordering {
    a.created
        .trim()
        .cmp(b.created.trim())
        .then_with(|| id_number(&a.id).cmp(&id_number(&b.id)))
        .then_with(|| a.file.cmp(&b.file))
}

/// The numeric part of an id such as `"007"`; ids that are not numbers sort last.
fn id_number(id: &str) -> u64 {
    id.trim().parse().unwrap_or(u64::MAX)
}

/// The frontmatter fields the rich editor shows as a form.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fields {
    pub title: String,
    /// Everyone it's assigned to, the first the main one (ticket 254).
    pub assignees: Vec<String>,
    /// The repository it's about, a bare name; empty for none.
    pub repository: String,
    pub tags: Vec<String>,
    pub branch_type: String,
    pub priority: String,
    pub points: String,
    /// `YYYY-MM-DD` or empty, and how it repeats (ticket 143).
    pub due: String,
    pub recur: String,
}

impl From<&Frontmatter> for Fields {
    fn from(fm: &Frontmatter) -> Fields {
        Fields {
            title: fm.title.clone(),
            assignees: fm.assignees.clone(),
            repository: fm.repository.clone(),
            tags: fm.tags.clone(),
            branch_type: fm.branch_type.clone(),
            priority: fm.priority.clone(),
            points: fm.points.clone(),
            due: fm.due.clone(),
            recur: fm.recur.clone(),
        }
    }
}

/// A ticket's text cut into its `---` frontmatter block (delimiters and the
/// line break after the closing one included) and the body after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parts<'a> {
    pub frontmatter: Option<&'a str>,
    pub body: &'a str,
}

pub fn split(content: &str) -> Parts<'_> {
    let none = Parts {
        frontmatter: None,
        body: content,
    };
    let Some(rest) = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))
    else {
        return none;
    };
    let mut offset = content.len() - rest.len();
    for line in rest.split_inclusive('\n') {
        offset += line.len();
        if line.trim_end() == "---" {
            return Parts {
                frontmatter: Some(&content[..offset]),
                body: &content[offset..],
            };
        }
    }
    none
}

/// Applies `new` to `content`, rewriting only the frontmatter lines whose
/// value changed. Changing the branch type also sets `branch-name` to
/// `<type>/<title slug>` (or clears it), as `rite` names branches.
pub fn apply_fields(content: &str, new: &Fields) -> String {
    let parts = split(content);
    let Some(frontmatter) = parts.frontmatter else {
        return content.to_string();
    };
    let old = Fields::from(&parse_frontmatter(content));
    let mut fm = frontmatter.to_string();
    if new.title != old.title {
        fm = set_line(&fm, "title", &quote(&new.title));
    }
    if new.assignees != old.assignees {
        // As rite writes it: one person is `assignee` alone; several keep
        // `assignee` as the first and add the list (ticket 254).
        let people = people(&new.assignees);
        fm = set_line(
            &fm,
            "assignee",
            &quote(people.first().map(String::as_str).unwrap_or("")),
        );
        fm = if people.len() > 1 {
            let list: Vec<String> = people.iter().map(|who| quote(who)).collect();
            set_line(&fm, "assignees", &format!("[{}]", list.join(",")))
        } else {
            remove_line(&fm, "assignees")
        };
    }
    if new.repository != old.repository {
        fm = set_line(&fm, "repository", &quote(&new.repository));
    }
    if new.tags != old.tags {
        fm = set_line(&fm, "tags", &format!("[{}]", new.tags.join(",")));
    }
    if new.points.trim() != old.points.trim() {
        fm = set_line(&fm, "points", &quote(new.points.trim()));
    }
    if new.priority != old.priority {
        fm = set_line(&fm, "priority", &quote(&new.priority));
    }
    if new.due.trim() != old.due.trim() {
        fm = set_line(&fm, "due", &quote(new.due.trim()));
    }
    if new.recur.trim() != old.recur.trim() {
        fm = set_line(&fm, "recur", &quote(new.recur.trim()));
    }
    if new.branch_type != old.branch_type {
        fm = set_line(&fm, "branch-type", &quote(&new.branch_type));
        let name = if new.branch_type.is_empty() || new.branch_type == NO_BRANCH {
            String::new()
        } else {
            format!("{}/{}", new.branch_type, slugify(&new.title))
        };
        fm = set_line(&fm, "branch-name", &quote(&name));
    }
    fm + parts.body
}

/// Checks a ticket's priority before it is written: a missing or empty one
/// becomes [`DEFAULT_PRIORITY`], a valid one is left exactly as it is, and
/// anything else is refused with a message the user can act on.
pub fn ensure_priority(content: &str) -> Result<String, String> {
    let priority = parse_frontmatter(content).priority;
    let priority = priority.trim();
    // `auto` hands the ticket back to auto priority (ticket 215); rite-lib
    // turns it into a priority as it saves.
    if PRIORITIES.contains(&priority) || priority == "auto" {
        return Ok(content.to_string());
    }
    if !priority.is_empty() {
        return Err(format!(
            "\"{priority}\" is not a valid priority. Choose one of: {}.",
            PRIORITIES.join(", ")
        ));
    }
    let line = format!("priority: {}", quote(DEFAULT_PRIORITY));
    Ok(match split(content).frontmatter {
        Some(fm) => set_line(fm, "priority", &quote(DEFAULT_PRIORITY)) + split(content).body,
        None => format!("---\n{line}\n---\n{content}"),
    })
}

/// Sets (or, with `""`, clears) the ticket's `epic:` field, as `rite set
/// <ref> epic` does. A ticket without frontmatter is returned unchanged.
pub fn with_epic(content: &str, epic: &str) -> String {
    set_value(content, "epic", epic)
}

/// Sets one frontmatter value (as a quoted scalar), leaving every other line
/// as it is; also used for an epic's `_group.md`, which has the same shape.
/// Content without frontmatter is returned unchanged.
pub fn set_value(content: &str, key: &str, value: &str) -> String {
    let parts = split(content);
    match parts.frontmatter {
        Some(fm) => set_line(fm, key, &quote(value)) + parts.body,
        None => content.to_string(),
    }
}

/// Splits a comma-separated tag field into tags.
pub fn parse_tags(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(String::from)
        .collect()
}

/// A YAML double-quoted scalar, written the way `rite`'s `yaml` filter does.
fn quote(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `names` trimmed, empty ones dropped, each once (ignoring case).
pub fn people(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for who in names {
        let who = who.trim();
        if !who.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(who)) {
            out.push(who.to_string());
        }
    }
    out
}

/// Takes the `key:` line out of a frontmatter block, if it's there.
fn remove_line(frontmatter: &str, key: &str) -> String {
    frontmatter
        .split_inclusive('\n')
        .enumerate()
        .filter(|(i, l)| *i == 0 || l.split_once(':').is_none_or(|(k, _)| k.trim() != key))
        .map(|(_, l)| l)
        .collect()
}

/// Replaces the `key:` line inside a frontmatter block, or adds one before
/// the closing `---` if the ticket has no such key.
fn set_line(frontmatter: &str, key: &str, value: &str) -> String {
    let line = format!("{key}: {value}");
    let mut lines: Vec<&str> = frontmatter.split_inclusive('\n').collect();
    let is_key = |l: &str| l.split_once(':').is_some_and(|(k, _)| k.trim() == key);
    // Index 0 is the opening ---; the last line is the closing one.
    let last = lines.len() - 1;
    let newline = if lines[last].ends_with("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let owned = format!("{line}{newline}");
    match (1..last).find(|&i| is_key(lines[i])) {
        Some(i) => lines[i] = &owned,
        None => lines.insert(last, &owned),
    }
    lines.concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICKET: &str = "---\nid: \"002\"\ntitle: \"Old title\"\nassignee: \"\"\n\
                          branch-type: \"\"\nbranch-name: \"\"\ntags: [editor]\n\
                          locked-until: \"2026-10-03T19:24:39Z\"\n---\n\n## Notes\nBody\n";

    #[test]
    fn splits_frontmatter_from_body() {
        let parts = split(TICKET);
        assert!(parts.frontmatter.unwrap().ends_with("---\n"));
        assert_eq!(parts.body, "\n## Notes\nBody\n");
        assert_eq!(split("no frontmatter\n").frontmatter, None);
        assert_eq!(split("---\nunterminated\n").frontmatter, None);
    }

    #[test]
    fn unchanged_fields_leave_the_file_byte_identical() {
        let fields = Fields::from(&parse_frontmatter(TICKET));
        assert_eq!(apply_fields(TICKET, &fields), TICKET);
    }

    #[test]
    fn rewrites_only_changed_lines() {
        let mut fields = Fields::from(&parse_frontmatter(TICKET));
        fields.title = "Say \"hi\"".into();
        fields.tags = vec!["editor".into(), "ui".into()];
        let out = apply_fields(TICKET, &fields);
        assert!(out.contains("title: \"Say \\\"hi\\\"\"\n"), "{out}");
        assert!(out.contains("tags: [editor,ui]\n"), "{out}");
        assert!(out.contains("locked-until: \"2026-10-03T19:24:39Z\"\n"));
        assert!(out.ends_with("---\n\n## Notes\nBody\n"));
        assert_eq!(parse_frontmatter(&out).title, "Say \"hi\"");
    }

    #[test]
    fn due_dates_come_first_soonest_first_and_the_rest_keep_their_order() {
        let mut list = vec![
            card("001", "high", "2026-01-01"),
            card("002", "low", "2026-01-02"),
            card("003", "low", "2026-01-03"),
            card("004", "low", "2026-01-04"),
        ];
        list[1].due = "2026-11-20".into();
        list[3].due = "2026-11-01".into();
        due_first(&mut list);
        let ids: Vec<&str> = list.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["004", "002", "001", "003"]);
    }

    #[test]
    fn branch_type_sets_branch_name() {
        let mut fields = Fields::from(&parse_frontmatter(TICKET));
        fields.branch_type = "feature".into();
        let out = apply_fields(TICKET, &fields);
        assert!(
            out.contains("branch-type: \"feature\"\nbranch-name: \"feature/old-title\"\n"),
            "{out}"
        );
        fields.branch_type.clear();
        let cleared = apply_fields(&out, &fields);
        assert!(
            cleared.contains("branch-type: \"\"\nbranch-name: \"\"\n"),
            "{cleared}"
        );
    }

    #[test]
    fn the_repository_round_trips_and_is_normalised() {
        let ticket = "---\nid: \"009\"\ntitle: \"T\"\nrepository: \"acme/amd\"\n---\nBody\n";
        let mut fields = Fields::from(&crate::parse_frontmatter(ticket));
        assert_eq!(fields.repository, "amd");
        fields.repository = "another-repository".into();
        let saved = apply_fields(ticket, &fields);
        assert!(
            saved.contains("\nrepository: \"another-repository\"\n"),
            "{saved}"
        );
        fields.repository.clear();
        assert!(apply_fields(&saved, &fields).contains("\nrepository: \"\"\n"));
    }

    #[test]
    fn adds_a_missing_key() {
        let ticket = "---\nid: \"009\"\ntitle: \"T\"\n---\nBody\n";
        let fields = Fields {
            title: "T".into(),
            assignees: vec!["claude".into()],
            ..Fields::default()
        };
        assert_eq!(
            apply_fields(ticket, &fields),
            "---\nid: \"009\"\ntitle: \"T\"\nassignee: \"claude\"\n---\nBody\n"
        );
    }

    #[test]
    fn several_people_are_written_as_amd_writes_them() {
        let ticket = "---\nid: \"009\"\ntitle: \"T\"\nassignee: \"\"\n---\nBody\n";
        let mut fields = Fields::from(&crate::parse_frontmatter(ticket));
        fields.assignees = vec!["Sam".into(), "Jo, Jr".into(), "sam".into()];
        let two = apply_fields(ticket, &fields);
        assert_eq!(
            two,
            "---\nid: \"009\"\ntitle: \"T\"\nassignee: \"Sam\"\nassignees: [\"Sam\",\"Jo, Jr\"]\n---\nBody\n"
        );
        let read = crate::parse_frontmatter(&two);
        assert_eq!(read.assignees, ["Sam", "Jo, Jr"]);
        fields.assignees = vec!["Jo, Jr".into()];
        assert_eq!(
            apply_fields(&two, &fields),
            "---\nid: \"009\"\ntitle: \"T\"\nassignee: \"Jo, Jr\"\n---\nBody\n"
        );
        // A list an older build left behind, no longer starting with
        // `assignee`, isn't believed.
        let stale =
            crate::parse_frontmatter("---\nassignee: \"Ann\"\nassignees: [\"Sam\",\"Jo\"]\n---\n");
        assert_eq!(stale.assignees, ["Ann"]);
    }

    #[test]
    fn the_user_filter_matches_any_of_them_and_the_card_shortens() {
        let mut card = crate::TicketSummary {
            assignee: "Alex".into(),
            assignees: vec!["Alex".into(), "Sam".into()],
            ..Default::default()
        };
        assert!(crate::Assigned::User("Sam".into()).matches(&card));
        assert!(!crate::Assigned::Nobody.matches(&card));
        assert_eq!(card.people_label().as_deref(), Some("@Alex +1"));
        card.assignees.clear();
        assert_eq!(card.people_label().as_deref(), Some("@Alex"));
        card.assignee.clear();
        assert!(crate::Assigned::Nobody.matches(&card));
        assert_eq!(card.people_label(), None);
    }

    #[test]
    fn missing_priority_becomes_low() {
        let out = ensure_priority(TICKET).unwrap();
        assert_eq!(parse_frontmatter(&out).priority, "low");
        assert!(
            out.contains("priority: \"low\"\n---\n\n## Notes\nBody\n"),
            "{out}"
        );
        // Everything else is untouched.
        assert_eq!(out.replace("priority: \"low\"\n", ""), TICKET);
    }

    #[test]
    fn empty_priority_becomes_low_in_place() {
        let ticket = "---\nid: \"1\"\npriority: \"\"\ntags: []\n---\n";
        assert_eq!(
            ensure_priority(ticket).unwrap(),
            "---\nid: \"1\"\npriority: \"low\"\ntags: []\n---\n"
        );
    }

    #[test]
    fn valid_priorities_are_left_alone() {
        for p in PRIORITIES {
            let ticket = format!("---\nid: \"1\"\npriority: {p}\n---\nBody\n");
            assert_eq!(ensure_priority(&ticket).unwrap(), ticket);
        }
    }

    #[test]
    fn invalid_priorities_are_refused() {
        for bad in ["urgent", "High", "lowest"] {
            let ticket = format!("---\npriority: \"{bad}\"\n---\n");
            let err = ensure_priority(&ticket).unwrap_err();
            assert!(
                err.contains(&format!("\"{bad}\" is not a valid priority")),
                "{err}"
            );
            assert!(err.contains("low, medium, high"), "{err}");
        }
    }

    #[test]
    fn a_ticket_without_frontmatter_gains_one() {
        assert_eq!(
            ensure_priority("Just text\n").unwrap(),
            "---\npriority: \"low\"\n---\nJust text\n"
        );
    }

    #[test]
    fn priority_field_edits_one_line() {
        let ticket = ensure_priority(TICKET).unwrap();
        let mut fields = Fields::from(&parse_frontmatter(&ticket));
        fields.priority = "high".into();
        let out = apply_fields(&ticket, &fields);
        assert_eq!(
            out,
            ticket.replace("priority: \"low\"", "priority: \"high\"")
        );
    }

    fn card(id: &str, priority: &str, created: &str) -> crate::TicketSummary {
        crate::TicketSummary {
            repo: String::new(),
            file: format!("{id}-t.md"),
            column: crate::Column::Todo,
            id: id.into(),
            title: format!("T{id}"),
            assignee: String::new(),
            assignees: Vec::new(),
            tags: Vec::new(),
            priority: priority.into(),
            created: created.into(),
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

    fn order(mut cards: Vec<crate::TicketSummary>) -> Vec<String> {
        cards.sort_by(default_order);
        cards.into_iter().map(|c| c.id).collect()
    }

    #[test]
    fn higher_priority_comes_first() {
        let cards = vec![
            card("001", "low", "2026-01-01"),
            card("002", "high", "2026-03-01"),
            card("003", "medium", "2026-02-01"),
            card("004", "high", "2026-04-01"),
        ];
        assert_eq!(order(cards), ["002", "004", "003", "001"]);
    }

    #[test]
    fn same_priority_oldest_first_by_date_then_id() {
        let cards = vec![
            card("009", "high", "2026-10-03"),
            card("012", "high", "2026-09-01"),
            card("007", "high", "2026-10-03"),
        ];
        assert_eq!(order(cards), ["012", "007", "009"]);
    }

    #[test]
    fn missing_priority_ranks_as_low() {
        let cards = vec![
            card("002", "low", "2026-10-03"),
            card("001", "", "2026-10-03"),
            card("003", "medium", "2026-10-03"),
        ];
        assert_eq!(order(cards), ["003", "001", "002"]);
    }

    fn sorted(order: SortOrder, mut cards: Vec<crate::TicketSummary>) -> Vec<String> {
        cards.sort_by(|a, b| order.compare(a, b));
        cards.into_iter().map(|c| c.id).collect()
    }

    fn sample() -> Vec<crate::TicketSummary> {
        vec![
            card("001", "high", "2026-09-01"),
            card("002", "low", "2026-09-15"),
            card("003", "high", "2026-10-03"),
            card("004", "low", "2026-10-03"),
            card("005", "medium", "2026-10-03"),
        ]
    }

    #[test]
    fn youngest_first_puts_older_tickets_at_the_bottom() {
        // Newest date first; same day, higher number (made later) first.
        assert_eq!(
            sorted(SortOrder::YoungestFirst, sample()),
            ["005", "004", "003", "002", "001"]
        );
        let with_undated = vec![card("009", "low", ""), card("001", "low", "2026-01-01")];
        assert_eq!(
            sorted(SortOrder::YoungestFirst, with_undated),
            ["001", "009"]
        );
    }

    #[test]
    fn lowest_first_puts_lower_priority_at_the_top() {
        // Low (oldest first), then medium, then high.
        assert_eq!(
            sorted(SortOrder::LowestFirst, sample()),
            ["002", "004", "005", "001", "003"]
        );
    }

    #[test]
    fn default_sort_is_the_board_default() {
        assert_eq!(
            sorted(SortOrder::Default, sample()),
            ["001", "003", "005", "002", "004"]
        );
    }

    #[test]
    fn sort_keys_round_trip() {
        for order in SortOrder::ALL {
            assert_eq!(SortOrder::from_key(order.key()), order);
        }
        assert_eq!(SortOrder::from_key("nonsense"), SortOrder::Default);
    }

    #[test]
    fn search_finds_the_priority_filter_ticket_by_its_file_name() {
        // Ticket 005's title now says "sort", but its file still says "priority filter".
        let mut t = card("005", "low", "2026-10-03");
        t.title = "Add a sort filter dropdown next to the new ticket button on the board".into();
        t.file = "005-add-a-priority-filter-dropdown-next-to-the-new-ticket-button-on-the-board.md"
            .into();
        assert!(t.matches("priority"));
        assert!(t.matches("Priority Filter"));
        assert!(t.matches("  sort   dropdown "));
        assert!(!t.matches("priority search"), "every word must match");
        assert!(t.matches(""));
    }

    #[test]
    fn search_covers_body_assignee_and_epic_and_tags_only_with_a_hash() {
        let mut t = card("042", "high", "2026-10-03");
        t.text = "## Notes\nThe colour scheme should follow the OS theme.".into();
        t.tags = vec!["ui".into()];
        t.assignee = "mrdoodles".into();
        t.epic = Some("UserPreferences".into());
        for q in [
            "os theme",
            "mrdoodles",
            "userpref",
            "high",
            "042",
            "#UI",
            "#ui",
        ] {
            assert!(t.matches(q), "{q}");
        }
        // Tags are found with a #, never as text (ticket 123).
        assert!(!t.matches("database") && !t.matches("UI") && !t.matches("-#UI"));
    }

    #[test]
    fn undated_tickets_follow_dated_ones() {
        let cards = vec![card("001", "low", ""), card("002", "low", "2026-10-03")];
        assert_eq!(order(cards), ["002", "001"]);
    }

    #[test]
    fn epic_field_is_set_and_cleared() {
        let filed = with_epic(TICKET, "Board features");
        assert!(filed.contains("epic: \"Board features\"\n---\n"), "{filed}");
        let back = with_epic(&filed, "");
        assert!(back.contains("epic: \"\"\n---\n"), "{back}");
        assert_eq!(back.replace("epic: \"\"\n", ""), TICKET);
    }

    #[test]
    fn tags_parse_from_a_comma_list() {
        assert_eq!(parse_tags(" editor, ui ,,board"), ["editor", "ui", "board"]);
        assert!(parse_tags("  ").is_empty());
    }

    /// Ticket 230: what's needed first, then priority and age.
    #[test]
    fn automatic_puts_what_is_needed_first() {
        let card = |id: &str, priority: &str, links: &[&str]| crate::TicketSummary {
            id: id.into(),
            file: format!("{id}-t.md"),
            priority: priority.into(),
            created: "2026-10-01".into(),
            links: links.iter().map(|l| l.to_string()).collect(),
            ..Default::default()
        };
        let mut list = vec![
            card("003", "low", &["requires:004"]),
            card(
                "004",
                "low",
                &["requires:005", "requires:006", "required-by:003"],
            ),
            card("005", "low", &["required-by:004"]),
            card("006", "low", &["required-by:004"]),
            card("007", "high", &[]),
        ];
        SortOrder::Automatic.sort(&mut list);
        let ids: Vec<&str> = list.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["007", "005", "006", "004", "003"]);
        // A cycle still sorts everything once.
        let mut cycle = vec![
            card("001", "low", &["requires:002"]),
            card("002", "low", &["requires:001"]),
        ];
        SortOrder::Automatic.sort(&mut cycle);
        assert_eq!(cycle.len(), 2);
        assert_eq!(SortOrder::from_key("automatic"), SortOrder::Automatic);
    }

    #[test]
    fn the_form_writes_due_and_repeats_and_clears_them() {
        let text = "---\nid: \"022\"\ntitle: \"Check\"\npriority: \"low\"\n---\n\nBody\n";
        let mut fields = Fields::from(&parse_frontmatter(text));
        fields.due = "2026-10-16".into();
        fields.recur = "monthly".into();
        let saved = apply_fields(text, &fields);
        assert!(saved.contains("\ndue: \"2026-10-16\"\n"), "{saved}");
        assert!(saved.contains("\nrecur: \"monthly\"\n"), "{saved}");
        let back = Fields::from(&parse_frontmatter(&saved));
        assert_eq!(
            (back.due.as_str(), back.recur.as_str()),
            ("2026-10-16", "monthly")
        );
        fields.recur.clear();
        let cleared = apply_fields(&saved, &fields);
        assert!(cleared.contains("\nrecur: \"\"\n"), "{cleared}");
        assert!(cleared.ends_with("Body\n"));
    }
}
