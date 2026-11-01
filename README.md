<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="pages/logo/kanrite-dark.svg">
    <img src="pages/logo/kanrite-light.svg" alt="KanRite: ideate, plan, write, deliver" width="560">
  </picture>
</p>

# KanRite

**An open source, fully featured, multi-project agile management system for
individuals and teams.**

**[Website](https://kanrite.github.io/rite/)** ·
[Install](#install) ·
[User guide](docs/user-guide.md) ·
[Releases](https://github.com/kanrite/rite/releases) ·
[Contributing](CONTRIBUTING.md)

Your tickets, documents and ideas live in your own git repositories, as
markdown, right beside your work: not in someone else's cloud. KanRite works
well with AI, but AI is never a requirement; it's just as useful to a team or
a solo developer without it.

![The board in the default white and sky blue: a running sprint and two epics beside the backlog, todo, doing and done](pages/screenshots/board.png)

## Main features

- **Kanban boards** across several projects and several boards, with
  optional sprints and automatic ticket prioritisation to help manage your
  workflow.
- **Customisable tickets** with an optional draft status, parent and child
  tickets nested to any depth, relationships, priorities, due dates,
  recurring tickets and tags for filtering and sorting.
- **Your own board**: add, remove and rename columns, and set the workflow
  that suits your team.
- **Roadmaps and milestones** that set up their epics or sprints for you, to
  help manage larger projects and delivery dates.
- **Ideas**, to incubate new ideas and solve problems together, and turn them
  into backlog tickets, epics or sprints.
- **Documents** your team reads and writes, and can pin when they're reading
  or working from them: everything you need to be productive and informed.
- **Everything is backed by git**, giving you a full, intelligible audit
  trail and recovery when required.
- **Markdown in your repositories**, so your tickets, documents and ideas are
  accessible to your team, and to your AI.
- **A landing page** showing your priorities across all your projects, with
  statistics and navigation.
- **A desktop app** with rich text editing, and a **command line** for those
  who prefer it.
- **A built-in terminal**, and optional AI integration.
- **Optional speech to text and read aloud**, run entirely on your machine.
- **Free to use:** no servers, no hidden costs. Your data is where you need
  it, and in your control.

## In more detail

### Tickets

- **One flexible ticket**, rendered from templates each project can change.
- **Fields:** priority, story points (1, 2, 3, 5, 8, 13), one or more
  assignees from the project's users, tags, the repository it's about, and a due date.
- **Recurring tickets:** a monthly report or a yearly renewal comes round
  again: done makes the next one, due one interval on.
- **Typed links:** requires, related, parent and child, clone, supersedes,
  each written on both tickets and changeable later. A tree draws what each
  ticket requires and relates to, and cards show how many links they have.
- **Save and New:** found a ticket depends on one that doesn't exist yet?
  Save it and make the missing one, already linked. **Add child** breaks a
  big ticket into smaller ones the same way.
- **Who created it** is recorded; **who last edited it** comes from git.
- **Tags** are uppercase and can be limited to the project's own list; DRAFT
  and TRIAGE are built in.
- **Drafts** can't leave the backlog until they're marked ready.
- **Branches:** a ticket with a branch type gets its own git branch when it
  moves to todo.
- **Search and filters** by tag, assignee, repository and text.

### Workflow rules

- **One column at a time:** backlog → todo → doing → done, unless the team
  allows other moves.
- **Your own columns:** add Triage, Review, Released and the like, rename,
  reorder and remove them; each has a kind that decides what it does, and
  reset puts the standard four back.
- **Done waits** for the tickets a ticket requires and for its children.
- **A WIP limit on doing** (2 unless changed), with a deliberate override.
- **Locks:** a ticket is yours while it's in doing and is released when it
  leaves; someone else's lock is only broken on purpose.
- **Every change is committed and pushed**, and boards stay in step: rite
  catches up before a change, replays it if someone pushed first, and warns
  when many tickets vanish or history is rewritten.

### Planning

- **Epics and sprints**, both optional; a started sprint takes nothing more
  and gives nothing back, and can't start while it needs a ticket outside it.
  Backfill a running sprint from the backlog when it runs short, and close it
  when it ends: a report with its burn-up goes into your documents.
- **Auto priority:** a ticket is at least as urgent as what requires it,
  rises as its due date nears, and the **Automatic** sort shows what's
  needed first.
- **Roadmaps:** one per board, made of milestones that create its epics or
  sprints, with working days, time left and whether each is on track. A
  milestone's start and due dates can be changed later.
- **My work:** your todo and doing tickets across every project on your
  machine, and work to pick up when you have none.

### Documents and ideas

- **A documents repository per project** for ADRs, RFCs and docs, plus an
  optional one every project shares. Changes go straight to main, with
  history and restore.
- **Write in riteui:** rich text or markdown, folders, moves that keep links
  working, archiving, a pinned start page and search, and your own pinned
  documents, first in your list on every machine.
- **Ideas** before they're tickets: everyone votes up to ±3 and adds notes,
  and an idea becomes a backlog ticket in one click, or (for the project's
  admins) an epic or a sprint with the idea's write-up as its description
  (`rite idea ticket <idea> --as epic|sprint`).

### Teams and projects

- **`rite init`** creates a project's repositories on GitHub; **`rite join`**
  gives a teammate the project and adds them as a user.
- **Several repositories per project**, and several projects side by side;
  a new repository can be made from the ticket that needs it.
- **Admin** settings are committed for the team; **Preferences** are your
  own, on your machine.
- **Archive a project** that's no longer maintained: read-only, archived
  on GitHub and gone from rite, until `rite init` brings it back.

### Bringing work in, and getting it back

- **GitHub issues** arrive as TRIAGE tickets, each one only once.
- **Import** tickets from another board and documents from a folder.
- **`rite recover`** brings back deleted or abandoned tickets; **`rite
  renumber`** fixes duplicate numbers.

### The desktop board

| | |
|---|---|
| ![A ticket open in the editor](pages/screenshots/ticket.png) | ![A roadmap and its milestones](pages/screenshots/roadmap.png) |
| ![Documents, with an ADR open](pages/screenshots/documents.png) | ![Ideas, best voted first](pages/screenshots/ideas.png) |
| ![The landing page: My work and the projects](pages/screenshots/home.png) | ![The terminal in the bottom panel](pages/screenshots/panel.png) |


- **Drag and drop**, with the rules shown while you drag.
- **A landing page** with My work and every project, each with its tickets
  raised, done and closed over 7, 30 or 90 days and its running sprint's
  burn-up and burn-down (`rite stats` on the command line).
- **Live:** changes made anywhere (the command line, an editor, a pull) show
  within a second.
- **A bottom panel** with a terminal and an AI tab running your own AI tool.
- **Speech to text and read aloud**, optional and entirely on your machine.
- **An Editor button**, themes, accent colours and zoom.
- **Updates:** an orange bar in riteui tells you when a newer release is
  out; Update installs it and restarts (`rite update` does the same), never
  touching your projects. Releases stay compatible with each other, and a
  team can require a minimum version (`rite require-version`).

### AI, if you want it

- **`rite mcp`** lets any MCP-capable AI tool (Claude Code, Copilot, Cursor,
  Codex, Gemini CLI) read the board, documents, ideas and what's on screen,
  and change tickets under the same rules, each change committed. It never
  abandons tickets, breaks locks or changes settings.
- **Nothing built in:** rite holds no API keys, and nothing runs unless your
  AI tool starts it.

### Command line

- **Everything the board does is a command**, safe in scripts and CI: it
  never prompts without a terminal.
- **Shell completion** for bash, zsh and fish, epic names included, and
  `rite log` for a board's history.

Everything is explained in the [user guide](docs/user-guide.md).

## Install

Runs on **Linux, macOS and Windows** (x86_64 and arm64), and needs **git
2.29 or later**. On Linux or macOS, in a terminal:

```bash
curl -fsSL https://raw.githubusercontent.com/kanrite/rite/v5/install.sh | bash
```

That installs two commands, **`rite`** (the command line) and **`riteui`**
(the desktop board), into `/usr/local/bin` when it can, otherwise `~/bin`,
and on Linux adds the board to your application menu. To choose the folder
or pin a version, pass them after `bash -s --`:

```bash
curl -fsSL https://raw.githubusercontent.com/kanrite/rite/v5/install.sh | bash -s -- --dir ~/.local/bin --version v5.0.0
```

If your system isn't one KanRite runs on, the installer says why and
changes nothing. On Linux the desktop board needs WebKitGTK 4.1; if it's
missing, the installer names the package to add, and `rite` works without it.

On **Windows 10 or 11**, in PowerShell (no administrator needed):

```powershell
irm https://raw.githubusercontent.com/kanrite/rite/v5/install.ps1 | iex
```

That installs `rite.exe` and `riteui.exe` (and `amd.exe`, the old name) for you alone, into
`%LOCALAPPDATA%\Programs\KanRite\bin`, adds that folder to your PATH (open a
new terminal to use it), puts **KanRite** in the Start menu and lists it in
**Settings > Apps**. Git for Windows provides the git it needs. To pin a
version:

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/kanrite/rite/v5/install.ps1))) -Version v5.0.0
```

The first start of a download may bring up Windows' "Windows protected your
PC" warning, because the programs aren't signed yet: **More info**, then
**Run anyway**. You can still use KanRite inside
[WSL](https://learn.microsoft.com/windows/wsl/install) with the Linux line
instead.

### Updates

The desktop board looks for a newer release of KanRite on GitHub when it
starts and every few hours while it's open, asking GitHub at most once a
day (drafts and pre-releases are ignored). When there is one, an orange bar
across the top says so, with **What's new**, **Update** and **Later**:

- **What's new** shows the release notes of every release newer than
  yours, in the board itself; **Open on GitHub** shows them in your browser.
- **Update** installs it and restarts the board. Each release's download
  is checked against the checksum GitHub publishes, and only the two
  programs are replaced, never your projects or settings. On the command
  line, `rite update` does the same (`rite update --check` only looks, and
  `rite update --rollback` puts the previous version back).
- **Later** hides that version for a day, even if you restart; a newer
  release still shows straight away.
- If the check fails (offline, say), nothing is shown; it tries again an
  hour later.

To turn the check off, add `update-check = off` to
`~/.config/kanrite/config`, or set `RITE_UPDATE_CHECK=0` (which wins over
the setting). What it remembers, the releases it last saw and a Later, is
kept in `~/.local/state/kanrite/update.json` (or under `$XDG_STATE_HOME`),
on your machine only, never in a project's repositories.

Only an install made by the installer updates itself. Running the installer
again also updates, and is how an install from before 4.6 starts updating
itself. More in the [user guide](docs/user-guide.md#versions-and-your-team).

### Coming from agile-md

Before 5.0 the commands were `amd` and `amdui`. Updating a 4.x install
(its update bar, or `amd update`) brings it to 5.x and renames them; `amd`
and `amdui` keep working through 5.x and say once a day that they're now
`rite` and `riteui`. Your projects don't change. More in the
[user guide](docs/user-guide.md#coming-from-agile-md).

To uninstall, run `rite-uninstall` (the installer puts it beside `rite`; on
Windows, uninstall **KanRite** in Settings > Apps, or run
`rite-uninstall.ps1`). It
removes the programs and the menu entry, then asks whether to remove your
KanRite settings too; `--all` removes them without asking. Your projects,
in your git repositories, are never touched. Installed before it existed?
The installer does the same:

```bash
curl -fsSL https://raw.githubusercontent.com/kanrite/rite/v5/install.sh | bash -s -- --uninstall
```

Building from source is in [CONTRIBUTING.md](CONTRIBUTING.md).

## Quick start

Start a project, or join one, from the folder you keep projects in:

```bash
rite init acme              # make acme-tickets and acme-documents on GitHub and clone them here
rite join acme              # …or join a project someone else made
```

Then, from any of its repositories (or the folder holding them):

```bash
rite new "Publish to Marketplace"
rite todo 1                 # backlog -> todo
rite doing 1                # todo -> doing: it's yours (locked) while you work on it
rite done 1                 # doing -> done
rite board                  # every column
riteui                      # the desktop board
```

The [user guide](docs/user-guide.md) covers tickets, links and
dependencies, your own columns, epics and sprints, the desktop board,
documents, ideas, roadmaps, statistics, several repositories, teams and AI
tools.

## Why a directory instead of one big TODO.md?

A single file gets unwieldy once tasks have real content, and every edit is a
merge-conflict magnet. Separate files keep tasks self-describing, diffable and
independently movable, while the folders give an unambiguous board.

## Development

How to build KanRite, run its checks and work on the desktop board is in
[CONTRIBUTING.md](CONTRIBUTING.md); [CLAUDE.md](CLAUDE.md) describes the
architecture.

## Contributing

Bugs and ideas go in [issues](https://github.com/kanrite/rite/issues/new/choose);
they're pulled into the maintainers' board automatically and triaged there.
Pull requests are welcome: [CONTRIBUTING.md](CONTRIBUTING.md) has the build,
the checks CI runs, the branch and commit names, and how to become a
maintainer. Everyone taking part follows the
[code of conduct](CODE_OF_CONDUCT.md). Report a security problem privately,
through the repository's Security tab, not in an issue: see
[SECURITY.md](SECURITY.md).

## Licence

MIT: see [LICENSE](LICENSE). Contributions are accepted under the same
licence.
