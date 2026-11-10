//! `rite recover` (ticket 144): bringing back tickets that were deleted or
//! abandoned. Nothing a commit has seen is lost: removing a ticket from the
//! board is itself a commit, and the ticket's text is still in that commit's
//! parent. This finds those removals in the board's history and restores a
//! ticket from them, into the loose backlog, exactly as it was.
//!
//! What it can't do is undo a force push that rewrote the remote's history;
//! branch protection stops that, and every other clone still holds the old
//! history (ticket 145).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::board::{Board, Column};
use crate::task::{self, Task};

/// A ticket that was on the board and isn't any more.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removed {
    pub id: u32,
    /// The file name without `.md`, e.g. `042-fix-the-thing`.
    pub stem: String,
    pub title: String,
    /// Where it was, relative to the repository root, before it went.
    pub path: String,
    /// The commit that removed it; its parent still holds the ticket.
    pub commit: String,
    pub author: String,
    pub email: String,
    /// `YYYY-MM-DD HH:MM`, the commit's author date.
    pub when: String,
    /// `rite abandon` took it off (its commit says so, or this machine's
    /// archive holds it), rather than a plain delete.
    pub abandoned: bool,
}

impl Removed {
    /// `042`, as ids are shown everywhere else.
    pub fn id_display(&self) -> String {
        format!("{:03}", self.id)
    }

    /// Whether `who` matches the author's name or email, ignoring case.
    pub fn by(&self, who: &str) -> bool {
        let who = who.to_lowercase();
        self.author.to_lowercase().contains(&who) || self.email.to_lowercase().contains(&who)
    }
}

/// Every ticket removed from `board` that isn't on it now: the latest
/// removal of each, oldest first. A ticket moved between columns or into
/// an epic is a rename, not a removal, so it never appears here.
pub fn removed(board: &Board) -> Result<Vec<Removed>> {
    let repo = board.repo_root();
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "log",
            "--diff-filter=D",
            "-M",
            "--name-only",
            "--date=format:%Y-%m-%d %H:%M",
            "--pretty=format:>>>%H%x1f%an%x1f%ae%x1f%ad%x1f%s",
            "--reverse",
            "--",
        ])
        .arg(&board.root)
        .output()
        .context("running git log")?;
    if !out.status.success() {
        bail!("git log failed: is this board in a git repository with history?");
    }
    let text = String::from_utf8_lossy(&out.stdout);

    // Keyed by stem so a ticket removed, restored and removed again shows
    // its latest removal once.
    let mut found: BTreeMap<String, Removed> = BTreeMap::new();
    let mut header: Option<(String, String, String, String, bool)> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(">>>") {
            let mut parts = rest.split('\u{1f}');
            let commit = parts.next().unwrap_or_default().to_string();
            let author = parts.next().unwrap_or_default().to_string();
            let email = parts.next().unwrap_or_default().to_string();
            let when = parts.next().unwrap_or_default().to_string();
            let subject = parts.next().unwrap_or_default();
            header = Some((commit, author, email, when, subject.starts_with("abandon")));
            continue;
        }
        let (Some((commit, author, email, when, abandoned)), false) = (&header, line.is_empty())
        else {
            continue;
        };
        let Some((id, stem)) = ticket_file(line) else {
            continue;
        };
        let Some(text) = file_at(repo, &format!("{commit}^"), line) else {
            continue;
        };
        found.insert(
            stem.clone(),
            Removed {
                id,
                stem,
                title: task::meta_text_in(&text, "title").unwrap_or_default(),
                path: line.to_string(),
                commit: commit.clone(),
                author: author.clone(),
                email: email.clone(),
                when: when.clone(),
                abandoned: *abandoned,
            },
        );
    }

    // `rite abandon` keeps a copy in the (untracked) archive, so a ticket
    // found there was abandoned even when its removal was committed by
    // hand, under some other message.
    let archive = board.archive_dir();
    for r in found.values_mut() {
        if archive.join(format!("{}.md", r.stem)).exists() {
            r.abandoned = true;
        }
    }
    let on_board: Vec<String> = board.tasks()?.into_iter().map(|t| t.stem).collect();
    let mut list: Vec<Removed> = found
        .into_values()
        .filter(|r| !on_board.contains(&r.stem))
        .collect();
    list.sort_by(|a, b| a.when.cmp(&b.when).then(a.id.cmp(&b.id)));
    Ok(list)
}

/// Puts `removed` back on `board`, in the loose backlog, as it was just
/// before it was removed. Refused if a ticket with its id is on the board
/// (ids are never reused, so that only happens after a clash, 146). Returns
/// where it landed. Writes the file only; committing is the caller's (the
/// auto-commit wraps this).
pub fn restore(board: &Board, removed: &Removed) -> Result<PathBuf> {
    if let Some(clash) = board
        .tasks()?
        .into_iter()
        .find(|t| t.id == Some(removed.id))
    {
        bail!(
            "{} can't come back: {} is on the board with that number",
            removed.id_display(),
            clash.stem
        );
    }
    let text = file_at(
        board.repo_root(),
        &format!("{}^", removed.commit),
        &removed.path,
    )
    .with_context(|| format!("reading {} from {}", removed.path, removed.commit))?;
    let dir = board.dir(Column::Backlog);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let dest = dir.join(format!("{}.md", removed.stem));
    if dest.exists() {
        bail!("{} already exists", dest.display());
    }
    fs::write(&dest, text).with_context(|| format!("writing {}", dest.display()))?;
    Ok(dest)
}

/// `(id, stem)` when `path` is a ticket file (`NNN-slug.md`), not a board
/// README, a group's `_group.md` or a template.
pub(crate) fn ticket_file(path: &str) -> Option<(u32, String)> {
    let path = Path::new(path);
    if path.components().any(|c| c.as_os_str() == "templates") {
        return None;
    }
    let task = Task::from_path(path, Column::Backlog)?;
    Some((task.id?, task.stem))
}

/// `path`'s text at `rev`, or `None` if it wasn't there.
fn file_at(repo: &Path, rev: &str, path: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["show", &format!("{rev}:{path}")])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| crate::eol::lf(String::from_utf8_lossy(&out.stdout).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    /// A repository with a board and three committed tickets.
    fn board_with_history(dir: &Path) -> Board {
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "sam@example.com"]);
        git(dir, &["config", "user.name", "Sam"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
        let board = Board::at(dir.join("tasks"));
        board.create().unwrap();
        for (id, slug) in [(1, "first"), (2, "second"), (3, "third")] {
            fs::write(
                board
                    .dir(Column::Backlog)
                    .join(format!("{id:03}-{slug}.md")),
                format!("---\nid: \"{id:03}\"\ntitle: \"The {slug}\"\n---\n\nbody {id}\n"),
            )
            .unwrap();
        }
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "seed"]);
        board
    }

    #[test]
    fn deleted_and_abandoned_tickets_are_found_and_come_back_as_they_were() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_with_history(dir.path());
        let first = board.dir(Column::Backlog).join("001-first.md");
        let before = crate::eol::read(&first).unwrap();

        git(dir.path(), &["rm", "-q", "tasks/backlog/001-first.md"]);
        git(
            dir.path(),
            &["commit", "-q", "-m", "abandon: 001 \"The first\""],
        );
        git(dir.path(), &["rm", "-q", "tasks/backlog/002-second.md"]);
        git(dir.path(), &["commit", "-q", "-m", "tidy up"]);

        let list = removed(&board).unwrap();
        assert_eq!(list.len(), 2, "{list:?}");
        assert_eq!((list[0].id, list[0].abandoned), (1, true));
        assert_eq!((list[1].id, list[1].abandoned), (2, false));
        assert_eq!(list[0].title, "The first");
        assert!(list[0].by("sam") && list[0].by("EXAMPLE"));

        let back = restore(&board, &list[0]).unwrap();
        assert_eq!(crate::eol::read(back).unwrap(), before);
        // Back on the board, it drops out of the list.
        let list = removed(&board).unwrap();
        assert_eq!(list.iter().map(|r| r.id).collect::<Vec<_>>(), [2]);
    }

    #[test]
    fn a_move_between_columns_is_not_a_removal() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_with_history(dir.path());
        fs::create_dir_all(board.dir(Column::Todo)).unwrap();
        git(
            dir.path(),
            &[
                "mv",
                "tasks/backlog/003-third.md",
                "tasks/todo/003-third.md",
            ],
        );
        git(dir.path(), &["commit", "-q", "-m", "todo: 003"]);
        assert!(removed(&board).unwrap().is_empty());
    }

    #[test]
    fn a_number_back_in_use_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let board = board_with_history(dir.path());
        git(dir.path(), &["rm", "-q", "tasks/backlog/001-first.md"]);
        git(dir.path(), &["commit", "-q", "-m", "remove"]);
        let gone = removed(&board).unwrap().remove(0);
        fs::write(
            board.dir(Column::Backlog).join("001-other.md"),
            "---\nid: \"001\"\ntitle: \"Other\"\n---\n",
        )
        .unwrap();
        let err = restore(&board, &gone).unwrap_err().to_string();
        assert!(err.contains("001-other"), "{err}");
    }
}
