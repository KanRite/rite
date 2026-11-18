//! `rite doctor [--fix] [--json]` (ticket 351): rite-lib's checks of this
//! machine (`rite_lib::doctor`), plus the MCP one, whose reading of other
//! tools' config lives here beside `rite mcp setup` (ticket 350).

use std::path::Path;

use anyhow::{Context, Result};
use rite_lib::doctor::{self, Finding, Machine, Status};

/// The check id for an old `agile-md` MCP entry.
pub const MCP_OLD_ENTRY: &str = "mcp-old-entry";
/// The check ids for the `kanrite` entry: missing, or pointing nowhere.
pub const MCP_NOT_REGISTERED: &str = "mcp-not-registered";
pub const MCP_BROKEN_ENTRY: &str = "mcp-broken-entry";

pub fn run(fix: bool, json: bool) -> Result<()> {
    let machine = Machine::here().context("no config folder to look in")?;
    let home = rite_lib::home::dir();
    let fixed = if fix {
        doctor::fix(&machine)?
    } else {
        Vec::new()
    };
    let findings = findings(&machine, home.as_deref());
    let (problems, warnings) = doctor::counts(&findings);
    if json {
        let out = serde_json::json!({
            "checks": findings,
            "fixed": fixed,
            "problems": problems,
            "warnings": warnings,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        print!("{}", text(&findings, &fixed, fix));
    }
    if problems > 0 {
        // A problem is the answer, not an error: say nothing more, exit 1.
        use std::io::Write;
        std::io::stdout().flush().ok();
        std::process::exit(1);
    }
    Ok(())
}

fn findings(machine: &Machine, home: Option<&Path>) -> Vec<Finding> {
    let path = std::env::var_os("PATH");
    let mut all = doctor::check(machine);
    all.extend(doctor::git_setup(&git_answer));
    all.extend(doctor::install(path.as_deref()));
    all.extend(doctor::folders());
    all.extend(mcp(home));
    all.extend(mcp_registered(home, path.as_deref()));
    all
}

/// What `git` prints for `args`, trimmed, when it runs and succeeds.
fn git_answer(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The AI tools rite knows how to register with: the name `rite mcp setup`
/// takes, what to call it, and how to tell it's installed (its config
/// file or folder under the home folder).
const TOOLS: [(&str, &str, &str); 4] = [
    ("claude", "Claude Code", ".claude.json"),
    ("cursor", "Cursor", ".cursor"),
    ("codex", "Codex", ".codex"),
    ("gemini", "Gemini CLI", ".gemini"),
];

/// What a tool's config says its `kanrite` MCP server runs: `None` when
/// there is no entry, `Some(None)` for an entry with no `command`.
fn entry_command(home: &Path, client: &str) -> Option<Option<String>> {
    let server = crate::mcp::SERVER;
    let json = |path: &str| -> Option<Option<String>> {
        let text = std::fs::read_to_string(home.join(path)).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        let entry = value["mcpServers"].get(server)?;
        Some(entry["command"].as_str().map(str::to_string))
    };
    match client {
        "claude" => json(".claude.json"),
        "cursor" => json(".cursor/mcp.json"),
        "gemini" => json(".gemini/settings.json"),
        "codex" => {
            let text = std::fs::read_to_string(home.join(".codex/config.toml")).ok()?;
            let mut in_entry = false;
            let mut command = None;
            let mut found = false;
            for line in text.lines().map(str::trim) {
                if line.starts_with('[') {
                    in_entry = line == format!("[mcp_servers.{server}]")
                        || line == format!("[mcp_servers.\"{server}\"]");
                    found |= in_entry;
                } else if in_entry
                    && let Some(rest) = line.strip_prefix("command")
                    && let Some(value) = rest.trim_start().strip_prefix('=')
                {
                    command = Some(value.trim().trim_matches('"').to_string());
                }
            }
            found.then_some(command)
        }
        _ => None,
    }
}

/// Whether `rite mcp` can start from `command`: a bare name that's on the
/// PATH, or a path to a file that's there.
fn command_runs(command: &str, rites: &[std::path::PathBuf]) -> bool {
    if command.contains(['/', '\\']) {
        Path::new(command).is_file()
    } else {
        // Only `rite` is looked for: that is what `rite mcp setup` writes.
        command != "rite" || !rites.is_empty()
    }
}

/// `kanrite` registered in each AI tool that's installed, and running a
/// `rite` that's there (ticket 385). A tool with only the old `agile-md`
/// entry is the other check's. rite never writes another tool's config, so
/// the fix is a command to run.
fn mcp_registered(home: Option<&Path>, path: Option<&std::ffi::OsStr>) -> Vec<Finding> {
    let Some(home) = home else {
        return vec![ok_mcp("no home folder to look for AI tools in")];
    };
    let rites = doctor::rites_on_path(path);
    let old = crate::mcp::old_entries(home);
    let mut out = Vec::new();
    let mut tools = Vec::new();
    let mut registered = Vec::new();
    for (client, name, marker) in TOOLS {
        if !home.join(marker).exists() {
            continue;
        }
        tools.push(name);
        match entry_command(home, client) {
            None if old.contains(&client) => {}
            None => out.push(
                Finding {
                    check: MCP_NOT_REGISTERED,
                    status: Status::Warning,
                    title: format!("{name} has no kanrite MCP entry"),
                    detail: vec![
                        "the board, tickets and documents tools aren't available in it".to_string(),
                    ],
                    fix: None,
                }
                .by_hand(format!("rite mcp setup {client}")),
            ),
            Some(command) => {
                registered.push(name);
                let command = command.unwrap_or_default();
                if !command_runs(&command, &rites) {
                    out.push(
                        Finding {
                            check: MCP_BROKEN_ENTRY,
                            status: Status::Problem,
                            title: format!(
                                "{name}'s kanrite MCP entry runs `{command}`, which isn't there"
                            ),
                            detail: vec!["the tools fail to start in it".to_string()],
                            fix: None,
                        }
                        .by_hand(format!("rite mcp setup {client}")),
                    );
                }
            }
        }
    }
    if !out.is_empty() {
        return out;
    }
    if tools.is_empty() {
        return vec![ok_mcp("no AI tool found to register kanrite with")];
    }
    if registered.is_empty() {
        return vec![ok_mcp(
            "the AI tools here have only the old entry (see above)",
        )];
    }
    vec![ok_mcp(&format!(
        "kanrite is registered in {}",
        registered.join(", ")
    ))]
}

fn ok_mcp(title: &str) -> Finding {
    Finding {
        check: MCP_NOT_REGISTERED,
        status: Status::Ok,
        title: title.to_string(),
        detail: Vec::new(),
        fix: None,
    }
}

/// The pre-5.0 `agile-md` MCP entry in an AI tool's config, which runs an
/// `amd` that's gone. rite never writes another tool's config, so the fix
/// is a command to run, not `--fix`.
fn mcp(home: Option<&Path>) -> Vec<Finding> {
    let old = home.map(crate::mcp::old_entries).unwrap_or_default();
    if old.is_empty() {
        return vec![Finding {
            check: MCP_OLD_ENTRY,
            status: Status::Ok,
            title: "no AI tool still has the old agile-md MCP entry".to_string(),
            detail: Vec::new(),
            fix: None,
        }];
    }
    old.iter()
        .map(|client| {
            let finding = Finding {
                check: MCP_OLD_ENTRY,
                status: Status::Problem,
                title: format!("{client} still has the old agile-md MCP entry"),
                detail: vec![
                    "it runs `amd mcp`, which 5.0 renamed: the tools fail or show twice"
                        .to_string(),
                ],
                fix: None,
            };
            if *client == "claude" {
                finding.by_hand("rite mcp setup --fix")
            } else {
                finding.by_hand(format!(
                    "rite mcp setup {client} (then change its config by hand)"
                ))
            }
        })
        .collect()
}

/// The plain report: what `--fix` did, a line a finding, and a summary.
fn text(findings: &[Finding], fixed: &[String], fix: bool) -> String {
    let mut out = String::new();
    if fix {
        if fixed.is_empty() {
            out.push_str("Nothing for --fix to change.\n\n");
        } else {
            out.push_str("Fixed:\n");
            for line in fixed {
                out.push_str(&format!("  {line}\n"));
            }
            out.push('\n');
        }
    }
    for finding in findings {
        out.push_str(&format!("{:<8} {}\n", finding.status.word(), finding.title));
        for line in &finding.detail {
            out.push_str(&format!("         {line}\n"));
        }
        if let Some(fix) = &finding.fix {
            out.push_str(&format!("         fix: {}\n", fix.command));
        }
    }
    let (problems, warnings) = doctor::counts(findings);
    let automatic = findings
        .iter()
        .any(|f| f.fix.as_ref().is_some_and(|fix| fix.automatic));
    out.push_str(&format!(
        "\n{problems} {}, {warnings} {}",
        if problems == 1 { "problem" } else { "problems" },
        if warnings == 1 { "warning" } else { "warnings" },
    ));
    if automatic {
        out.push_str(": `rite doctor --fix` puts right what it can, in rite's own files only");
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_old_claude_entry_is_a_problem_naming_mcp_setup_fix() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join(".claude.json"),
            r#"{"mcpServers":{"agile-md":{}}}"#,
        )
        .unwrap();
        let found = mcp(Some(home.path()));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].status, Status::Problem);
        assert_eq!(
            found[0].fix.as_ref().unwrap().command,
            "rite mcp setup --fix"
        );
        assert!(!found[0].fix.as_ref().unwrap().automatic);
    }

    /// A home folder with a tool's config, and a PATH with a `rite` on it.
    fn rite_on_a_path(dir: &Path) -> std::ffi::OsString {
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join(rite_lib::update_apply::file_name("rite")), "x").unwrap();
        std::env::join_paths([bin]).unwrap()
    }

    #[test]
    fn a_tool_without_a_kanrite_entry_is_a_warning_naming_mcp_setup() {
        let home = tempfile::tempdir().unwrap();
        let path = rite_on_a_path(home.path());
        std::fs::write(home.path().join(".claude.json"), r#"{"mcpServers":{}}"#).unwrap();
        let found = mcp_registered(Some(home.path()), Some(path.as_os_str()));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].check, MCP_NOT_REGISTERED);
        assert_eq!(found[0].status, Status::Warning);
        assert_eq!(
            found[0].fix.as_ref().unwrap().command,
            "rite mcp setup claude"
        );
    }

    #[test]
    fn a_kanrite_entry_that_runs_rite_is_ok_in_every_tool_that_has_one() {
        let home = tempfile::tempdir().unwrap();
        let path = rite_on_a_path(home.path());
        std::fs::write(
            home.path().join(".claude.json"),
            r#"{"mcpServers":{"kanrite":{"command":"rite","args":["mcp"]}}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(home.path().join(".codex")).unwrap();
        std::fs::write(
            home.path().join(".codex/config.toml"),
            "[mcp_servers.kanrite]\ncommand = \"rite\"\nargs = [\"mcp\"]\n",
        )
        .unwrap();
        let found = mcp_registered(Some(home.path()), Some(path.as_os_str()));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].status, Status::Ok);
        assert!(
            found[0].title.contains("Claude Code, Codex"),
            "{}",
            found[0].title
        );
    }

    #[test]
    fn an_entry_whose_command_is_not_there_is_a_problem() {
        let home = tempfile::tempdir().unwrap();
        let gone = home.path().join("gone/rite");
        std::fs::write(
            home.path().join(".claude.json"),
            serde_json::json!({"mcpServers": {"kanrite": {"command": gone}}}).to_string(),
        )
        .unwrap();
        let found = mcp_registered(Some(home.path()), None);
        assert_eq!(found[0].status, Status::Problem);
        assert_eq!(found[0].check, MCP_BROKEN_ENTRY);
        // A bare `rite` with none on the PATH is the same.
        std::fs::write(
            home.path().join(".claude.json"),
            r#"{"mcpServers":{"kanrite":{"command":"rite"}}}"#,
        )
        .unwrap();
        assert_eq!(
            mcp_registered(Some(home.path()), None)[0].status,
            Status::Problem
        );
    }

    #[test]
    fn only_the_old_entry_is_left_to_the_old_entry_check() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join(".claude.json"),
            r#"{"mcpServers":{"agile-md":{}}}"#,
        )
        .unwrap();
        let found = mcp_registered(Some(home.path()), None);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].status, Status::Ok);
        assert!(found[0].title.contains("only the old entry"));
    }

    #[test]
    fn no_tool_installed_is_ok() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            mcp_registered(Some(home.path()), None)[0].status,
            Status::Ok
        );
        assert_eq!(mcp_registered(None, None)[0].status, Status::Ok);
    }

    #[test]
    fn no_old_entry_is_ok() {
        let home = tempfile::tempdir().unwrap();
        let found = mcp(Some(home.path()));
        assert_eq!(found[0].status, Status::Ok);
        assert_eq!(mcp(None)[0].status, Status::Ok);
    }

    #[test]
    fn the_report_says_what_fix_would_do() {
        let home = tempfile::tempdir().unwrap();
        let said = text(&mcp(Some(home.path())), &[], false);
        assert!(said.starts_with("ok "), "{said}");
        assert!(said.ends_with("0 problems, 0 warnings\n"), "{said}");
        let said = text(&[], &["took x off".to_string()], true);
        assert!(said.starts_with("Fixed:\n  took x off\n"), "{said}");
    }
}
