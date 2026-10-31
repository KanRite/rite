//! rite — a tiny filesystem Kanban in markdown (agile-md).
//!
//! Tasks are markdown files that move between `todo/`, `doing/` and `done/` in
//! `<repo-root>/tasks`. Status is the folder; `git mv` is the audit trail; the
//! ticket format is a MiniJinja template you can edit.

mod completions;
mod docs;
mod form;
mod idea;
mod init;
mod join;
mod mcp;
mod project;
mod render;
mod update;
mod value;

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use clap::builder::PossibleValuesParser;
use clap::{Args, CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use time::format_description::well_known::Rfc3339;

use agile_md::board::{ABANDON, Board, Column, Kind};
use agile_md::create::{Draft, NewTicket};
use agile_md::registry::Registry;
use agile_md::templates::{DEFAULT_TEMPLATE, TEMPLATE_SUFFIX, Templates};
use agile_md::{board_settings, branch, git, task, templates, transition};

/// `rite import --help`'s examples and what each kind does (ticket 162).
const COLUMN_HELP: &str = "\
A board starts with Backlog, To do, Doing and Done. A team can add its own
columns, rename any of them, move them along and take its own ones away
again; every rule follows a column's kind, so a Review of kind doing is
locked while a ticket is in it and has its own WIP limit, and a Released of
kind done counts as done everywhere. A rename only changes the name: the
folder, and every ticket's history, stays put. The four standard columns
can be renamed and moved but not removed.

Examples:
  rite column add Review --kind doing --after doing
  rite column add Triage --kind backlog --before backlog
  rite column rename todo Ready
  rite column wip review 3
  rite column remove review --move-to doing
  rite review 12            move ticket 12 into Review (or: rite move 12 review)
  rite column allow triage todo    an extra one-way move, beside the next column
  rite column flow any             any column to any other
  rite column landing triage       new tickets arrive in Triage
  rite column reset                back to the standard four

Whatever the flow, a draft stays in the backlog, a started sprint's tickets
only move on to todo, done waits for what a ticket requires, and WIP limits
and locks apply.

Teammates need amd 4.1 or later to see tickets in an added column.";

const IMPORT_HELP: &str = "\
Stand where the things should go; name where they come from. Nothing is
overwritten: what's left out is printed and appended to import.log in the
folder you ran it from (never committed).

Kinds:
  tickets    Run from this board's tasks/ directory; FROM is another board's
             tasks/ directory. Each ticket keeps its column, gets a fresh
             number, and keeps parent and related links within the batch. A
             ticket whose title is already on this board is left out. One
             commit. The source board is never changed.
  documents  Run from the folder of a documents repository the files should
             go into; FROM is the folder to copy. Markdown, and the files it
             links to, are mirrored; hidden files are left out. Committed on
             the main branch and pushed.

GitHub issues aren't imported here: riteui pulls them in when its landing page
opens, and `rite issues` checks now.

Examples:
  cd acme-tickets/tasks && rite import tickets ../../old-project/tasks --dry-run
  cd acme-tickets/tasks && rite import ../../old-project/tasks   # a bare path is tickets
  cd acme-documents/docs && rite import documents ~/notes

--dry-run shows what would happen and writes nothing.";

const AFTER_HELP: &str = "\
A <REF> is a task id (e.g. 7 or 007) or a unique slug substring. Leave it out
of `rite new` in a terminal and rite fills in a form; leave it out of a
destination verb (see below) and rite lists that destination instead of
moving anything.

One kind of ticket. A branch type — feature, bugfix, hotfix, release,
chore — is what gives it a branch, so `rite todo` on a bugfix titled
\"Crash on save\" creates and switches to bugfix/crash-on-save when it enters
todo/. Leave the type out and there is nothing to check out: the ticket
still moves, the working tree is left alone.

New tickets land in backlog/, priority low. `rite set` sizes, prioritises
(low, medium, high) and files them; `rite group` divides the backlog into
epics and sprints — both optional, and a ticket moves in or out of either
one at a time with `rite set <ref> epic <name>`, or all at once with `rite
group empty <name>`; `rite group rename` renames one and relabels its
tickets. The loose backlog is just the default epic: a ticket in any epic
or sprint can move on to todo, and sent back from todo it returns to its
epic. A sprint takes only sized tickets, and once started it takes nothing
more and gives nothing back — its tickets only move on, to todo. `rite tag
<tag>` lists every ticket carrying it, across every column. Any listing
form — `board`, a bare destination verb, an epic by name — takes
`--tag <tag>` to narrow it to tickets carrying that tag instead; `--tag
-<tag>` leaves them out, and `--tag` can be repeated (`--tag FUTURE --tag
-BUSINESS`: tagged FUTURE, not BUSINESS; several wanted tags need just
one).

`doing/` carries a WIP limit (`RITE_WIP_LIMIT`, or `wip-limit` in
`.settings`; `rite wip-limit <n>` sets it) — unset or unparsable defaults to
2, and 0 turns it off. Only doing is limited, so work can't get stuck in
todo. A move into a full doing/ is refused, naming the limit and the
current count; `--force` overrides it. The limit only ever gates entry:
moving a ticket *out* of doing/ is never blocked.

Tickets nest: `--parent <ref>` puts one under another, and the board shows
the tree. Nesting is the parent/child relationship; an epic or sprint is a
folder the backlog is divided into. A ticket can have both.

`rite lock <ref>` claims a ticket's lease for your git identity
(`user.email`), so anyone else's edits or moves on it are refused with a
clear message until it's released or it lapses on its own (8 hours by
default). `rite unlock <ref>` releases your own; `rite unlock <ref> --force`
breaks someone else's, a separate, deliberate act. Entering `doing` claims
a free lease automatically, and leaving it gives yours back: a ticket is
locked while it's being worked on. A lease is advisory, not a guarantee:
it only stops local tools from writing, and a concurrent edit on another
clone still surfaces as a normal git merge conflict.

`rite todo`/`doing`/`done`/`backlog`/`abandon <ref>` move a ticket there,
one column at a time — a ticket can't jump from backlog/ to done/; leave
off <ref> and it lists that destination instead of moving anything.
The same works for any epic by name: `rite checkout 7` files ticket 7 under
the checkout epic, `rite checkout` lists what's in it. Entering `todo` is the
one with a side effect: it creates or switches to the ticket's branch, the
same way `rite start` used to. Abandoning a ticket deletes that branch too,
if it's still empty — nothing lost, nothing left behind.

`rite init <project>` creates a private `<project>-tickets` GitHub repository
(under the GitHub organisation or personal account named <project> — pass
--org to use a different owner), clones it as a child of the current
directory, scaffolds the board inside it, and registers it. Must be run
from a plain directory, never from inside an existing git repository —
`rite init <project> --local-only` scaffolds the board in the repository
you're standing in instead, with no network and nothing created remotely,
the project name still recorded, just without a dedicated tickets repo.
Either way, init records you — git's user.name and user.email — as the
project's first user. Running it again on an existing project adds you if
you're missing, so a project made before that gets its first user too.
<project> cannot be changed once set — every ticket records which
repository it's about using this name, and changing it later means finding
and updating every ticket by hand, on every clone, with no way to do it
atomically. Choose the name you'd want a stranger to see in a year.

`rite join <project>` is how everyone else gets the project: run from a
plain directory, it clones `<project>-tickets` and `<project>-documents`
side by side over plain git (your SSH key is all it needs, no gh token;
--org as for init, RITE_GIT_BASE to clone from somewhere other than
git@github.com:), registers both, and adds you — git's user.name and
user.email — to the project's `users`, pushed straight to the tickets
repository. It refuses if either directory is already there. Unlike
`rite register`, which records a repository already on this machine, join
is the one-off step that puts the project here in the first place.

A project's repositories that aren't on this machine show as (not cloned)
and can't be chosen for a ticket. `rite clone` lists them; `rite clone <name>`
(or `--all`) clones them beside the tickets repository, from wherever the
tickets repository came from (its origin, with the name swapped in), and
registers them.

Several projects can share one folder (agile-md-tickets beside
multi-user-test-tickets). `rite project` lists them and marks the one rite
uses; `rite project use <name>` switches, by writing .current-project in
that folder (this machine only, never committed). From the folder itself,
or a code repository beside the projects, rite follows that file; inside a
tickets repository, that repository's own board always wins. With several
projects and no choice made, rite says so rather than guessing. `rite init`
and `rite join` never switch you: adding a second project keeps the first
one current.

A board with a remote stays in step with it. Before a change, rite fetches
and fast-forwards your clone, so you never number a ticket or take a lease
from a stale copy; a listing does the same at most every 10 seconds. If
someone pushes in the moment before you do, rite replays your change on top
of theirs when it can, and otherwise undoes it, saves what you wrote, and
exits non-zero so you can run it again on the up-to-date board. No network
only warns. RITE_SYNC=0 (or `sync = off` in .settings) turns this off.

`rite mcp` lets an AI tool work on the board through rite (an MCP server):
it reads the board, tickets, documents, ideas and what riteui shows, and
creates, moves, edits, assigns, notes and files tickets under the same
rules, each committed. It never abandons, unlocks or changes settings. Your
AI tool starts it; `rite mcp setup` prints how to connect Claude Code,
VS Code, Cursor, Codex or Gemini CLI.

Environment:
  RITE_DIR           board directory name under the repository root (default: tasks)
  RITE_BRANCH_TYPES  comma-separated branch types (default: feature, bugfix,
                     hotfix, release, chore)
  RITE_YES           set to 1 to create a missing board without prompting
  RITE_NO_INPUT      set to 1 to never prompt (same as --no-input)
  RITE_NO_BRANCH     set to 1 to never create branches (same as --no-branch)
  NO_COLOR           set to any value for plain output (same as --plain)
  EDITOR             editor used by `rite edit` (default: vi)
  RITE_SYNC          set to 0 to stop catching up with the remote (default: on)
  RITE_TICKET_NUMBERING  local (default) or cloud — cloud needs a KanRite
                     plan, which isn't available yet
The AMD_ names from before the rename (AMD_DIR, AMD_YES, …) still work; a
RITE_ one wins when both are set. So does `amd`, which runs `rite`.

RITE_BRANCH_TYPES and EDITOR can also be set in the settings file
(`branch-types`, `editor`) — the environment variable wins when both are set.

Creating a task interactively ends in $EDITOR with the rendered ticket, so
the notes and checklist get filled in there and then (--no-edit to skip).

Tasks are rendered from MiniJinja templates. `rite templates` lists them and
`rite templates eject task` writes an editable copy into <board>/templates/.
A template that uses `extra.<name>` gets asked for it in the form.";

#[derive(Parser, Debug)]
#[command(
    name = "rite",
    version,
    about = "A tiny filesystem Kanban: markdown tasks in todo/, doing/ and done/",
    after_help = AFTER_HELP,
    disable_help_subcommand = false
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
    /// Never prompt; missing values are an error instead
    #[arg(long, global = true)]
    no_input: bool,
    /// Plain text output instead of the rich board
    #[arg(long, global = true)]
    plain: bool,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Create a project's tickets and documents repositories
    ///
    /// Creates or connects {project}-tickets and {project}-documents and
    /// records you as one of the project's users
    Init {
        /// The project's declared identity — cannot be changed once set
        #[arg(value_name = "PROJECT")]
        project: String,
        /// Scaffold the board in this repository instead of a dedicated
        /// tickets repository — no network, nothing created remotely
        #[arg(long)]
        local_only: bool,
        /// GitHub org/user to create the tickets repository under
        /// (default: the project name itself)
        #[arg(long, value_name = "ORG")]
        org: Option<String>,
    },
    /// Join an existing project
    ///
    /// Clones its tickets and documents repositories here over plain git
    /// and adds you to its users
    Join {
        /// The project to join, as `rite init` named it
        #[arg(value_name = "PROJECT")]
        project: String,
        /// GitHub org/user the repositories live under
        /// (default: the project name itself)
        #[arg(long, value_name = "ORG")]
        org: Option<String>,
    },
    /// List the projects in this folder, switch between them, or archive one
    ///
    /// Shows which one rite uses; `rite project use <name>` switches, and
    /// `rite project archive [name]` puts one away for good (`rite init
    /// <name>` brings it back)
    Project {
        #[command(subcommand)]
        action: Option<ProjectAction>,
    },
    /// Create a task in backlog/ from a template
    New(NewArgs),
    /// Show all columns (the default command)
    Board {
        /// Only show tasks carrying this tag; -TAG leaves them out; repeatable
        #[arg(
            short = 't',
            long = "tag",
            value_name = "TAG",
            allow_hyphen_values = true
        )]
        tag: Vec<String>,
    },
    /// List every ticket carrying a tag
    Tag {
        #[arg(value_name = "TAG")]
        tag: String,
    },
    /// Show or change the board's columns
    ///
    /// With no action, each column: its name, folder, kind (backlog, todo,
    /// doing or done: what it does), WIP limit and how many tickets it
    /// holds. Every change is committed like any other setting
    #[command(after_help = COLUMN_HELP)]
    Column {
        #[command(subcommand)]
        action: Option<ColumnAction>,
    },
    /// Make a new repository for the project (or connect one that already
    /// exists), clone it beside the tickets repository and register it
    Repo {
        #[command(subcommand)]
        action: RepoAction,
    },
    /// Move a ticket to any column by its name or folder
    ///
    /// The same as the column's own verb (`rite review 12`), and the way to
    /// reach a column named like one of rite's own commands
    Move {
        #[arg(value_name = "REF")]
        task: String,
        #[arg(value_name = "COLUMN")]
        column: String,
        /// Move into a doing column at its WIP limit anyway
        #[arg(long)]
        force: bool,
    },
    /// Show or change the project's tag list
    ///
    /// With no action, every listed tag and how many tickets carry it. Tags
    /// are always UPPERCASE
    Tags {
        #[command(subcommand)]
        action: Option<TagsAction>,
    },
    /// Import tickets from another board, or documents, into where you are
    #[command(after_help = IMPORT_HELP)]
    Import {
        /// What to import: `tickets` or `documents`. A path on its own is a
        /// tickets import from that board
        #[arg(value_name = "KIND")]
        first: String,
        /// Where from. tickets: another board's tasks directory (holding
        /// backlog/ todo/ doing/ done/). documents: the folder to copy
        #[arg(value_name = "FROM")]
        path: Option<PathBuf>,
        /// Preview what would happen without writing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Read the project's documents: the start page, a search, the page tree
    ///
    /// The start page is the one pinned in riteui, else the README. Changes
    /// are made in the documents repository itself, like any clone
    Docs {
        #[command(subcommand)]
        action: DocsAction,
    },
    /// Let an AI assistant read and change the board (an MCP server)
    ///
    /// Your AI tool (Claude Code, Copilot in VS Code, Cursor, Codex, Gemini
    /// CLI…) starts `rite mcp` and talks to it on stdin and stdout; you don't
    /// run it yourself. Its tools read the board, tickets, documents, ideas
    /// and what riteui shows, and create, move, edit, assign, note and file
    /// tickets, each committed like any rite change. `rite mcp setup [tool]`
    /// prints how to connect one
    Mcp {
        #[command(subcommand)]
        action: Option<McpAction>,
    },
    /// The project's ideas, best first; put one forward, vote, add notes
    ///
    /// Ideas live in the project's documents repository, in ideas/.
    /// Everyone votes up to three + or three − on each; the
    /// best-voted come first. `rite idea ticket <ref>` makes a backlog
    /// ticket from one
    Idea {
        #[command(subcommand)]
        action: Option<IdeaAction>,
        /// Include archived ideas in the list
        #[arg(long)]
        all: bool,
    },
    /// Pull in open GitHub issues as TRIAGE tickets now, or switch the
    /// automatic pull on or off
    ///
    /// riteui pulls them in by itself when its landing page opens, for each
    /// project whose switch is on (the default). With no action, this
    /// checks now: every declared repository on GitHub, or --repo
    Issues {
        /// `on` or `off`: whether the landing page pulls this project's
        /// issues in
        #[arg(value_name = "on|off", value_parser = PossibleValuesParser::new(["on", "off"]))]
        action: Option<String>,
        /// Check this repository (owner/repo) instead of the declared ones
        #[arg(long, value_name = "OWNER/REPO")]
        repo: Option<String>,
        /// Only issues with this GitHub label
        #[arg(long, value_name = "LABEL")]
        label: Option<String>,
        /// Show what would come in without writing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Move a task into backlog/, or list it with no ref
    Backlog {
        #[arg(value_name = "REF")]
        task: Option<String>,
        /// With no ref, only show tasks carrying this tag; -TAG leaves them
        /// out; repeatable
        #[arg(
            short = 't',
            long = "tag",
            value_name = "TAG",
            allow_hyphen_values = true
        )]
        tag: Vec<String>,
    },
    /// Move a task into todo/, or list it with no ref
    ///
    /// Entering todo/ creates or switches to the ticket's branch
    Todo {
        #[arg(value_name = "REF")]
        task: Option<String>,
        /// Branch to create instead of the one in the ticket
        #[arg(short, long, value_name = "NAME")]
        branch: Option<String>,
        /// Move the task without touching branches
        #[arg(long)]
        no_branch: bool,
        /// With no ref, only show tasks carrying this tag; -TAG leaves them
        /// out; repeatable
        #[arg(
            short = 't',
            long = "tag",
            value_name = "TAG",
            allow_hyphen_values = true
        )]
        tag: Vec<String>,
        /// Move it even if todo/ is already at its WIP limit
        #[arg(long)]
        force: bool,
    },
    /// Move a task into doing/, or list it with no ref
    Doing {
        #[arg(value_name = "REF")]
        task: Option<String>,
        /// With no ref, only show tasks carrying this tag; -TAG leaves them
        /// out; repeatable
        #[arg(
            short = 't',
            long = "tag",
            value_name = "TAG",
            allow_hyphen_values = true
        )]
        tag: Vec<String>,
        /// Move it even if doing/ is already at its WIP limit
        #[arg(long)]
        force: bool,
    },
    /// Move a task into done/, or list it with no ref
    Done {
        #[arg(value_name = "REF")]
        task: Option<String>,
        /// With no ref, only show tasks carrying this tag; -TAG leaves them
        /// out; repeatable
        #[arg(
            short = 't',
            long = "tag",
            value_name = "TAG",
            allow_hyphen_values = true
        )]
        tag: Vec<String>,
    },
    /// Take a ticket off the board into archive/, or list the archive with
    /// no ref
    Abandon {
        #[arg(value_name = "REF")]
        task: Option<String>,
        /// With no ref, only show tasks carrying this tag; -TAG leaves them
        /// out; repeatable
        #[arg(
            short = 't',
            long = "tag",
            value_name = "TAG",
            allow_hyphen_values = true
        )]
        tag: Vec<String>,
    },
    /// Fix tickets that share a number
    ///
    /// One keeps it (the first committed), the others get new numbers.
    /// Shows the plan; --apply carries it out
    Renumber {
        /// Carry out the plan, in one commit
        #[arg(long)]
        apply: bool,
        /// The ticket to keep its number (its name, or a unique part of it)
        #[arg(long, value_name = "TICKET")]
        keep: Option<String>,
    },
    /// Bring back deleted or abandoned tickets
    ///
    /// With no number, lists them: who removed each and when. Restored
    /// tickets go to the backlog exactly as they were, in one commit
    Recover {
        /// Ticket numbers to restore
        #[arg(value_name = "ID")]
        ids: Vec<String>,
        /// Restore every listed ticket (narrowed by --by and --since)
        #[arg(long, conflicts_with = "ids")]
        all: bool,
        /// Only tickets removed by this person (name or email, any part)
        #[arg(long, value_name = "WHO")]
        by: Option<String>,
        /// Only tickets removed on or after this date (YYYY-MM-DD)
        #[arg(long, value_name = "DATE")]
        since: Option<String>,
    },
    /// Every ticket linked to a ticket, in either direction
    ///
    /// Requires, related, parent and children, clones, supersedes: whether
    /// the link is recorded on this ticket or only on the other one (a
    /// one-sided link from --one-way, a hand edit or an import), each with
    /// where it is
    #[command(alias = "related")]
    Links {
        #[arg(value_name = "REF")]
        task: String,
        /// Print JSON, for scripts
        #[arg(long)]
        json: bool,
    },
    /// Draw how tickets hang together: what each requires (solid) and
    /// what it relates to (dashed)
    ///
    /// With a ticket, its family: every ticket joined to it by requires or
    /// related. Without, every family on the board. Tickets in a started
    /// sprint aren't expanded. Each line says where the ticket is, and
    /// "waits on" names what it requires that isn't done. Done tickets are
    /// left out unless --done. With --children, parent and child instead:
    /// the ticket's ancestors and everything under it
    Tree {
        #[arg(value_name = "REF")]
        task: Option<String>,
        /// Include done tickets
        #[arg(long)]
        done: bool,
        /// Parent and child (the hierarchy) instead of requires and related
        #[arg(long)]
        children: bool,
    },
    /// Print a task
    Show {
        #[arg(value_name = "REF")]
        task: Option<String>,
    },
    /// Open a task in $EDITOR
    Edit {
        #[arg(value_name = "REF")]
        task: Option<String>,
    },
    /// Assign a ticket to someone, or to several people
    ///
    /// The names replace whoever is assigned, the first being the main
    /// assignee; `--add` and `--remove` change the list instead. With no
    /// name, picks one of the project's users in a terminal (and unassigns
    /// elsewhere)
    Assign {
        #[arg(value_name = "REF")]
        task: String,
        /// Who it belongs to; `@me` is your git user name, empty unassigns
        #[arg(value_name = "WHO", conflicts_with = "clear")]
        who: Vec<String>,
        /// Add these people after whoever is assigned
        #[arg(long, conflicts_with_all = ["remove", "clear"], requires = "who")]
        add: bool,
        /// Take these people off
        #[arg(long, conflicts_with = "clear", requires = "who")]
        remove: bool,
        /// Unassign everyone
        #[arg(long)]
        clear: bool,
    },
    /// Claim a ticket's lease so nobody else's writes land on it
    Lock {
        #[arg(value_name = "REF")]
        task: String,
    },
    /// Make a ticket a draft
    ///
    /// It's tagged DRAFT and stays in the backlog (and out of sprints) until
    /// `rite ready` takes it out of draft
    Draft {
        #[arg(value_name = "REF")]
        task: String,
    },
    /// Take a ticket out of draft, so it can leave the backlog
    Ready {
        #[arg(value_name = "REF")]
        task: String,
    },
    /// Release a ticket's lease
    Unlock {
        #[arg(value_name = "REF")]
        task: String,
        /// Break someone else's lease instead of only releasing your own
        #[arg(long)]
        force: bool,
    },
    /// Set a field on a ticket: points, epic, order, title, priority,
    /// repository, branch-type (a branch type, `none` for no branch, empty
    /// for not chosen; with a repository it must be one or the other),
    /// due (YYYY-MM-DD), recur (weekly, fortnightly, monthly,
    /// quarterly, yearly or `every N days|weeks|months|years`: done makes the
    /// next one), or requires (the tickets it requires, as a list:
    /// `rite set 12 requires 3,7`; both ends are linked, empty clears)
    Set {
        #[arg(value_name = "REF")]
        task: String,
        #[arg(
            value_name = "FIELD",
            value_parser = PossibleValuesParser::new(SET_FIELDS),
        )]
        field: String,
        /// The new value; empty clears the field
        #[arg(value_name = "VALUE", default_value = "")]
        value: String,
        /// More fields and values, all set in one commit: `rite set 12
        /// repository acme-api branch-type feature` gives a ticket past the
        /// backlog both halves of the pair at once
        #[arg(value_name = "FIELD VALUE")]
        more: Vec<String>,
    },
    /// Epics and sprints: the folders the backlog is divided into
    Group {
        #[command(subcommand)]
        command: Option<GroupCmd>,
    },
    /// Set doing/'s WIP limit. 0 turns it off; an absent or unparsable
    /// setting defaults to 2
    WipLimit {
        #[arg(value_name = "N")]
        limit: u32,
    },
    /// Show or switch auto priority: priorities set from requires links,
    /// due dates and time limits
    ///
    /// When on, a ticket is at least as urgent as the most urgent ticket
    /// that requires it, down a chain. It's raised as its due date nears, a
    /// step for each quarter of the time from its creation to its due date:
    /// medium from 25%, high from 50%, top from 75% (and once overdue). With time limits
    /// (`limits TODO DOING`, days; 0 is none), it's raised a step for each
    /// quarter of the limit it spends in todo or doing: medium from 25%,
    /// high from 50%, top from 75%; time in todo keeps counting once it's in
    /// doing. Raised tickets record why (`priority-from`, `priority-own`).
    /// Switching it off puts them back. With no value, says whether it's on
    /// and lists the raised tickets
    AutoPriority {
        #[arg(value_name = "on|off|limits", value_parser = PossibleValuesParser::new(["on", "off", "limits"]))]
        action: Option<String>,
        /// With `limits`: the days a ticket should take in todo, then doing
        #[arg(value_name = "DAYS", num_args = 0..=2)]
        days: Vec<u32>,
    },
    /// Show or switch whether a priority set by hand wins over auto priority
    ///
    /// On (the default): `rite set <ref> priority low` pins the ticket and
    /// auto priority leaves it; `rite set <ref> priority auto` unpins it.
    /// Off: a hand-set priority is only a floor auto priority can raise
    PriorityOverride {
        #[arg(value_name = "on|off", value_parser = PossibleValuesParser::new(["on", "off"]))]
        action: Option<String>,
    },
    /// Show or set the oldest KanRite version allowed to change this project
    ///
    /// For a team that has adopted a change older builds don't understand:
    /// anyone on an older KanRite version can then only read the project until
    /// they update (`rite update`). Only you set it, never a release; it
    /// asks first (or --yes), and is committed for the team. --clear
    /// removes it
    RequireVersion {
        #[arg(value_name = "VERSION")]
        version: Option<String>,
        /// Remove the requirement
        #[arg(long, conflicts_with = "version")]
        clear: bool,
        /// Don't ask
        #[arg(long)]
        yes: bool,
    },
    /// Show or set how near a due date sends a ticket straight to todo
    ///
    /// A ticket in the loose backlog, not a draft, made or given a due date
    /// with this many days left or fewer (or overdue) goes straight to todo.
    /// 3 by default; 0 never
    DueToTodo {
        #[arg(value_name = "DAYS")]
        days: Option<u32>,
    },
    /// Show or switch whether the backlog column can be folded away on the
    /// desktop board (off: it can)
    BacklogAlwaysShown {
        #[arg(value_name = "on|off", value_parser = PossibleValuesParser::new(["on", "off"]))]
        action: Option<String>,
    },
    /// Retired: a TRIAGE ticket, such as an imported GitHub issue, is
    /// always high (ticket 248)
    #[command(hide = true)]
    TriagePriority {
        #[arg(value_name = "PRIORITY")]
        priority: Option<String>,
    },
    /// Show or set the name riteui shows for the project
    ///
    /// The project keeps its name (`kanrite`) for folders and `rite project
    /// use`; this is what people read (`KanRite`): the window title, the
    /// board's header and the landing page. With no name, says it
    DisplayName {
        /// The name to show, quoted if it has spaces
        #[arg(value_name = "NAME")]
        name: Option<String>,
        /// Show the project's own name again
        #[arg(long, conflicts_with = "name")]
        clear: bool,
    },
    /// Show or set the documents repository every project shares
    ///
    /// Shown in riteui's Documents beside the project's own. With no name,
    /// says which it is
    SharedDocuments {
        /// The repository's name, as cloned beside the project's
        #[arg(value_name = "REPOSITORY")]
        name: Option<String>,
        /// Name none
        #[arg(long, conflicts_with = "name")]
        clear: bool,
    },
    /// Add a repository to the project
    ///
    /// Declares its name for the whole project (shared, git-tracked) and
    /// its path on this machine (local only)
    Register {
        #[arg(value_name = "PATH")]
        path: Option<String>,
        /// The project to declare it on, when the folder holds several
        /// (default: the board you're in, or the folder's current project
        /// — asked to confirm, since that's a team-wide change)
        #[arg(long, value_name = "NAME")]
        project: Option<String>,
        /// Take the named repository off the project's declared list, for
        /// everyone (and off this machine's registry); same as `rite deregister`
        #[arg(long, value_name = "NAME", conflicts_with = "path")]
        remove: Option<String>,
    },
    /// Clone a repository the project declares that isn't here yet
    ///
    /// Clones it beside the tickets repository and registers it. With no
    /// name, lists the ones missing
    Clone {
        #[arg(value_name = "NAME", conflicts_with = "all")]
        name: Option<String>,
        /// Clone every declared repository that's missing here
        #[arg(long)]
        all: bool,
    },
    /// Take a repository off the project
    ///
    /// For everyone: off its declared list (committed and pushed), so it
    /// leaves the repository lists, and off this machine. One that's only registered here just loses that. The clone
    /// itself is left alone
    Deregister {
        #[arg(value_name = "NAME")]
        name: String,
        /// The project to take it off, when the folder holds several
        /// (default: the board you're in, or the folder's current project
        /// — asked to confirm, since that's a team-wide change)
        #[arg(long, value_name = "NAME")]
        project: Option<String>,
    },
    /// Link tickets together, with a type; both ends record it
    ///
    /// By default the tickets are just related: both get the other in
    /// `related`. `--type` says how the first stands to the others, and each
    /// of them records the opposite (`rite link 12 13 --type requires` makes 13
    /// required-by 12). `--remove` unlinks. Both ends are one commit.
    ///
    /// Types: requires, required-by, parent-of, child-of, relates-to,
    /// clone-of, cloned-by, supersedes, superseded-by.
    /// A ticket has one parent and is a clone of one original.
    ///
    /// A ticket can't move to done until the tickets it requires and its
    /// children are done. `--replace <type> --type relates-to` changes such
    /// a link (`rite link 12 16 --replace requires --type relates-to`), and
    /// `--remove` takes it away.
    Link {
        #[arg(value_name = "REF")]
        task: String,
        #[arg(value_name = "REF", required = true)]
        to: Vec<String>,
        /// How the first ticket stands to the others
        #[arg(
            long = "type",
            value_name = "TYPE",
            default_value = "relates-to",
            value_parser = clap::builder::PossibleValuesParser::new(
                agile_md::links::LinkType::ALL.map(agile_md::links::LinkType::slug)
            ),
        )]
        kind: String,
        /// Take the link away instead of adding it
        #[arg(long, conflicts_with = "replace")]
        remove: bool,
        /// Only record the link on the first ticket
        #[arg(long, conflicts_with = "replace")]
        one_way: bool,
        /// Change an existing link of this type to --type, on both tickets
        #[arg(
            long,
            value_name = "TYPE",
            value_parser = clap::builder::PossibleValuesParser::new(
                agile_md::links::LinkType::ALL.map(agile_md::links::LinkType::slug)
            ),
        )]
        replace: Option<String>,
    },
    /// Duplicate a ticket into the backlog, linked to it as a clone
    ///
    /// The copy keeps the title, body, tags, branch type, repository, parent,
    /// priority and points. It starts unassigned, unlocked and in no epic, with
    /// its checklist unticked. It gets `clone-of: <id>` and the original gets
    /// the copy in `clones`; `rite show` prints both.
    Duplicate {
        #[arg(value_name = "REF")]
        task: String,
    },
    /// Open the desktop board
    ///
    /// The same window as the `riteui` command
    Gui,
    /// List your todo and doing tickets across every project
    ///
    /// Highest priority first, read as this machine last saw each board.
    /// With nothing assigned to you, offers work to pick instead
    Mine {
        /// Include projects you've hidden
        #[arg(long)]
        all: bool,
        /// Order by priority (then oldest), age, or points
        #[arg(long, value_name = "ORDER", default_value = "priority",
              value_parser = PossibleValuesParser::new(["priority", "age", "points"]))]
        sort: String,
        /// Print the list as JSON
        #[arg(long)]
        json: bool,
        /// Leave a project out of your work (by name), on this machine only
        #[arg(long, value_name = "PROJECT", conflicts_with = "unhide")]
        hide: Option<String>,
        /// Put a hidden project back
        #[arg(long, value_name = "PROJECT")]
        unhide: Option<String>,
        /// With nothing to do, also offer unassigned tickets (todo and
        /// doing first, then the backlog); remembered on this machine
        #[arg(long, conflicts_with = "no_unassigned")]
        unassigned: bool,
        /// With nothing to do, offer only backlog tickets assigned to you;
        /// remembered on this machine
        #[arg(long)]
        no_unassigned: bool,
    },
    /// Update rite and riteui to the newest release
    ///
    /// One release at a time, each verified against GitHub's checksum and
    /// run once before it replaces the last; only the two binaries are
    /// written, never your projects or settings. Only an install made by
    /// KanRite's installer updates itself; anything else says how
    Update {
        /// Only say whether there's a newer release
        #[arg(long)]
        check: bool,
        /// Put back the binaries the last update replaced
        #[arg(long, conflicts_with = "check")]
        rollback: bool,
    },
    /// Print a shell completion script (bash, zsh, fish, …)
    Completions {
        /// Shell to generate for; defaults to $SHELL
        #[arg(value_name = "SHELL")]
        shell: Option<Shell>,
    },
    /// Inspect and customise the task templates
    Templates {
        #[command(subcommand)]
        command: Option<TemplateCmd>,
    },
    /// Show the board's history of moves
    ///
    /// From git: date, time, action, id, title, sha. Reads paths and rename
    /// status only, never the commit message, so it works over history from
    /// before this existed too
    Log,
    /// List the tickets whose repository and branch type don't go together
    ///
    /// On a tickets board a ticket with a repository needs a branch type
    /// (`none` for no branch), and one with a branch type needs a
    /// repository. New and edited tickets are made drafts until they do;
    /// this lists ones that don't, done excepted. It only reads
    Check,
    /// Tickets raised, done and closed lately, and each running sprint's
    /// burn-up and burn-down, across the project's boards
    ///
    /// Read from git history when asked; nothing is stored. Done is a move
    /// into a done column; closed is a ticket abandoned or deleted
    Stats {
        /// The period in days (default: the landing page's, else 30)
        #[arg(long, value_name = "N")]
        days: Option<u32>,
    },
    /// Move a task into an epic, or list it with no ref — checked against
    /// the board's current epics (the four columns and `abandon` are their
    /// own named commands above, and always win if a name collides)
    #[command(external_subcommand)]
    Verb(Vec<String>),
}

#[derive(Args, Debug)]
struct NewArgs {
    /// Task title; omit it in a terminal to fill in the form
    #[arg(value_name = "TITLE")]
    title: Option<String>,
    /// Branch type: feature, bugfix, hotfix, release, chore. Leave it out for
    /// a ticket with no branch
    #[arg(
        long = "branch-type",
        alias = "type",
        value_name = "TYPE",
        value_parser = value::BranchType,
    )]
    branch_type: Option<String>,
    /// The ticket this one sits under (an epic, a story, anything)
    #[arg(short = 'p', long, value_name = "REF")]
    parent: Option<String>,
    /// Who it's assigned to (nobody by default; `@me` is you), repeatable
    /// for several people, the first being the main one. Tickets can be
    /// created now and assigned later
    #[arg(short = 'a', long, alias = "owner", value_name = "WHO")]
    assignee: Vec<String>,
    /// The repository the work is in — one cloned on this machine. Its
    /// branch is made there on entering todo
    #[arg(short = 'R', long, value_name = "REPO")]
    repository: Option<String>,
    /// Another ticket this one relates to (repeatable)
    #[arg(short = 'r', long = "related", value_name = "REF")]
    related: Vec<String>,
    /// A ticket this one requires (repeatable): linked both ways, and it
    /// must be done first
    #[arg(long = "requires", value_name = "REF")]
    requires: Vec<String>,
    /// When it's due, YYYY-MM-DD
    #[arg(long, value_name = "DATE")]
    due: Option<String>,
    /// How it repeats: weekly, fortnightly, monthly, quarterly, yearly or
    /// "every N days|weeks|months|years". Done makes the next one
    #[arg(long, value_name = "RULE")]
    recur: Option<String>,
    /// Tag to add (repeatable)
    #[arg(short = 't', long = "tag", value_name = "TAG")]
    tags: Vec<String>,
    /// Create it as a draft: tagged DRAFT, it stays in the backlog until
    /// `rite ready` takes it out of draft
    #[arg(long)]
    draft: bool,
    /// Template to render the ticket from (default: ticket). A board can add
    /// its own under <board>/templates/
    #[arg(
        short = 'T',
        long = "template",
        alias = "ticket",
        default_value = DEFAULT_TEMPLATE,
        value_name = "TYPE",
        value_parser = value::TicketType,
    )]
    template: String,
    /// Ask for every field, even the ones given as arguments
    #[arg(short, long)]
    interactive: bool,
    /// Extra template variable, available as extra.KEY (repeatable)
    #[arg(short = 's', long = "set", value_name = "KEY=VALUE", value_parser = parse_key_value)]
    set: Vec<(String, String)>,
    /// Open the ticket body in $EDITOR, even when the title came from the CLI
    #[arg(short, long)]
    edit: bool,
    /// Create the ticket from the template without opening an editor
    #[arg(long, conflicts_with = "edit")]
    no_edit: bool,
}

#[derive(Subcommand, Debug)]
enum GroupCmd {
    /// List the epics and sprints in this backlog
    List,
    /// Create an epic
    Epic {
        #[arg(value_name = "NAME")]
        name: String,
        #[arg(long, value_name = "TEXT", default_value = "")]
        description: String,
    },
    /// Create a sprint
    Sprint {
        #[arg(value_name = "NAME")]
        name: String,
        #[arg(long, value_name = "TEXT", default_value = "")]
        description: String,
        /// How long it runs
        #[arg(long, value_name = "DAYS", default_value_t = agile_md::group::DEFAULT_DAYS)]
        days: u32,
    },
    /// Start a sprint. There is no way back: a started sprint takes nothing
    /// more and gives nothing back — its tickets only move on, to todo
    Start {
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// Close a started sprint: it becomes read-only. Unfinished tickets
    /// still in its folder go back to the backlog; those in todo or doing
    /// carry on. A report with its burn-up goes into the documents
    /// repository's sprint/history/
    Close {
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// Pull tickets from the loose backlog into a running sprint, up to N
    /// points: sized, unassigned, not drafts, highest priority first
    Backfill {
        #[arg(value_name = "NAME")]
        name: String,
        /// How many points to add at most
        #[arg(long, value_name = "N")]
        points: u32,
    },
    /// Empty an epic or sprint's folder into the loose backlog at once —
    /// every ticket it holds, no precondition
    Empty {
        #[arg(value_name = "NAME")]
        name: String,
        /// Where its tickets go: the loose backlog (the default), or todo,
        /// keeping their epic label; only those still in the backlog move
        #[arg(long, value_name = "backlog|todo", default_value = "backlog", value_parser = PossibleValuesParser::new(["backlog", "todo"]))]
        to: String,
    },
    /// Delete an epic or sprint outright. Refuses while any ticket in it,
    /// in any column, isn't done — finish them or `group empty` it first
    Delete {
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// Rename an epic or sprint: its folder, and every ticket filed in it
    Rename {
        #[arg(value_name = "NAME")]
        name: String,
        #[arg(value_name = "NEW_NAME")]
        to: String,
    },
}

#[derive(Subcommand, Debug)]
enum TemplateCmd {
    /// List the available templates (the default)
    List,
    /// Print a template's source
    Show {
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// Copy a built-in template into <board>/templates/ so you can edit it
    Eject {
        #[arg(default_value = DEFAULT_TEMPLATE, value_name = "NAME")]
        name: String,
        /// Overwrite an existing board template
        #[arg(short, long)]
        force: bool,
    },
}

fn main() -> ExitCode {
    // Behave like a normal Unix tool when the reader goes away — `rite board |
    // head` is an ordinary thing to do. Rust ignores SIGPIPE by default, which
    // turns a closed pipe into a panic on the next print rather than a quiet
    // exit.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    // Run as `amd`, the shim of the old name (ticket 309): said once a day.
    agile_md::renamed::when_run_by_old_name();
    // The machine folders' new name (ticket 310), once.
    agile_md::places::move_old_folders_and_say();

    // The command tree is big enough that clap's recursive build and parse
    // overflow Windows' 1 MB main-thread stack (Unix gives 8 MB), so the
    // whole run, notes included (they are thread-local), gets a thread of
    // its own.
    std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(run_and_report)
        .ok()
        .and_then(|handle| handle.join().ok())
        .unwrap_or(ExitCode::FAILURE)
}

/// Stack for the thread `main` runs the command on.
const STACK_SIZE: usize = 16 * 1024 * 1024;

fn run_and_report() -> ExitCode {
    let result = run();
    // What auto priority moved along with the change (ticket 215).
    for note in agile_md::auto_priority::take_notes() {
        println!("{note}");
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("rite: {err:#}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Subcommand, Debug)]
enum McpAction {
    /// Print how to connect an AI tool to `rite mcp`
    Setup {
        /// claude, vscode, cursor, codex or gemini; every one when left out
        #[arg(value_name = "TOOL", value_parser = PossibleValuesParser::new(mcp::CLIENTS))]
        tool: Option<String>,
        /// Replace Claude Code's old `agile-md` entry with `kanrite` (through
        /// `claude mcp`); the other tools are only told what to change
        #[arg(long)]
        fix: bool,
    },
}

#[derive(Subcommand, Debug)]
enum DocsAction {
    /// Print the start page
    Start {
        /// Print only its path
        #[arg(long)]
        path: bool,
    },
    /// Search the project's documents and the shared ones, ignoring case
    Search {
        #[arg(value_name = "TEXT", required = true, num_args = 1..)]
        query: Vec<String>,
    },
    /// The pages the start page links to, and what they link to, as a tree
    Tree,
    /// Your pinned documents, kept in the project's documents repository so
    /// they follow you to every machine
    Pins,
    /// Pin a document to your list: `<repository>/<path>`
    Pin {
        #[arg(value_name = "REPO/PATH")]
        document: String,
    },
    /// Take a document off your pinned list
    Unpin {
        #[arg(value_name = "REPO/PATH")]
        document: String,
    },
}

#[derive(Subcommand, Debug)]
enum IdeaAction {
    /// Put an idea forward, with your name on it unless --anonymous
    New {
        #[arg(value_name = "TITLE")]
        title: String,
        /// Leave your name off it
        #[arg(long)]
        anonymous: bool,
        /// Write it up in $EDITOR first
        #[arg(short, long)]
        edit: bool,
    },
    /// Show an idea: its write-up, votes and notes
    Show {
        #[arg(value_name = "IDEA")]
        reference: String,
    },
    /// Vote: + up to +++, or - down to --- (or -2, +3); each sign is one
    /// click, and your vote stays within three either way
    Vote {
        #[arg(value_name = "IDEA")]
        reference: String,
        #[arg(value_name = "VOTE", allow_hyphen_values = true)]
        clicks: String,
    },
    /// Add a note under an idea
    Note {
        #[arg(value_name = "IDEA")]
        reference: String,
        #[arg(value_name = "TEXT")]
        text: String,
    },
    /// Make a backlog ticket from an idea, linked both ways, or with `--as`
    /// an epic or a sprint (empty, its description the idea's write-up)
    Ticket {
        #[arg(value_name = "IDEA")]
        reference: String,
        /// What to make: a backlog ticket (the default), an epic or a sprint
        #[arg(long = "as", value_name = "KIND", default_value = "ticket",
              value_parser = ["ticket", "epic", "sprint"])]
        kind: String,
        /// The epic's or sprint's name (default: the idea's title)
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// A sprint's length in days
        #[arg(long, value_name = "DAYS", default_value_t = agile_md::group::DEFAULT_DAYS)]
        days: u32,
    },
}

#[derive(Subcommand, Debug)]
enum ProjectAction {
    /// Make a project current for this folder (writes .current-project);
    /// with no name, pick one in a terminal
    Use {
        #[arg(value_name = "NAME")]
        name: Option<String>,
    },
    /// Archive a project that's no longer maintained: committed for the
    /// whole team, its repositories archived on GitHub (`gh repo archive`:
    /// tickets, documents and every declared one), and taken off rite on
    /// this machine, folders kept. Read-only, off My work and the landing
    /// page everywhere. `rite init <name>` brings it back
    Archive {
        /// The project (default: the current one)
        #[arg(value_name = "NAME")]
        name: Option<String>,
        /// Always on now (ticket 263); kept so older scripts still run
        #[arg(long, hide = true)]
        github: bool,
    },
    /// Bring an archived project back where it's still known, unarchiving
    /// its repositories on GitHub first; `rite init <name>` does it anywhere
    Unarchive {
        /// The project (default: the current one)
        #[arg(value_name = "NAME")]
        name: Option<String>,
        /// Always on now (ticket 263); kept so older scripts still run
        #[arg(long, hide = true)]
        github: bool,
    },
}

fn run() -> Result<()> {
    // Run by the completion scripts on every TAB (ticket 028): quiet, no
    // prompt, no fetch. Looked for before clap so it's no subcommand, which
    // clap_complete would put in every script, hidden or not.
    if env::args().nth(1).as_deref() == Some(completions::DESTINATIONS) {
        completions::destinations(&Cli::command());
        return Ok(());
    }
    let cli = Cli::parse();
    form::set_no_input(cli.no_input);
    render::set_plain(cli.plain);
    let command = cli.command.unwrap_or(Cmd::Board { tag: Vec::new() });

    // Completions don't need a board — and often run before there is one.
    // Updating rite needs no board, and works from anywhere (ticket 191).
    if let Cmd::Update { check, rollback } = command {
        return update::run(check, rollback);
    }
    if let Cmd::Completions { shell } = command {
        return completions::print(shell, &mut Cli::command());
    }

    // The desktop board draws every registered repository, so it must not
    // insist on standing in one — `rite gui` works from anywhere. The `riteui`
    // binary is the same call without the subcommand.
    if let Cmd::Gui = command {
        return cmd_gui();
    }

    // `init` is the one command that runs without an existing board.
    if let Cmd::Init {
        project,
        local_only,
        org,
    } = command
    {
        return init::run(project, local_only, org);
    }
    if let Cmd::Join { project, org } = command {
        return join::run(project, org);
    }
    // `register` declares a repository for the whole team, so which project
    // it lands on is checked before any board is chosen (ticket 103).
    if let Cmd::Register {
        path,
        project,
        remove,
    } = command
    {
        let board = register_board(path.as_deref(), project.as_deref(), remove.as_deref())?;
        agile_md::project_trust::check(&board)?;
        agile_md::archive::ensure_writable(&board)?;
        migrate_settings(&board)?;
        return match remove {
            Some(name) => cmd_unregister(&board, &name),
            None => cmd_register(&board, path.as_deref()),
        };
    }
    // `deregister` can take a repository off the project for everyone, so it
    // gets the same check of which project (ticket 120).
    if let Cmd::Deregister { name, project } = command {
        let board = register_board(None, project.as_deref(), Some(&name))?;
        agile_md::project_trust::check(&board)?;
        agile_md::archive::ensure_writable(&board)?;
        migrate_settings(&board)?;
        return cmd_deregister(&board, &name);
    }
    if let Cmd::Mine {
        all,
        sort,
        json,
        hide,
        unhide,
        unassigned,
        no_unassigned,
    } = &command
    {
        if *unassigned || *no_unassigned {
            agile_md::my_work::set_include_unassigned(*unassigned)?;
        }
        return cmd_mine(*all, sort, *json, hide.as_deref(), unhide.as_deref());
    }
    // Documents go into the project's documents repository, from wherever
    // they are: no board needed here, and none offered (ticket 079).
    if let Cmd::Import {
        first,
        path,
        dry_run,
    } = &command
    {
        // Documents go into the documents repository you're standing in:
        // no board needed here, and none offered (tickets 079, 173).
        if first == "documents" {
            return cmd_import_documents(path.as_deref(), *dry_run);
        }
        if first == "issues" {
            bail!(
                "GitHub issues are pulled in automatically when riteui's landing page opens; `rite issues` checks now"
            );
        }
    }
    // The AI tool starts this in the project's folder; each tool call finds
    // the board itself (ticket 186).
    if let Cmd::Mcp { action } = command {
        return match action {
            None => mcp::run(),
            Some(McpAction::Setup { tool, fix }) => {
                let home = agile_md::home::dir();
                if fix {
                    let home = home.context("no home folder to look in")?;
                    println!("{}", mcp::fix(&home, mcp::run_claude)?);
                } else {
                    println!("{}", mcp::setup(tool.as_deref(), home.as_deref())?);
                }
                Ok(())
            }
        };
    }
    // Documents are read from the project; no board command needed (ticket
    // 179).
    if let Cmd::Docs { action } = command {
        return docs::run(match action {
            DocsAction::Start { path } => docs::Action::Start { path },
            DocsAction::Search { query } => docs::Action::Search {
                query: query.join(" "),
            },
            DocsAction::Tree => docs::Action::Tree,
            DocsAction::Pins => docs::Action::Pins,
            DocsAction::Pin { document } => docs::Action::Pin { document, on: true },
            DocsAction::Unpin { document } => docs::Action::Pin {
                document,
                on: false,
            },
        });
    }
    // Ideas are documents, found from the project; no board command needed
    // (ticket 131).
    if let Cmd::Idea { action, all } = command {
        let action = match action {
            None => idea::Action::List { all },
            Some(IdeaAction::New {
                title,
                anonymous,
                edit,
            }) => idea::Action::New {
                title,
                anonymous,
                edit,
            },
            Some(IdeaAction::Show { reference }) => idea::Action::Show { reference },
            Some(IdeaAction::Vote { reference, clicks }) => {
                idea::Action::Vote { reference, clicks }
            }
            Some(IdeaAction::Note { reference, text }) => idea::Action::Note { reference, text },
            Some(IdeaAction::Ticket {
                reference,
                kind,
                name,
                days,
            }) => idea::Action::Ticket {
                reference,
                kind,
                name: name.unwrap_or_default(),
                days,
            },
        };
        return idea::run(action, &editor_command());
    }
    if let Cmd::Project { action } = command {
        return match action {
            None => project::list(),
            Some(ProjectAction::Use { name }) => project::use_project(name),
            Some(ProjectAction::Archive { name, .. }) => project::archive(name, true),
            Some(ProjectAction::Unarchive { name, .. }) => project::archive(name, false),
        };
    }

    let board = ensure_board()?;
    // Notices if this board's declared project identity has silently
    // changed since rite last saw it — see project_trust.rs.
    agile_md::project_trust::check(&board)?;
    // An archived project only reads (ticket 157).
    if changes_board(&command) {
        agile_md::archive::ensure_writable(&board)?;
    }
    // In step with the remote before anything else (ticket 102): a change
    // never lands on a stale clone, and a listing shows teammates' work.
    for event in agile_md::sync::catch_up(&board, sync_purpose(&command))? {
        match event {
            transition::Event::Info(text) => eprintln!("{text}"),
            transition::Event::Warn(text) => eprintln!("rite: {text}"),
        }
    }
    // A one-time carry-over from the documents repository: only on a
    // change, never by just reading the board (ticket 193).
    if changes_board(&command) {
        migrate_settings(&board)?;
    }
    match command {
        Cmd::Init { .. }
        | Cmd::Join { .. }
        | Cmd::Project { .. }
        | Cmd::Idea { .. }
        | Cmd::Docs { .. }
        | Cmd::Mcp { .. }
        | Cmd::Mine { .. }
        | Cmd::Completions { .. }
        | Cmd::Update { .. }
        | Cmd::Gui => {
            unreachable!("handled above")
        }
        Cmd::WipLimit { limit } => cmd_wip_limit(&board, limit),
        Cmd::SharedDocuments { name, clear } => cmd_shared_documents(&board, name, clear),
        Cmd::DisplayName { name, clear } => cmd_display_name(&board, name, clear),
        Cmd::TriagePriority { priority } => cmd_triage_priority(&board, priority),
        Cmd::AutoPriority { action, days } => cmd_auto_priority(&board, action.as_deref(), &days),
        Cmd::PriorityOverride { action } => cmd_priority_override(&board, action.as_deref()),
        Cmd::DueToTodo { days } => cmd_due_to_todo(&board, days),
        Cmd::RequireVersion {
            version,
            clear,
            yes,
        } => cmd_require_version(&board, version.as_deref(), clear, yes),
        Cmd::BacklogAlwaysShown { action } => cmd_backlog_always_shown(&board, action.as_deref()),
        Cmd::Register { .. } | Cmd::Deregister { .. } => unreachable!("handled above"),
        Cmd::Clone { name, all } => cmd_clone(&board, name.as_deref(), all),
        Cmd::New(args) => cmd_new(&board, args),
        Cmd::Board { tag } => render::columns(
            &board,
            &board.workflow().columns().collect::<Vec<_>>(),
            &tag,
        ),
        Cmd::Tag { tag } => render::named(&format!("tag: {tag}"), board.tasks_tagged(&tag)?, &[]),
        Cmd::Tags { action } => cmd_tags(&board, action),
        Cmd::Column { action } => cmd_column(&board, action),
        Cmd::Repo { action } => cmd_repo(&board, action),
        Cmd::Move {
            task,
            column,
            force,
        } => {
            let to = board
                .workflow()
                .find(&column)
                .with_context(|| column_list(&board, &column))?;
            cmd_destination(&board, Destination::Column(to), Some(task), &[], force)
        }
        Cmd::Import {
            first,
            path,
            dry_run,
        } => {
            let path = match (first.as_str(), path) {
                ("tickets", Some(path)) => path,
                ("tickets", None) => {
                    bail!("usage: rite import tickets <PATH>, from the board's tasks directory")
                }
                (_, None) => PathBuf::from(first),
                (_, Some(_)) => {
                    bail!("rite import takes `tickets <PATH>` or `documents <PATH>`, not {first:?}")
                }
            };
            cmd_import(&board, &path, dry_run)
        }
        Cmd::Issues {
            action,
            repo,
            label,
            dry_run,
        } => cmd_issues(
            &board,
            action.as_deref(),
            repo.as_deref(),
            label.as_deref(),
            dry_run,
        ),
        Cmd::Backlog { task, tag } => cmd_destination(
            &board,
            Destination::Column(Column::Backlog),
            task,
            &tag,
            false,
        ),
        Cmd::Todo {
            task,
            branch,
            no_branch,
            tag,
            force,
        } => match task {
            Some(reference) => {
                let task = board.find(&reference)?;
                cmd_enter_todo(&board, &task, branch, no_branch, force)
            }
            None => render::columns(&board, &[Column::Todo], &tag),
        },
        Cmd::Doing { task, tag, force } => cmd_destination(
            &board,
            Destination::Column(Column::Doing),
            task,
            &tag,
            force,
        ),
        Cmd::Done { task, tag } => {
            cmd_destination(&board, Destination::Column(Column::Done), task, &tag, false)
        }
        Cmd::Abandon { task, tag } => {
            cmd_destination(&board, Destination::Abandon, task, &tag, false)
        }
        Cmd::Verb(args) => {
            let mut args = args.into_iter();
            let name = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("usage: rite <destination> [ref] [--tag TAG]"))?;
            let mut task = None;
            let mut tag: Vec<String> = Vec::new();
            let mut force = false;
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--force" | "-f" => force = true,
                    "--tag" | "-t" => {
                        let value = args
                            .next()
                            .ok_or_else(|| anyhow::anyhow!("rite {name}: --tag needs a value"))?;
                        tag.push(value);
                    }
                    _ if task.is_none() => task = Some(arg),
                    other => bail!("rite {name}: unexpected argument '{other}'"),
                }
            }
            let destination = resolve_destination(&board, &name)?;
            cmd_destination(&board, destination, task, &tag, force)
        }
        Cmd::Show { task } => {
            let task = resolve(
                &board,
                task,
                "show",
                &board.workflow().columns().collect::<Vec<_>>(),
            )?;
            let body =
                fs::read(&task.path).with_context(|| format!("reading {}", task.path.display()))?;
            io::stdout().write_all(&body)?;
            if let Some(line) = lock_note(&board, &task) {
                eprintln!("{line}");
            }
            // Why its priority is what it is (ticket 215).
            if let Some(line) = agile_md::auto_priority::note(&board, &task) {
                eprintln!("{line}");
            }
            // Overdue, and how it repeats (ticket 143).
            if agile_md::recur::overdue(&task, agile_md::recur::today()) {
                eprintln!("overdue: it was due {}", task.due().unwrap_or_default());
            }
            if let Some(rule) = task.recur() {
                eprintln!("repeats {rule}: done makes the next one");
            }
            // Who to ask about it (ticket 155), on stderr like the lock note.
            if let Some(line) = agile_md::authorship::summary(&board, &task) {
                eprintln!("{line}");
            }
            Ok(())
        }
        Cmd::Edit { task } => {
            let task = resolve(
                &board,
                task,
                "edit",
                &board.workflow().columns().collect::<Vec<_>>(),
            )?;
            let (id_display, title) = (task.id_display(), task.title());
            // No separate "did it actually change" check needed: the
            // snapshot diff in with_auto_commit is already empty when the
            // editor exits without saving, so nothing gets committed.
            with_auto_commit(&board, "edit", &id_display, &title, || {
                open_editor(&task.path)
            })
        }
        Cmd::Set {
            task,
            field,
            value,
            more,
        } if field == "requires" && more.is_empty() => {
            let task = resolve(
                &board,
                Some(task),
                "set",
                &board.workflow().columns().collect::<Vec<_>>(),
            )?;
            let mut wanted = Vec::new();
            for reference in agile_md::task::parse_list(&value) {
                let other = board.find(&reference)?;
                if other.path == task.path {
                    bail!("a ticket can't require itself");
                }
                wanted.push(other);
            }
            report(&transition::set_requires(&board, &task, &wanted)?);
            Ok(())
        }
        Cmd::Set {
            task,
            field,
            value,
            more,
        } => {
            let pairs = set_pairs(field, value, more)?;
            let task = resolve(
                &board,
                Some(task),
                "set",
                &board.workflow().columns().collect::<Vec<_>>(),
            )?;
            let (id_display, title) = (task.id_display(), task.title());
            let given = |name: &str| {
                pairs
                    .iter()
                    .find(|(field, _)| field == name)
                    .map(|(_, value)| value.clone())
            };
            let (repository, branch_type) = (given("repository"), given("branch-type"));
            with_auto_commit(&board, "set", &id_display, &title, || {
                // A repository and a branch type go together (ticket 066):
                // checked once, as they'll be after every field is set, so
                // a ticket past the backlog can be given both at once
                // (ticket 258). One without the other is a draft in the
                // backlog, and refused past it.
                let why = if repository.is_some() || branch_type.is_some() {
                    transition::check_pairing(
                        &board,
                        &task,
                        &repository
                            .clone()
                            .unwrap_or_else(|| task.repository().unwrap_or_default()),
                        &branch_type
                            .clone()
                            .unwrap_or_else(|| task.branch_type().unwrap_or_default()),
                    )?
                } else {
                    None
                };
                let mut due_changed = false;
                for (field, value) in &pairs {
                    match field.as_str() {
                        "points" => {
                            task.set_points(task.hash()?, value)?;
                            println!("{} sized {}", task.id_display(), display(value));
                        }
                        // Filing goes through the board, not the ticket: it moves the
                        // file, and the sprint rules live there.
                        "epic" => {
                            let epic = (!value.is_empty()).then_some(value.as_str());
                            board.set_epic(&task, epic)?;
                            println!("{} filed under {}", task.id_display(), display(value));
                        }
                        "order" => {
                            let rank: f64 = value
                                .parse()
                                .with_context(|| format!("{value} is not a number"))?;
                            task.set_order(task.hash()?, rank)?;
                            println!("{} ranked {rank}", task.id_display());
                        }
                        "title" => {
                            task.set_title(task.hash()?, value)?;
                            println!("{} retitled", task.id_display());
                        }
                        // By hand (ticket 215): pins it against auto priority
                        // when override is on; `auto` unpins.
                        "priority" => {
                            println!(
                                "{}",
                                agile_md::auto_priority::set_by_hand(&board, &task, value)?
                            );
                        }
                        "repository" => {
                            task.set_repository(task.hash()?, value)?;
                            let name = agile_md::task::repo_name(value);
                            if !name.is_empty()
                                && !board_settings::load(&board).repos().contains(&name)
                            {
                                eprintln!(
                                    "rite: {name} isn't declared for this project — `rite register` it there so the team sees it"
                                );
                            }
                            println!("{} repository {}", task.id_display(), display(&name));
                        }
                        "branch-type" => {
                            task.set_branch_type(task.hash()?, value)?;
                            println!(
                                "{} branch type {}",
                                task.id_display(),
                                display(&branch::normalise(value))
                            );
                        }
                        // Ticket 143: a due date, and a repeat that makes the
                        // next one when this one is done.
                        "due" => {
                            // Not in an epic (ticket 253).
                            let epic = match task.column.kind() {
                                agile_md::board::Kind::Backlog => task.epic.clone(),
                                _ => task.epic_meta(),
                            };
                            board.refuse_due_in_epic(
                                &task.id_display(),
                                epic.as_deref(),
                                Some(value),
                            )?;
                            task.set_due(task.hash()?, value)?;
                            println!("{} due {}", task.id_display(), display(value));
                            due_changed = true;
                        }
                        "recur" => {
                            task.set_recur(task.hash()?, value)?;
                            match task.recur() {
                                Some(rule) => println!("{} repeats {rule}", task.id_display()),
                                None => println!("{} doesn't repeat", task.id_display()),
                            }
                        }
                        other => bail!("unknown field {other}"),
                    }
                }
                if let Some(drafted) = transition::settle_pairing(&task, why)? {
                    eprintln!("rite: {drafted}");
                }
                // Due soon: straight to todo (ticket 253), once every field
                // is set, since it moves the file.
                if due_changed
                    && let Some(dated) = agile_md::task::Task::from_path(&task.path, task.column)
                    && let Some(line) = transition::straight_to_todo(&board, &dated)
                {
                    println!("{line}");
                }
                Ok(())
            })
        }
        Cmd::Group { command } => cmd_group(&board, command.unwrap_or(GroupCmd::List)),
        Cmd::Assign {
            task,
            who,
            add,
            remove,
            clear,
        } => {
            let task = resolve(
                &board,
                Some(task),
                "assign",
                &board.workflow().columns().collect::<Vec<_>>(),
            )?;
            let (id_display, title) = (task.id_display(), task.title());
            // No name: the picker in a terminal (ticket 089); anywhere else
            // it unassigns, as it always has, so scripts are unchanged.
            let named: Vec<String> = who
                .iter()
                .map(|who| resolve_assignee(&board.root, Some(who)))
                .collect();
            let current = task.assignees();
            // Several people (ticket 254): the names replace the list, or
            // `--add`/`--remove` change it.
            let who = if add {
                agile_md::task::regroup(&current, &named, &[])
            } else if remove {
                agile_md::task::regroup(&current, &[], &named)
            } else if named.is_empty() && !clear && form::available() {
                // No name: the picker in a terminal (ticket 089); anywhere
                // else it unassigns, as it always has, so scripts are
                // unchanged.
                let one = form::assignee(
                    board_settings::load(&board).user_names(),
                    git::config(&board.root, "user.name"),
                    &task.assignee().unwrap_or_default(),
                )?;
                agile_md::task::regroup(&[one], &[], &[])
            } else {
                agile_md::task::regroup(&named, &[], &[])
            };
            with_auto_commit(&board, "assign", &id_display, &title, || {
                task.set_assignees(task.hash()?, &who)?;
                match who.is_empty() {
                    true => println!("{} unassigned", task.id_display()),
                    false => println!("{} assigned to {}", task.id_display(), who.join(", ")),
                }
                Ok(())
            })?;
            for warning in board_settings::unknown_assignees(&board, &who) {
                eprintln!("warning: {warning}");
            }
            Ok(())
        }
        Cmd::Lock { task } => cmd_lock(&board, &task),
        Cmd::Draft { task } => cmd_draft(&board, &task, true),
        Cmd::Ready { task } => cmd_draft(&board, &task, false),
        Cmd::Unlock { task, force } => cmd_unlock(&board, &task, force),
        Cmd::Link {
            task,
            to,
            kind,
            remove,
            one_way,
            replace,
        } => match replace {
            Some(old) => cmd_relink(&board, &task, &to, &old, &kind),
            None => cmd_link(&board, &task, &to, &kind, remove, one_way),
        },
        Cmd::Duplicate { task } => {
            let original = board.find(&task)?;
            report(&transition::clone_ticket(&board, &original)?);
            Ok(())
        }
        Cmd::Templates { command } => cmd_templates(&board, command.unwrap_or(TemplateCmd::List)),
        Cmd::Log => cmd_log(&board),
        Cmd::Stats { days } => cmd_stats(days),
        Cmd::Check => cmd_check(&board),
        Cmd::Links { task, json } => {
            let task = board.find(&task)?;
            let linked = agile_md::links::linked(&board, &task)?;
            if json {
                let rows: Vec<serde_json::Value> = linked
                    .iter()
                    .map(|l| {
                        serde_json::json!({
                            "kind": l.kind.slug(),
                            "id": l.task.id_display(),
                            "title": l.task.title(),
                            "file": l.task.file_name(),
                            "column": l.task.column.as_str(),
                            "place": agile_md::requires::place(&board, &l.task),
                            "recorded_here": l.recorded_here,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&rows)?);
                return Ok(());
            }
            println!("{} {}", task.id_display(), task.title());
            if linked.is_empty() {
                println!("  linked to no other ticket");
            }
            let width = linked
                .iter()
                .map(|l| l.kind.phrase().len())
                .max()
                .unwrap_or(0);
            for l in &linked {
                let one_sided = if l.recorded_here {
                    String::new()
                } else {
                    format!("  (recorded on {} only)", l.task.id_display())
                };
                println!(
                    "  {:width$}  {} {} · {}{one_sided}",
                    l.kind.phrase(),
                    l.task.id_display(),
                    l.task.title(),
                    agile_md::requires::place(&board, &l.task),
                );
            }
            Ok(())
        }
        Cmd::Tree {
            task,
            done,
            children: true,
        } => {
            // Parent and child (ticket 261).
            let around = task.map(|t| board.find(&t)).transpose()?;
            let trees = agile_md::hierarchy::hierarchy(&board, around.as_ref(), done)?;
            match (&around, trees.is_empty()) {
                (Some(t), true) => {
                    println!("{} has no parent and no children", t.id_display())
                }
                (None, true) if done => println!("No ticket on this board has children"),
                (None, true) => println!(
                    "No ticket on this board has children that aren't done (rite tree --children --done shows them)"
                ),
                _ => render::tree(&board, &trees),
            }
            Ok(())
        }
        Cmd::Tree { task, done, .. } => {
            let around = task.map(|t| board.find(&t)).transpose()?;
            let trees = agile_md::requires::family(&board, around.as_ref(), done)?;
            // Empty only because done tickets were left out: say so.
            let all_done = trees.is_empty()
                && !done
                && !agile_md::requires::family(&board, around.as_ref(), true)?.is_empty();
            match (&around, trees.is_empty()) {
                (Some(t), true) if all_done => println!(
                    "Everything {} is linked to is done (rite tree {} --done shows it)",
                    t.id_display(),
                    t.id_display()
                ),
                (Some(t), true) => println!(
                    "{} requires nothing and has no related tickets",
                    t.id_display()
                ),
                (None, true) if all_done => {
                    println!("Every ticket linked to another is done (rite tree --done shows them)")
                }
                (None, true) => println!("No ticket on this board requires or relates to another"),
                _ => render::tree(&board, &trees),
            }
            Ok(())
        }
        Cmd::Recover {
            ids,
            all,
            by,
            since,
        } => cmd_recover(&board, &ids, all, by.as_deref(), since.as_deref()),
        Cmd::Renumber { apply, keep } => cmd_renumber(&board, apply, keep.as_deref()),
    }
}

/// `rite renumber` (ticket 146): show, or with `--apply` fix, tickets that
/// share a number.
fn cmd_renumber(board: &Board, apply: bool, keep: Option<&str>) -> Result<()> {
    use agile_md::renumber;
    let clashes = renumber::plan(board, keep)?;
    if clashes.is_empty() {
        println!("no two tickets share a number");
        return Ok(());
    }
    for clash in &clashes {
        println!("{:03}: {} keeps it", clash.id, clash.keep.stem);
        for (task, to) in &clash.moves {
            println!("     {} becomes {to:03}", task.stem);
        }
    }
    if !apply {
        println!("(nothing changed; `rite renumber --apply` does this)");
        return Ok(());
    }
    let ids: Vec<String> = clashes.iter().map(|c| format!("{:03}", c.id)).collect();
    let report = with_auto_commit(board, "renumber", &ids.join(", "), "shared numbers", || {
        renumber::apply(board, &clashes)
    })?;
    for (from, to) in &report.renamed {
        println!("renamed {from} -> {to}");
    }
    for (ticket, what) in &report.updated {
        println!("updated {ticket}: {what}");
    }
    if !report.ambiguous.is_empty() {
        eprintln!("check these by hand: they could mean either ticket, so they were left alone");
        for (ticket, what) in &report.ambiguous {
            eprintln!("  {ticket}: {what}");
        }
    }
    Ok(())
}

/// `rite recover` (ticket 144): list tickets removed from the board, or
/// restore some into the backlog in one commit.
fn cmd_recover(
    board: &Board,
    ids: &[String],
    all: bool,
    by: Option<&str>,
    since: Option<&str>,
) -> Result<()> {
    use agile_md::recover;
    if let Some(date) = since
        && !(date.len() == 10 && date.as_bytes()[4] == b'-' && date.as_bytes()[7] == b'-')
    {
        bail!("--since takes a date as YYYY-MM-DD, not '{date}'");
    }
    let listed: Vec<recover::Removed> = recover::removed(board)?
        .into_iter()
        .filter(|r| by.is_none_or(|who| r.by(who)))
        .filter(|r| since.is_none_or(|date| r.when.as_str() >= date))
        .collect();

    if ids.is_empty() && !all {
        if listed.is_empty() {
            println!("nothing to recover: no removed ticket is missing from the board");
        }
        for r in &listed {
            let how = if r.abandoned { "abandoned" } else { "deleted" };
            println!(
                "{}  {}  {how} by {}  {}",
                r.id_display(),
                r.when,
                r.author,
                r.title
            );
        }
        return Ok(());
    }

    let chosen: Vec<recover::Removed> = if all {
        listed
    } else {
        let mut chosen = Vec::new();
        for id in ids {
            let number: u32 = id
                .parse()
                .with_context(|| format!("'{id}' isn't a ticket number"))?;
            match listed.iter().find(|r| r.id == number) {
                Some(r) => chosen.push(r.clone()),
                None => bail!(
                    "{number:03} isn't a removed ticket missing from the board (`rite recover` lists them)"
                ),
            }
        }
        chosen
    };
    if chosen.is_empty() {
        println!("nothing to recover");
        return Ok(());
    }
    let ids: Vec<String> = chosen.iter().map(|r| r.id_display()).collect();
    let detail = if chosen.len() == 1 {
        chosen[0].title.clone()
    } else {
        format!("{} tickets", chosen.len())
    };
    with_auto_commit(board, "recover", &ids.join(", "), &detail, || {
        for r in &chosen {
            recover::restore(board, r)?;
            println!("recovered {} -> backlog/  {}", r.stem, r.title);
        }
        Ok(())
    })
}

/// `rite mine` (ticket 020): your todo and doing tickets across every project
/// on this machine. Works from anywhere: it isn't about one board.
fn cmd_mine(
    all: bool,
    sort: &str,
    json: bool,
    hide: Option<&str>,
    unhide: Option<&str>,
) -> Result<()> {
    use agile_md::my_work::{self, Sort};
    let projects = my_work::projects();
    if let Some(name) = hide.or(unhide) {
        let matches: Vec<_> = projects.iter().filter(|p| p.name == name).collect();
        let project = match matches.as_slice() {
            [one] => *one,
            [] => bail!(
                "no project called '{name}' on this machine (projects: {})",
                projects
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            _ => bail!(
                "'{name}' is cloned in more than one folder; hide it from the desktop board instead"
            ),
        };
        my_work::set_hidden(&project.root, hide.is_some())?;
        let what = if hide.is_some() {
            "hidden from"
        } else {
            "back in"
        };
        println!("{} is {what} your work on this machine", project.name);
        return Ok(());
    }
    let here = env::current_dir().unwrap_or_default();
    let Some((email, name)) = my_work::me(&here) else {
        bail!("git has no user.email, so rite can't tell which tickets are yours");
    };
    let order = match sort {
        "age" => Sort::Age,
        "points" => Sort::Points,
        _ => Sort::Priority,
    };
    let items = my_work::my_work(&email, name.as_deref(), all, order);
    if json {
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }
    let line = |item: &my_work::WorkItem| {
        let points = if item.points.is_empty() {
            "      ".to_string()
        } else {
            format!("{:>2} pts", item.points)
        };
        // Waiting to be triaged (ticket 248): first, and said so.
        let triage = if item.triage { "TRIAGE  " } else { "" };
        println!(
            "{:<5}  {:<6}  {}  {:<16}  {}  {triage}{}",
            item.column, item.priority, points, item.project_shown, item.id, item.title
        );
    };
    items.iter().for_each(line);
    // Nothing to do (ticket 160): work you could take (unassigned todo and
    // doing first, when unassigned work is included, then the backlog's
    // highest priority). A hidden project stays hidden (ticket 263), its
    // TRIAGE tickets and work to pick listed all the same.
    if items.is_empty() {
        let pick = my_work::to_pick(&email, name.as_deref(), my_work::include_unassigned());
        if pick.is_empty() {
            println!("nothing in todo or doing is assigned to you, and nothing to pick");
        } else {
            println!("nothing in todo or doing is assigned to you — work to pick:");
            pick.iter().for_each(line);
        }
    }
    let hidden = projects.iter().filter(|p| p.hidden).count();
    if hidden > 0 && !all {
        eprintln!(
            "{hidden} hidden project{} left out; --all includes {}",
            if hidden == 1 { "" } else { "s" },
            if hidden == 1 { "it" } else { "them" }
        );
    }
    Ok(())
}

/// What `rite show` says about a ticket's lease, on stderr so the ticket on
/// stdout stays exactly the file: the same four cases as riteui's editor
/// (ticket 110). A lapsed lease is said as such, never as unlocked.
fn lock_note(board: &Board, task: &agile_md::task::Task) -> Option<String> {
    let by = task.locked_by()?;
    let until = task.locked_until()?.format(&Rfc3339).ok()?;
    let mine = git::config(board.repo_root(), "user.email").as_deref() == Some(by.as_str());
    Some(match (task.lease_active(), mine) {
        (true, true) => format!("locked by you until {until}"),
        (true, false) => format!("locked by {by} until {until}: only they can change it"),
        (false, true) => {
            format!(
                "your lock lapsed at {until} — `rite lock {}` to keep working",
                task.id_display()
            )
        }
        (false, false) => {
            format!("locked by {by} until {until} (lapsed): anyone can change it now")
        }
    })
}

/// Claim a ticket's lease for the current git identity. Refuses if someone
/// else's active lease is already there — the same `write_checked` gate
/// every other setter goes through, with no special casing needed for take.
fn cmd_lock(board: &Board, reference: &str) -> Result<()> {
    let task = board.find(reference)?;
    let (id_display, title) = (task.id_display(), task.title());
    with_auto_commit(board, "lock", &id_display, &title, || {
        let hours = board_settings::load(board).lease_hours();
        task.take_lease(task.hash()?, hours)?;
        let until = task
            .locked_until()
            .and_then(|until| until.format(&Rfc3339).ok())
            .unwrap_or_default();
        println!("{} locked until {until}", task.id_display());
        Ok(())
    })
}

/// `rite check` (ticket 066): tickets whose repository and branch type don't
/// go together. Reads only; fixing one is `rite set`.
fn cmd_check(board: &Board) -> Result<()> {
    let found = agile_md::pairing::mismatched(board)?;
    if found.is_empty() {
        if board.is_tickets_board() {
            println!("every ticket's repository and branch type go together");
        } else {
            println!("this board is inside its code repository, so a ticket needs no repository");
        }
        return Ok(());
    }
    for (task, why) in &found {
        println!(
            "{} {}/ {}: {why}",
            task.id_display(),
            task.column,
            task.title()
        );
    }
    println!(
        "{} to settle: `rite set <ref> branch-type <type|none>` or `rite set <ref> repository <name>`",
        found.len()
    );
    Ok(())
}

/// `rite draft` / `rite ready` (ticket 114): add or remove the DRAFT tag. A
/// ticket that's already so says so and changes nothing.
fn cmd_draft(board: &Board, reference: &str, draft: bool) -> Result<()> {
    let task = board.find(reference)?;
    let (id_display, title) = (task.id_display(), task.title());
    let verb = if draft { "draft" } else { "ready" };
    // Not while its repository and branch type don't go together (ticket 066).
    if !draft {
        agile_md::pairing::ready(board, &task)?;
    }
    with_auto_commit(board, verb, &id_display, &title, || {
        let changed = task.set_draft(task.hash()?, draft)?;
        let id = task.id_display();
        match (changed, draft) {
            (true, true) => {
                println!("{id} is a draft: it stays in the backlog until `rite ready {id}`")
            }
            (true, false) => println!("{id} is ready: it can leave the backlog"),
            (false, true) => println!("{id} is already a draft"),
            (false, false) => println!("{id} isn't a draft"),
        }
        Ok(())
    })
}

/// Release a ticket's lease — only your own unless `--force` says to break
/// someone else's, a deliberate, separate act rather than a side effect of
/// an ordinary release.
fn cmd_unlock(board: &Board, reference: &str, force: bool) -> Result<()> {
    let task = board.find(reference)?;
    let (id_display, title) = (task.id_display(), task.title());
    with_auto_commit(board, "unlock", &id_display, &title, || {
        let hash = task.hash()?;
        if force {
            task.break_lease(hash)?;
            println!("{} lease broken", task.id_display());
        } else {
            task.release_lease(hash)?;
            println!("{} unlocked", task.id_display());
        }
        Ok(())
    })
}

/// Move a task into todo/ and put the working tree on its branch. The work
/// lives in `agile_md::transition` so every front end does exactly the
/// same; this only prints what it reports.
fn cmd_enter_todo(
    board: &Board,
    task: &task::Task,
    override_branch: Option<String>,
    no_branch: bool,
    force: bool,
) -> Result<()> {
    let outcome = transition::enter_todo(
        board,
        task,
        &transition::TodoOptions {
            force,
            branch: override_branch,
            no_branch,
            to: None,
        },
    )?;
    report(&outcome);
    Ok(())
}

/// Print a transition's events the way the CLI always has: information on
/// stdout, best-effort failures on stderr behind the `rite:` prefix.
fn report(outcome: &transition::Outcome) {
    for event in &outcome.events {
        match event {
            transition::Event::Info(text) => println!("{text}"),
            transition::Event::Warn(text) => eprintln!("rite: {text}"),
        }
    }
}

/// Link tickets together (ticket 207). Both ends record the link — the
/// opposite kind on the other ticket — unless the caller asks for one
/// direction only.
fn cmd_link(
    board: &Board,
    reference: &str,
    to: &[String],
    kind: &str,
    remove: bool,
    one_way: bool,
) -> Result<()> {
    let kind = agile_md::links::LinkType::parse(kind)
        .with_context(|| format!("unknown link type '{kind}'"))?;
    let task = board.find(reference)?;
    let mut others = Vec::new();
    for other in to {
        let other = board.find(other)?;
        if other.path == task.path {
            bail!("a ticket can't be linked to itself");
        }
        others.push(other);
    }
    report(&transition::link_tickets(
        board, &task, kind, &others, remove, one_way,
    )?);
    Ok(())
}

/// `rite link <ref> <ref>... --replace OLD --type NEW` (ticket 213): change
/// each link's type on both ends, in one commit.
fn cmd_relink(board: &Board, reference: &str, to: &[String], old: &str, new: &str) -> Result<()> {
    use agile_md::links::LinkType;
    let old = LinkType::parse(old).with_context(|| format!("unknown link type '{old}'"))?;
    let new = LinkType::parse(new).with_context(|| format!("unknown link type '{new}'"))?;
    let task = board.find(reference)?;
    for other in to {
        let other = board.find(other)?;
        report(&transition::retype_link(board, &task, old, new, &other)?);
    }
    Ok(())
}

/// Opens the desktop board: the `riteui` binary installed beside this one,
/// or else the first on `PATH`. The board is its own program (Tauri), so
/// `rite` carries no window stack (ticket 084).
fn cmd_gui() -> Result<()> {
    let beside = env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("riteui")))
        .filter(|path| path.is_file());
    let program = beside.unwrap_or_else(|| PathBuf::from("riteui"));
    match Command::new(&program).status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => bail!("riteui exited with {status}"),
        Err(err) if err.kind() == io::ErrorKind::NotFound => bail!(
            "the desktop board isn't installed: riteui ships with rite — \
             run the install again (see the README), or build it from a clone (CONTRIBUTING.md)"
        ),
        Err(err) => Err(err).context("starting riteui"),
    }
}

/// Move a ticket and say so. The wording is the CLI's, not the library's —
/// `Board::move_task` returns where the ticket landed and leaves the reporting
/// to whoever asked for the move; `transition::move_to` does the move, the
/// lease and the auto-commit.
fn move_and_report(board: &Board, task: &task::Task, to: Column, force: bool) -> Result<()> {
    report(&transition::move_to(board, task, to, force)?);
    Ok(())
}

/// Runs `mutate`, then commits and pushes what it changed (see
/// `transition::with_auto_commit`), warning rather than failing if that
/// can't be done.
fn with_auto_commit<T>(
    board: &Board,
    verb: &str,
    subject: &str,
    detail: &str,
    mutate: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let (result, warning) = transition::with_auto_commit(board, verb, subject, detail, mutate)?;
    if let Some(warning) = warning {
        eprintln!("rite: {warning}");
    }
    Ok(result)
}

/// Where a destination verb can send a ticket: one of the four columns,
/// taking it off the board entirely, or filing it under an existing epic.
enum Destination {
    Column(Column),
    Abandon,
    Epic(String),
}

/// Check a destination verb against the board — a column, `abandon`, or the
/// name of an epic that already exists — and say exactly what's valid when
/// it's none of those, since that list is the whole point of the error.
fn resolve_destination(board: &Board, name: &str) -> Result<Destination> {
    if let Some(column) = board.workflow().find(name) {
        return Ok(Destination::Column(column));
    }
    if name.eq_ignore_ascii_case(ABANDON) {
        return Ok(Destination::Abandon);
    }
    let epics = board.epics_in(Column::Backlog)?;
    if epics.iter().any(|epic| epic == name) {
        return Ok(Destination::Epic(name.to_string()));
    }
    let columns: Vec<String> = board.workflow().columns().map(|c| c.to_string()).collect();
    let epics_display = if epics.is_empty() {
        "none".to_string()
    } else {
        epics.join(", ")
    };
    bail!(
        "'{name}' is not a column, '{ABANDON}', or an epic on this board\n      (columns: {}; epics: {epics_display})",
        columns.join(", "),
    );
}

/// Move a ticket to a destination, or — with no ref — list what's there,
/// narrowed to `tag` when given. Every destination verb goes through this one
/// function, static columns and dynamic epic names alike, so they can't drift
/// from each other.
fn cmd_destination(
    board: &Board,
    destination: Destination,
    task: Option<String>,
    tag: &[String],
    force: bool,
) -> Result<()> {
    match (destination, task) {
        (Destination::Column(column), Some(reference)) => {
            let task = board.find(&reference)?;
            // Any todo-kind column makes the branch, as `rite todo` does.
            if column.is(Kind::Todo) {
                let outcome = transition::enter_todo(
                    board,
                    &task,
                    &transition::TodoOptions {
                        force,
                        to: Some(column),
                        ..Default::default()
                    },
                )?;
                report(&outcome);
                return Ok(());
            }
            move_and_report(board, &task, column, force)
        }
        (Destination::Column(column), None) => render::columns(board, &[column], tag),
        (Destination::Abandon, Some(reference)) => {
            let task = board.find(&reference)?;
            let (id_display, title) = (task.id_display(), task.title());
            with_auto_commit(board, "abandon", &id_display, &title, || {
                let name = task.file_name();
                let (_, deleted_branch) = board.archive(&task)?;
                println!("archived {name} -> archive/");
                if let Some(branch) = deleted_branch {
                    println!("deleted {branch} (empty)");
                }
                Ok(())
            })
        }
        (Destination::Abandon, None) => render::named(ABANDON, board.archived()?, tag),
        (Destination::Epic(name), Some(reference)) => {
            let task = board.find(&reference)?;
            let (id_display, title) = (task.id_display(), task.title());
            with_auto_commit(board, "set", &id_display, &title, || {
                board.set_epic(&task, Some(&name))?;
                println!("{} filed under {name}", task.id_display());
                Ok(())
            })
        }
        (Destination::Epic(name), None) => {
            let tasks = board.tasks_in_group(&name)?;
            // A sprint holding tickets with due dates (ticket 253): they
            // may not fit its days.
            if board.group(&name).is_ok_and(|g| g.is_sprint()) {
                let dated: Vec<String> = tasks
                    .iter()
                    .filter_map(|t| t.due().map(|due| format!("{} (due {due})", t.id_display())))
                    .collect();
                if !dated.is_empty() {
                    eprintln!(
                        "warning: {name} holds tickets with due dates: {}. Check they fit the sprint",
                        dated.join(", ")
                    );
                }
            }
            render::named(&name, tasks, tag)
        }
    }
}

/// An empty value reads better as a word than as nothing at all.
fn display(value: &str) -> String {
    match value.is_empty() {
        true => "(cleared)".to_string(),
        false => value.to_string(),
    }
}

fn cmd_group(board: &Board, command: GroupCmd) -> Result<()> {
    use agile_md::group::{Group, Kind, State};
    match command {
        GroupCmd::List => {
            let groups = board.groups()?;
            if groups.is_empty() {
                println!("(no epics or sprints — add one with: rite group epic <name>)");
                return Ok(());
            }
            for group in groups {
                let tickets = board.tasks_in_group(&group.name)?;
                let points: i64 = tickets
                    .iter()
                    .filter_map(|task| task.points())
                    .filter_map(|p| p.trim().parse::<i64>().ok())
                    .sum();
                let detail = match group.is_sprint() {
                    true => format!("sprint  {}d  {}", group.days, group.state.as_str()),
                    false => "epic".to_string(),
                };
                println!(
                    "{:<24} {detail:<22} {} ticket(s), {points} point(s)",
                    group.name,
                    tickets.len()
                );
            }
            Ok(())
        }
        GroupCmd::Epic { name, description } => {
            with_auto_commit(board, "group-epic", &name, &description, || {
                let group = Group {
                    dir: board.dir(Column::Backlog).join(&name),
                    name: name.clone(),
                    kind: Kind::Epic,
                    description: description.clone(),
                    days: agile_md::group::DEFAULT_DAYS,
                    state: State::Pending,
                };
                board.create_group(&group)?;
                println!("created epic {name}");
                Ok(())
            })
        }
        GroupCmd::Sprint {
            name,
            description,
            days,
        } => with_auto_commit(board, "group-sprint", &name, &description, || {
            let group = Group {
                dir: board.dir(Column::Backlog).join(&name),
                name: name.clone(),
                kind: Kind::Sprint,
                description: description.clone(),
                days,
                state: State::Pending,
            };
            board.create_group(&group)?;
            println!("created sprint {name} ({days} days)");
            Ok(())
        }),
        GroupCmd::Start { name } => with_auto_commit(board, "group-start", &name, "", || {
            // Refused while its tickets require others outside it (ticket 214).
            agile_md::requires::start_sprint(board, &name)?;
            println!("{name} started — its tickets are now fixed");
            Ok(())
        }),
        GroupCmd::Close { name } => {
            use agile_md::{documents, sprints, stats};
            let now = stats::now();
            let repo = board
                .repo_root()
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let closed = with_auto_commit(board, "group-close", &name, "", || {
                sprints::close(board, &repo, &name, now)
            })?;
            let done: u32 = closed.done.iter().map(|d| d.2).sum();
            println!(
                "{name} closed: {done} of {} points done, {} ticket(s) not done",
                closed.burn.total,
                closed.not_done.len()
            );
            if !closed.returned.is_empty() {
                println!("back to the backlog: {}", closed.returned.join(", "));
            }
            let own = documents::sources(board)
                .into_iter()
                .find(|s| s.kind == documents::Kind::Own);
            match own.and_then(|s| s.root.map(|root| (s.name, root))) {
                Some((source, root)) => {
                    let (path, filed) = sprints::file_report(&root, &closed, now)?;
                    if let Some(warning) = &filed.warning {
                        eprintln!("rite: {warning}");
                    }
                    println!("report: {source}/{path}");
                }
                None => eprintln!(
                    "rite: no documents repository cloned here, so no close report was written"
                ),
            }
            Ok(())
        }
        GroupCmd::Backfill { name, points } => {
            with_auto_commit(board, "group-backfill", &name, "", || {
                let filed = agile_md::sprints::backfill(board, &name, points)?;
                println!("backfilled {name}: {}", filed.join(", "));
                Ok(())
            })
        }
        GroupCmd::Empty { name, to } if to == "todo" => {
            with_auto_commit(board, "group-todo", &name, "", || {
                let result = transition::group_to_todo(board, &name)?;
                println!(
                    "{name}: {} ticket(s) moved to todo/{}",
                    result.moved.len(),
                    if result.moved.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", result.moved.join(", "))
                    }
                );
                for (id, err) in &result.skipped {
                    eprintln!("rite: {id} not moved: {err}");
                }
                Ok(())
            })
        }
        GroupCmd::Empty { name, .. } => with_auto_commit(board, "group-empty", &name, "", || {
            let result = board.empty_group(&name)?;
            println!(
                "{} emptied into backlog/ ({} ticket(s))",
                name,
                result.moved.len()
            );
            for (stem, err) in &result.skipped {
                eprintln!("rite: {stem} not moved: {err}");
            }
            Ok(())
        }),
        GroupCmd::Delete { name } => with_auto_commit(board, "group-delete", &name, "", || {
            board.delete_group(&name)?;
            println!("deleted {name}");
            Ok(())
        }),
        GroupCmd::Rename { name, to } => {
            with_auto_commit(board, "group-rename", &name, &to, || {
                board.rename_group(&name, &to)?;
                println!("renamed {name} -> {}", to.trim());
                Ok(())
            })
        }
    }
}

/// Sets doing/'s WIP limit — the only way to set an explicit value besides
/// `riteui`'s Settings modal or hand-editing `.settings`. `limit` is a `u32`,
/// so clap already refuses a non-numeric argument before this runs; `0` is
/// a valid value and means the limit is turned off.
fn cmd_wip_limit(board: &Board, limit: u32) -> Result<()> {
    // Committed and pushed like a ticket move, so it reaches the whole team
    // (ticket 111).
    with_auto_commit(board, "settings", "wip-limit", &limit.to_string(), || {
        let mut settings = board_settings::load(board);
        settings.set_wip_limit(Column::Doing, Some(limit))?;
        settings.save(board)
    })?;
    match limit {
        0 => println!("doing/'s WIP limit is now off"),
        n => println!("doing/'s WIP limit is now {n}"),
    }
    Ok(())
}

/// Names the documents repository every project shares (ticket 129), or
/// clears it, committed and pushed like any setting; with neither, says
/// which it is.
fn cmd_shared_documents(board: &Board, name: Option<String>, clear: bool) -> Result<()> {
    if name.is_none() && !clear {
        match board_settings::load(board).shared_documents() {
            Some(name) => println!("{name}"),
            None => eprintln!("no shared documents repository (rite shared-documents <name>)"),
        }
        return Ok(());
    }
    let name = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    let what = name.clone().unwrap_or_else(|| "cleared".to_string());
    with_auto_commit(board, "settings", "shared-documents", &what, || {
        let mut settings = board_settings::load(board);
        settings.set_shared_documents(name.as_deref())?;
        settings.save(board)
    })?;
    match name {
        Some(name) => println!("shared documents: {name}"),
        None => println!("no shared documents repository"),
    }
    Ok(())
}

/// `rite display-name [NAME | --clear]` (ticket 326): the name riteui and
/// the MCP tools show for the project, committed and pushed like any
/// setting; with neither, says what's shown.
fn cmd_display_name(board: &Board, name: Option<String>, clear: bool) -> Result<()> {
    let settings = board_settings::load(board);
    if name.is_none() && !clear {
        match settings.display_name() {
            Some(name) => println!("{name}"),
            None => eprintln!(
                "no display name: the project's own name is shown (rite display-name <name>)"
            ),
        }
        return Ok(());
    }
    let name = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    // Checked before anything is committed.
    settings.clone().set_display_name(name.as_deref())?;
    let what = name.clone().unwrap_or_else(|| "cleared".to_string());
    with_auto_commit(board, "settings", "display-name", &what, || {
        let mut settings = board_settings::load(board);
        settings.set_display_name(name.as_deref())?;
        settings.save(board)
    })?;
    match name {
        Some(name) => println!("display name: {name}"),
        None => println!("no display name: the project's own name is shown"),
    }
    Ok(())
}

/// `rite auto-priority [on|off]` (ticket 215): switching commits the
/// setting and every priority it moves, together.
fn cmd_auto_priority(board: &Board, action: Option<&str>, days: &[u32]) -> Result<()> {
    if action != Some("limits") && !days.is_empty() {
        bail!("days go with `limits`: rite auto-priority limits TODO DOING");
    }
    if action == Some("limits") {
        return cmd_time_limits(board, days);
    }
    let Some(action) = action else {
        let settings = board_settings::load(board);
        let on = settings.auto_priority();
        println!("auto priority is {}", if on { "on" } else { "off" });
        println!(
            "due dates raise a ticket a step each quarter from its creation to its due date: medium, high, then top"
        );
        println!("{}", time_limits_line(settings.time_limits()));
        for task in board.tasks()? {
            if let Some(note) = agile_md::auto_priority::note(board, &task) {
                println!("{}: {note}", task.id_display());
            }
        }
        return Ok(());
    };
    let on = action == "on";
    with_auto_commit(board, "settings", "auto-priority", action, || {
        let mut settings = board_settings::load(board);
        settings.set_auto_priority(on);
        settings.save(board)
    })?;
    println!(
        "{}",
        if on {
            "auto priority is on: a ticket is at least as urgent as what requires it, and rises as its due date nears"
        } else {
            "auto priority is off: raised priorities are put back"
        }
    );
    Ok(())
}

/// How auto priority treats time in todo and doing (ticket 252), in a line.
fn time_limits_line(limits: board_settings::TimeLimits) -> String {
    let one = |days: u32, column: &str| match days {
        0 => format!("no time limit in {column}"),
        days => format!("{days} days in {column}"),
    };
    format!(
        "time limits: {}, {} (a step up for each quarter used)",
        one(limits.todo, "todo"),
        one(limits.doing, "doing")
    )
}

/// `rite auto-priority limits [TODO DOING]` (ticket 252): show or set how
/// long a ticket should take in todo and in doing. One commit, with the
/// priorities it moves.
fn cmd_time_limits(board: &Board, days: &[u32]) -> Result<()> {
    let [todo, doing] = days else {
        if days.is_empty() {
            println!(
                "{}",
                time_limits_line(board_settings::load(board).time_limits())
            );
            return Ok(());
        }
        bail!("give both: rite auto-priority limits TODO DOING (e.g. 10 5; 0 is no limit)");
    };
    let wanted = board_settings::TimeLimits {
        todo: *todo,
        doing: *doing,
    };
    with_auto_commit(
        board,
        "settings",
        "time-limits",
        &format!("{todo} {doing}"),
        || {
            let mut settings = board_settings::load(board);
            settings.set_time_limits(wanted);
            settings.save(board)
        },
    )?;
    println!("{}", time_limits_line(wanted));
    if !board_settings::load(board).auto_priority() {
        println!("(auto priority is off: `rite auto-priority on` to use them)");
    }
    Ok(())
}

/// `rite require-version [VERSION] [--clear] [--yes]` (ticket 193).
fn cmd_require_version(board: &Board, version: Option<&str>, clear: bool, yes: bool) -> Result<()> {
    let current = board_settings::load(board).min_version();
    if clear {
        if current.is_none() {
            println!("this project requires no version");
            return Ok(());
        }
        with_auto_commit(board, "settings", "min-version", "none", || {
            let mut settings = board_settings::load(board);
            settings.set_min_version(None);
            settings.save(board)
        })?;
        println!("this project no longer requires a version");
        return Ok(());
    }
    let Some(version) = version else {
        match current {
            Some(v) => println!(
                "this project needs KanRite {v} or newer to change it; older builds only read it"
            ),
            None => println!("this project requires no version: any KanRite version can change it"),
        }
        return Ok(());
    };
    let version = agile_md::compat::check_required(version)?;
    let warning = format!(
        "Everyone on a KanRite version older than {version} will only be able to read this project until they update. Continue?"
    );
    if !yes {
        if !form::available() {
            bail!("{warning} Add --yes to confirm");
        }
        if !form::confirm(&warning)? {
            bail!("cancelled");
        }
    }
    with_auto_commit(
        board,
        "settings",
        "min-version",
        &version.to_string(),
        || {
            let mut settings = board_settings::load(board);
            settings.set_min_version(Some(&version.to_string()));
            settings.save(board)
        },
    )?;
    println!(
        "this project now needs KanRite {version} or newer to change it; older builds only read it"
    );
    Ok(())
}

/// `rite due-to-todo [DAYS]` (ticket 253).
fn cmd_due_to_todo(board: &Board, days: Option<u32>) -> Result<()> {
    let say = |days: u32| match days {
        0 => "a due date never sends a ticket straight to todo".to_string(),
        days => format!("a ticket due within {days} days goes straight to todo"),
    };
    let Some(days) = days else {
        println!("{}", say(board_settings::load(board).due_to_todo_days()));
        return Ok(());
    };
    with_auto_commit(
        board,
        "settings",
        "due-to-todo-days",
        &days.to_string(),
        || {
            let mut settings = board_settings::load(board);
            settings.set_due_to_todo_days(days);
            settings.save(board)
        },
    )?;
    println!("{}", say(days));
    Ok(())
}

/// `rite backlog-always-shown [on|off]` (ticket 253).
fn cmd_backlog_always_shown(board: &Board, action: Option<&str>) -> Result<()> {
    let say = |on: bool| {
        if on {
            "the backlog column is always shown: it can't be folded away"
        } else {
            "the backlog column can be folded away"
        }
    };
    let Some(action) = action else {
        println!(
            "{}",
            say(board_settings::load(board).backlog_always_shown())
        );
        return Ok(());
    };
    let on = action == "on";
    with_auto_commit(board, "settings", "backlog-always-shown", action, || {
        let mut settings = board_settings::load(board);
        settings.set_backlog_always_shown(on);
        settings.save(board)
    })?;
    println!("{}", say(on));
    Ok(())
}

/// `rite priority-override [on|off]` (ticket 215).
fn cmd_priority_override(board: &Board, action: Option<&str>) -> Result<()> {
    let Some(action) = action else {
        let on = board_settings::load(board).priority_override();
        println!(
            "priority override is {}",
            if on {
                "on: a priority set by hand pins the ticket"
            } else {
                "off: a priority set by hand is a floor auto priority can raise"
            }
        );
        return Ok(());
    };
    let on = action == "on";
    with_auto_commit(board, "settings", "priority-override", action, || {
        let mut settings = board_settings::load(board);
        settings.set_priority_override(on);
        settings.save(board)
    })?;
    println!(
        "{}",
        if on {
            "priority override is on: a priority set by hand pins the ticket"
        } else {
            "priority override is off: a priority set by hand is a floor"
        }
    );
    Ok(())
}

/// `rite triage-priority` is retired (ticket 248): every TRIAGE ticket is
/// high, so there's nothing to choose.
fn cmd_triage_priority(_board: &Board, priority: Option<String>) -> Result<()> {
    match priority {
        None => println!("top (every TRIAGE ticket is top; it's no longer a setting)"),
        Some(_) => bail!(
            "the triage priority is no longer a setting: every TRIAGE ticket is top, and heads everyone's My work"
        ),
    }
    Ok(())
}

/// Carries settings still kept in the project's documents repository into
/// the tickets `.settings`, once, committed and pushed (ticket 111). Silent
/// once everything is across.
fn migrate_settings(board: &Board) -> Result<()> {
    let mut moved = Vec::new();
    with_auto_commit(board, "settings", "migrate", "from documents", || {
        moved = board_settings::migrate_from_documents(board)?;
        Ok(())
    })?;
    if !moved.is_empty() {
        let from = board_settings::documents_settings_dir(board)
            .map(|dir| dir.display().to_string())
            .unwrap_or_else(|| "the documents repository".to_string());
        eprintln!(
            "moved {} from {from} into the tickets .settings, where board settings live now — \
             {from} is no longer read; delete it with a documents PR",
            moved.join(", ")
        );
    }
    Ok(())
}

/// Registers a repository, both halves at once: declares its name for the
/// whole project (`board.settings`, shared, git-tracked — everyone sees it
/// listed on their next pull) and its path on this machine
/// (`registry.rs`, local, never shared). `path` defaults to the repository
/// you're standing in, same as the old `rite repos add`. Both halves are
/// idempotent, so registering something already declared/already local is
/// never an error.
fn cmd_register(board: &Board, path: Option<&str>) -> Result<()> {
    let path = match path {
        Some(path) => PathBuf::from(path),
        None => git::repo_root().ok_or_else(|| anyhow::anyhow!("not inside a git repository"))?,
    };
    let canonical =
        dunce::canonicalize(&path).with_context(|| format!("{} doesn't exist", path.display()))?;
    let name = canonical
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| anyhow::anyhow!("{} has no name", canonical.display()))?;

    // The declaration is the team's, so it's committed and pushed (ticket
    // 111); the registry below is this machine's alone.
    with_auto_commit(board, "register", &name, "", || {
        let mut settings = board_settings::load(board);
        settings.declare_repo(&name)?;
        settings.save(board)
    })?;

    let mut registry = Registry::load();
    let added = registry.add(&canonical)?;
    registry.save()?;
    init::warn_if_cloned_twice(&registry, &canonical);

    match added {
        true => println!("registered {name}"),
        false => println!("{name} was already registered on this machine"),
    }
    Ok(())
}

/// `rite repo` (ticket 228).
#[derive(Subcommand, Debug)]
enum RepoAction {
    /// Make a repository on GitHub under the tickets repository's owner (or
    /// beside a local origin), or connect it if it's already there; clone
    /// it beside the tickets repository, declare it for the project and
    /// register it here. With --for, the ticket's repository is set and a
    /// note says so
    New {
        #[arg(value_name = "NAME")]
        name: String,
        #[arg(long, value_name = "TEXT")]
        description: Option<String>,
        /// Public, whatever the tickets repository is
        #[arg(long, conflicts_with = "private")]
        public: bool,
        /// Private, whatever the tickets repository is
        #[arg(long)]
        private: bool,
        /// No starter README
        #[arg(long)]
        no_readme: bool,
        /// The ticket it's for: its repository is set, and a note added
        #[arg(long = "for", value_name = "REF")]
        for_ticket: Option<String>,
    },
}

/// Makes or connects a repository for the project (ticket 228), in one
/// commit on the tickets repository with the ticket it's for.
fn cmd_repo(board: &Board, action: RepoAction) -> Result<()> {
    use agile_md::new_repo::{self, NewRepo};
    let RepoAction::New {
        name,
        description,
        public,
        private,
        no_readme,
        for_ticket,
    } = action;
    let task = for_ticket.map(|r| board.find(&r)).transpose()?;
    let new = NewRepo {
        name: name.clone(),
        description: description
            .clone()
            .unwrap_or_else(|| task.as_ref().map(|t| t.title()).unwrap_or_default()),
        private: match (public, private) {
            (true, _) => Some(false),
            (_, true) => Some(true),
            _ => None,
        },
        readme: !no_readme,
        for_ticket: task.as_ref().map(|t| (t.id_display(), t.title())),
    };
    let made = new_repo::create(board, &new, &mut new_repo::gh)?;
    new_repo::register(&made)?;
    let drafted = with_auto_commit(board, "repo", "new", &made.name, || {
        new_repo::declare(board, &made.name)?;
        match &task {
            Some(task) => new_repo::record_on_ticket(board, task, &made),
            None => Ok(None),
        }
    })?;
    let what = if made.created { "created" } else { "connected" };
    println!("{what} {} at {}", made.name, made.path.display());
    if let Some(task) = &task {
        println!("{} is now about {}", task.id_display(), made.name);
    }
    for note in made.notes.iter().chain(&drafted) {
        eprintln!("rite: {note}");
    }
    Ok(())
}

/// `rite column` (ticket 224): the CLI side of Admin's Columns section.
#[derive(Subcommand, Debug)]
enum ColumnAction {
    /// Add a column. Its kind decides what it does: backlog (where work
    /// waits), todo (entering it makes the branch), doing (locked while in
    /// it, with a WIP limit) or done (waits for what the ticket requires)
    Add {
        #[arg(value_name = "NAME")]
        name: String,
        #[arg(long, value_name = "backlog|todo|doing|done")]
        kind: String,
        /// Put it after this column (the end when neither is given)
        #[arg(long, value_name = "COLUMN", conflicts_with = "before")]
        after: Option<String>,
        /// Put it before this column
        #[arg(long, value_name = "COLUMN")]
        before: Option<String>,
        /// Its folder under tasks/ (made from the name when left out)
        #[arg(long, value_name = "FOLDER")]
        folder: Option<String>,
    },
    /// Give a column a new name. Its folder stays as it is
    Rename {
        #[arg(value_name = "COLUMN")]
        column: String,
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// Remove an added column (the standard four can't go). One with
    /// tickets needs --move-to
    Remove {
        #[arg(value_name = "COLUMN")]
        column: String,
        /// Where its tickets go first
        #[arg(long, value_name = "COLUMN")]
        move_to: Option<String>,
    },
    /// Move a column along the board
    Move {
        #[arg(value_name = "COLUMN")]
        column: String,
        #[arg(
            long,
            value_name = "COLUMN",
            conflicts_with = "before",
            required_unless_present = "before"
        )]
        after: Option<String>,
        #[arg(long, value_name = "COLUMN")]
        before: Option<String>,
    },
    /// Set a doing column's WIP limit, or `off`
    Wip {
        #[arg(value_name = "COLUMN")]
        column: String,
        #[arg(value_name = "N|off")]
        limit: String,
    },
    /// Which moves are allowed: `next` (the next column either way, plus
    /// any moves allowed with `rite column allow`; the default) or `any`
    /// (any column to any other). Shows it with no value
    Flow {
        #[arg(value_name = "next|any")]
        flow: Option<String>,
    },
    /// Allow an extra one-way move under the `next` flow, e.g. `rite column
    /// allow triage todo`
    Allow {
        #[arg(value_name = "FROM")]
        from: String,
        #[arg(value_name = "TO")]
        to: String,
    },
    /// Stop allowing an extra move
    Disallow {
        #[arg(value_name = "FROM")]
        from: String,
        #[arg(value_name = "TO")]
        to: String,
    },
    /// Where new tickets land: a backlog column (Backlog unless changed)
    Landing {
        #[arg(value_name = "COLUMN")]
        column: String,
    },
    /// Back to the standard four: tickets in added columns move to the
    /// standard column of the same kind, and the names, flow, extra moves,
    /// landing column and column WIP limits go back to standard
    Reset,
}

/// `name`'s column on `board`, or an error listing the columns there are.
fn find_column(board: &Board, name: &str) -> Result<Column> {
    board
        .workflow()
        .find(name)
        .with_context(|| column_list(board, name))
}

fn column_list(board: &Board, name: &str) -> String {
    let names: Vec<String> = board
        .workflow()
        .defs()
        .iter()
        .map(|d| d.column.as_str().to_string())
        .collect();
    format!(
        "'{name}' isn't a column on this board (columns: {})",
        names.join(", ")
    )
}

/// Lists the board's columns, or changes them (ticket 224), each change
/// committed like a setting.
fn cmd_column(board: &Board, action: Option<ColumnAction>) -> Result<()> {
    use agile_md::columns::{self, Position};
    let position = |after: Option<String>, before: Option<String>| -> Result<Position> {
        Ok(match (after, before) {
            (Some(a), _) => Position::After(find_column(board, &a)?),
            (None, Some(b)) => Position::Before(find_column(board, &b)?),
            (None, None) => Position::End,
        })
    };
    match action {
        None => {
            let workflow = board.workflow();
            let settings = board_settings::load(board);
            for def in workflow.defs() {
                let count = board.tasks_in(def.column)?.len();
                let wip = match settings.wip_limit(def.column) {
                    Some(n) => format!(", WIP limit {n}"),
                    None if def.column.is(Kind::Doing) => ", no WIP limit".to_string(),
                    None => String::new(),
                };
                println!(
                    "{}  {}/  {}{wip}  ({count})",
                    def.label,
                    def.column.as_str(),
                    def.column.kind().as_str()
                );
            }
            let workflow = board.workflow();
            let label = |c: Column| workflow.label(c);
            match workflow.flow() {
                agile_md::workflow::Flow::Any => println!("flow: any (any column to any other)"),
                agile_md::workflow::Flow::Next => {
                    let extra: Vec<String> = workflow
                        .moves()
                        .iter()
                        .map(|(f, t)| format!("{} → {}", label(*f), label(*t)))
                        .collect();
                    if extra.is_empty() {
                        println!("flow: next (one column at a time)");
                    } else {
                        println!("flow: next, plus {}", extra.join(", "));
                    }
                }
            }
            println!("new tickets land in: {}", label(workflow.landing()));
            if let Some(problem) = board.workflow_problem() {
                eprintln!("rite: {problem}");
            }
            Ok(())
        }
        Some(ColumnAction::Flow { flow: None }) => {
            println!("{}", board.workflow().flow().as_str());
            Ok(())
        }
        Some(ColumnAction::Flow { flow: Some(flow) }) => {
            let flow = agile_md::workflow::Flow::parse(&flow)
                .with_context(|| format!("'{flow}' isn't a flow: next or any"))?;
            with_auto_commit(board, "column", "flow", flow.as_str(), || {
                columns::set_flow(board, flow)
            })?;
            println!("flow: {}", flow.as_str());
            Ok(())
        }
        Some(ColumnAction::Allow { from, to }) => {
            let (f, t) = (find_column(board, &from)?, find_column(board, &to)?);
            let what = format!("{}>{}", f.as_str(), t.as_str());
            with_auto_commit(board, "column", "allow", &what, || {
                columns::allow_move(board, f, t, true)
            })?;
            let w = board.workflow();
            println!("allowed {} → {}", w.label(f), w.label(t));
            if w.flow() == agile_md::workflow::Flow::Any {
                eprintln!("rite: the flow is any, so every move is allowed anyway");
            }
            Ok(())
        }
        Some(ColumnAction::Disallow { from, to }) => {
            let (f, t) = (find_column(board, &from)?, find_column(board, &to)?);
            let what = format!("{}>{}", f.as_str(), t.as_str());
            with_auto_commit(board, "column", "disallow", &what, || {
                columns::allow_move(board, f, t, false)
            })?;
            let w = board.workflow();
            println!("no longer allowed: {} → {}", w.label(f), w.label(t));
            Ok(())
        }
        Some(ColumnAction::Landing { column }) => {
            let col = find_column(board, &column)?;
            let label = board.workflow().label(col);
            with_auto_commit(board, "column", "landing", &label, || {
                columns::set_landing(board, col)
            })?;
            println!("new tickets land in {label}");
            Ok(())
        }
        Some(ColumnAction::Reset) => {
            let moved = with_auto_commit(board, "column", "reset", "standard", || {
                columns::reset(board)
            })?;
            for (file, to) in &moved {
                println!("moved {file} -> {to}/");
            }
            println!("columns back to Backlog | To do | Doing | Done");
            Ok(())
        }
        Some(ColumnAction::Add {
            name,
            kind,
            after,
            before,
            folder,
        }) => {
            let kind = Kind::parse(&kind)
                .with_context(|| format!("'{kind}' isn't a kind: backlog, todo, doing or done"))?;
            let at = position(after, before)?;
            let column = with_auto_commit(board, "column", "add", name.trim(), || {
                columns::add(board, &name, kind, at, folder.as_deref())
            })?;
            println!(
                "added {} ({}/, {})",
                name.trim(),
                column.as_str(),
                kind.as_str()
            );
            eprintln!(
                "rite: teammates need amd 4.1 or later to see tickets in {}/",
                column.as_str()
            );
            Ok(())
        }
        Some(ColumnAction::Rename { column, name }) => {
            let col = find_column(board, &column)?;
            with_auto_commit(board, "column", "rename", name.trim(), || {
                columns::rename(board, col, &name)
            })?;
            println!("{}/ is now called {}", col.as_str(), name.trim());
            Ok(())
        }
        Some(ColumnAction::Remove { column, move_to }) => {
            let col = find_column(board, &column)?;
            let to = move_to.map(|m| find_column(board, &m)).transpose()?;
            let label = board.workflow().label(col);
            let moved = with_auto_commit(board, "column", "remove", &label, || {
                columns::remove(board, col, to)
            })?;
            if let Some(to) = to
                && !moved.is_empty()
            {
                println!("moved {} ticket(s) to {}/", moved.len(), to.as_str());
            }
            println!("removed {label}");
            Ok(())
        }
        Some(ColumnAction::Move {
            column,
            after,
            before,
        }) => {
            let col = find_column(board, &column)?;
            let at = position(after, before)?;
            let label = board.workflow().label(col);
            with_auto_commit(board, "column", "move", &label, || {
                columns::reorder(board, col, at)
            })?;
            let order: Vec<String> = board
                .workflow()
                .defs()
                .iter()
                .map(|d| d.label.clone())
                .collect();
            println!("{}", order.join(" | "));
            Ok(())
        }
        Some(ColumnAction::Wip { column, limit }) => {
            let col = find_column(board, &column)?;
            let limit = match limit.trim().to_ascii_lowercase().as_str() {
                "off" | "0" => None,
                n => Some(
                    n.parse::<u32>()
                        .with_context(|| format!("'{n}' isn't a number or off"))?,
                ),
            };
            let label = board.workflow().label(col);
            with_auto_commit(board, "column", "wip", &label, || {
                columns::set_wip(board, col, limit)
            })?;
            match limit {
                Some(n) => println!("{label}: WIP limit {n}"),
                None => println!("{label}: no WIP limit"),
            }
            Ok(())
        }
    }
}

/// `rite tags` (ticket 122): the CLI side of Admin's Tags section.
#[derive(Subcommand, Debug)]
enum TagsAction {
    /// Add a tag to the project's list, in UPPERCASE
    Add {
        #[arg(value_name = "TAG")]
        tag: String,
    },
    /// Take a tag off the list. Only an unused tag can go: one no ticket on
    /// any of the project's boards carries
    Remove {
        #[arg(value_name = "TAG")]
        tag: String,
    },
    /// Prevent arbitrary tags: allow only listed tags on tickets (`on`, the
    /// default), or any tag with a warning for unlisted ones (`off`)
    Only {
        #[arg(value_name = "on|off")]
        state: String,
    },
}

/// Lists the project's tags with how many tickets on its boards carry each,
/// or changes the list. Every change is committed like any other setting.
fn cmd_tags(board: &Board, action: Option<TagsAction>) -> Result<()> {
    let repos = agile_md::model::find_repos()?;
    let boards: Vec<&Board> = repos.repos.iter().map(|(_, _, board)| board).collect();
    match action {
        None => {
            let settings = board_settings::load(board);
            let listed = settings.tags();
            let counts = agile_md::model::tag_counts(&boards)?;
            if listed.is_empty() {
                println!(
                    "nothing listed yet: only the built-in DRAFT and TRIAGE; `rite tags add <TAG>` adds one"
                );
            }
            for tag in &listed {
                let n = counts.get(tag).copied().unwrap_or(0);
                println!("{tag}\t{n}{}", if n == 0 { "\tunused" } else { "" });
            }
            let unlisted: Vec<String> = counts
                .iter()
                .filter(|(tag, _)| !listed.contains(tag) && !task::is_reserved_tag(tag))
                .map(|(tag, n)| format!("{tag} ({n})"))
                .collect();
            if !unlisted.is_empty() {
                println!("in use but not listed: {}", unlisted.join(", "));
            }
            let built_in: Vec<String> = task::RESERVED_TAGS
                .iter()
                .map(|tag| format!("{tag} ({})", counts.get(*tag).copied().unwrap_or(0)))
                .collect();
            println!("built in: {}", built_in.join(", "));
            let only = if settings.only_listed_tags() {
                "on (arbitrary tags are prevented)"
            } else {
                "off (arbitrary tags are allowed, with a warning)"
            };
            println!("only listed tags: {only}");
            Ok(())
        }
        Some(TagsAction::Add { tag }) => {
            let mut added = String::new();
            with_auto_commit(board, "tags", "add", &task::normalize_tag(&tag), || {
                let mut settings = board_settings::load(board);
                added = settings.add_tag(&tag)?;
                settings.save(board)
            })?;
            println!("added {added}");
            Ok(())
        }
        Some(TagsAction::Remove { tag }) => {
            let mut removed = String::new();
            with_auto_commit(board, "tags", "remove", &task::normalize_tag(&tag), || {
                removed = agile_md::model::remove_unused_tag(board, &boards, &tag)?;
                Ok(())
            })?;
            println!("removed {removed}");
            Ok(())
        }
        Some(TagsAction::Only { state }) => {
            let on = match state.trim().to_ascii_lowercase().as_str() {
                "on" => true,
                "off" => false,
                other => bail!("'{other}': say `on` or `off`"),
            };
            with_auto_commit(
                board,
                "tags",
                "only listed",
                if on { "on" } else { "off" },
                || {
                    let mut settings = board_settings::load(board);
                    settings.set_only_listed_tags(on);
                    settings.save(board)
                },
            )?;
            println!("only listed tags: {}", if on { "on" } else { "off" });
            Ok(())
        }
    }
}

/// `rite register --remove <name>`: takes a repository off the project's
/// declared list for everyone, committed and pushed, and off this machine's
/// registry if it's there. The clone itself is left alone.
fn cmd_unregister(board: &Board, name: &str) -> Result<()> {
    let mut removed = false;
    with_auto_commit(board, "unregister", name, "", || {
        let mut settings = board_settings::load(board);
        removed = settings.undeclare_repo(name);
        if removed {
            settings.save(board)?;
        }
        Ok(())
    })?;
    if !removed {
        bail!("{name} isn't declared for this project");
    }
    let mut registry = Registry::load();
    if registry.remove(name) {
        registry.save()?;
    }
    println!("{name} is no longer part of the project (its clone is untouched)");
    Ok(())
}

/// Which project's board `rite register` declares on (ticket 103). Normally
/// the board you're in, as for any command. But from a repository with no
/// board of its own, in a folder holding several projects, that board is
/// whichever `.current-project` names — easy to get wrong, and the result
/// is a team-wide declaration on the wrong project. So there it asks
/// first, in a terminal, and refuses without one, naming the fixes.
/// `--project` always says outright. `removing` is the repository being
/// taken off (`deregister`, `register --remove`), which the question names.
fn register_board(
    path: Option<&str>,
    project: Option<&str>,
    removing: Option<&str>,
) -> Result<Board> {
    let dir = agile_md::current_project::project_dir()?;
    let projects = agile_md::current_project::projects(&dir);
    if let Some(name) = project {
        let entry = projects.iter().find(|p| p.name == name).ok_or_else(|| {
            let names: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
            anyhow::anyhow!(
                "no project '{name}' in {} (one of: {})",
                dir.display(),
                names.join(", ")
            )
        })?;
        return Ok(Board::in_repo(&entry.tickets));
    }
    let own_board = Board::locate().is_ok_and(|board| board.root.is_dir());
    if own_board || projects.len() <= 1 {
        return ensure_board();
    }

    let board = ensure_board()?;
    let current = board_settings::load(&board)
        .project()
        .map(str::to_string)
        .unwrap_or_else(|| agile_md::current_project::project_name(board.repo_root()));
    let repo_name = match removing {
        Some(name) => name.to_string(),
        None => {
            let repo = match path {
                Some(path) => PathBuf::from(path),
                None => git::repo_root()
                    .ok_or_else(|| anyhow::anyhow!("not inside a git repository"))?,
            };
            dunce::canonicalize(&repo)
                .unwrap_or(repo)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        }
    };
    let hint = projects
        .iter()
        .find(|p| p.name != current && repo_name.starts_with(&p.name))
        .map(|p| format!(" — did you mean --project {}?", p.name))
        .unwrap_or_default();
    let (asking, refused, would) = match removing {
        Some(_) => ("Take", "nothing deregistered", "taken off"),
        None => ("Register", "nothing registered", "declared on"),
    };
    if form::available() {
        let preposition = if removing.is_some() { "off" } else { "with" };
        let question = format!(
            "{asking} {repo_name} {preposition} project {current} (this folder's current project){hint}"
        );
        if form::confirm(&question)? {
            return Ok(board);
        }
        bail!("{refused}");
    }
    bail!(
        "{repo_name} would be {would} project '{current}', this folder's current project, \
         and this folder holds {} projects — pass --project <name>, run it from \
         <name>-tickets, or `rite project use <name>` first{hint}",
        projects.len()
    )
}

/// `rite clone [name | --all]` (ticket 108).
fn cmd_clone(board: &Board, name: Option<&str>, all: bool) -> Result<()> {
    let missing = agile_md::clone::missing(board);
    let names: Vec<String> = match (name, all) {
        (Some(name), _) => vec![name.to_string()],
        (None, true) => missing,
        (None, false) => {
            if missing.is_empty() {
                println!("every declared repository is cloned here");
            } else {
                println!("not cloned here (rite clone <name>, or --all):");
                for name in &missing {
                    println!("  {name}");
                }
            }
            return Ok(());
        }
    };
    if names.is_empty() {
        println!("every declared repository is cloned here");
        return Ok(());
    }
    let mut failed = Vec::new();
    for name in &names {
        match agile_md::clone::clone_repo(board, name) {
            Ok(cloned) => {
                if cloned.already_there {
                    println!(
                        "registered {name}: already cloned at {}",
                        cloned.path.display()
                    );
                } else {
                    println!(
                        "cloned {name} into {} (from {})",
                        cloned.path.display(),
                        cloned.url
                    );
                }
                for other in cloned.also_at {
                    eprintln!(
                        "warning: {name} is also cloned at {} — two copies of one repository \
                         drift apart; prefer one",
                        other.display()
                    );
                }
            }
            Err(err) if names.len() == 1 => return Err(err),
            Err(err) => {
                eprintln!("rite: {name}: {err:#}");
                failed.push(name.as_str());
            }
        }
    }
    if !failed.is_empty() {
        bail!("couldn't clone {}", failed.join(", "));
    }
    Ok(())
}

/// Removes only the *local* registration — the project-wide declaration is
/// untouched, so `name` goes back to showing greyed out in `riteui` rather
/// than disappearing for the rest of the team.
/// `rite deregister <name>` (ticket 120). A repository the project declares
/// comes off the project for everyone, exactly as `register --remove`: off the
/// declared list, committed and pushed, so it leaves every teammate's
/// repository filter and fields, and off this machine's registry. One that's only registered here (a project's own
/// tickets or documents repository, say) just loses that registration. The
/// clone is never touched.
fn cmd_deregister(board: &Board, name: &str) -> Result<()> {
    if board_settings::load(board)
        .repos()
        .iter()
        .any(|r| r == name)
    {
        return cmd_unregister(board, name);
    }
    let mut registry = Registry::load();
    if !registry.remove(name) {
        bail!("'{name}' is neither declared for this project nor registered on this machine");
    }
    registry.save()?;
    println!("deregistered {name} on this machine");
    Ok(())
}

/// The new-ticket form's repository question: the project's declared
/// repositories that are cloned on this machine, or none. One you haven't got
/// can't be chosen (tickets 101/107), so it's named in the help instead.
fn pick_repository(board: &Board) -> Result<String> {
    let declared = board_settings::load(board).repos();
    if declared.is_empty() {
        return Ok(String::new());
    }
    let (here, missing): (Vec<String>, Vec<String>) = declared
        .into_iter()
        .partition(|name| agile_md::registry::clone_of(&board.root, name).is_some());
    if !missing.is_empty() {
        eprintln!(
            "not cloned on this machine, so not offered: {}",
            missing.join(", ")
        );
    }
    let none = "(none)".to_string();
    let mut options = vec![none.clone()];
    options.extend(here);
    let chosen = form::select("Repository:", options, &none)?;
    Ok(if chosen == none {
        String::new()
    } else {
        chosen
    })
}

/// The fields `rite set` can change.
const SET_FIELDS: [&str; 10] = [
    "points",
    "epic",
    "order",
    "title",
    "priority",
    "repository",
    "branch-type",
    "due",
    "recur",
    "requires",
];

/// `rite set`'s field and value, then any more `FIELD VALUE` pairs (ticket
/// 258), each field once; a last field with no value clears it, as a lone
/// one does. `requires` has a commit of its own, so it's set alone.
fn set_pairs(field: String, value: String, more: Vec<String>) -> Result<Vec<(String, String)>> {
    let mut pairs = vec![(field, value)];
    let mut rest = more.into_iter();
    while let Some(field) = rest.next() {
        if !SET_FIELDS.contains(&field.as_str()) {
            bail!(
                "{field} isn't a field rite set knows: {}",
                SET_FIELDS.join(", ")
            );
        }
        if pairs.iter().any(|(seen, _)| *seen == field) {
            bail!("{field} is given twice");
        }
        pairs.push((field, rest.next().unwrap_or_default()));
    }
    if pairs.len() > 1 && pairs.iter().any(|(field, _)| field == "requires") {
        bail!("set requires on its own: `rite set <ref> requires 3,7`");
    }
    Ok(pairs)
}

/// `@me` is whoever git says you are — the name that ends up in the commits.
fn resolve_assignee(repo: &Path, who: Option<&str>) -> String {
    match who {
        Some("@me") => git::config(repo, "user.name").unwrap_or_else(|| "me".to_string()),
        Some(who) => who.trim().to_string(),
        None => String::new(),
    }
}

/// Whether `command` changes the project, which an archived one refuses
/// (ticket 157): everything [`sync_purpose`] calls a write, plus the
/// settings commands it reads for, when they're given a value.
fn changes_board(command: &Cmd) -> bool {
    sync_purpose(command) == agile_md::sync::Purpose::Write
        || matches!(
            command,
            Cmd::WipLimit { .. }
                | Cmd::TriagePriority { priority: Some(_) }
                | Cmd::AutoPriority {
                    action: Some(_),
                    ..
                }
                | Cmd::PriorityOverride { action: Some(_) }
                | Cmd::DueToTodo { days: Some(_) }
                | Cmd::RequireVersion {
                    version: Some(_),
                    ..
                }
                | Cmd::RequireVersion { clear: true, .. }
                | Cmd::BacklogAlwaysShown { action: Some(_) }
                | Cmd::SharedDocuments { name: Some(_), .. }
                | Cmd::SharedDocuments { clear: true, .. }
                | Cmd::DisplayName { name: Some(_), .. }
                | Cmd::DisplayName { clear: true, .. }
                | Cmd::Templates {
                    command: Some(TemplateCmd::Eject { .. })
                }
        )
}

/// Whether `command` changes the board (catch up strictly first, refusing a
/// clone that can't be brought up to date) or only looks at it (catch up if
/// it's cheap, and warn rather than refuse). A destination verb or an epic
/// name with a ticket ref moves it; without one it lists.
fn sync_purpose(command: &Cmd) -> agile_md::sync::Purpose {
    use agile_md::sync::Purpose::{Lookup, Read, Write};
    let moves = |task: &Option<String>| if task.is_some() { Write } else { Read };
    match command {
        Cmd::Show { task: Some(_) } => Lookup,
        Cmd::Renumber { apply: false, .. } => Read,
        Cmd::Import { dry_run: true, .. } | Cmd::Issues { dry_run: true, .. } => Read,
        // Listing what was removed reads; restoring writes.
        Cmd::Recover { ids, all, .. } => {
            if ids.is_empty() && !all {
                Read
            } else {
                Write
            }
        }
        Cmd::Board { .. }
        | Cmd::Tag { .. }
        | Cmd::Tags { action: None }
        | Cmd::Column { action: None }
        | Cmd::Column {
            action: Some(ColumnAction::Flow { flow: None }),
        }
        | Cmd::Show { .. }
        | Cmd::Tree { .. }
        | Cmd::Links { .. }
        | Cmd::Log
        | Cmd::Check
        | Cmd::Stats { .. }
        | Cmd::Templates { .. }
        | Cmd::Register { .. }
        | Cmd::Clone { .. }
        | Cmd::WipLimit { .. }
        | Cmd::SharedDocuments { .. }
        | Cmd::DisplayName { .. }
        | Cmd::TriagePriority { .. }
        | Cmd::AutoPriority { action: None, .. }
        | Cmd::PriorityOverride { action: None }
        | Cmd::DueToTodo { days: None }
        | Cmd::RequireVersion {
            version: None,
            clear: false,
            ..
        }
        | Cmd::BacklogAlwaysShown { action: None }
        | Cmd::Group {
            command: None | Some(GroupCmd::List),
        } => Read,
        Cmd::Backlog { task, .. }
        | Cmd::Todo { task, .. }
        | Cmd::Doing { task, .. }
        | Cmd::Done { task, .. }
        | Cmd::Abandon { task, .. } => moves(task),
        Cmd::Verb(args) => {
            let mut rest = args.iter().skip(1);
            let mut has_ref = false;
            while let Some(arg) = rest.next() {
                if arg == "--tag" || arg == "-t" {
                    rest.next();
                } else if !arg.starts_with('-') {
                    has_ref = true;
                }
            }
            if has_ref { Write } else { Read }
        }
        _ => Write,
    }
}

/// Open the board, offering to create one when it isn't there: prompt when
/// interactive, create outright with `RITE_YES=1`, and otherwise fail with a
/// clear message — never block waiting on a pipe.
fn ensure_board() -> Result<Board> {
    let board = Board::discover()?;
    if board.root.is_dir() {
        return Ok(board);
    }
    let create = if agile_md::renamed::var("RITE_YES").as_deref() == Ok("1") {
        true
    } else if form::available() {
        eprintln!("No task board found at {}", board.root.display());
        form::confirm("Create an empty board here?")?
    } else {
        return Board::open();
    };
    if !create {
        bail!("no board created");
    }
    board.create()?;
    eprintln!("Created board at {}", board.root.display());
    Ok(board)
}

/// Resolve a task reference, or offer a picker when it was left out.
fn resolve(
    board: &Board,
    reference: Option<String>,
    command: &str,
    columns: &[Column],
) -> Result<task::Task> {
    if let Some(reference) = reference {
        return board.find(&reference);
    }
    if !form::available() {
        bail!("usage: rite {command} <ref>");
    }
    let mut choices = Vec::new();
    for column in columns {
        choices.extend(board.tasks_in(*column)?);
    }
    if choices.is_empty() {
        let names: Vec<String> = columns.iter().map(|c| format!("{c}/")).collect();
        bail!("no tasks in {}", names.join(" or "));
    }
    form::select_task(&format!("Which task should rite {command}?"), choices)
}

/// Link a new ticket to each ticket it requires, both ends.
fn requires_links(
    board: &Board,
    task: &agile_md::task::Task,
    refs: &[String],
) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for reference in refs {
        let other = board.find(reference)?;
        lines.extend(agile_md::links::apply(
            task,
            agile_md::links::LinkType::Requires,
            &other,
            false,
            false,
        )?);
    }
    Ok(lines)
}

fn cmd_new(board: &Board, args: NewArgs) -> Result<()> {
    let templates = Templates::load(board)?;
    // Resolved before anything is written, so a typo creates nothing.
    let requires = args.requires.clone();
    // Checked before anything is written (ticket 143).
    if let Some(due) = &args.due {
        agile_md::recur::parse_date(due)?;
    }
    if let Some(rule) = &args.recur {
        agile_md::recur::Every::parse(rule)?;
    }
    for reference in &requires {
        board.find(reference)?;
    }
    // A bare `rite new` (or -i) fills the whole form in; otherwise only the
    // fields the chosen template needs and the arguments didn't supply.
    let full_form = args.interactive || args.title.is_none();
    if full_form && !form::available() {
        bail!("usage: rite new \"<title>\" [-t tag ...]");
    }

    // The title first: it's the one thing every ticket has, and it names the
    // file whatever the labels turn out to be.
    let title = match args.title {
        Some(title) if !args.interactive => title,
        title => form::title(title.as_deref())?,
    };
    branch::validate_sluggable(&title)?;

    // A branch type is optional, and empty by default: a ticket only gets a
    // branch when you say what kind of work it is.
    let branch_type = match args.branch_type {
        Some(chosen) => branch::normalise(&chosen),
        None if full_form => branch::normalise(&form::select(
            "Branch type:",
            branch::choices(),
            &branch::NONE.to_string(),
        )?),
        None => String::new(),
    };
    let template = args.template.clone();

    // One parent instead of epic and story: nesting gives both, and any depth
    // past them.
    let parent = match args.parent {
        Some(parent) => Some(parent),
        None if full_form => {
            let answer = form::label(
                "Parent:",
                "",
                board.stems()?,
                "the ticket this one sits under; blank for none",
            )?;
            (!answer.is_empty()).then_some(answer)
        }
        None => None,
    };

    let assignees: Vec<String> = if !args.assignee.is_empty() {
        args.assignee
            .iter()
            .map(|who| resolve_assignee(&board.root, Some(who)))
            .collect()
    } else if full_form {
        vec![form::assignee(
            board_settings::load(board).user_names(),
            git::config(&board.root, "user.name"),
            "",
        )?]
    } else {
        Vec::new()
    };

    let repository = match args.repository {
        Some(name) => name,
        None if full_form => pick_repository(board)?,
        None => String::new(),
    };

    let related = if full_form && args.related.is_empty() {
        form::related("", board.stems()?)?
    } else {
        args.related
    };

    // Offered from the project's tag list; only with arbitrary tags allowed
    // and nothing listed, from the tags already in use (tickets 122, 167).
    // Saved in uppercase either way.
    let settings = board_settings::load(board);
    let listed = settings.tags();
    let tags = if full_form {
        let known = if listed.is_empty() && !settings.only_listed_tags() {
            board.tags()?
        } else {
            listed
        };
        form::tags(&args.tags.join(", "), known)?
    } else {
        args.tags
    };
    // A draft is a ticket carrying DRAFT (ticket 114). The form asks unless
    // `--draft` already said so, or the tags already carry it.
    let mut tags = tags;
    let draft = args.draft
        || tags
            .iter()
            .any(|t| task::normalize_tag(t) == task::DRAFT_TAG)
        || (full_form && form::confirm("Save as a draft?")?);
    if draft {
        tags.push(task::DRAFT_TAG.to_string());
    }
    let tags = task::normalize_tags(&tags);
    if let Some(warning) = board_settings::load(board).check_tags(&tags)? {
        eprintln!("rite: {warning}");
    }

    // Fields the template itself asks for: `{{ extra.owner }}` becomes a
    // question, so a template can't reference a value nobody collected.
    let mut extra: BTreeMap<String, String> = args.set.into_iter().collect();
    let required = templates.required_extras(&template)?;
    let missing: Vec<&String> = required
        .iter()
        .filter(|key| !extra.contains_key(*key))
        .collect();
    if !missing.is_empty() {
        if !form::available() {
            let flags: Vec<String> = missing.iter().map(|key| format!("--set {key}=…")).collect();
            bail!("template '{template}' needs {}", flags.join(" "));
        }
        for key in missing {
            let answer = form::text(&format!("{}:", templates::field_label(key)), None, None)?;
            extra.insert(key.clone(), answer);
        }
    }

    let draft = Draft::prepare(
        board,
        &templates,
        NewTicket {
            title,
            assignees,
            repository,
            template,
            branch_type,
            parent,
            related,
            clone_of: None,
            tags,
            extra,
            land_in: None,
        },
    )?;

    // The form doesn't stop at the metadata: unless told otherwise, it opens
    // the rendered ticket so the notes and checklist are filled in as part of
    // creating it. Nothing is written until the editor exits, so abandoning
    // the edit leaves no half-made task behind.
    let edit = !args.no_edit && form::available() && (args.edit || full_form);
    let body = if edit {
        Some(form::body(&draft.body, OsStr::new(&editor_command()))?)
    } else {
        None
    };

    let note = if draft.branch.is_empty() {
        String::new()
    } else {
        format!(" ({})", draft.branch)
    };
    let name = draft
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Not `with_auto_commit`: that helper takes its commit message eagerly,
    // but a new ticket's id isn't allocated until `draft.write` actually
    // runs (`Board::record_id`), so there's nothing to capture beforehand
    // the way every other verb captures its ticket's existing id_display.
    let auto_commit = board_settings::load(board).auto_commit();
    let before = auto_commit.then(|| git::dirty_paths(board.repo_root()).unwrap_or_default());
    let created = draft.write(board, body)?;
    if let Some(due) = &args.due {
        created.task.set_due(created.task.hash()?, due)?;
    }
    if let Some(rule) = &args.recur {
        created.task.set_recur(created.task.hash()?, rule)?;
    }
    // Due soon: straight to todo (ticket 253), in the same commit.
    let moved = args.due.as_ref().and_then(|_| {
        agile_md::task::Task::from_path(&created.task.path, created.task.column)
            .and_then(|dated| transition::straight_to_todo(board, &dated))
    });
    // What it requires (ticket 214), linked both ways in the same commit.
    for line in requires_links(board, &created.task, &requires)? {
        println!("{line}");
    }
    transition::settle_priorities(board);
    // The same commit, push and replay-or-undo every other change gets
    // (ticket 102): a clash with a ticket someone else numbered at the same
    // moment is undone here, and fails, rather than reaching the remote.
    let message = format!(
        "new: {} \"{}\"",
        created.task.id_display(),
        created.task.title()
    );
    if let Some(warning) = transition::commit_since(board, before, &message)? {
        eprintln!("rite: {warning}");
    }
    for id in &created.linked {
        println!("{id} now links to {}", created.task.id_display());
    }
    println!("created {}/{name}{note}", created.task.column);
    if let Some(moved) = &moved {
        println!("{moved}");
    }
    if let Some(drafted) = &created.drafted {
        eprintln!("rite: {drafted}");
    }
    for warning in board_settings::unknown_assignees(board, &created.task.assignees()) {
        eprintln!("warning: {warning}");
    }
    Ok(())
}

/// `rite import tickets <PATH>` (ticket 173): run from
/// the board's tasks directory, naming another board's tasks directory.
/// Never a second copy of a ticket already here; what's left out is printed
/// and appended to `import.log` here. One auto-commit.
fn cmd_import(board: &Board, path: &Path, dry_run: bool) -> Result<()> {
    let here = env::current_dir().context("reading the current directory")?;
    let same = |a: &Path, b: &Path| match (dunce::canonicalize(a), dunce::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if !same(&here, &board.root) {
        bail!(
            "run rite import tickets from the board's tasks directory: cd {}",
            board.root.display()
        );
    }
    // Anything a board has is enough (ticket 251): some of its column
    // folders, or tickets lying loose in it.
    if !agile_md::import::looks_importable(path) {
        bail!(
            "{} has nothing to import: no backlog/, todo/, doing/ or done/ folder, and no tickets (NNN-title.md) in it",
            path.display()
        );
    }
    if same(path, &board.root) {
        bail!("that's this board's own tasks directory");
    }
    let detail = format!("from {}", path.display());
    let summary = if dry_run {
        agile_md::import::import(board, path, true)?
    } else {
        with_auto_commit(board, "import", "tickets", &detail, || {
            agile_md::import::import(board, path, false)
        })?
    };
    let left_out: Vec<String> = summary
        .skipped
        .iter()
        .map(|(file, reason)| format!("{}: {reason}", file.display()))
        .collect();
    for line in &left_out {
        eprintln!("skipped {line}");
    }
    // What the source was missing or had out of place (ticket 251).
    if !summary.missing_columns.is_empty() {
        eprintln!(
            "note: {} has no {} folder, taken as empty",
            path.display(),
            summary
                .missing_columns
                .iter()
                .map(|c| format!("{c}/"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if summary.loose > 0 {
        eprintln!(
            "note: {} ticket{} loose in {}, outside any column: {} into the backlog",
            summary.loose,
            if summary.loose == 1 { " is" } else { "s are" },
            path.display(),
            if dry_run { "would go" } else { "imported" }
        );
    }
    if !dry_run
        && let Some(log) = agile_md::import_log::append(
            &here,
            &format!("rite import tickets {}", path.display()),
            &left_out,
        )?
    {
        eprintln!("(also in {})", log.display());
    }
    if summary.imported.is_empty() {
        println!("nothing to import from {}", path.display());
        return Ok(());
    }
    for ticket in &summary.imported {
        let flag = if ticket.duplicate {
            "  [DUPLICATE]"
        } else {
            ""
        };
        println!(
            "{} -> {} \"{}\" ({}){flag}",
            ticket.old_id, ticket.new_id, ticket.title, ticket.column
        );
    }
    let dup = summary.duplicate_count();
    let mut line = format!(
        "{} {} ticket{}",
        if dry_run { "would import" } else { "imported" },
        summary.imported.len(),
        if summary.imported.len() == 1 { "" } else { "s" }
    );
    if dup > 0 {
        line.push_str(&format!(", {dup} flagged DUPLICATE"));
    }
    if !summary.skipped.is_empty() {
        line.push_str(&format!(", {} skipped", summary.skipped.len()));
    }
    if dry_run {
        line.push_str(" — nothing written");
    }
    println!("{line}");
    // Ticket 066: a repository without a branch type, or the reverse.
    let mismatched = summary.mismatched_count();
    if mismatched > 0 {
        println!(
            "note: {mismatched} {} a repository and a branch type that don't go together: \
             drafts in the backlog, the rest left as they are; `rite check` lists them",
            if mismatched == 1 { "has" } else { "have" }
        );
    }
    Ok(())
}

/// `rite issues [on|off]` (tickets 158, 173): with no action, open GitHub
/// issues into the loose backlog now, tagged TRIAGE, in one auto-commit (an
/// issue imported before, even one whose ticket has since gone, is
/// skipped); `on`/`off` switches whether riteui's landing page does it by
/// itself.
fn cmd_issues(
    board: &Board,
    action: Option<&str>,
    repo: Option<&str>,
    label: Option<&str>,
    dry_run: bool,
) -> Result<()> {
    use agile_md::import_issues;
    if let Some(action) = action {
        let on = action == "on";
        with_auto_commit(board, "settings", "import-issues", action, || {
            let mut settings = board_settings::load(board);
            settings.set_import_issues(on);
            settings.save(board)
        })?;
        println!(
            "{}",
            if on {
                "riteui's landing page pulls in this project's GitHub issues"
            } else {
                "GitHub issues are only pulled in by `rite issues` now"
            }
        );
        return Ok(());
    }
    let targets = import_issues::targets(board, repo)?;
    let plan = import_issues::plan(board, &targets, label)?;
    for (slug, why) in &plan.skipped {
        eprintln!("rite: couldn't read the issues of {slug}: {why}");
    }
    let done = if dry_run || plan.new.is_empty() {
        plan.new.clone()
    } else {
        // Only the repositories it could read (ticket 180).
        let slugs: Vec<&str> = targets
            .iter()
            .map(|t| t.slug.as_str())
            .filter(|s| !plan.skipped.iter().any(|(k, _)| k == s))
            .collect();
        let detail = format!("{} from {}", plan.new.len(), slugs.join(", "));
        let (done, warning) =
            transition::with_auto_commit(board, "import", "issues", &detail, || {
                import_issues::apply(board, &plan)
            })?;
        if let Some(warning) = warning {
            eprintln!("rite: {warning}");
        }
        done
    };
    for planned in &done {
        println!("{}", planned.line());
    }
    println!("{}", plan.summary(dry_run));
    Ok(())
}

/// `rite import documents <PATH>` (tickets 079, 170, 173): run from the
/// folder of a documents repository the files should go into, naming the
/// tree to import. Never overwrites: what's already there is printed and
/// appended to `import.log` here. Committed on the default branch and
/// pushed (ADR 0007). `--dry-run` only shows the plan.
fn cmd_import_documents(path: Option<&Path>, dry_run: bool) -> Result<()> {
    let Some(path) = path else {
        bail!(
            "usage: rite import documents <PATH>, from the folder of the documents repository it should go into"
        );
    };
    let here = env::current_dir().context("reading the current directory")?;
    let repo = agile_md::git::checked(&here, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .ok();
    let name = |root: &Path| {
        root.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    // A documents repository: `*-documents`, or the one the project shares.
    let shared = Board::discover()
        .ok()
        .and_then(|board| board_settings::load(&board).shared_documents());
    let repo = repo.filter(|root| {
        let n = name(root);
        n.ends_with("-documents") || shared.as_deref() == Some(n.as_str())
    });
    let Some(repo) = repo else {
        bail!(
            "run rite import documents from inside a documents repository, in the folder the files should go into (e.g. cd acme-documents/docs)"
        );
    };
    // Not into an archived project's documents (ticket 157).
    if !dry_run
        && let Ok(board) = Board::discover()
        && name(&repo)
            == format!(
                "{}-documents",
                agile_md::current_project::project_name(board.repo_root())
            )
    {
        agile_md::archive::ensure_writable(&board)?;
    }
    let here = dunce::canonicalize(&here).unwrap_or(here);
    let root = dunce::canonicalize(&repo).unwrap_or(repo);
    let into = here
        .strip_prefix(&root)
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    let plan = agile_md::import_documents::plan(path, &root, Some(&into))?;
    let repo_name = name(&root);
    let left_out: Vec<String> = plan
        .clashes
        .iter()
        .map(|clash| format!("{clash}: already in {repo_name}, not overwritten"))
        .collect();
    for line in &left_out {
        eprintln!("skipped {line}");
    }
    if plan.copies.is_empty() {
        println!("nothing to import from {}", plan.source.display());
        if !dry_run {
            log_left_out(&here, path, &left_out)?;
        }
        return Ok(());
    }
    for (_, to) in &plan.copies {
        println!("{} {to}", if dry_run { "would add" } else { "added" });
    }
    let count = plan.copies.len();
    let files = if count == 1 { "file" } else { "files" };
    if dry_run {
        println!("would import {count} {files} into {repo_name} — nothing written");
        return Ok(());
    }
    // Straight to main, pushed (ADR 0007, ticket 170).
    let outcome = agile_md::import_documents::apply(&plan)?;
    log_left_out(&here, path, &left_out)?;
    println!(
        "imported {count} {files} into {repo_name} on {}{}",
        outcome.branch,
        if outcome.pushed { ", pushed" } else { "" }
    );
    if let Some(warning) = &outcome.warning {
        eprintln!("rite: {warning}");
    }
    Ok(())
}

/// What a documents import left out, into `import.log` where it ran.
fn log_left_out(here: &Path, path: &Path, left_out: &[String]) -> Result<()> {
    if let Some(log) = agile_md::import_log::append(
        here,
        &format!("rite import documents {}", path.display()),
        left_out,
    )? {
        eprintln!("(also in {})", log.display());
    }
    Ok(())
}

/// The board transition log: `date time action id title sha`, one line per
/// file change under the board, oldest first. Reads paths and rename
/// status from `git log --name-status -M`, never the commit message
/// (`%s`), so it's indifferent to whatever wrote the commit — including
/// history from before auto-commit existed, or a hand-written message.
/// `rite stats` (ticket 233): counts over a period and each running
/// sprint's burn, for every board of the project.
fn cmd_stats(days: Option<u32>) -> Result<()> {
    use agile_md::stats;
    let days = days.unwrap_or_else(stats::period);
    let found = agile_md::model::find_repos()?;
    let boards: Vec<&Board> = found.repos.iter().map(|(_, _, b)| b).collect();
    let now = stats::now();
    let c = stats::counts(&boards, days, now)?;
    println!(
        "last {days} days: {} raised · {} done · {} closed",
        c.raised, c.done, c.closed
    );
    let row = |pick: fn(&stats::Week) -> usize| {
        c.weeks
            .iter()
            .map(|w| pick(w).to_string())
            .collect::<Vec<_>>()
            .join(" ")
    };
    println!("  by week, oldest first:");
    println!("    raised {}", row(|w| w.raised));
    println!("    done   {}", row(|w| w.done));
    println!("    closed {}", row(|w| w.closed));
    for (name, _, board) in &found.repos {
        for burn in stats::burns(board, name, now)? {
            let done = burn.done_by_day.last().copied().unwrap_or(0);
            println!(
                "sprint {} ({name}): started {}, day {} of {}, {done} of {} points done",
                burn.sprint,
                burn.start,
                burn.done_by_day.len(),
                burn.days,
                burn.total
            );
            println!("  burn-up   {}", spark(&burn.done_by_day, burn.total));
            let left: Vec<u32> = burn.done_by_day.iter().map(|d| burn.total - d).collect();
            println!("  burn-down {}", spark(&left, burn.total));
        }
    }
    Ok(())
}

/// One block a day, its height the share of `top`.
fn spark(values: &[u32], top: u32) -> String {
    const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    values
        .iter()
        .map(|&v| {
            let step = (v.min(top) * 7 + top / 2).checked_div(top).unwrap_or(0);
            BLOCKS[step as usize]
        })
        .collect()
}

fn cmd_log(board: &Board) -> Result<()> {
    let repo = board.repo_root();
    let board_rel = board
        .root
        .strip_prefix(repo)
        .unwrap_or(Path::new("tasks"))
        .to_path_buf();
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "log",
            "--name-status",
            "-M",
            "--date=format:%Y-%m-%d %H:%M",
            "--pretty=format:>>>%h|%ad",
            "--reverse",
        ])
        .arg("--")
        .arg(&board.root)
        .output()
        .context("running git log")?;
    if !out.status.success() {
        bail!("git log failed");
    }
    let text = String::from_utf8(out.stdout).context("git log produced non-utf8 output")?;

    let mut sha = String::new();
    let mut when = String::new();
    let mut any = false;
    for line in text.lines() {
        if let Some(header) = line.strip_prefix(">>>") {
            let (h, w) = header.split_once('|').unwrap_or((header, ""));
            sha = h.to_string();
            when = w.to_string();
            continue;
        }
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split('\t');
        let Some(status) = fields.next() else {
            continue;
        };
        let (action, path) = match status.as_bytes().first() {
            Some(b'A') => ("add".to_string(), fields.next()),
            Some(b'M') => ("edit".to_string(), fields.next()),
            Some(b'D') => ("remove".to_string(), fields.next()),
            Some(b'R') => {
                let (_old, new) = (fields.next(), fields.next());
                let action = new
                    .map(Path::new)
                    .and_then(|p| p.strip_prefix(&board_rel).ok())
                    .and_then(|p| p.parent())
                    .and_then(|p| p.file_name())
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "move".to_string());
                (action, new)
            }
            _ => continue,
        };
        let Some(path) = path else { continue };
        // A column's placeholder (`.gitkeep`) or the archive's `.gitignore`
        // isn't a ticket: adding a column shouldn't read as a new ticket.
        if Path::new(path)
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        {
            continue;
        }
        let title = Path::new(path)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let title = match title.split_once('-') {
            Some((_, rest)) => rest.replace('-', " "),
            None => title.replace('-', " "),
        };
        let id = Path::new(path)
            .file_stem()
            .and_then(|stem| stem.to_string_lossy().split('-').next().map(str::to_string))
            .unwrap_or_default();
        println!("{when}  {action:<8} {id:>4}  {title}  {sha}");
        any = true;
    }
    if !any {
        println!("(no board history yet)");
    }
    Ok(())
}

fn cmd_templates(board: &Board, command: TemplateCmd) -> Result<()> {
    let templates = Templates::load(board)?;
    match command {
        TemplateCmd::List => {
            let width = templates
                .list()
                .map(|(name, _)| name.len())
                .max()
                .unwrap_or(0);
            for (name, source) in templates.list() {
                println!("{name:width$}  {}", source.describe());
            }
            Ok(())
        }
        TemplateCmd::Show { name } => {
            print!("{}", templates.source_text(&name)?);
            Ok(())
        }
        TemplateCmd::Eject { name, force } => {
            let source = templates::builtin_source(&name)
                .with_context(|| format!("'{name}' is not a built-in template"))?;
            let dir = board.templates_dir();
            fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
            let path = dir.join(format!("{name}{TEMPLATE_SUFFIX}"));
            if path.exists() && !force {
                bail!(
                    "{} already exists (use --force to overwrite)",
                    path.display()
                );
            }
            fs::write(&path, source.as_bytes())
                .with_context(|| format!("writing {}", path.display()))?;
            println!("wrote {}", path.display());
            Ok(())
        }
    }
}

/// `$EDITOR`, else the settings file's `editor` key, else `vi` — used by
/// both `rite edit` and the form's body step. GUI-only, so this lives in the
/// CLI rather than rite-lib: the GUI never spawns an editor.
fn editor_command() -> String {
    let config = agile_md::settings::Config::load();
    agile_md::settings::resolved("EDITOR", &config, "editor")
        .filter(|editor| !editor.is_empty())
        .unwrap_or_else(|| "vi".to_string())
}

fn open_editor(path: &Path) -> Result<()> {
    let editor = editor_command();
    let status = Command::new(&editor)
        .arg(path)
        .status()
        .with_context(|| format!("running {editor}"))?;
    if !status.success() {
        bail!("{editor} exited with {status}");
    }
    Ok(())
}

fn parse_key_value(raw: &str) -> Result<(String, String), String> {
    match raw.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.to_string(), value.to_string())),
        _ => Err(format!("expected KEY=VALUE, got '{raw}'")),
    }
}
