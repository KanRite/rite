//! Shell completion scripts, generated from the clap command itself.
//!
//! `rite completions [SHELL]` prints the script to stdout; with no argument it
//! works the shell out from `$SHELL`. Generating rather than shipping checked-in
//! scripts means completions can never drift from the CLI — a new subcommand or
//! flag is completable the moment it exists.
//!
//! The value-level completions come from the parsers in `value.rs`: `--type`
//! offers the change types this board accepts, `--template` the built-in ticket
//! types, `rite set` its fields.
//!
//! **Epic names and added columns** (ticket 028) are destination verbs too
//! (`rite checkout 7`), but they're the board's, not clap's, so no generated
//! script can know them. For bash, zsh and fish the script gets a hook that
//! runs the hidden `rite __destinations` at TAB time and offers what it
//! prints: an epic made, renamed or deleted (here, by a pull, or in riteui)
//! shows on the next TAB, with no shell reload. Only a new `rite` release's
//! own commands need the script made again.

use std::env;
use std::io::{self, Write};
use std::path::Path;

use anyhow::{Result, bail};
use clap::Command;
use clap_complete::{Shell, generate};
use rite_lib::board::{Board, Column};

/// Print the completion script for `shell`, or for `$SHELL` when it's `None`.
pub fn print(shell: Option<Shell>, command: &mut Command) -> Result<()> {
    let shell = match shell {
        Some(shell) => shell,
        None => from_env()?,
    };
    io::stdout().write_all(script(shell, command).as_bytes())?;
    // The script goes to stdout so it can be redirected; the hint goes to
    // stderr so it survives the redirect without corrupting the script.
    eprintln!("# install it with:\n# {}", install_hint(shell));
    Ok(())
}

/// The generated script, with the destination hook for the shells that take
/// one.
fn script(shell: Shell, command: &mut Command) -> String {
    let name = command.get_name().to_string();
    let mut out = Vec::new();
    generate(shell, command, name, &mut out);
    let script = String::from_utf8_lossy(&out).into_owned();
    with_old_name(shell, with_destinations(shell, script))
}

/// Completes `amd`, the old name kept as a shim through 5.x (ticket 309),
/// the same as `rite`.
fn with_old_name(shell: Shell, script: String) -> String {
    match shell {
        Shell::Bash => script
            .lines()
            .map(|line| {
                if line.trim_start().starts_with("complete -F _rite") && line.ends_with(" rite") {
                    format!("{line} amd\n")
                } else {
                    format!("{line}\n")
                }
            })
            .collect(),
        Shell::Zsh => script.replacen("#compdef rite\n", "#compdef rite amd\n", 1),
        Shell::Fish => format!("{script}complete -c amd -w rite\n"),
        _ => script,
    }
}

/// The argument `rite` answers with its destinations, before clap parses.
pub const DESTINATIONS: &str = "__destinations";

/// What the shell runs at TAB time. `command` skips an alias or function of
/// the same name; errors go nowhere, so a folder with no board offers nothing.
const LIST: &str = "command rite __destinations 2>/dev/null";

/// Adds the destination hook to a generated script. Each shell's change is
/// made against a line clap_complete writes; if a later version stops
/// writing it, the script is left as generated (the unit tests notice).
fn with_destinations(shell: Shell, script: String) -> String {
    match shell {
        Shell::Bash => {
            const REGISTER: &str = "complete -F _rite ";
            if !script.contains(REGISTER) {
                return script;
            }
            let hook = format!(
                "_rite_destinations() {{\n\
                 \x20   _rite \"$@\"\n\
                 \x20   local cur=\"${{COMP_WORDS[COMP_CWORD]}}\"\n\
                 \x20   if [[ ${{COMP_CWORD}} -eq 1 && ${{cur}} != -* ]]; then\n\
                 \x20       COMPREPLY+=( $(compgen -W \"$({LIST})\" -- \"${{cur}}\") )\n\
                 \x20   fi\n\
                 }}\n\n"
            );
            let script = script.replace(REGISTER, "complete -F _rite_destinations ");
            match script.rfind("if [[ \"${BASH_VERSINFO") {
                Some(at) => format!("{}{hook}{}", &script[..at], &script[at..]),
                None => format!("{script}\n{hook}"),
            }
        }
        Shell::Zsh => {
            const DESCRIBE: &str = "    _describe -t commands 'rite commands' commands \"$@\"";
            let add = format!(
                "    local -a destinations; destinations=(${{(f)\"$({LIST})\"}})\n\
                 \x20   commands+=(${{^destinations}}':move a ticket there, or list it')\n"
            );
            script.replacen(DESCRIBE, &format!("{add}{DESCRIBE}"), 1)
        }
        Shell::Fish => format!(
            "{script}complete -c rite -n \"__fish_rite_needs_command\" -f -a \"({LIST})\" \
             -d 'Move a ticket there, or list it'\n"
        ),
        _ => script,
    }
}

/// Prints the board's destination verbs that aren't clap's own: added
/// columns' folders and the backlog's epics and sprints, one a line. A name
/// a command already has is left out (named commands win, ADR-0006), and so
/// is one with a space, which a shell would split. Nothing at all when
/// there's no board: it's run on every TAB, anywhere.
pub fn destinations(command: &Command) {
    let Ok(board) = Board::discover() else {
        return;
    };
    let mut names: Vec<String> = board
        .workflow()
        .columns()
        .filter(|column| !Column::STANDARD.contains(column))
        .map(|column| column.to_string())
        .collect();
    names.extend(board.epics_in(Column::Backlog).unwrap_or_default());
    let mut out = io::stdout().lock();
    for name in names {
        if name.contains(char::is_whitespace) || is_command(command, &name) {
            continue;
        }
        if writeln!(out, "{name}").is_err() {
            return;
        }
    }
}

fn is_command(command: &Command, name: &str) -> bool {
    command
        .get_subcommands()
        .any(|sub| sub.get_name() == name || sub.get_all_aliases().any(|alias| alias == name))
}

/// Work the shell out from `$SHELL` (`/bin/zsh` -> zsh).
fn from_env() -> Result<Shell> {
    let shell = env::var("SHELL").unwrap_or_default();
    let name = Path::new(&shell)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    match name {
        "bash" => Ok(Shell::Bash),
        "zsh" => Ok(Shell::Zsh),
        "fish" => Ok(Shell::Fish),
        "elvish" => Ok(Shell::Elvish),
        "pwsh" | "powershell" => Ok(Shell::PowerShell),
        "" => bail!("$SHELL isn't set — name the shell: rite completions bash|zsh|fish"),
        other => {
            bail!("don't know how to complete for '{other}' — try: rite completions bash|zsh|fish")
        }
    }
}

/// Where the script wants to live, printed as a hint after generating it.
pub fn install_hint(shell: Shell) -> &'static str {
    match shell {
        Shell::Bash => {
            "rite completions bash > /usr/local/etc/bash_completion.d/rite\n\
                        # or: echo 'source <(rite completions bash)' >> ~/.bashrc"
        }
        Shell::Zsh => {
            "rite completions zsh > \"${fpath[1]}/_rite\"\n\
                       # then: rm -f ~/.zcompdump; compinit"
        }
        Shell::Fish => "rite completions fish > ~/.config/fish/completions/rite.fish",
        _ => "redirect the output into your shell's completion directory",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn script(shell: Shell) -> String {
        super::script(shell, &mut crate::Cli::command())
    }

    #[test]
    fn bash_zsh_and_fish_ask_rite_for_the_destinations_at_tab_time() {
        let bash = script(Shell::Bash);
        assert!(bash.contains("_rite_destinations() {"), "{bash}");
        assert!(bash.contains("complete -F _rite_destinations "), "{bash}");
        assert!(!bash.contains("complete -F _rite "), "{bash}");
        assert!(bash.contains(" rite amd\n"), "{bash}");
        assert!(bash.contains(LIST), "{bash}");
        let zsh = script(Shell::Zsh);
        assert!(zsh.contains("commands+=(${^destinations}"), "{zsh}");
        assert!(zsh.contains(LIST), "{zsh}");
        let fish = script(Shell::Fish);
        assert!(fish.contains(&format!("-a \"({LIST})\"")), "{fish}");
        assert!(zsh.starts_with("#compdef rite amd\n"), "{zsh}");
        assert!(fish.contains("complete -c amd -w rite\n"), "{fish}");
    }

    #[test]
    fn the_hidden_command_stays_out_of_the_scripts() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let script = script(shell);
            assert!(
                !script.replace(LIST, "").contains("__destinations"),
                "{shell}"
            );
        }
    }

    #[test]
    fn a_command_name_is_never_offered_as_a_destination() {
        let command = crate::Cli::command();
        assert!(is_command(&command, "draft"));
        assert!(is_command(&command, "related"));
        assert!(!is_command(&command, "checkout"));
    }

    #[test]
    fn every_shell_gets_a_script_that_knows_the_commands() {
        for (shell, marker) in [
            (Shell::Bash, "complete -F"),
            (Shell::Zsh, "#compdef rite"),
            (Shell::Fish, "complete -c rite"),
        ] {
            let script = script(shell);
            assert!(script.contains(marker), "{shell}: missing {marker}");
            // "start" is deliberately not checked here: `rite group start`
            // would make the substring match trivially, masking the fact
            // that `rite start` itself no longer exists as a top-level command.
            for command in [
                "new",
                "todo",
                "doing",
                "backlog",
                "abandon",
                "link",
                "duplicate",
                "templates",
                "completions",
            ] {
                assert!(script.contains(command), "{shell}: missing {command}");
            }
        }
    }

    #[test]
    fn scripts_complete_the_values_too() {
        // The parsers advertise their possible values, so `--branch-type <TAB>`
        // offers branch types rather than falling back to filenames.
        //
        // These used to assert on "feat" and "development" — words that only
        // ever appeared in help text describing ticket types that no longer
        // exist. The assertions passed because the documentation was wrong, so
        // pin them to values a parser actually advertises.
        let fish = script(Shell::Fish);
        assert!(fish.contains("bugfix"), "{fish}");
        assert!(fish.contains("ticket"), "{fish}");
        let bash = script(Shell::Bash);
        assert!(bash.contains("points epic order title"), "{bash}");
    }
}
