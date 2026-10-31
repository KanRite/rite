//! A task: one markdown file, `NNN-slug.md`, living in a column directory.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::board::{Board, Column};
use crate::branch;
use crate::git;

/// Longest slug we put in a filename (matches the pre-Rust `cut -c1-50`).
const MAX_SLUG: usize = 50;

/// Heading the child wikilinks are collected under.
const CHILDREN_HEADING: &str = "## Children";

/// A cheap fingerprint of a ticket's on-disk content, captured when it's read
/// with the intent of writing back to it, and checked again immediately
/// before the write actually happens — the lost-update fix from
/// `docs/adr/0007-concurrency-and-locking.md`. Not cryptographic: this only
/// needs to notice a change, not resist a deliberate collision.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ContentHash(u64);

impl std::fmt::Display for ContentHash {
    /// Sixteen hex digits — how a front end that can't hold a `u64` (a web
    /// page) carries the fingerprint between opening a ticket and saving it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

impl std::str::FromStr for ContentHash {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        u64::from_str_radix(s, 16).map(ContentHash)
    }
}

fn hash_of(text: &str) -> ContentHash {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    ContentHash(hasher.finish())
}

/// A write was refused because the ticket changed since it was last read.
/// The caller's decision may no longer be valid — re-read and retry, rather
/// than the write silently discarding whatever changed it.
#[derive(Debug)]
pub struct Conflict {
    pub path: PathBuf,
}

impl std::fmt::Display for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} changed since it was last read", self.path.display())
    }
}

impl std::error::Error for Conflict {}

/// A write was refused because someone else holds an active lease on this
/// ticket — `docs/adr/0007-concurrency-and-locking.md`'s coordination layer,
/// on top of the lost-update fix above. `until` is when it lapses on its
/// own; nobody has to release it for that to happen.
#[derive(Debug)]
pub struct LeaseHeld {
    pub path: PathBuf,
    pub holder: String,
    pub until: OffsetDateTime,
}

impl std::fmt::Display for LeaseHeld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let until = self
            .until
            .format(&Rfc3339)
            .unwrap_or_else(|_| self.until.to_string());
        write!(
            f,
            "{} is leased by {} until {until} — use `rite unlock --force` to break it",
            self.path.display(),
            self.holder
        )
    }
}

impl std::error::Error for LeaseHeld {}

#[derive(Clone, Debug)]
pub struct Task {
    pub path: PathBuf,
    pub column: Column,
    /// The `NNN` prefix, when the filename has one.
    pub id: Option<u32>,
    /// Filename without the `.md` extension, e.g. `001-first-task`.
    pub stem: String,
    /// The epic folder the ticket sits in, for tickets filed under one. The
    /// folder is what decides this — the `epic` frontmatter key follows it,
    /// not the other way round, so moving the file is what changes the epic.
    pub epic: Option<String>,
}

/// Every field the ticket editor can change in one pass, for
/// [`Task::apply_edit`]. `epic` is deliberately not here — it moves the file
/// (`Board::set_epic`), which is a different, adjacent action from a
/// frontmatter-only write; the editor calls that separately, after this one.
pub struct EditFields<'a> {
    pub title: &'a str,
    /// Everyone assigned, in order (ticket 254); empty for nobody.
    pub assignees: &'a [String],
    pub points: &'a str,
    pub tags: &'a [String],
    /// Branch type; `branch-name` is not taken as input — it's re-derived
    /// from this and `title`, the same pairing `create::Draft::prepare`
    /// establishes at creation.
    pub branch_type: &'a str,
    /// A ref (id or slug), resolved against `board` the same way
    /// `create::Draft::prepare` resolves one. Empty clears it.
    pub parent: &'a str,
    /// Refs, resolved the same way.
    pub related: &'a [String],
    pub body: &'a str,
}

impl Task {
    /// Build a task from a path inside a column directory.
    pub fn from_path(path: &Path, column: Column) -> Option<Self> {
        if path.extension()? != "md" {
            return None;
        }
        let stem = path.file_stem()?.to_str()?.to_string();
        let id = stem
            .split_once('-')
            .and_then(|(head, _)| head.parse::<u32>().ok());
        Some(Self {
            path: path.to_path_buf(),
            column,
            id,
            stem,
            epic: None,
        })
    }

    pub fn file_name(&self) -> String {
        format!("{}.md", self.stem)
    }

    /// Look up a frontmatter key. Tolerant by design: task files are rendered
    /// from user-editable templates, so this reads the first `key: value` line
    /// rather than pretending to be a YAML parser.
    pub fn meta(&self, key: &str) -> Option<String> {
        let text = crate::eol::read(&self.path).ok()?;
        meta_in(&text, key).map(str::to_string)
    }

    /// Fingerprint the ticket's current on-disk content. Call this right
    /// before deciding what to write, and pass the result to the write —
    /// if the file changed in between, the write is refused rather than
    /// silently discarding whatever changed it.
    pub fn hash(&self) -> Result<ContentHash> {
        let text = crate::eol::read(&self.path)
            .with_context(|| format!("reading {}", self.path.display()))?;
        Ok(hash_of(&text))
    }

    /// Who holds the lease, if the ticket records one — regardless of
    /// whether it has since lapsed. Use [`Task::lease_active`] to ask
    /// whether it still counts.
    pub fn locked_by(&self) -> Option<String> {
        self.meta("locked-by").filter(|who| !who.is_empty())
    }

    /// When the recorded lease lapses, if it has one.
    pub fn locked_until(&self) -> Option<OffsetDateTime> {
        self.meta("locked-until")
            .filter(|value| !value.is_empty())
            .and_then(|value| OffsetDateTime::parse(&value, &Rfc3339).ok())
    }

    /// Whether a lease is currently in force: held by someone, and not yet
    /// lapsed. A lapsed lease is not an error state
    /// (`docs/adr/0007-concurrency-and-locking.md`) — it just stops
    /// counting, with nothing to clean up.
    pub fn lease_active(&self) -> bool {
        match (self.locked_by(), self.locked_until()) {
            (Some(_), Some(until)) => until > OffsetDateTime::now_utc(),
            _ => false,
        }
    }

    /// Read-modify-write against this ticket, refusing with [`Conflict`] if
    /// the file no longer matches `expected`, or [`LeaseHeld`] if someone
    /// else's lease is still active. `modify` receives the freshly re-read,
    /// verified-current text and returns the replacement.
    fn write_checked(
        &self,
        expected: ContentHash,
        modify: impl FnOnce(&str) -> Result<String>,
    ) -> Result<()> {
        let text = crate::eol::read(&self.path)
            .with_context(|| format!("reading {}", self.path.display()))?;
        self.check_lease(&text)?;
        if hash_of(&text) != expected {
            return Err(Conflict {
                path: self.path.clone(),
            }
            .into());
        }
        let updated = modify(&text)?;
        fs::write(&self.path, updated).with_context(|| format!("writing {}", self.path.display()))
    }

    /// Releases the lease if the current git identity holds it — lapsed or
    /// not — and leaves anyone else's alone (ticket 092: a ticket leaving
    /// active work gives back the lease its move into doing took, ticket 156).
    /// Returns whether it released anything. Someone else's lease, even a
    /// lapsed one, is never this function's to clear; `break_lease` is.
    pub fn release_if_mine(&self) -> Result<bool> {
        let Some(holder) = self.locked_by() else {
            return Ok(false);
        };
        if self.git_identity().as_deref() != Some(holder.as_str()) {
            return Ok(false);
        }
        self.release_lease(self.hash()?)?;
        Ok(true)
    }

    /// Whether `email` (this clone's identity) is one of the people the
    /// ticket `text` is assigned to: by the name the project's users list
    /// gives that email, else git's `user.name`.
    fn assigned_to_me(&self, text: &str, email: &str) -> bool {
        let people = assignees_in(text);
        if people.is_empty() {
            return false;
        }
        let Some(repo) = self.path.ancestors().find(|dir| git::is_repo_dir(dir)) else {
            return false;
        };
        let board = crate::board::Board::in_repo(repo);
        let mine =
            crate::my_work::names_for(&board, email, git::config(repo, "user.name").as_deref());
        people
            .iter()
            .any(|who| mine.contains(&who.trim().to_lowercase()))
    }

    /// `git config user.email`, resolved from this ticket's own directory
    /// rather than the process's current one — a desktop board open across
    /// several registered repositories isn't standing in any one of them.
    fn git_identity(&self) -> Option<String> {
        let repo = self.path.parent().unwrap_or_else(|| Path::new("."));
        git::config(repo, "user.email")
    }

    /// Refuse if someone else holds an active lease on this ticket. Content
    /// setters get this for free from `write_checked`; `board.rs`'s
    /// `move_task`/`archive`/`set_epic` rename the file rather than writing
    /// through it, so they call this explicitly, up front, before touching
    /// the filesystem — checking only *after* the rename (via
    /// `set_epic_meta`'s own `write_checked`, say) would risk a half-moved,
    /// half-refused ticket.
    pub(crate) fn ensure_not_leased(&self) -> Result<()> {
        let text = crate::eol::read(&self.path)
            .with_context(|| format!("reading {}", self.path.display()))?;
        self.check_lease(&text)
    }

    /// The generic lease gate every `write_checked`-based setter goes
    /// through: refuse if `text` (already read, not re-read here) records
    /// an active lease held by someone other than the current git identity.
    /// `take_lease`/`release_lease` need no special casing beyond this —
    /// taking is correctly blocked when someone else holds it, and
    /// releasing is correctly blocked unless the caller *is* the holder.
    /// `break_lease` is the one deliberate bypass.
    fn check_lease(&self, text: &str) -> Result<()> {
        let Some(holder) = meta_in(text, "locked-by").filter(|who| !who.is_empty()) else {
            return Ok(());
        };
        let Some(until) = meta_in(text, "locked-until")
            .filter(|value| !value.is_empty())
            .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
        else {
            return Ok(());
        };
        if until <= OffsetDateTime::now_utc() {
            return Ok(());
        }
        let me = self.git_identity();
        if me.as_deref() == Some(holder) {
            return Ok(());
        }
        // Everyone the ticket is assigned to may write it while it's locked
        // (ticket 254); the lease still names one holder.
        if let Some(me) = &me
            && self.assigned_to_me(text, me)
        {
            return Ok(());
        }
        Err(LeaseHeld {
            path: self.path.clone(),
            holder: holder.to_string(),
            until,
        }
        .into())
    }

    /// Claim the lease for the current git identity (`git config
    /// user.email`), for `hours` from now. Inherits `write_checked`'s
    /// generic lease gate with no extra logic: refuses if someone else's
    /// active lease is already there, succeeds otherwise — including
    /// refreshing one you already hold.
    pub fn take_lease(&self, expected: ContentHash, hours: f64) -> Result<()> {
        let holder = self.git_identity().with_context(|| {
            "no git user.email configured — set one with `git config user.email you@example.com`"
        })?;
        let until = OffsetDateTime::now_utc() + time::Duration::seconds_f64(hours * 3600.0);
        let until = until.format(&Rfc3339)?;
        self.write_checked(expected, |text| {
            let text = set_meta(text, "locked-by", &format!("{holder:?}"))?;
            set_meta(&text, "locked-until", &format!("{until:?}"))
        })
    }

    /// Release the lease — only the caller's own. Releasing someone else's
    /// isn't release, it's [`Task::break_lease`], and stays a separate,
    /// deliberate act rather than a side effect of this one.
    pub fn release_lease(&self, expected: ContentHash) -> Result<()> {
        self.write_checked(expected, |text| {
            let text = set_meta(text, "locked-by", "\"\"")?;
            set_meta(&text, "locked-until", "\"\"")
        })
    }

    /// Clear the lease regardless of who holds it. The one deliberate
    /// bypass of `write_checked`'s generic lease gate — used only by `rite
    /// unlock --force`, never as a side effect of an ordinary write.
    pub fn break_lease(&self, expected: ContentHash) -> Result<()> {
        let text = crate::eol::read(&self.path)
            .with_context(|| format!("reading {}", self.path.display()))?;
        if hash_of(&text) != expected {
            return Err(Conflict {
                path: self.path.clone(),
            }
            .into());
        }
        let text = set_meta(&text, "locked-by", "\"\"")?;
        let updated = set_meta(&text, "locked-until", "\"\"")?;
        fs::write(&self.path, updated).with_context(|| format!("writing {}", self.path.display()))
    }

    /// Id as displayed on the board: frontmatter first, then the filename.
    pub fn id_display(&self) -> String {
        self.meta("id")
            .filter(|id| !id.is_empty())
            .or_else(|| self.id.map(|n| format!("{n:03}")))
            .unwrap_or_else(|| "???".to_string())
    }

    /// Title as displayed on the board, falling back to the filename.
    pub fn title(&self) -> String {
        self.meta("title")
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| self.stem.clone())
    }

    /// Retitle the ticket. Only the frontmatter changes: the filename keeps
    /// its original slug, because the id is what every reference uses and
    /// renaming the file would break `git log --follow` for a cosmetic gain.
    pub fn set_title(&self, expected: ContentHash, title: &str) -> Result<()> {
        self.write_checked(expected, |text| {
            set_meta(text, "title", &format!("{title:?}"))
        })
    }

    /// Everything below the closing frontmatter fence — the part a human
    /// actually writes.
    pub fn body(&self) -> Result<String> {
        let text = crate::eol::read(&self.path)
            .with_context(|| format!("reading {}", self.path.display()))?;
        Ok(split_body(&text)
            .map(|(_, body)| body.to_string())
            .unwrap_or(text))
    }

    /// Replace the body, leaving the frontmatter exactly as it was.
    pub fn set_body(&self, expected: ContentHash, body: &str) -> Result<()> {
        self.write_checked(expected, |text| {
            let head = split_body(text)
                .map(|(head, _)| head.to_string())
                .ok_or_else(|| anyhow::anyhow!("no frontmatter in {}", self.path.display()))?;
            let mut out = head;
            out.push_str(body);
            if !out.ends_with('\n') {
                out.push('\n');
            }
            Ok(out)
        })
    }

    /// The first person the ticket is assigned to, if anyone: what
    /// `assignee` holds. Also reads `owner`, which the field was briefly
    /// called, so no board is left stranded. Everyone is [`assignees`].
    ///
    /// [`assignees`]: Self::assignees
    pub fn assignee(&self) -> Option<String> {
        self.meta("assignee")
            .or_else(|| self.meta("owner"))
            .filter(|who| !who.is_empty())
    }

    /// Everyone the ticket is assigned to, in order (ticket 254). One person
    /// is `assignee` alone, as it always was; several also write an
    /// `assignees` list whose first name is `assignee`, so a build from
    /// before 254 still sees the first person. The list is believed only
    /// while its first name matches `assignee`: one that doesn't was left
    /// behind by an older build reassigning the ticket, and `assignee` wins.
    pub fn assignees(&self) -> Vec<String> {
        crate::eol::read(&self.path)
            .map(|text| assignees_in(&text))
            .unwrap_or_default()
    }

    /// Whether `name` is one of the people assigned, ignoring case.
    pub fn is_assigned_to(&self, name: &str) -> bool {
        let name = name.trim().to_lowercase();
        !name.is_empty()
            && self
                .assignees()
                .iter()
                .any(|who| who.trim().to_lowercase() == name)
    }

    /// The repository this ticket is about (tickets 101/107): its
    /// `repository` field, as a bare name — `acme/amd` and `amd` are the
    /// same repository. `None` when the ticket doesn't say.
    pub fn repository(&self) -> Option<String> {
        self.meta("repository")
            .map(|value| repo_name(&value))
            .filter(|name| !name.is_empty())
    }

    /// Point the ticket at `name`, or clear it with an empty name. Only a
    /// repository cloned on this machine can be chosen ([`check_repository`]).
    pub fn set_repository(&self, expected: ContentHash, name: &str) -> Result<()> {
        let name = repo_name(name);
        check_repository(&self.path, &name)?;
        self.write_checked(expected, |text| {
            set_meta(text, "repository", &format!("{name:?}"))
        })
    }

    /// When it's due (ticket 143): `YYYY-MM-DD`.
    pub fn due(&self) -> Option<String> {
        self.meta("due").filter(|due| !due.is_empty())
    }

    /// Set when it's due, or clear it with an empty value; anything but a
    /// `YYYY-MM-DD` date is refused.
    pub fn set_due(&self, expected: ContentHash, due: &str) -> Result<()> {
        let due = due.trim();
        if !due.is_empty() {
            crate::recur::parse_date(due)?;
        }
        self.write_checked(expected, |text| match due {
            "" => Ok(remove_meta(text, "due")),
            due => set_meta(text, "due", &format!("{due:?}")),
        })
    }

    /// When it entered todo (ticket 252), `YYYY-MM-DD`: kept through doing,
    /// so the time it waited carries on counting for auto priority.
    pub fn todo_since(&self) -> Option<String> {
        self.meta(TODO_SINCE).filter(|d| !d.is_empty())
    }

    /// When it entered doing (ticket 252), `YYYY-MM-DD`.
    pub fn doing_since(&self) -> Option<String> {
        self.meta(DOING_SINCE).filter(|d| !d.is_empty())
    }

    /// Set (`Some`) or remove (`None`) when it entered todo and doing, in
    /// one write.
    pub fn set_column_dates(
        &self,
        expected: ContentHash,
        todo: Option<&str>,
        doing: Option<&str>,
    ) -> Result<()> {
        self.write_checked(expected, |text| {
            let text = match todo {
                Some(date) => set_meta(text, TODO_SINCE, &format!("{date:?}"))?,
                None => remove_meta(text, TODO_SINCE),
            };
            match doing {
                Some(date) => set_meta(&text, DOING_SINCE, &format!("{date:?}")),
                None => Ok(remove_meta(&text, DOING_SINCE)),
            }
        })
    }

    /// How it repeats (ticket 143), as written: `monthly`, `every 2 weeks`.
    pub fn recur(&self) -> Option<String> {
        self.meta("recur").filter(|rule| !rule.is_empty())
    }

    /// Make it repeat (`weekly`, `monthly`, `every 3 days`…), or stop it
    /// with an empty value. Stored in the rule's plain form.
    pub fn set_recur(&self, expected: ContentHash, rule: &str) -> Result<()> {
        let rule = match rule.trim() {
            "" => String::new(),
            given => crate::recur::Every::parse(given)?.to_string(),
        };
        self.write_checked(expected, |text| match rule.as_str() {
            "" => Ok(remove_meta(text, "recur")),
            rule => set_meta(text, "recur", &format!("{rule:?}")),
        })
    }

    /// The ticket a recurring one was made from (ticket 143).
    pub fn recurs_from(&self) -> Option<String> {
        self.meta("recurs-from").filter(|id| !id.is_empty())
    }

    pub(crate) fn set_recurs_from(&self, expected: ContentHash, id: &str) -> Result<()> {
        self.write_checked(expected, |text| {
            set_meta(text, "recurs-from", &format!("{id:?}"))
        })
    }

    /// Assign the ticket to one person, or clear it with an empty name.
    pub fn assign(&self, expected: ContentHash, who: &str) -> Result<()> {
        let who = who.trim();
        let names: Vec<String> = (!who.is_empty())
            .then(|| who.to_string())
            .into_iter()
            .collect();
        self.set_assignees(expected, &names)
    }

    /// Assign the ticket to these people, in order, or nobody (ticket 254).
    pub fn set_assignees(&self, expected: ContentHash, names: &[String]) -> Result<()> {
        self.write_checked(expected, |text| with_assignees(text, names))
    }

    /// Story points, when the ticket has been sized. Free-form on purpose:
    /// teams use 1/2/3/5/8, or T-shirt sizes, and the board only needs to show
    /// it back.
    pub fn points(&self) -> Option<String> {
        self.meta("points").filter(|points| !points.is_empty())
    }

    /// Size the ticket, or clear the size with an empty string.
    pub fn set_points(&self, expected: ContentHash, points: &str) -> Result<()> {
        validate_points(points)?;
        self.write_checked(expected, |text| {
            set_meta(text, "points", &format!("{points:?}"))
        })
    }

    /// Replace the whole ticket with `text` — what an editor that shows the
    /// raw file saves. Hash-checked like every write, and checked like
    /// every other write path: a missing priority becomes `low`, an invalid
    /// one or invalid points are refused. The lease (`locked-by`/
    /// `locked-until`) is kept as it is on disk: a lease changes only by
    /// locking and unlocking, never as a side effect of saving an edit.
    pub fn set_text(&self, expected: ContentHash, text: &str) -> Result<()> {
        let text = ensure_priority(text)?;
        let text = uppercase_tags(&text)?;
        if let Some(points) = meta_in(&text, "points") {
            validate_points(points)?;
        }
        // Ticket 143: a due date and a repeat rite can read.
        if let Some(due) = meta_in(&text, "due").filter(|d| !d.trim().is_empty()) {
            crate::recur::parse_date(due)?;
        }
        if let Some(rule) = meta_in(&text, "recur").filter(|r| !r.trim().is_empty()) {
            crate::recur::Every::parse(rule)?;
        }
        // Re-pointing the ticket at another repository needs that one to be
        // here; leaving a teammate's choice alone never does.
        let chosen = repo_name(meta_in(&text, "repository").unwrap_or_default());
        if self.repository().unwrap_or_default() != chosen {
            check_repository(&self.path, &chosen)?;
        }
        self.write_checked(expected, |current| {
            let mut out = text.clone();
            for key in ["locked-by", "locked-until"] {
                let disk = meta_in(current, key).unwrap_or_default();
                if meta_in(&out, key).unwrap_or_default() != disk {
                    out = set_meta(&out, key, &format!("{disk:?}"))?;
                }
            }
            Ok(out)
        })
    }

    /// How urgent the ticket is: one of [`PRIORITIES`], `low` when unset.
    pub fn priority(&self) -> String {
        // A ticket waiting to be triaged is always top (tickets 248, 252),
        // even one written before that rule, whatever its file says.
        if self.has_tag(TRIAGE_TAG) {
            return TOP.to_string();
        }
        self.meta("priority")
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| DEFAULT_PRIORITY.to_string())
    }

    /// Set the ticket's priority; anything but one of [`PRIORITIES`] is
    /// refused.
    pub fn set_priority(&self, expected: ContentHash, priority: &str) -> Result<()> {
        validate_priority(priority)?;
        if priority != TOP && self.has_tag(TRIAGE_TAG) {
            bail!(
                "{} is waiting to be triaged, so it's top: take {TRIAGE_TAG} off first",
                self.id_display()
            );
        }
        self.write_checked(expected, |text| {
            set_meta(text, "priority", &format!("{priority:?}"))
        })
    }

    /// Write the priority and auto priority's notes on it (ticket 215) in
    /// one checked write: `priority`, and `priority-from`, `priority-own`
    /// and `priority-pinned`, each set when `Some` and removed when `None`.
    pub fn write_priority(
        &self,
        expected: ContentHash,
        priority: &str,
        from: Option<&str>,
        own: Option<&str>,
        pinned: bool,
    ) -> Result<()> {
        validate_priority(priority)?;
        if let Some(own) = own {
            validate_priority(own)?;
        }
        // A TRIAGE ticket stays top (tickets 248, 252), whatever is being
        // put back.
        let priority = if self.has_tag(TRIAGE_TAG) {
            TOP
        } else {
            priority
        };
        self.write_checked(expected, |text| {
            let mut text = set_meta(text, "priority", &format!("{priority:?}"))?;
            for (key, value) in [
                ("priority-from", from.map(str::to_string)),
                ("priority-own", own.map(str::to_string)),
                ("priority-pinned", pinned.then(|| "yes".to_string())),
            ] {
                text = match value {
                    Some(value) => set_meta(&text, key, &format!("{value:?}"))?,
                    None => remove_meta(&text, key),
                };
            }
            Ok(text)
        })
    }

    /// The epic recorded in the frontmatter. `Task::epic` — set from the
    /// folder — is the authority; this is what a reader outside the board
    /// sees, and the two are kept in step when a ticket is filed.
    pub fn epic_meta(&self) -> Option<String> {
        self.meta("epic").filter(|epic| !epic.is_empty())
    }

    /// Record the epic in the frontmatter, so the ticket says which epic it
    /// belongs to even when read on its own.
    pub fn set_epic_meta(&self, expected: ContentHash, epic: &str) -> Result<()> {
        self.write_checked(expected, |text| {
            set_meta(text, "epic", &format!("{epic:?}"))
        })
    }

    /// Where the ticket sits within its column, for boards ordered by hand.
    /// Absent means unranked, which sorts after everything ranked — the same
    /// treatment an unset priority gets, so a board nobody has dragged still
    /// reads in id order.
    pub fn order(&self) -> Option<f64> {
        self.meta("order")
            .filter(|value| !value.is_empty())
            .and_then(|value| value.trim().parse::<f64>().ok())
    }

    /// Rank the ticket within its column. Fractional on purpose: a card
    /// dropped between two others takes the midpoint of its neighbours, so one
    /// drag rewrites one file instead of renumbering the column — and two
    /// branches that both reorder collide over one ticket rather than all.
    pub fn set_order(&self, expected: ContentHash, rank: f64) -> Result<()> {
        self.write_checked(expected, |text| {
            set_meta(text, "order", &format!("\"{rank}\""))
        })
    }

    /// Title, assignee, points, tags, branch type (and the `branch-name` it
    /// derives), parent, related, and body, applied together in **one**
    /// write. Required, not just tidier, once writes are hash-checked: a
    /// form that captures one hash when it opens and then calls separate
    /// setters against it could never have the second one succeed, since the
    /// first write already changed the file the hash was checked against.
    /// This is what makes `docs/adr/0006-the-operation-vocabulary.md`'s "one
    /// write per action" true for the editor, which is also what
    /// `docs/adr/0007-concurrency-and-locking.md`'s optimistic writes assume.
    ///
    /// `parent`/`related` are resolved against `board` before anything is
    /// written — an unknown ref is refused clean, the same as
    /// `create::Draft::prepare`. Unlike creation (and unlike `rite link`),
    /// only *this* ticket's own frontmatter changes here: the parent's
    /// `## Children` list and a related ticket's own back-link are not
    /// touched. Doing that safely would mean hash-checking several files as
    /// one transaction, which nothing in this codebase does today — a known,
    /// deliberate gap rather than a half-built reciprocal write; `rite link`
    /// remains the one path that updates both sides.
    pub fn apply_edit(
        &self,
        expected: ContentHash,
        board: &Board,
        fields: EditFields,
    ) -> Result<()> {
        let branch_type = branch::normalise(fields.branch_type);
        branch::validate_branch_type(&branch_type)?;
        let branch_name = branch::for_title(&branch_type, fields.title)?;
        validate_points(fields.points)?;
        let parent = match fields.parent {
            "" => String::new(),
            reference => board.find(reference)?.id_display(),
        };
        let mut related = Vec::new();
        for reference in fields.related {
            related.push(board.find(reference)?.id_display());
        }
        related.sort();
        related.dedup();

        self.write_checked(expected, |text| {
            let text = set_meta(text, "title", &format!("{:?}", fields.title))?;
            let text = with_assignees(&text, fields.assignees)?;
            let text = set_meta(&text, "points", &format!("{:?}", fields.points))?;
            let tags = normalize_tags(fields.tags);
            let text = set_meta(&text, "tags", &format!("[{}]", tags.join(",")))?;
            let text = set_meta(&text, "branch-type", &format!("{branch_type:?}"))?;
            let text = set_meta(&text, "branch-name", &format!("{branch_name:?}"))?;
            let text = set_meta(&text, "parent", &format!("{parent:?}"))?;
            let text = set_meta(&text, "related", &format!("[{}]", related.join(",")))?;
            // Adding TRIAGE makes it top (tickets 248, 252).
            let text = ensure_priority(&text)?;
            let head = split_body(&text)
                .map(|(head, _)| head.to_string())
                .ok_or_else(|| anyhow::anyhow!("no frontmatter in {}", self.path.display()))?;
            let mut out = head;
            out.push_str(fields.body);
            if !out.ends_with('\n') {
                out.push('\n');
            }
            Ok(out)
        })
    }

    /// Set the branch type (`none` for no branch, empty for not chosen), and
    /// the `branch-name` it gives with the title, as creation does.
    pub fn set_branch_type(&self, expected: ContentHash, kind: &str) -> Result<()> {
        let kind = branch::normalise(kind);
        branch::validate_branch_type(&kind)?;
        let name = branch::for_title(&kind, &self.title())?;
        self.write_checked(expected, |text| {
            let text = set_meta(text, "branch-type", &format!("{kind:?}"))?;
            set_meta(&text, "branch-name", &format!("{name:?}"))
        })
    }

    /// The branch type, if the ticket carries one. Falls back to the old
    /// `type` key.
    pub fn branch_type(&self) -> Option<String> {
        self.meta("branch-type")
            .or_else(|| self.meta("type"))
            .filter(|kind| !kind.is_empty())
    }

    /// The id of this ticket's parent, if it has one. Ids rather than paths,
    /// so the link survives a rename or a move between columns.
    pub fn parent(&self) -> Option<String> {
        self.meta("parent").filter(|parent| !parent.is_empty())
    }

    /// The tickets this one is related to: ids as recorded in `related`.
    pub fn related(&self) -> Vec<String> {
        self.meta("related")
            .map(|raw| parse_list(&raw))
            .unwrap_or_default()
    }

    /// The id of the ticket this one is a copy of, if it is a clone.
    pub fn clone_of(&self) -> Option<String> {
        self.meta("clone-of").filter(|id| !id.is_empty())
    }

    /// The tickets that are copies of this one: ids as recorded in `clones`.
    pub fn clones(&self) -> Vec<String> {
        self.meta("clones")
            .map(|raw| parse_list(&raw))
            .unwrap_or_default()
    }

    /// This ticket's tags, as recorded in `tags`.
    pub fn tags(&self) -> Vec<String> {
        self.meta("tags")
            .map(|raw| parse_list(&raw))
            .unwrap_or_default()
    }

    /// Whether this ticket carries `tag`. Case is ignored (ticket 122):
    /// tags are saved in uppercase, but a ticket written before that may
    /// still carry `cli` where the list says `CLI`.
    pub fn has_tag(&self, tag: &str) -> bool {
        let tag = normalize_tag(tag);
        self.tags().iter().any(|known| normalize_tag(known) == tag)
    }

    /// Whether this ticket passes a tag filter (ticket 123): each term is a
    /// tag to include (`FUTURE`) or, with a dash, to leave out (`-FUTURE`);
    /// a leading `#` is allowed (`#FUTURE`, `-#FUTURE`). With included
    /// terms the ticket needs just one of them; it must carry none of the
    /// excluded ones. Whole tags, case ignored; no terms passes everything.
    pub fn matches_tags(&self, terms: &[String]) -> bool {
        let (mut wanted, mut unwanted) = (Vec::new(), Vec::new());
        for term in terms {
            let term = term.trim();
            let (out, tag) = match term.strip_prefix('-') {
                Some(rest) => (true, rest),
                None => (false, term),
            };
            let tag = tag.trim_start_matches('#');
            if tag.is_empty() {
                continue;
            }
            if out {
                unwanted.push(tag);
            } else {
                wanted.push(tag);
            }
        }
        (wanted.is_empty() || wanted.iter().any(|t| self.has_tag(t)))
            && !unwanted.iter().any(|t| self.has_tag(t))
    }

    /// Whether this ticket is a draft (ticket 114): it carries [`DRAFT_TAG`],
    /// and so can't leave the backlog until that's taken off.
    pub fn is_draft(&self) -> bool {
        self.has_tag(DRAFT_TAG)
    }

    /// Make this ticket a draft, or take it out of draft, by adding or
    /// removing [`DRAFT_TAG`]. The other tags are kept, in uppercase.
    /// Returns whether the file changed.
    pub fn set_draft(&self, expected: ContentHash, draft: bool) -> Result<bool> {
        let mut tags = normalize_tags(&self.tags());
        let had = tags.iter().any(|tag| tag == DRAFT_TAG);
        if had == draft {
            return Ok(false);
        }
        if draft {
            tags.push(DRAFT_TAG.to_string());
        } else {
            tags.retain(|tag| tag != DRAFT_TAG);
        }
        self.write_checked(expected, |text| {
            set_meta(text, "tags", &format!("[{}]", tags.join(",")))
        })?;
        Ok(true)
    }

    /// Add ids to the `related` list, in id order and without duplicates.
    /// Returns whether the file changed.
    pub fn add_related(&self, expected: ContentHash, ids: &[String]) -> Result<bool> {
        let mut related = self.related();
        let mut changed = false;
        for id in ids {
            if !related.iter().any(|known| known == id) {
                related.push(id.clone());
                changed = true;
            }
        }
        if !changed {
            return Ok(false);
        }
        related.sort();
        self.write_checked(expected, |text| {
            set_meta(text, "related", &format!("[{}]", related.join(",")))
        })?;
        Ok(true)
    }

    /// Add a copy's id to the `clones` list, in id order and without
    /// duplicates. Returns whether the file changed.
    pub fn add_clone(&self, expected: ContentHash, id: &str) -> Result<bool> {
        let mut clones = self.clones();
        if clones.iter().any(|known| known == id) {
            return Ok(false);
        }
        clones.push(id.to_string());
        clones.sort();
        self.write_checked(expected, |text| {
            set_meta(text, "clones", &format!("[{}]", clones.join(",")))
        })?;
        Ok(true)
    }

    /// The typed links recorded in `links` (ticket 207). An entry this
    /// version doesn't understand is skipped, not an error, so a board
    /// written by a newer one still opens.
    pub fn links(&self) -> Vec<crate::links::Link> {
        self.meta("links")
            .map(|raw| parse_list(&raw))
            .unwrap_or_default()
            .iter()
            .filter_map(|entry| crate::links::Link::parse(entry))
            .collect()
    }

    /// Add a typed link to `links`, kept in order and without duplicates.
    /// Returns whether the file changed.
    pub fn add_link(&self, expected: ContentHash, link: &crate::links::Link) -> Result<bool> {
        let mut links = self.links();
        if links.contains(link) {
            return Ok(false);
        }
        links.push(link.clone());
        links.sort();
        self.write_checked(expected, |text| set_links(text, &links))?;
        Ok(true)
    }

    /// Take a typed link out of `links`. Returns whether the file changed.
    pub fn remove_link(&self, expected: ContentHash, link: &crate::links::Link) -> Result<bool> {
        let mut links = self.links();
        let before = links.len();
        links.retain(|known| known != link);
        if links.len() == before {
            return Ok(false);
        }
        self.write_checked(expected, |text| set_links(text, &links))?;
        Ok(true)
    }

    /// Set the `parent` key (empty clears it). Only this file changes: the
    /// parent's `## Children` list is [`Task::add_child_link`]'s business.
    pub fn set_parent(&self, expected: ContentHash, id: &str) -> Result<()> {
        self.write_checked(expected, |text| {
            set_meta(text, "parent", &format!("{id:?}"))
        })
    }

    /// Set the `clone-of` key (empty clears it, leaving the key as `""`).
    pub fn set_clone_of(&self, expected: ContentHash, id: &str) -> Result<()> {
        self.write_checked(expected, |text| {
            set_meta(text, "clone-of", &format!("{id:?}"))
        })
    }

    /// Take ids off the `related` list. Returns whether the file changed.
    pub fn remove_related(&self, expected: ContentHash, id: &str) -> Result<bool> {
        let mut related = self.related();
        let before = related.len();
        related.retain(|known| known != id);
        if related.len() == before {
            return Ok(false);
        }
        self.write_checked(expected, |text| {
            set_meta(text, "related", &format!("[{}]", related.join(",")))
        })?;
        Ok(true)
    }

    /// Take a copy's id off the `clones` list. Returns whether the file changed.
    pub fn remove_clone(&self, expected: ContentHash, id: &str) -> Result<bool> {
        let mut clones = self.clones();
        let before = clones.len();
        clones.retain(|known| known != id);
        if clones.len() == before {
            return Ok(false);
        }
        self.write_checked(expected, |text| {
            set_meta(text, "clones", &format!("[{}]", clones.join(",")))
        })?;
        Ok(true)
    }

    /// Take a child's wikilink out of the `## Children` list. Returns
    /// whether the file changed.
    pub fn remove_child_link(&self, expected: ContentHash, stem: &str) -> Result<bool> {
        let text = crate::eol::read(&self.path)
            .with_context(|| format!("reading {}", self.path.display()))?;
        let link = format!("- [[{stem}]]");
        if !text.lines().any(|line| line.trim() == link) {
            return Ok(false);
        }
        self.write_checked(expected, |text| {
            let mut out: String = text
                .lines()
                .filter(|line| line.trim() != link)
                .map(|line| format!("{line}\n"))
                .collect();
            if !text.ends_with('\n') {
                out.pop();
            }
            Ok(out)
        })?;
        Ok(true)
    }

    /// Record a child under a `## Children` heading, as a wikilink an editor
    /// can follow. The heading is added when it isn't there yet.
    pub fn add_child_link(&self, expected: ContentHash, stem: &str) -> Result<bool> {
        let text = crate::eol::read(&self.path)
            .with_context(|| format!("reading {}", self.path.display()))?;
        let link = format!("- [[{stem}]]");
        if text.lines().any(|line| line.trim() == link) {
            return Ok(false);
        }
        self.write_checked(expected, |text| {
            let mut updated = text.trim_end().to_string();
            if !text.contains(CHILDREN_HEADING) {
                updated.push_str("\n\n");
                updated.push_str(CHILDREN_HEADING);
                updated.push('\n');
            }
            // Append under the heading: children are listed in creation
            // order, which is id order.
            updated.push('\n');
            updated.push_str(&link);
            updated.push('\n');
            Ok(updated)
        })?;
        Ok(true)
    }

    /// The branch this ticket is worked on, as recorded when it was created.
    /// Editing `branch-name` changes what `rite todo` switches to on entry.
    /// Falls back to the old `branch` key.
    pub fn branch(&self) -> Option<String> {
        self.meta("branch-name")
            .or_else(|| self.meta("branch"))
            .filter(|branch| !branch.is_empty())
    }
}

/// `[003,007]` -> `["003", "007"]`. Tolerant of spaces and quotes, since a
/// human may well have typed the line.
/// A tag as it's stored (ticket 122): trimmed, unquoted and UPPERCASE, so
/// tags stand out. Every writer stores tags this way; every match compares
/// in this form, so a ticket written before that still matches.
pub fn normalize_tag(tag: &str) -> String {
    tag.trim().trim_matches(['"', '\'']).to_uppercase()
}

/// The tag that makes a ticket a draft (ticket 114). A draft can't leave
/// the backlog, and can't go into a sprint, until the tag is taken off.
pub const DRAFT_TAG: &str = "DRAFT";

/// The tag an imported GitHub issue arrives with (ticket 158): new work
/// nobody has looked at yet. Unlike DRAFT it holds nothing back; taking it
/// off is what triaging a ticket means.
pub const TRIAGE_TAG: &str = "TRIAGE";

/// Tags the tool itself gives a meaning to. They're always allowed, even
/// when a project only allows its listed tags, always offered, and can't be
/// taken off the project's list.
pub const RESERVED_TAGS: [&str; 2] = [DRAFT_TAG, TRIAGE_TAG];

/// Whether `tag` is one of [`RESERVED_TAGS`] (ignoring case).
pub fn is_reserved_tag(tag: &str) -> bool {
    let tag = normalize_tag(tag);
    RESERVED_TAGS.contains(&tag.as_str())
}

/// [`normalize_tag`] over a list, keeping the first of any duplicates and
/// dropping empty ones.
pub fn normalize_tags(tags: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in tags.iter().map(|t| normalize_tag(t)) {
        if !tag.is_empty() && !out.contains(&tag) {
            out.push(tag);
        }
    }
    out
}

/// Refuses a tag that isn't one word: empty, or holding whitespace, a
/// comma, a bracket or a quote, any of which would break the `tags: [..]`
/// list it's written into.
pub fn validate_tag(tag: &str) -> Result<()> {
    if tag.is_empty() {
        bail!("a tag can't be empty");
    }
    if let Some(bad) = tag
        .chars()
        .find(|c| c.is_whitespace() || matches!(c, ',' | '[' | ']' | '"' | '\''))
    {
        bail!("'{tag}' isn't one word: a tag can't contain {bad:?}");
    }
    Ok(())
}

/// `text` with its `tags` list in uppercase ([`normalize_tag`]), unchanged
/// when there's no `tags` key or it's already so.
fn uppercase_tags(text: &str) -> Result<String> {
    let Some(raw) = meta_in(text, "tags") else {
        return Ok(text.to_string());
    };
    let tags = parse_list(raw);
    let upper = normalize_tags(&tags);
    if upper == tags {
        return Ok(text.to_string());
    }
    set_meta(text, "tags", &format!("[{}]", upper.join(",")))
}

/// The `assignees` key (ticket 254).
pub const ASSIGNEES: &str = "assignees";

/// `text` assigned to `names`, in order (ticket 254): `assignee` is the
/// first (empty for nobody); an `assignees` list is written only for two or
/// more, quoted so a comma in a name survives, and removed otherwise.
/// Blank and repeated names are dropped.
pub fn with_assignees(text: &str, names: &[String]) -> Result<String> {
    let mut list: Vec<String> = Vec::new();
    for name in names {
        let name = name.trim();
        if !name.is_empty() && !list.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            list.push(name.to_string());
        }
    }
    let first = list.first().cloned().unwrap_or_default();
    let text = set_meta(text, "assignee", &format!("{first:?}"))?;
    if list.len() > 1 {
        let quoted: Vec<String> = list.iter().map(|n| format!("{n:?}")).collect();
        set_meta(&text, ASSIGNEES, &format!("[{}]", quoted.join(",")))
    } else {
        Ok(remove_meta(&text, ASSIGNEES))
    }
}

/// [`Task::assignees`], read from a ticket's text.
pub fn assignees_in(text: &str) -> Vec<String> {
    let Some(first) = meta_in(text, "assignee")
        .or_else(|| meta_in(text, "owner"))
        .filter(|who| !who.is_empty())
        .map(str::to_string)
    else {
        return Vec::new();
    };
    let list = meta_in(text, ASSIGNEES)
        .map(parse_names)
        .unwrap_or_default();
    if list.first() == Some(&first) {
        list
    } else {
        vec![first]
    }
}

/// `current` with `add` put after it and `remove` taken out, each name once
/// and compared ignoring case; the first left is the main assignee.
pub fn regroup(current: &[String], add: &[String], remove: &[String]) -> Vec<String> {
    let same = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());
    let mut out: Vec<String> = Vec::new();
    for who in current.iter().chain(add) {
        let who = who.trim();
        if !who.is_empty()
            && !out.iter().any(|o| same(o, who))
            && !remove.iter().any(|r| same(r, who))
        {
            out.push(who.to_string());
        }
    }
    out
}

/// A flow list of names, `["Alex Morgan","Sam Lee"]`: like [`parse_list`],
/// but a quoted name may hold a comma.
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

pub fn parse_list(raw: &str) -> Vec<String> {
    raw.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|item| item.trim().trim_matches(['"', '\'']).to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

/// The only sizes a ticket's `points` may take — standard sprint-planning
/// Fibonacci, fixed rather than configurable: nobody asked for a board to
/// pick its own scale, and a fixed list is what makes the GUI's points
/// field a dropdown rather than free text in the first place.
pub const VALID_POINTS: [&str; 6] = ["1", "2", "3", "5", "8", "13"];

/// A ticket's priority, lowest first (ticket 083). Every ticket has one;
/// a ticket without the field reads as [`DEFAULT_PRIORITY`]. `top` (ticket
/// 252) is above high: what a TRIAGE ticket is, and what anyone can choose.
pub const PRIORITIES: [&str; 4] = ["low", "medium", "high", TOP];

/// The most urgent priority (ticket 252): every TRIAGE ticket's.
pub const TOP: &str = "top";

/// When a ticket entered todo, and doing (ticket 252).
pub const TODO_SINCE: &str = "todo-since";
pub const DOING_SINCE: &str = "doing-since";

/// The priority of a new ticket, and of one that doesn't say.
pub const DEFAULT_PRIORITY: &str = "low";

pub(crate) fn validate_priority(priority: &str) -> Result<()> {
    if PRIORITIES.contains(&priority) {
        return Ok(());
    }
    bail!(
        "invalid priority '{priority}' — must be one of {}",
        PRIORITIES.join(", ")
    );
}

/// `text` with a priority: a missing or empty one becomes
/// [`DEFAULT_PRIORITY`]; an invalid one is refused, so a ticket can never be
/// written with a priority nobody can sort by. Text with no frontmatter is
/// returned as it is.
pub fn ensure_priority(text: &str) -> Result<String> {
    if !text.starts_with("---") {
        return Ok(text.to_string());
    }
    // A TRIAGE ticket is always top (tickets 248, 252), whatever it was given.
    if text_has_triage(text) {
        return set_meta(text, "priority", &format!("{TOP:?}"));
    }
    match meta_in(text, "priority").filter(|p| !p.is_empty()) {
        Some(priority) => {
            validate_priority(priority)?;
            Ok(text.to_string())
        }
        None => set_meta(text, "priority", &format!("{DEFAULT_PRIORITY:?}")),
    }
}

/// Whether a ticket's text carries the TRIAGE tag, case ignored.
fn text_has_triage(text: &str) -> bool {
    meta_in(text, "tags")
        .map(parse_list)
        .unwrap_or_default()
        .iter()
        .any(|t| t.trim().eq_ignore_ascii_case(TRIAGE_TAG))
}

/// Empty (unsized) or one of `VALID_POINTS` — checked here so every write
/// path (`set_points`, `apply_edit`) enforces it the same way, rather than
/// each call site remembering to.
fn validate_points(points: &str) -> Result<()> {
    if points.is_empty() || VALID_POINTS.contains(&points) {
        return Ok(());
    }
    bail!(
        "invalid points '{points}' — must be one of {} (or empty to clear)",
        VALID_POINTS.join(", ")
    );
}

/// Replace a frontmatter value, adding the key before the closing fence when
/// the ticket doesn't have it yet — tasks written by an older template, or by
/// hand, still get linked.
/// A `repository` value as a bare repository name: `acme/amd`,
/// `git@github.com:acme/amd.git` and `amd` all name `amd`.
pub fn repo_name(value: &str) -> String {
    let value = value.trim().trim_end_matches('/');
    let value = value.strip_suffix(".git").unwrap_or(value);
    value
        .rsplit(['/', ':'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Refuses a repository that isn't cloned on this machine (tickets 101/107):
/// you can only point a ticket at a repository you have. Empty — no
/// repository — is always fine.
pub fn check_repository(near: &Path, name: &str) -> Result<()> {
    if name.is_empty() {
        return Ok(());
    }
    crate::board_settings::validate_project_name(name)?;
    if crate::registry::clone_of(near, name).is_none() {
        bail!("{name} isn't cloned on this machine — clone it first (and register it)");
    }
    Ok(())
}

/// Write the `links` key: a flow list of `kind:id` entries.
fn set_links(text: &str, links: &[crate::links::Link]) -> Result<String> {
    let entries: Vec<String> = links.iter().map(|link| link.entry()).collect();
    set_meta(text, "links", &format!("[{}]", entries.join(",")))
}

/// `text` without the frontmatter line for `key`, if it has one.
pub(crate) fn remove_meta(text: &str, key: &str) -> String {
    if !text.starts_with("---") {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut closed = false;
    for (i, line) in text.lines().enumerate() {
        if i > 0 && !closed && line.trim_end() == "---" {
            closed = true;
        } else if i > 0
            && !closed
            && let Some(rest) = line.strip_prefix(key)
            && rest.starts_with(':')
        {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !text.ends_with('\n') {
        out.pop();
    }
    out
}

pub(crate) fn set_meta(text: &str, key: &str, value: &str) -> Result<String> {
    if !text.starts_with("---") {
        bail!("no frontmatter to update");
    }
    let mut out = String::with_capacity(text.len() + value.len());
    let mut lines = text.lines();
    let Some(open) = lines.next() else {
        bail!("no frontmatter to update");
    };
    out.push_str(open);
    out.push('\n');

    let mut written = false;
    let mut closed = false;
    for line in lines {
        if !closed && line.trim_end() == "---" {
            if !written {
                out.push_str(&format!("{key}: {value}\n"));
                written = true;
            }
            closed = true;
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if !closed
            && !written
            && let Some(rest) = line.strip_prefix(key)
            && rest.starts_with(':')
        {
            out.push_str(&format!("{key}: {value}\n"));
            written = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !closed {
        bail!("frontmatter is not closed");
    }
    Ok(out)
}

/// Search a task's frontmatter block (or, if it has none, the whole file) for
/// the first `key: value` line.
/// Split a task file into its frontmatter (fences included, trailing newline
/// kept) and the body below it. `None` when there is no closing fence, so a
/// half-written file is left alone rather than rewritten into nonsense.
pub fn split_body(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    // 1 for the newline the match starts with, 4 for "---\n".
    let head_len = "---\n".len() + end + 1 + "---\n".len();
    Some(text.split_at(head_len))
}

pub(crate) fn meta_in<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let mut lines = text.lines();
    let fenced = text.starts_with("---");
    if fenced {
        lines.next();
    }
    for line in lines {
        if fenced && line.trim_end() == "---" {
            break;
        }
        if let Some(rest) = line.strip_prefix(key)
            && let Some(value) = rest.strip_prefix(':')
        {
            return Some(unquote(value.trim()));
        }
    }
    None
}

pub(crate) fn unquote(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[bytes.len() - 1] == bytes[0]
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

/// `"Publish to Marketplace"` -> `"publish-to-marketplace"`.
///
/// The result is ASCII, because the slug is a filename and a branch name and
/// those are worth keeping boring. Letters and digits from any script are
/// **transliterated** rather than dropped, so `Ünïcödé` becomes `unicode` and
/// `北京` becomes `bei-jing` instead of collapsing to noise.
///
/// Symbols are not transliterated: an emoji in a title is decoration, and
/// `party 🎉` should be `party`, not `party-tada`.
pub fn slugify(title: &str) -> String {
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
            // A letter or digit from another script: use its closest ASCII.
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

/// `body` with `note` added at the end of its `## Notes` section, which is
/// made at the end if there isn't one.
pub fn with_note(body: &str, note: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let Some(start) = lines.iter().position(|l| l.trim() == "## Notes") else {
        let mut out = body.trim_end().to_string();
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        return format!("{out}## Notes\n\n{note}\n");
    };
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, l)| l.starts_with("## "))
        .map_or(lines.len(), |(i, _)| i);
    // Back over the blank lines before the next section.
    let mut at = end;
    while at > start + 1 && lines[at - 1].trim().is_empty() {
        at -= 1;
    }
    let mut out: Vec<String> = lines[..at].iter().map(|l| l.to_string()).collect();
    out.push(String::new());
    out.push(note.to_string());
    if end < lines.len() {
        out.push(String::new());
        out.extend(lines[end..].iter().map(|l| l.to_string()));
    }
    let mut text = out.join("\n");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_lowercases_and_joins_words() {
        assert_eq!(slugify("Publish to Marketplace"), "publish-to-marketplace");
    }

    #[test]
    fn slugify_collapses_runs_and_trims_edges() {
        assert_eq!(slugify("  --Fix:  the *thing*!  "), "fix-the-thing");
    }

    #[test]
    fn slugify_truncates_without_a_trailing_dash() {
        let slug = slugify(&"word ".repeat(30));
        assert!(slug.len() <= MAX_SLUG, "{slug}");
        assert!(!slug.ends_with('-'), "{slug}");
    }

    #[test]
    fn slugify_transliterates_rather_than_dropping() {
        assert_eq!(
            slugify(r#"Ünïcödé "quoted" ticket"#),
            "unicode-quoted-ticket"
        );
        assert_eq!(slugify("Größe ändern"), "grosse-andern");
        assert_eq!(slugify("Añadir sesión"), "anadir-sesion");
        assert_eq!(slugify("Москва"), "moskva");
        assert_eq!(slugify("北京"), "bei-jing");
    }

    #[test]
    fn slugify_treats_symbols_as_separators_not_words() {
        // deunicode would call this one "tada"; a branch name shouldn't say so.
        assert_eq!(slugify("party 🎉 time"), "party-time");
        assert_eq!(slugify("100% done"), "100-done");
    }

    #[test]
    fn slugify_is_empty_when_nothing_survives() {
        // The caller rejects these — an empty slug can't make a branch name.
        assert_eq!(slugify("***"), "");
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("🎉🎉"), "");
    }

    #[test]
    fn from_path_parses_the_id_prefix() {
        let task = Task::from_path(Path::new("/b/todo/007-do-it.md"), Column::Todo).unwrap();
        assert_eq!(task.id, Some(7));
        assert_eq!(task.stem, "007-do-it");
        assert_eq!(task.file_name(), "007-do-it.md");
    }

    #[test]
    fn from_path_ignores_non_markdown_and_unnumbered_files() {
        assert!(Task::from_path(Path::new("/b/todo/.gitkeep"), Column::Todo).is_none());
        let task = Task::from_path(Path::new("/b/todo/notes.md"), Column::Todo).unwrap();
        assert_eq!(task.id, None);
    }

    #[test]
    fn meta_reads_the_frontmatter_block_only() {
        let text = "---\nid: \"001\"\ntitle: \"First task\"\ntags: [x,y]\n---\n\n## Notes\n\ntitle: not this one\n";
        assert_eq!(meta_in(text, "id"), Some("001"));
        assert_eq!(meta_in(text, "title"), Some("First task"));
        assert_eq!(meta_in(text, "tags"), Some("[x,y]"));
        assert_eq!(meta_in(text, "missing"), None);
    }

    /// A ticket as Git for Windows checks it out, or Notepad saves it
    /// (ticket 277).
    const CRLF_TICKET: &str = "---\r\nid: \"001\"\r\ntitle: \"Windows ticket\"\r\n\
        priority: \"low\"\r\ntags: [UI]\r\n---\r\n\r\n## Notes\r\n\r\nWritten on Windows.\r\n";

    #[test]
    fn a_crlf_ticket_reads_like_any_other() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("001-windows-ticket.md");
        fs::write(&path, CRLF_TICKET).unwrap();
        let task = Task::from_path(&path, Column::Backlog).unwrap();
        assert_eq!(task.meta("title").as_deref(), Some("Windows ticket"));
        assert_eq!(task.meta("priority").as_deref(), Some("low"));
        assert_eq!(task.title(), "Windows ticket");
        assert!(task.has_tag("UI"));
        let body = task.body().unwrap();
        assert!(
            body.contains("Written on Windows.") && !body.contains('\r'),
            "{body:?}"
        );
    }

    #[test]
    fn editing_a_crlf_ticket_writes_lf_and_changes_only_that_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("001-windows-ticket.md");
        fs::write(&path, CRLF_TICKET).unwrap();
        let task = Task::from_path(&path, Column::Backlog).unwrap();
        task.set_priority(task.hash().unwrap(), "high").unwrap();
        let expected = CRLF_TICKET
            .replace("\r\n", "\n")
            .replace("priority: \"low\"", "priority: \"high\"");
        assert_eq!(fs::read_to_string(&path).unwrap(), expected);
    }

    #[test]
    fn meta_falls_back_to_the_whole_file_without_frontmatter() {
        assert_eq!(meta_in("# Heading\ntitle: Bare\n", "title"), Some("Bare"));
    }

    #[test]
    fn lists_are_parsed_leniently() {
        assert_eq!(parse_list("[003,007]"), ["003", "007"]);
        assert_eq!(parse_list("[ \"003\", 007 ]"), ["003", "007"]);
        assert_eq!(parse_list("[]"), [] as [String; 0]);
    }

    /// A task backed by a real file, so the read/write pair can be exercised.
    fn ranked(dir: &std::path::Path, name: &str, body: &str) -> Task {
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        Task::from_path(&path, Column::Todo).unwrap()
    }

    #[test]
    fn tags_reads_the_list_and_has_tag_ignores_case() {
        let dir = tempfile::tempdir().unwrap();
        let task = ranked(
            dir.path(),
            "001-a.md",
            "---\nid: \"001\"\ntitle: \"A\"\ntags: [DUPLICATE,urgent]\n---\n\nbody\n",
        );
        assert_eq!(task.tags(), ["DUPLICATE", "urgent"]);
        assert!(task.has_tag("DUPLICATE"));
        // Ticket 122: tags are uppercase, and a ticket written before that
        // still matches its tag in any case.
        assert!(task.has_tag("duplicate"));
        assert!(task.has_tag("URGENT"));
        assert!(!task.has_tag("missing"));
    }

    #[test]
    fn matches_tags_includes_any_wanted_tag_and_drops_any_unwanted_one() {
        let dir = tempfile::tempdir().unwrap();
        let tagged = |name: &str, tags: &str| {
            ranked(
                dir.path(),
                name,
                &format!("---\nid: \"001\"\ntitle: \"A\"\ntags: [{tags}]\n---\n\nbody\n"),
            )
        };
        let future = tagged("001-a.md", "FUTURE");
        let both = tagged("002-b.md", "FUTURE,BUSINESS");
        let business = tagged("003-c.md", "business");
        let none = tagged("004-d.md", "");
        let terms = |t: &[&str]| t.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // No terms (or only empty ones) pass everything.
        for task in [&future, &both, &business, &none] {
            assert!(task.matches_tags(&[]) && task.matches_tags(&terms(&["-", "#", "-#"])));
        }
        // Wanted tags: any one will do, case ignored, whole tags only.
        let wanted = terms(&["future", "BUSINESS"]);
        assert!(future.matches_tags(&wanted) && both.matches_tags(&wanted));
        assert!(business.matches_tags(&wanted) && !none.matches_tags(&wanted));
        assert!(!future.matches_tags(&terms(&["FUT"])));
        // Unwanted tags leave a ticket out; a # is allowed.
        assert!(!future.matches_tags(&terms(&["-FUTURE"])));
        assert!(business.matches_tags(&terms(&["-#FUTURE"])));
        assert!(none.matches_tags(&terms(&["-FUTURE"])));
        // Both together: tagged FUTURE and not BUSINESS.
        let narrowed = terms(&["FUTURE", "-BUSINESS"]);
        assert!(future.matches_tags(&narrowed) && !both.matches_tags(&narrowed));
        assert!(!business.matches_tags(&narrowed) && !none.matches_tags(&narrowed));
    }

    #[test]
    fn tags_normalise_to_one_uppercase_word() {
        assert_eq!(normalize_tag("  future "), "FUTURE");
        assert_eq!(normalize_tag("\"cli\""), "CLI");
        let tags = ["cli".to_string(), "CLI".into(), "".into(), "Ux".into()];
        assert_eq!(normalize_tags(&tags), ["CLI", "UX"]);
        assert!(validate_tag("FUTURE").is_ok());
        assert!(validate_tag("NEXT-RELEASE").is_ok());
        for bad in ["", "TWO WORDS", "A,B", "[X]", "\"Q\""] {
            assert!(validate_tag(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn saving_a_whole_ticket_stores_its_tags_in_uppercase() {
        let dir = tempfile::tempdir().unwrap();
        let task = ranked(
            dir.path(),
            "001-a.md",
            "---\nid: \"001\"\ntitle: \"A\"\npriority: \"low\"\ntags: [cli]\n---\n\nbody\n",
        );
        let edited = "---\nid: \"001\"\ntitle: \"A\"\npriority: \"low\"\ntags: [cli,future,Cli]\n---\n\nbody\n";
        task.set_text(task.hash().unwrap(), edited).unwrap();
        let saved = crate::eol::read(&task.path).unwrap();
        assert!(saved.contains("tags: [CLI,FUTURE]"), "{saved}");
        assert!(
            saved.ends_with("\n\nbody\n"),
            "the body is untouched: {saved}"
        );
    }

    #[test]
    fn body_is_everything_below_the_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let task = ranked(
            dir.path(),
            "001-a.md",
            "---\nid: \"001\"\ntitle: \"A\"\n---\n\n## Notes\n\nwords\n",
        );
        assert_eq!(task.body().unwrap(), "\n## Notes\n\nwords\n");

        task.set_body(task.hash().unwrap(), "\n## Notes\n\nrewritten\n")
            .unwrap();
        let text = crate::eol::read(&task.path).unwrap();
        assert!(
            text.starts_with("---\nid: \"001\"\ntitle: \"A\"\n---\n"),
            "{text}"
        );
        assert!(text.ends_with("rewritten\n"), "{text}");
        // Editing the body must not disturb the frontmatter.
        assert_eq!(task.title(), "A");
    }

    #[test]
    fn a_file_without_a_closing_fence_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let task = ranked(dir.path(), "001-a.md", "---\nid: \"001\"\nno fence here\n");
        assert!(task.set_body(task.hash().unwrap(), "new").is_err());
    }

    #[test]
    fn hash_changes_when_content_changes_and_not_otherwise() {
        let dir = tempfile::tempdir().unwrap();
        let task = ranked(
            dir.path(),
            "001-a.md",
            "---\nid: \"001\"\ntitle: \"A\"\n---\n\nbody\n",
        );
        let h1 = task.hash().unwrap();
        assert_eq!(task.hash().unwrap(), h1, "reading twice must not change it");

        task.set_title(h1, "B").unwrap();
        let h2 = task.hash().unwrap();
        assert_ne!(h2, h1, "the write must change the fingerprint");
    }

    /// docs/adr/0007-concurrency-and-locking.md's actual motivating scenario:
    /// a caller reads a ticket, something else changes it before the caller
    /// writes back, and the write must refuse rather than silently
    /// discarding the intervening change.
    #[test]
    fn a_write_against_a_stale_hash_is_refused_and_the_other_change_survives() {
        let dir = tempfile::tempdir().unwrap();
        let task = ranked(
            dir.path(),
            "001-a.md",
            "---\nid: \"001\"\ntitle: \"Original\"\nassignee: \"\"\n---\n\nbody\n",
        );
        let stale = task.hash().unwrap();

        // Someone else's write, after the hash was captured.
        task.assign(task.hash().unwrap(), "sam").unwrap();

        // The first caller, still holding the pre-assignment hash, tries to
        // write based on what it read before sam's change landed.
        let err = task.set_title(stale, "Renamed").unwrap_err();
        assert!(
            err.downcast_ref::<Conflict>().is_some(),
            "must be the typed Conflict error, not a generic one: {err}"
        );

        // sam's change is still there — not overwritten, not merged away.
        assert_eq!(task.assignee(), Some("sam".to_string()));
        assert_eq!(task.title(), "Original", "the stale write must not apply");
    }

    #[test]
    fn a_write_against_the_current_hash_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let task = ranked(
            dir.path(),
            "001-a.md",
            "---\nid: \"001\"\ntitle: \"Original\"\n---\n\nbody\n",
        );
        task.set_title(task.hash().unwrap(), "Renamed").unwrap();
        assert_eq!(task.title(), "Renamed");
    }

    /// A board with one existing ticket (`001`) to resolve `parent`/`related`
    /// refs against, matching the fixture `create.rs`'s own tests use.
    fn board_with_a_ticket() -> (tempfile::TempDir, Board) {
        let dir = tempfile::tempdir().unwrap();
        let board = Board::at(dir.path().join("tasks"));
        board.create().unwrap();
        ranked(
            &board.dir(Column::Backlog),
            "001-existing.md",
            "---\nid: \"001\"\ntitle: \"Existing\"\n---\n\nbody\n",
        );
        (dir, board)
    }

    fn no_edit() -> EditFields<'static> {
        EditFields {
            title: "",
            assignees: &[],
            points: "",
            tags: &[],
            branch_type: "",
            parent: "",
            related: &[],
            body: "",
        }
    }

    #[test]
    fn apply_edit_writes_every_field_in_one_pass() {
        let (_dir, board) = board_with_a_ticket();
        let task = ranked(
            &board.dir(Column::Backlog),
            "002-a.md",
            "---\nid: \"002\"\ntitle: \"Old\"\nassignee: \"\"\npoints: \"\"\n---\n\nold body\n",
        );
        let tags = ["urgent".to_string(), "gui".to_string()];
        let related = ["001".to_string()];
        let sam = ["sam".to_string()];
        task.apply_edit(
            task.hash().unwrap(),
            &board,
            EditFields {
                title: "New",
                assignees: &sam,
                points: "5",
                tags: &tags,
                branch_type: "feature",
                parent: "001",
                related: &related,
                body: "new body\n",
            },
        )
        .unwrap();
        assert_eq!(task.title(), "New");
        assert_eq!(task.assignee(), Some("sam".to_string()));
        assert_eq!(task.points(), Some("5".to_string()));
        assert_eq!(
            task.tags(),
            ["URGENT", "GUI"],
            "saved in uppercase (ticket 122)"
        );
        assert_eq!(task.branch_type(), Some("feature".to_string()));
        assert_eq!(task.branch(), Some("feature/new".to_string()));
        assert_eq!(task.parent(), Some("001".to_string()));
        assert_eq!(task.related(), ["001"]);
        assert_eq!(task.body().unwrap(), "new body\n");
    }

    #[test]
    fn apply_edit_refuses_an_unknown_parent_before_writing_anything() {
        let (_dir, board) = board_with_a_ticket();
        let task = ranked(
            &board.dir(Column::Backlog),
            "002-a.md",
            "---\nid: \"002\"\ntitle: \"Old\"\n---\n\nbody\n",
        );
        let err = task
            .apply_edit(
                task.hash().unwrap(),
                &board,
                EditFields {
                    parent: "99",
                    ..no_edit()
                },
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("no task matches '99'"), "{err}");
        assert_eq!(task.title(), "Old", "nothing must have been written");
    }

    #[test]
    fn apply_edit_refuses_an_unknown_branch_type_before_writing_anything() {
        let (_dir, board) = board_with_a_ticket();
        let task = ranked(
            &board.dir(Column::Backlog),
            "002-a.md",
            "---\nid: \"002\"\ntitle: \"Old\"\n---\n\nbody\n",
        );
        let err = task
            .apply_edit(
                task.hash().unwrap(),
                &board,
                EditFields {
                    branch_type: "feat",
                    ..no_edit()
                },
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown branch type 'feat'"), "{err}");
        assert_eq!(task.title(), "Old", "nothing must have been written");
    }

    /// The bug a naive four-separate-writes save would have: the hash
    /// captured once when a form opens can only ever match the file once,
    /// since the first write already changes it. apply_edit exists so this
    /// never happens — proved directly by showing the sequential version
    /// really would fail on its second call against the same hash.
    #[test]
    fn a_second_sequential_write_against_the_same_captured_hash_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let task = ranked(
            dir.path(),
            "001-a.md",
            "---\nid: \"001\"\ntitle: \"Old\"\nassignee: \"\"\n---\n\nbody\n",
        );
        let opened_at = task.hash().unwrap();
        task.set_title(opened_at, "New").unwrap();
        let err = task.assign(opened_at, "sam").unwrap_err();
        assert!(err.downcast_ref::<Conflict>().is_some(), "{err}");
    }

    #[test]
    fn set_title_changes_the_frontmatter_not_the_filename() {
        let dir = tempfile::tempdir().unwrap();
        let task = ranked(
            dir.path(),
            "001-old-slug.md",
            "---\nid: \"001\"\ntitle: \"Old\"\n---\n\nbody\n",
        );
        task.set_title(task.hash().unwrap(), "Brand new title")
            .unwrap();
        assert_eq!(task.title(), "Brand new title");
        // The slug is how history follows the file; renaming for a retitle
        // would break `git log --follow` for no gain.
        assert_eq!(task.stem, "001-old-slug");
        assert!(task.path.exists());
    }

    #[test]
    fn order_reads_a_rank_and_survives_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let task = ranked(
            dir.path(),
            "001-a.md",
            "---\nid: \"001\"\norder: \"1.5\"\n---\n\n## Notes\n",
        );
        assert_eq!(task.order(), Some(1.5));

        task.set_order(task.hash().unwrap(), 0.75).unwrap();
        assert_eq!(task.order(), Some(0.75));
        let text = crate::eol::read(&task.path).unwrap();
        assert!(text.ends_with("## Notes\n"), "{text}");
        assert_eq!(text.matches("order").count(), 1, "{text}");
    }

    #[test]
    fn an_unranked_task_has_no_order() {
        let dir = tempfile::tempdir().unwrap();
        // Missing, empty and unparseable all mean "not ranked" rather than
        // zero — a rank of 0 would sort a task to the front of its column.
        for body in [
            "---\nid: \"001\"\n---\n",
            "---\nid: \"001\"\norder: \"\"\n---\n",
            "---\nid: \"001\"\norder: \"soon\"\n---\n",
        ] {
            let task = ranked(dir.path(), "001-a.md", body);
            assert_eq!(task.order(), None, "{body}");
        }
    }

    #[test]
    fn set_order_adds_the_key_when_the_task_predates_ordering() {
        let dir = tempfile::tempdir().unwrap();
        let task = ranked(dir.path(), "001-a.md", "---\nid: \"001\"\n---\n\nbody\n");
        task.set_order(task.hash().unwrap(), -1.0).unwrap();
        assert_eq!(task.order(), Some(-1.0));
        assert!(
            crate::eol::read(&task.path).unwrap().ends_with("body\n"),
            "the body must survive"
        );
    }

    #[test]
    fn set_points_accepts_every_valid_value() {
        let dir = tempfile::tempdir().unwrap();
        let task = ranked(
            dir.path(),
            "001-a.md",
            "---\nid: \"001\"\npoints: \"\"\n---\n\nb\n",
        );
        for value in VALID_POINTS {
            task.set_points(task.hash().unwrap(), value).unwrap();
            assert_eq!(task.points(), Some(value.to_string()));
        }
    }

    #[test]
    fn set_points_clears_with_an_empty_string() {
        let dir = tempfile::tempdir().unwrap();
        let task = ranked(
            dir.path(),
            "001-a.md",
            "---\nid: \"001\"\npoints: \"5\"\n---\n\nb\n",
        );
        task.set_points(task.hash().unwrap(), "").unwrap();
        assert_eq!(task.points(), None);
    }

    #[test]
    fn set_points_rejects_a_non_fibonacci_value() {
        let dir = tempfile::tempdir().unwrap();
        let task = ranked(
            dir.path(),
            "001-a.md",
            "---\nid: \"001\"\npoints: \"\"\n---\n\nb\n",
        );
        let err = task
            .set_points(task.hash().unwrap(), "4")
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid points '4'"), "{err}");
        assert!(err.contains("1, 2, 3, 5, 8, 13"), "{err}");
        assert_eq!(task.points(), None, "the rejected write must not land");
    }

    #[test]
    fn priority_defaults_to_low_and_only_takes_the_three_values() {
        let dir = tempfile::tempdir().unwrap();
        let task = ranked(dir.path(), "001-a.md", "---\nid: \"001\"\n---\n\nb\n");
        assert_eq!(task.priority(), "low");
        task.set_priority(task.hash().unwrap(), "high").unwrap();
        assert_eq!(task.priority(), "high");
        let err = task
            .set_priority(task.hash().unwrap(), "urgent")
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid priority 'urgent'"), "{err}");
        assert!(err.contains("low, medium, high, top"), "{err}");
        assert_eq!(task.priority(), "high", "the rejected write must not land");
    }

    #[test]
    fn a_triage_ticket_is_always_top() {
        let text = "---\nid: \"001\"\ntags: [triage]\npriority: \"low\"\n---\n\nb\n";
        let saved = ensure_priority(text).unwrap();
        assert!(saved.contains("priority: \"top\""), "{saved}");
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("backlog")).unwrap();
        let path = dir.path().join("backlog/001-x.md");
        std::fs::write(&path, text).unwrap();
        let task = Task::from_path(&path, crate::board::Column::Backlog).unwrap();
        assert_eq!(
            task.priority(),
            "top",
            "read as top even before it's rewritten"
        );
        let err = task
            .set_priority(task.hash().unwrap(), "medium")
            .unwrap_err();
        assert!(err.to_string().contains("take TRIAGE off first"), "{err}");
        task.set_priority(task.hash().unwrap(), "top").unwrap();
    }

    #[test]
    fn ensure_priority_fills_a_missing_one_and_refuses_a_bad_one() {
        let filled = ensure_priority("---\nid: \"001\"\n---\n\nb\n").unwrap();
        assert_eq!(filled, "---\nid: \"001\"\npriority: \"low\"\n---\n\nb\n");
        let kept = "---\npriority: \"medium\"\n---\n";
        assert_eq!(ensure_priority(kept).unwrap(), kept);
        assert!(ensure_priority("---\npriority: \"soon\"\n---\n").is_err());
        assert_eq!(ensure_priority("no frontmatter").unwrap(), "no frontmatter");
    }

    #[test]
    fn apply_edit_also_rejects_a_non_fibonacci_points_value() {
        let (_dir, board) = board_with_a_ticket();
        let task = ranked(
            &board.dir(Column::Backlog),
            "002-a.md",
            "---\nid: \"002\"\ntitle: \"Old\"\nassignee: \"\"\npoints: \"\"\n---\n\nbody\n",
        );
        let sam = ["sam".to_string()];
        let err = task
            .apply_edit(
                task.hash().unwrap(),
                &board,
                EditFields {
                    title: "New",
                    assignees: &sam,
                    points: "7",
                    ..no_edit()
                },
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid points '7'"), "{err}");
        // Nothing else was written either -- validation happens before any
        // part of the combined write.
        assert_eq!(task.title(), "Old");
    }

    #[test]
    fn repo_name_reduces_any_repository_value_to_its_bare_name() {
        for value in [
            "amd",
            "acme/amd",
            "git@github.com:acme/amd.git",
            "https://github.com/acme/amd/",
            "  amd.git ",
        ] {
            assert_eq!(repo_name(value), "amd", "{value}");
        }
        assert_eq!(repo_name(""), "");
    }

    #[test]
    fn no_repository_is_always_allowed() {
        check_repository(Path::new("."), "").unwrap();
    }

    #[test]
    fn set_meta_replaces_an_existing_key() {
        let text = "---\nid: \"001\"\nrelated: []\n---\n\n## Notes\n";
        let updated = set_meta(text, "related", "[003]").unwrap();
        assert!(updated.contains("\nrelated: [003]\n"), "{updated}");
        assert!(updated.ends_with("## Notes\n"), "{updated}");
        assert_eq!(updated.matches("related").count(), 1, "{updated}");
    }

    #[test]
    fn set_meta_adds_a_missing_key_before_the_fence() {
        let text = "---\nid: \"001\"\n---\n\nbody\n";
        let updated = set_meta(text, "related", "[003]").unwrap();
        assert_eq!(updated, "---\nid: \"001\"\nrelated: [003]\n---\n\nbody\n");
    }

    #[test]
    fn set_meta_leaves_the_body_alone() {
        let text = "---\nid: \"001\"\n---\n\nrelated: not frontmatter\n";
        let updated = set_meta(text, "related", "[003]").unwrap();
        assert!(
            updated.contains("\nrelated: not frontmatter\n"),
            "{updated}"
        );
    }

    #[test]
    fn set_meta_needs_frontmatter() {
        assert!(set_meta("no frontmatter here\n", "related", "[1]").is_err());
        assert!(set_meta("---\nid: \"1\"\n", "related", "[1]").is_err());
    }

    #[test]
    fn unquote_strips_matching_quotes_only() {
        assert_eq!(unquote("\"First task\""), "First task");
        assert_eq!(unquote("'First task'"), "First task");
        assert_eq!(unquote("[x,y]"), "[x,y]");
        assert_eq!(unquote("\""), "\"");
    }

    // -- Leases (docs/adr/0007-concurrency-and-locking.md) ------------------

    /// A fresh git repo with `user.email` set to `email` — `git::config` is
    /// `-C`-scoped (see `git.rs`), so each test gets its own isolated
    /// identity with no process-wide state and no cross-test locking needed.
    fn identity(dir: &Path, email: &str) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["init", "-q"])
            .status()
            .unwrap();
        assert!(status.success());
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["config", "user.email", email])
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn future() -> String {
        (OffsetDateTime::now_utc() + time::Duration::hours(1))
            .format(&Rfc3339)
            .unwrap()
    }

    fn past() -> String {
        (OffsetDateTime::now_utc() - time::Duration::hours(1))
            .format(&Rfc3339)
            .unwrap()
    }

    #[test]
    fn take_lease_records_the_current_identity_and_a_future_expiry() {
        let dir = tempfile::tempdir().unwrap();
        identity(dir.path(), "me@example.com");
        let task = ranked(
            dir.path(),
            "001-a.md",
            "---\nid: \"001\"\ntitle: \"A\"\nlocked-by: \"\"\nlocked-until: \"\"\n---\n\nbody\n",
        );
        task.take_lease(task.hash().unwrap(), 8.0).unwrap();
        assert_eq!(task.locked_by(), Some("me@example.com".to_string()));
        assert!(task.lease_active());
        assert!(task.locked_until().unwrap() > OffsetDateTime::now_utc());
    }

    #[test]
    fn an_ordinary_setter_is_blocked_by_someone_elses_active_lease() {
        let dir = tempfile::tempdir().unwrap();
        identity(dir.path(), "me@example.com");
        let task = ranked(
            dir.path(),
            "001-a.md",
            &format!(
                "---\nid: \"001\"\ntitle: \"A\"\nlocked-by: \"other@example.com\"\nlocked-until: \"{}\"\n---\n\nbody\n",
                future()
            ),
        );
        let err = task.set_title(task.hash().unwrap(), "New").unwrap_err();
        assert!(err.to_string().contains("other@example.com"), "{err}");
        assert_eq!(task.title(), "A", "the blocked write must not have landed");
    }

    #[test]
    fn an_ordinary_setter_succeeds_when_the_active_lease_is_your_own() {
        let dir = tempfile::tempdir().unwrap();
        identity(dir.path(), "me@example.com");
        let task = ranked(
            dir.path(),
            "001-a.md",
            &format!(
                "---\nid: \"001\"\ntitle: \"A\"\nlocked-by: \"me@example.com\"\nlocked-until: \"{}\"\n---\n\nbody\n",
                future()
            ),
        );
        task.set_title(task.hash().unwrap(), "New").unwrap();
        assert_eq!(task.title(), "New");
    }

    fn leased(dir: &Path, holder: &str, until: &str) -> Task {
        ranked(
            dir,
            "001-a.md",
            &format!(
                "---\nid: \"001\"\ntitle: \"A\"\nlocked-by: \"{holder}\"\nlocked-until: \"{until}\"\n---\n\nbody\n"
            ),
        )
    }

    #[test]
    fn release_if_mine_releases_your_own_lease() {
        let dir = tempfile::tempdir().unwrap();
        identity(dir.path(), "me@example.com");
        let task = leased(dir.path(), "me@example.com", &future());
        assert!(task.release_if_mine().unwrap());
        assert_eq!(task.locked_by(), None);
        assert_eq!(task.locked_until(), None);
    }

    #[test]
    fn release_if_mine_clears_your_own_lapsed_lease_too() {
        let dir = tempfile::tempdir().unwrap();
        identity(dir.path(), "me@example.com");
        let task = leased(dir.path(), "me@example.com", &past());
        assert!(task.release_if_mine().unwrap());
        assert_eq!(task.locked_by(), None);
    }

    #[test]
    fn release_if_mine_never_touches_someone_elses_lease_active_or_lapsed() {
        for until in [future(), past()] {
            let dir = tempfile::tempdir().unwrap();
            identity(dir.path(), "me@example.com");
            let task = leased(dir.path(), "other@example.com", &until);
            assert!(!task.release_if_mine().unwrap());
            assert_eq!(task.locked_by(), Some("other@example.com".to_string()));
        }
    }

    #[test]
    fn release_if_mine_with_no_lease_does_nothing() {
        let dir = tempfile::tempdir().unwrap();
        identity(dir.path(), "me@example.com");
        let task = leased(dir.path(), "", "");
        let before = task.hash().unwrap();
        assert!(!task.release_if_mine().unwrap());
        assert_eq!(task.hash().unwrap(), before, "nothing was rewritten");
    }

    #[test]
    fn a_lapsed_lease_blocks_nothing() {
        let dir = tempfile::tempdir().unwrap();
        identity(dir.path(), "me@example.com");
        let task = ranked(
            dir.path(),
            "001-a.md",
            &format!(
                "---\nid: \"001\"\ntitle: \"A\"\nlocked-by: \"other@example.com\"\nlocked-until: \"{}\"\n---\n\nbody\n",
                past()
            ),
        );
        assert!(!task.lease_active());
        task.set_title(task.hash().unwrap(), "New").unwrap();
        assert_eq!(task.title(), "New");
    }

    #[test]
    fn take_lease_refuses_when_someone_else_holds_an_active_one() {
        let dir = tempfile::tempdir().unwrap();
        identity(dir.path(), "me@example.com");
        let task = ranked(
            dir.path(),
            "001-a.md",
            &format!(
                "---\nid: \"001\"\ntitle: \"A\"\nlocked-by: \"other@example.com\"\nlocked-until: \"{}\"\n---\n\nbody\n",
                future()
            ),
        );
        assert!(task.take_lease(task.hash().unwrap(), 8.0).is_err());
    }

    #[test]
    fn release_lease_only_clears_your_own() {
        let dir = tempfile::tempdir().unwrap();
        identity(dir.path(), "me@example.com");
        let until = future();
        let mine = ranked(
            dir.path(),
            "001-a.md",
            &format!(
                "---\nid: \"001\"\ntitle: \"A\"\nlocked-by: \"me@example.com\"\nlocked-until: \"{until}\"\n---\n\nbody\n"
            ),
        );
        mine.release_lease(mine.hash().unwrap()).unwrap();
        assert!(!mine.lease_active());

        let theirs = ranked(
            dir.path(),
            "002-b.md",
            &format!(
                "---\nid: \"002\"\ntitle: \"B\"\nlocked-by: \"other@example.com\"\nlocked-until: \"{until}\"\n---\n\nbody\n"
            ),
        );
        assert!(
            theirs.release_lease(theirs.hash().unwrap()).is_err(),
            "releasing someone else's lease isn't release, it's break_lease"
        );
        assert!(theirs.lease_active());
    }

    #[test]
    fn break_lease_clears_it_regardless_of_holder() {
        let dir = tempfile::tempdir().unwrap();
        identity(dir.path(), "me@example.com");
        let task = ranked(
            dir.path(),
            "001-a.md",
            &format!(
                "---\nid: \"001\"\ntitle: \"A\"\nlocked-by: \"other@example.com\"\nlocked-until: \"{}\"\n---\n\nbody\n",
                future()
            ),
        );
        task.break_lease(task.hash().unwrap()).unwrap();
        assert!(!task.lease_active());
        assert_eq!(task.locked_by(), None);
    }

    #[test]
    fn one_assignee_is_written_as_before_and_several_add_a_list() {
        let text = "---\nid: \"002\"\ntitle: \"A\"\nassignee: \"\"\n---\n\nbody\n";
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let one = with_assignees(text, &names(&["Sam Lee"])).unwrap();
        assert!(one.contains("assignee: \"Sam Lee\""), "{one}");
        assert!(!one.contains("assignees:"), "{one}");
        let two = with_assignees(text, &names(&["Sam Lee", "Jo, Jr", " sam lee "])).unwrap();
        assert!(two.contains("assignee: \"Sam Lee\""), "{two}");
        assert!(two.contains("assignees: [\"Sam Lee\",\"Jo, Jr\"]"), "{two}");
        // Back to one: the list goes.
        let back = with_assignees(&two, &names(&["Jo, Jr"])).unwrap();
        assert!(back.contains("assignee: \"Jo, Jr\""), "{back}");
        assert!(!back.contains("assignees:"), "{back}");
        let none = with_assignees(&two, &[]).unwrap();
        assert!(none.contains("assignee: \"\""), "{none}");
        assert!(!none.contains("assignees:"), "{none}");
    }

    #[test]
    fn the_list_counts_only_while_it_starts_with_the_assignee() {
        let dir = tempfile::tempdir().unwrap();
        let backlog = dir.path().join("backlog");
        fs::create_dir_all(&backlog).unwrap();
        let task = ranked(
            &backlog,
            "003-a.md",
            "---\nid: \"003\"\ntitle: \"A\"\nassignee: \"Sam\"\nassignees: [\"Sam\",\"Jo\"]\n---\n",
        );
        assert_eq!(task.assignees(), ["Sam", "Jo"]);
        assert!(task.is_assigned_to("jo"));
        assert!(!task.is_assigned_to("Ann"));
        // An older build changed `assignee` and left the list: the field wins.
        let stale = ranked(
            &backlog,
            "004-a.md",
            "---\nid: \"004\"\ntitle: \"A\"\nassignee: \"Ann\"\nassignees: [\"Sam\",\"Jo\"]\n---\n",
        );
        assert_eq!(stale.assignees(), ["Ann"]);
        assert!(!stale.is_assigned_to("Jo"));
    }

    #[test]
    fn regroup_adds_after_and_removes_ignoring_case() {
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            regroup(
                &names(&["Sam", "Jo"]),
                &names(&["ann", "jo"]),
                &names(&["SAM"])
            ),
            ["Jo", "ann"]
        );
        assert!(regroup(&names(&["Sam"]), &[], &names(&["sam"])).is_empty());
    }
}
