//! Where new ticket numbers come from (ticket 102).
//!
//! **`local`** — the default, and what individuals and small teams get for
//! free: the board's own counter (`<repo-root>/.next-id`), with a scan of
//! the columns as a safety net (`Board::local_next_id`). Two people creating
//! tickets at the same moment can race for the same number; `sync.rs`
//! catches that before anything reaches the remote.
//!
//! **`cloud`** — a future paid plan, where a KanRite service hands out each
//! number, so a race can't happen at all. There are no plans yet: this is a
//! **stub** that refuses with an explanation. The seam is `Cloud::next_id`:
//! it will make an authenticated request for the project's next number (the
//! project id is the board's declared `project`), and `record_id` stays a
//! no-op because the service records what it hands out.
//!
//! Chosen by the project's `ticket-numbering` setting (`RITE_TICKET_NUMBERING`
//! overrides it), read through `board_settings`.

use anyhow::{Result, bail};

use crate::board::Board;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Numbering {
    Local,
    Cloud,
}

impl Numbering {
    /// The numbering this board's project has chosen. An unknown value is an
    /// error rather than a silent fall back to `local`: a team that set
    /// `cloud` must not quietly get colliding local numbers instead.
    pub fn for_board(board: &Board) -> Result<Numbering> {
        match crate::board_settings::load(board)
            .ticket_numbering()
            .as_str()
        {
            "local" => Ok(Numbering::Local),
            "cloud" => Ok(Numbering::Cloud),
            other => bail!("unknown ticket-numbering '{other}' — use 'local' or 'cloud'"),
        }
    }

    pub fn next_id(self, board: &Board) -> Result<u32> {
        match self {
            Numbering::Local => board.local_next_id(),
            Numbering::Cloud => cloud_unavailable(),
        }
    }

    pub fn record_id(self, board: &Board, used: u32) -> Result<()> {
        match self {
            Numbering::Local => board.local_record_id(used),
            // The service records what it hands out; nothing to keep here.
            Numbering::Cloud => Ok(()),
        }
    }
}

fn cloud_unavailable() -> Result<u32> {
    bail!(
        "cloud ticket numbering needs a KanRite plan, which isn't available yet — \
         set ticket-numbering = local (or unset RITE_TICKET_NUMBERING)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board_with(settings: &str) -> (tempfile::TempDir, Board) {
        let dir = tempfile::tempdir().unwrap();
        let board = Board::at(dir.path().join("tasks"));
        std::fs::create_dir_all(board.root.join("backlog")).unwrap();
        std::fs::write(dir.path().join(".settings"), settings).unwrap();
        (dir, board)
    }

    #[test]
    fn local_is_the_default_and_numbers_as_before() {
        let (_dir, board) = board_with("next-id = 7\n");
        assert_eq!(Numbering::for_board(&board).unwrap(), Numbering::Local);
        assert_eq!(board.next_id().unwrap(), 7);
        board.record_id(7).unwrap();
        assert_eq!(board.next_id().unwrap(), 8);
    }

    #[test]
    fn cloud_refuses_until_there_is_a_plan() {
        let (_dir, board) = board_with("ticket-numbering = cloud\n");
        assert_eq!(Numbering::for_board(&board).unwrap(), Numbering::Cloud);
        let err = board.next_id().unwrap_err().to_string();
        assert!(err.contains("needs a KanRite plan"), "{err}");
        board.record_id(3).unwrap();
    }

    #[test]
    fn an_unknown_numbering_is_an_error_not_a_silent_local() {
        let (_dir, board) = board_with("ticket-numbering = abacus\n");
        assert!(Numbering::for_board(&board).is_err());
        assert!(board.next_id().is_err());
    }
}
