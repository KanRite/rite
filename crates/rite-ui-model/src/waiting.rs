//! "Done waits" on the page (ticket 213): which tickets a card must wait
//! for before it can go to done. The same rule as `rite_lib::links::
//! waiting_on`, which `Board::move_task` enforces; this is only so a card
//! can say so and a drag onto done is refused before the drop. A ticket
//! waits for those it requires (its own `requires:` link, or another's
//! `required-by:` naming it) and for its children (their `parent`), unless
//! they are done. A ticket that isn't on the board holds nothing back.
//! The same reading of "requires" gives `sprint_gaps` (ticket 214): what
//! stops a sprint starting.

use crate::{Board, Column, Kind, TicketSummary, Workflow, split_link};

fn number(id: &str) -> Option<u32> {
    id.trim().trim_start_matches('#').parse().ok()
}

/// Every ticket on the board, wherever it sits: the columns and the epics.
pub fn all_tickets(board: &Board) -> Vec<&TicketSummary> {
    board
        .columns
        .iter()
        .flat_map(|c| c.tickets.iter())
        .chain(board.epics.iter().flat_map(|e| e.tickets.iter()))
        .collect()
}

/// The tickets in `all` that `ticket` waits on: on its own board, not done,
/// and required by it or its children. Sorted by number.
pub fn waiting_on<'a>(
    ticket: &TicketSummary,
    all: impl IntoIterator<Item = &'a TicketSummary>,
) -> Vec<&'a TicketSummary> {
    let me = number(&ticket.id);
    let all: Vec<&TicketSummary> = all.into_iter().collect();
    let required = requires(ticket, all.iter().copied());
    let mut out: Vec<&TicketSummary> = all
        .into_iter()
        .filter(|other| {
            other.repo == ticket.repo
                && other.file != ticket.file
                && !other.column.is(Kind::Done)
                && (required.iter().any(|r| r.file == other.file)
                    || (me.is_some() && number(&other.parent) == me))
        })
        .collect();
    out.sort_by_key(|t| number(&t.id));
    out.dedup_by(|a, b| a.file == b.file);
    out
}

/// Whether a ticket may be dropped in `to`: refused into done while it
/// waits on others.
pub fn done_rule(to: Column, waiting: &[&TicketSummary], ticket: &str) -> Result<(), String> {
    if !to.is(Kind::Done) || waiting.is_empty() {
        return Ok(());
    }
    let ids: Vec<&str> = waiting.iter().map(|t| t.id.as_str()).collect();
    let them = if ids.len() == 1 { "it" } else { "they" };
    Err(format!(
        "{ticket} waits on {}: {them} must be done first, or the link changed or removed.",
        ids.join(", ")
    ))
}

/// The tickets `ticket` requires, of those in `all` (on its own board):
/// its `requires:` links, and others' `required-by:` naming it. Done or not.
pub fn requires<'a>(
    ticket: &TicketSummary,
    all: impl IntoIterator<Item = &'a TicketSummary>,
) -> Vec<&'a TicketSummary> {
    let me = number(&ticket.id);
    let names = |entries: &[String], kind: &str, id: Option<u32>| {
        id.is_some()
            && entries
                .iter()
                .filter_map(|e| split_link(e))
                .any(|(k, i)| k == kind && number(i) == id)
    };
    all.into_iter()
        .filter(|other| {
            other.repo == ticket.repo
                && other.file != ticket.file
                && (names(&ticket.links, "requires", number(&other.id))
                    || names(&other.links, "required-by", me))
        })
        .collect()
}

/// Whether a card belongs to the epic or sprint `name`: its backlog
/// folder, or its `epic` label once it has moved on.
fn in_group(ticket: &TicketSummary, name: &str) -> bool {
    match ticket.column {
        Column::Backlog => ticket.epic.as_deref() == Some(name),
        _ => ticket.epic_label == name,
    }
}

/// Where a card is, for a message: `Backlog`, `Backlog › s13`, `To do`,
/// in the board's own column names.
pub fn place(ticket: &TicketSummary, workflow: &Workflow) -> String {
    match (&ticket.epic, ticket.column) {
        (Some(group), Column::Backlog) => format!("{} › {group}", workflow.label(Column::Backlog)),
        (_, column) => workflow.label(column),
    }
}

/// A sprint ticket that requires one outside the sprint, not done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gap {
    pub id: String,
    pub file: String,
    /// Where the sprint's ticket is, to open its tree.
    pub column: Column,
    pub epic: Option<String>,
    pub required_id: String,
    pub required_place: String,
}

/// What stops sprint `name` in `repo` starting (ticket 214): the same rule
/// as `rite_lib::requires::sprint_gaps`, from the board's cards (`all`, as
/// [`all_tickets`] gives them).
pub fn sprint_gaps(
    all: &[&TicketSummary],
    repo: &str,
    name: &str,
    workflow: &Workflow,
) -> Vec<Gap> {
    let mut gaps = Vec::new();
    for ticket in all.iter().filter(|t| t.repo == repo && in_group(t, name)) {
        for needed in requires(ticket, all.iter().copied()) {
            if needed.column.is(Kind::Done) || in_group(needed, name) {
                continue;
            }
            gaps.push(Gap {
                id: ticket.id.clone(),
                file: ticket.file.clone(),
                column: ticket.column,
                epic: ticket.epic.clone(),
                required_id: needed.id.clone(),
                required_place: place(needed, workflow),
            });
        }
    }
    gaps.sort_by(|a, b| (&a.id, &a.required_id).cmp(&(&b.id, &b.required_id)));
    gaps
}

/// Whether a card has a parent or children on its board (ticket 261), for
/// the hierarchy panel: its `parent` names a ticket there, or another's
/// names it.
pub fn has_hierarchy<'a>(
    ticket: &TicketSummary,
    all: impl IntoIterator<Item = &'a TicketSummary>,
) -> bool {
    let me = number(&ticket.id);
    let parent = number(&ticket.parent);
    all.into_iter().any(|other| {
        other.repo == ticket.repo
            && other.file != ticket.file
            && ((parent.is_some() && number(&other.id) == parent)
                || (me.is_some() && number(&other.parent) == me))
    })
}

/// Whether a card has a family for the tree (ticket 214): it requires or
/// relates to another, or another requires or relates to it.
pub fn has_family<'a>(
    ticket: &TicketSummary,
    all: impl IntoIterator<Item = &'a TicketSummary>,
) -> bool {
    let tied = |entries: &[String]| {
        entries
            .iter()
            .filter_map(|e| split_link(e))
            .any(|(kind, _)| matches!(kind, "requires" | "required-by"))
    };
    if tied(&ticket.links) || !ticket.related.is_empty() {
        return true;
    }
    let me = number(&ticket.id);
    me.is_some()
        && all.into_iter().any(|other| {
            other.repo == ticket.repo
                && other.file != ticket.file
                && (other.related.iter().any(|r| number(r) == me)
                    || other
                        .links
                        .iter()
                        .filter_map(|e| split_link(e))
                        .any(|(k, i)| matches!(k, "requires" | "required-by") && number(i) == me))
        })
}

/// How many other tickets a card is linked to in any way (ticket 216):
/// requires or required by, related, parent or child, clone, supersedes,
/// recorded on either ticket. Each other ticket counts once.
pub fn relationship_count<'a>(
    ticket: &TicketSummary,
    all: impl IntoIterator<Item = &'a TicketSummary>,
) -> usize {
    let me = number(&ticket.id);
    let names_me = |id: &str| me.is_some() && number(id) == me;
    // Every ticket number this one names, whatever the relationship.
    let mut mine: Vec<u32> = ticket
        .links
        .iter()
        .filter_map(|e| split_link(e))
        .filter_map(|(_, id)| number(id))
        .chain(ticket.related.iter().filter_map(|r| number(r)))
        .chain(ticket.clones.iter().filter_map(|c| number(c)))
        .chain(number(&ticket.parent))
        .chain(number(&ticket.clone_of))
        .collect();
    for other in all {
        if other.repo != ticket.repo || other.file == ticket.file {
            continue;
        }
        let names_it = other
            .links
            .iter()
            .filter_map(|e| split_link(e))
            .any(|(_, id)| names_me(id))
            || other.related.iter().any(|r| names_me(r))
            || other.clones.iter().any(|c| names_me(c))
            || names_me(&other.parent)
            || names_me(&other.clone_of);
        if names_it && let Some(n) = number(&other.id) {
            mine.push(n);
        }
    }
    mine.retain(|n| Some(*n) != me);
    mine.sort_unstable();
    mine.dedup();
    mine.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: &str, column: Column) -> TicketSummary {
        TicketSummary {
            repo: "acme-tickets".into(),
            file: format!("{id}-t.md"),
            id: id.into(),
            column,
            ..TicketSummary::default()
        }
    }

    #[test]
    fn a_ticket_waits_for_what_it_requires_and_its_children_until_they_are_done() {
        let mut parent = card("001", Column::Doing);
        parent.links = vec!["requires:002".into()];
        let required = card("002", Column::Todo);
        let mut child = card("003", Column::Backlog);
        child.parent = "001".into();
        let mut finished = card("004", Column::Done);
        finished.parent = "001".into();
        let mut requiring = card("005", Column::Todo);
        requiring.links = vec!["required-by:001".into()];
        let unrelated = card("006", Column::Todo);
        let all = [
            &parent, &required, &child, &finished, &requiring, &unrelated,
        ];
        let waits: Vec<&str> = waiting_on(&parent, all)
            .iter()
            .map(|t| t.id.as_str())
            .collect();
        assert_eq!(waits, ["002", "003", "005"]);
        let w = waiting_on(&parent, all);
        assert!(done_rule(Column::Done, &w, "001").is_err());
        assert!(done_rule(Column::Doing, &w, "001").is_ok());
        assert!(done_rule(Column::Done, &[], "001").is_ok());
        // Another board's 002 is a different ticket.
        let mut elsewhere = card("002", Column::Todo);
        elsewhere.repo = "acme-web".into();
        let mut lone = card("001", Column::Doing);
        lone.links = vec!["requires:002".into()];
        assert!(waiting_on(&lone, [&elsewhere]).is_empty());
    }

    #[test]
    fn a_sprint_cannot_start_while_its_tickets_require_ones_outside_it() {
        let in_sprint = |id: &str| {
            let mut t = card(id, Column::Backlog);
            t.epic = Some("s12".into());
            t
        };
        let mut a = in_sprint("001");
        a.links = vec!["requires:002".into(), "requires:003".into()];
        let b = in_sprint("002"); // in the sprint: fine
        let loose = card("003", Column::Backlog); // outside: a gap
        let mut c = in_sprint("004");
        c.links = vec!["requires:005".into()];
        let finished = card("005", Column::Done); // done: fine
        let mut started_on = card("006", Column::Todo);
        started_on.epic_label = "s12".into(); // moved on, still the sprint's
        let mut needs = card("007", Column::Backlog);
        needs.links = vec!["required-by:006".into()]; // recorded on the other end
        let all = [&a, &b, &loose, &c, &finished, &started_on, &needs];
        let gaps: Vec<(String, String, String)> =
            sprint_gaps(&all, "acme-tickets", "s12", &Workflow::standard())
                .into_iter()
                .map(|g| (g.id, g.required_id, g.required_place))
                .collect();
        assert_eq!(
            gaps,
            [
                ("001".into(), "003".into(), "Backlog".into()),
                ("006".into(), "007".into(), "Backlog".into())
            ]
        );
        assert!(sprint_gaps(&all, "acme-tickets", "s13", &Workflow::standard()).is_empty());
    }

    #[test]
    fn a_card_has_a_hierarchy_with_a_parent_or_children_on_its_board() {
        let mut parent = card("001", Column::Backlog);
        let mut child = card("002", Column::Todo);
        child.parent = "001".into();
        let alone = card("003", Column::Backlog);
        let all = [parent.clone(), child.clone(), alone.clone()];
        assert!(has_hierarchy(&parent, &all));
        assert!(has_hierarchy(&child, &all));
        assert!(!has_hierarchy(&alone, &all));
        // A parent on another board isn't this one's.
        parent.repo = "acme-api".into();
        let all = [parent, child.clone()];
        assert!(!has_hierarchy(&child, &all));
    }

    #[test]
    fn a_card_has_a_family_when_it_requires_relates_or_is_required() {
        let mut a = card("001", Column::Todo);
        a.links = vec!["requires:002".into()];
        let b = card("002", Column::Todo); // required by 001: recorded on 001 only here
        let mut c = card("003", Column::Todo);
        c.related = vec!["004".into()];
        let d = card("004", Column::Todo);
        let mut e = card("005", Column::Todo);
        e.links = vec!["supersedes:006".into()]; // not a requires or related tie
        let f = card("006", Column::Todo);
        let all = [&a, &b, &c, &d, &e, &f];
        let has = |t: &TicketSummary| has_family(t, all);
        assert!(has(&a) && has(&b) && has(&c) && has(&d));
        assert!(!has(&e) && !has(&f));
    }

    #[test]
    fn relationships_are_counted_once_per_other_ticket_from_either_end() {
        let mut a = card("001", Column::Todo);
        a.links = vec!["requires:002".into(), "supersedes:002".into()];
        a.related = vec!["003".into()];
        let b = card("002", Column::Todo);
        let c = card("003", Column::Todo);
        let mut child = card("004", Column::Todo);
        child.parent = "001".into();
        let mut copy = card("005", Column::Todo);
        copy.clone_of = "001".into();
        let lone = card("006", Column::Todo);
        let all = [&a, &b, &c, &child, &copy, &lone];
        assert_eq!(relationship_count(&a, all), 4, "002 twice is one ticket");
        assert_eq!(relationship_count(&b, all), 1, "named only on 001");
        assert_eq!(relationship_count(&child, all), 1);
        assert_eq!(relationship_count(&lone, all), 0);
    }
}
