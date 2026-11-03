# Contributing to KanRite

Thanks for taking an interest. KanRite is MIT licensed; anything you
contribute is under the same licence.

## Reporting a bug or asking for something

Open an [issue](https://github.com/kanrite/rite/issues/new/choose). There's
a form for bugs and one for ideas.

The project's own board is KanRite itself: open issues are pulled into the
maintainers' backlog automatically, tagged `TRIAGE`, and looked at from
there. Once one becomes work, the issue is where you'll hear about it.

For a security problem, don't open a public issue; see "Security" below.

## Changing the code

1. Fork the repository and branch from `main`. Branch names start with a
   type: `feature/`, `bugfix/`, `hotfix/`, `release/` or `chore/`
   (for example `bugfix/drag-onto-a-full-column`). The type also decides
   what CI runs: `feature/`, `bugfix/` and `chore/` pull requests are
   checked on Linux x86_64, and `release/` and `hotfix/` ones on every
   platform (Linux x86_64 and Arm, Windows and macOS). To try any branch on
   every platform, run CI by hand: `gh workflow run ci.yml --ref <branch>`.
2. Make the change. "Building from source" below has the build; CI runs
   the same checks, so run them first:

   ```bash
   cargo xtask check
   ```

   That builds the board's page, then runs `cargo fmt --all --check`,
   clippy (also for the page as WebAssembly), `cargo test --workspace
   --all-features` and `bash tests/test.sh`, stopping at the first failure.
   It prints each command as it runs it, so a failing one can be run on
   its own. On Windows the CLI tests (a bash script) are skipped for now.

3. Commit with a conventional type: `feat`, `fix`, `docs`, `style`,
   `refactor`, `perf`, `test`, `build`, `ci`, `chore` or `revert`
   (`fix: a drag onto a full column asks first`).
4. Open a pull request against `main` and say which issue it's for. A
   maintainer reviews it; `main` needs one approval and green checks.

Some things worth knowing first:

- **`tests/test.sh` is the specification.** A change the command line can
  see gets an assertion there, not only a Rust unit test, and the
  command's `--help` text should still be true.
- **Ticket logic lives in `rite-lib`.** The command line and the desktop
  board both call it; neither has rules of its own.
- **The ticket file format is a public contract.** Templates and the
  `TaskContext` variables are used by other people's boards, so adding is
  fine and renaming breaks them.
- [CLAUDE.md](CLAUDE.md) describes the architecture in detail.

## Building from source

People installing KanRite use the release (the curl line in the README),
which needs none of this. To build it yourself you need
[Rust](https://rustup.rs) and git 2.29 or later. The desktop board's page is
Rust compiled to WebAssembly by [Trunk](https://trunkrs.dev) and built into
`riteui` at compile time, so it also needs Trunk and the wasm target, and on
Linux WebKitGTK and ALSA's headers (ALSA is for speech to text's
microphone):

```bash
cargo install --locked trunk
rustup target add wasm32-unknown-unknown
sudo apt install libwebkit2gtk-4.1-dev libasound2-dev   # Debian/Ubuntu
```

Then, in a clone, on Linux, macOS or Windows:

```bash
cargo xtask build --release   # the board's page, then target/release/{rite,riteui}
cargo xtask install           # builds and installs both
```

The tasks live in `crates/xtask`, written in Rust so the same commands work
everywhere with nothing to install beyond Rust (`cargo xtask` lists them).
`cargo xtask build rite` builds the command line on its own, with no window
or web stack and none of the board's tools; without Trunk, a plain
`cargo xtask build` does the same and says what's missing.

`cargo xtask install` runs `./install.sh --from-source` on Linux and macOS,
which also installs riteui's launcher and `rite-uninstall` (`--dir DIR` picks
the folder). On Windows it copies `rite.exe` and `riteui.exe` into Cargo's
bin folder (`%USERPROFILE%\.cargo\bin`, already on the PATH where rustup
is installed), or `--dir DIR`; there's no uninstaller or Start menu entry
there yet.

The workspace is one library and two front ends: `crates/rite-lib` (all the
board logic), `crates/rite-cli` (`rite`), and the desktop board in
`crates/rite-ui` (Tauri), `crates/rite-ui-web` (the Leptos page) and
`crates/rite-ui-model` (types both share).

To work on the board with live reload, run
`cargo tauri dev -- --no-default-features` in `crates/rite-ui`: it runs
`trunk serve` for you, and `--no-default-features` serves the page live
instead of the copy built into the binary.

For screenshots, or to try a change on a full board,
`scripts/demo-board.sh [--open] [DIR]` builds a demo project: a fictional
shop with every column, epics, a running sprint, assignees, priorities,
points, tags, a draft and locks, all made-up names at example.com. It
keeps its own config in `DIR/config`, so your own boards and settings are
untouched. `DIR` defaults to `~/kanrite-demo` and must be outside any git
repository. It prints the command to open it, and `--open` opens it
straight away. The riteui it opens is the one beside the `rite` that
built it, so to look at your branch:

```bash
cargo xtask build && AMD=target/debug/rite scripts/demo-board.sh --open
```

## Maintainers

The maintainers' tickets (`kanrite-tickets`) and design documents
(`kanrite-documents`) are private repositories in the
[KanRite](https://github.com/kanrite) organisation. To work from them,
become a maintainer: you're added to the organisation, and then
`rite join kanrite` clones both and adds you to the project's users.
Everyone else works from issues and pull requests here.

## Security

Please report a vulnerability privately rather than in an issue: use
[Report a vulnerability](https://github.com/kanrite/rite/security/advisories/new)
on the repository's Security tab. [SECURITY.md](SECURITY.md) says what's
covered and what happens next.

## Conduct

Everyone taking part is expected to follow the
[code of conduct](CODE_OF_CONDUCT.md).
