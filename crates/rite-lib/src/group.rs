//! Groups: the folders a backlog is divided into.
//!
//! An **epic** is a body of work; a **sprint** is a slice of time. Both are a
//! directory under `backlog/` holding tickets, and both describe themselves in
//! a `_group.md` inside that directory — same shape as a ticket, so the board
//! stays readable and editable without the tool.
//!
//! The underscore keeps it at the top of a directory listing and out of the
//! ticket scan; it is deliberately not a dotfile, because a board you cannot
//! see the whole of is not a filesystem board.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// The file inside a group directory that describes it.
pub const GROUP_FILE: &str = "_group.md";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Epic,
    Sprint,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Epic => "epic",
            Kind::Sprint => "sprint",
        }
    }

    fn parse(value: &str) -> Kind {
        match value.trim().to_ascii_lowercase().as_str() {
            "sprint" => Kind::Sprint,
            // Anything else is an epic: a folder someone made by hand, with no
            // _group.md at all, is a perfectly good epic.
            _ => Kind::Epic,
        }
    }
}

/// Whether a sprint has been started. One way only — a sprint that has begun
/// cannot be returned to pending, because the point of the flag is to mark the
/// moment its contents stopped being negotiable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Pending,
    Started,
    /// Finished (ticket 052): read-only, nothing goes in or out.
    Closed,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::Started => "started",
            State::Closed => "closed",
        }
    }

    fn parse(value: &str) -> State {
        match value.trim().to_ascii_lowercase().as_str() {
            "started" => State::Started,
            "closed" => State::Closed,
            _ => State::Pending,
        }
    }
}

/// A sprint's length in days when nobody says otherwise.
pub const DEFAULT_DAYS: u32 = 14;

#[derive(Clone, Debug)]
pub struct Group {
    /// The directory itself, `…/backlog/<name>`.
    pub dir: PathBuf,
    /// The directory's name, which is what tickets record.
    pub name: String,
    pub kind: Kind,
    pub description: String,
    /// Sprints only; ignored for epics.
    pub days: u32,
    /// Sprints only; epics are always pending and nobody asks.
    pub state: State,
}

impl Group {
    /// Read a group from its directory. A directory with no `_group.md` is an
    /// epic with no description — that is what a folder someone created by
    /// hand means.
    pub fn read(dir: &Path) -> Result<Group> {
        let name = dir
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow::anyhow!("{} has no name", dir.display()))?
            .to_string();

        let text = crate::eol::read(dir.join(GROUP_FILE)).unwrap_or_default();
        Ok(Group {
            dir: dir.to_path_buf(),
            name,
            kind: field(&text, "kind").map_or(Kind::Epic, |v| Kind::parse(&v)),
            description: field(&text, "description").unwrap_or_default(),
            days: field(&text, "days")
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(DEFAULT_DAYS),
            state: field(&text, "started").map_or(State::Pending, |v| State::parse(&v)),
        })
    }

    pub fn is_sprint(&self) -> bool {
        self.kind == Kind::Sprint
    }

    /// Whether a ticket may still be moved in or out. A started sprint is a
    /// commitment; changing what is in it after the fact is what makes a
    /// sprint report meaningless.
    pub fn accepts_changes(&self) -> bool {
        !(self.is_sprint() && self.state != State::Pending)
    }

    /// Why it takes no changes, for a refusal: "has started" or "is
    /// closed".
    pub fn locked_word(&self) -> &'static str {
        if self.state == State::Closed {
            "is closed"
        } else {
            "has started"
        }
    }

    /// Close a started sprint (ticket 052): it becomes read-only. A pending
    /// one has nothing to close, and an epic has no timebox.
    pub fn close(&mut self) -> Result<()> {
        if !self.is_sprint() {
            bail!("{} is an epic, not a sprint", self.name);
        }
        match self.state {
            State::Pending => bail!("{} hasn't started, so there's nothing to close", self.name),
            State::Closed => bail!("{} is closed already", self.name),
            State::Started => {}
        }
        self.state = State::Closed;
        self.save()
    }

    /// Write the group's own file. Creates the directory if needed.
    pub fn save(&self) -> Result<()> {
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))?;
        let body = format!(
            "---\nkind: {:?}\nname: {:?}\ndescription: {:?}\ndays: {:?}\nstarted: {:?}\n---\n\n{}\n",
            self.kind.as_str(),
            self.name,
            self.description,
            self.days.to_string(),
            self.state.as_str(),
            self.description,
        );
        let path = self.dir.join(GROUP_FILE);
        fs::write(&path, body).with_context(|| format!("writing {}", path.display()))
    }

    /// Start a sprint. Refuses on an epic, and is a no-op on a sprint already
    /// started rather than an error, so a double click cannot fail.
    pub fn start(&mut self) -> Result<()> {
        if !self.is_sprint() {
            bail!("{} is an epic, not a sprint", self.name);
        }
        self.state = State::Started;
        self.save()
    }
}

/// Read one `key: "value"` line, the same forgiving parse tickets get.
fn field(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(key)
            && let Some(value) = rest.strip_prefix(':')
        {
            // Written with `{:?}`, so read with its escapes undone (tickets
            // 379, 384): a description holding `"` came back with
            // backslashes, and saving it again doubled them.
            return Some(crate::task::unescape(value.trim()));
        }
        if line == "---" && !text.starts_with("---") {
            break;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group_dir(kind: Kind, name: &str) -> (tempfile::TempDir, Group) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        let group = Group {
            dir: path,
            name: name.to_string(),
            kind,
            description: "why it exists".into(),
            days: DEFAULT_DAYS,
            state: State::Pending,
        };
        group.save().unwrap();
        (dir, group)
    }

    #[test]
    fn a_group_round_trips_through_its_file() {
        let (_dir, group) = group_dir(Kind::Sprint, "sprint-1");
        let read = Group::read(&group.dir).unwrap();
        assert_eq!(read.kind, Kind::Sprint);
        assert_eq!(read.name, "sprint-1");
        assert_eq!(read.description, "why it exists");
        assert_eq!(read.days, 14, "the default when nobody says otherwise");
        assert_eq!(read.state, State::Pending);
    }

    #[test]
    fn a_bare_directory_is_an_epic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("checkout");
        fs::create_dir_all(&path).unwrap();
        let group = Group::read(&path).unwrap();
        assert_eq!(group.kind, Kind::Epic, "a folder made by hand still works");
        assert!(group.description.is_empty());
        assert!(group.accepts_changes());
    }

    #[test]
    fn starting_a_sprint_is_one_way() {
        let (_dir, mut group) = group_dir(Kind::Sprint, "sprint-2");
        assert!(group.accepts_changes(), "pending sprints take tickets");

        group.start().unwrap();
        assert_eq!(Group::read(&group.dir).unwrap().state, State::Started);
        assert!(
            !group.accepts_changes(),
            "a started sprint is a commitment, not a list"
        );

        // Nothing offers a way back: there is no `stop`, and re-reading keeps
        // it started.
        group.start().unwrap();
        assert_eq!(Group::read(&group.dir).unwrap().state, State::Started);
    }

    #[test]
    fn epics_are_never_startable_and_always_open() {
        let (_dir, mut group) = group_dir(Kind::Epic, "checkout");
        assert!(group.start().is_err(), "an epic has no start");
        assert!(group.accepts_changes());
    }

    #[test]
    fn a_description_survives_editing() {
        let (_dir, mut group) = group_dir(Kind::Epic, "checkout");
        group.description = "rewritten".into();
        group.save().unwrap();
        assert_eq!(Group::read(&group.dir).unwrap().description, "rewritten");
    }

    #[test]
    fn a_description_with_quotes_round_trips() {
        // Tickets 379 and 384: read back as written, and saved again unchanged.
        let (_dir, mut group) = group_dir(Kind::Epic, "checkout");
        let words = r#"The "checkout" flow, C:\temp"#;
        group.description = words.into();
        group.save().unwrap();
        let read = Group::read(&group.dir).unwrap();
        assert_eq!(read.description, words);
        read.save().unwrap();
        assert_eq!(Group::read(&group.dir).unwrap().description, words);
    }
}
