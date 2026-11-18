//! The checks of AI tools' MCP config (tickets 351 and 385): the old `agile-md`
//! entry, and whether `kanrite` is registered in each installed tool and
//! runs a `rite` that is there. Read only: rite never writes another tool's
//! config, so every fix is a command to run.

use std::path::Path;

use super::{Finding, Status};

/// The MCP server's name since 5.0, and what it was called before.
pub const SERVER: &str = "kanrite";
pub const OLD_SERVER: &str = "agile-md";

/// The check id for an old `agile-md` MCP entry.
pub const MCP_OLD_ENTRY: &str = "mcp-old-entry";
/// The check ids for the `kanrite` entry: missing, or pointing nowhere.
pub const MCP_NOT_REGISTERED: &str = "mcp-not-registered";
pub const MCP_BROKEN_ENTRY: &str = "mcp-broken-entry";

/// Both checks, for `home` (the home folder, if there is one) and the
/// `PATH` a `rite` is looked for on.
pub fn check(home: Option<&Path>, path: Option<&std::ffi::OsStr>) -> Vec<Finding> {
    let mut out = old_entry(home);
    out.extend(registered(home, path));
    out
}

/// The tools that still hold the pre-5.0 `agile-md` entry, found by reading
/// their config files under `home` (nothing is changed). VS Code keeps its
/// servers per project, so it isn't looked at.
pub fn old_entries(home: &Path) -> Vec<&'static str> {
    let in_json = |path: &str| {
        std::fs::read_to_string(home.join(path))
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .is_some_and(|v| v["mcpServers"].get(OLD_SERVER).is_some())
    };
    let in_codex = std::fs::read_to_string(home.join(".codex/config.toml")).is_ok_and(|text| {
        text.lines().any(|line| {
            let line = line.trim();
            line == format!("[mcp_servers.{OLD_SERVER}]")
                || line == format!("[mcp_servers.\"{OLD_SERVER}\"]")
        })
    });
    [
        ("claude", in_json(".claude.json")),
        ("cursor", in_json(".cursor/mcp.json")),
        ("codex", in_codex),
        ("gemini", in_json(".gemini/settings.json")),
    ]
    .into_iter()
    .filter_map(|(client, found)| found.then_some(client))
    .collect()
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
    let server = SERVER;
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
fn registered(home: Option<&Path>, path: Option<&std::ffi::OsStr>) -> Vec<Finding> {
    let Some(home) = home else {
        return vec![ok_mcp("no home folder to look for AI tools in")];
    };
    let rites = super::rites_on_path(path);
    let old = old_entries(home);
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
fn old_entry(home: Option<&Path>) -> Vec<Finding> {
    let old = home.map(old_entries).unwrap_or_default();
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
        let found = old_entry(Some(home.path()));
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
        std::fs::write(bin.join(crate::update_apply::file_name("rite")), "x").unwrap();
        std::env::join_paths([bin]).unwrap()
    }

    #[test]
    fn a_tool_without_a_kanrite_entry_is_a_warning_naming_mcp_setup() {
        let home = tempfile::tempdir().unwrap();
        let path = rite_on_a_path(home.path());
        std::fs::write(home.path().join(".claude.json"), r#"{"mcpServers":{}}"#).unwrap();
        let found = registered(Some(home.path()), Some(path.as_os_str()));
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
        let found = registered(Some(home.path()), Some(path.as_os_str()));
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
        let found = registered(Some(home.path()), None);
        assert_eq!(found[0].status, Status::Problem);
        assert_eq!(found[0].check, MCP_BROKEN_ENTRY);
        // A bare `rite` with none on the PATH is the same.
        std::fs::write(
            home.path().join(".claude.json"),
            r#"{"mcpServers":{"kanrite":{"command":"rite"}}}"#,
        )
        .unwrap();
        assert_eq!(
            registered(Some(home.path()), None)[0].status,
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
        let found = registered(Some(home.path()), None);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].status, Status::Ok);
        assert!(found[0].title.contains("only the old entry"));
    }

    #[test]
    fn no_tool_installed_is_ok() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(registered(Some(home.path()), None)[0].status, Status::Ok);
        assert_eq!(registered(None, None)[0].status, Status::Ok);
    }

    #[test]
    fn no_old_entry_is_ok() {
        let home = tempfile::tempdir().unwrap();
        let found = old_entry(Some(home.path()));
        assert_eq!(found[0].status, Status::Ok);
        assert_eq!(old_entry(None)[0].status, Status::Ok);
    }
}
