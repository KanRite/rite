//! riteui — the KanRite desktop board.
//!
//! The same window `rite gui` opens, with a name of its own so it can be
//! launched the way an app is launched: from a dock, a runner, a shortcut,
//! or just by typing `riteui`. Everything it does goes through the library,
//! so the command line and the board remain one implementation.

use std::process::ExitCode;

const USAGE: &str = "\
riteui — the KanRite desktop board

Usage: riteui [OPTIONS]

Options:
  -h, --help     Print this help
  -V, --version  Print the version

The board resolves the current project from where it's launched (a plain
directory holding a project's sibling repos, or a repo inside one), plus
whatever else that project has registered — `rite register`/`rite deregister`
manage that. Launched from anywhere else, it opens the board it showed last.
`rite` is the command line, `rite gui` opens this same window.";

fn main() -> ExitCode {
    // Deliberately no clap: a window takes no arguments, and the two flags
    // everyone tries anyway are cheaper to answer here than to pull a parser
    // in for.
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Run as `amdui`, the shim of the old name (ticket 309): said once a day.
    rite_lib::renamed::when_run_by_old_name();
    // The machine folders' new name (ticket 310), once.
    rite_lib::places::move_old_folders_and_say();
    rite_ui::adopt_old_speech_setup();
    match args.first().map(String::as_str) {
        None => {}
        Some("-h" | "--help") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("-V" | "--version") => {
            println!("riteui {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("riteui: unexpected argument '{other}'\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    }
    rite_ui::run();
    ExitCode::SUCCESS
}
