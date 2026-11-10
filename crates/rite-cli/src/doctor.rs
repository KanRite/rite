//! `rite doctor [--fix] [--json]` (ticket 351): rite-lib's checks of this
//! machine (`rite_lib::doctor`), plus the MCP one, whose reading of other
//! tools' config lives here beside `rite mcp setup` (ticket 350).

use std::path::Path;

use anyhow::{Context, Result};
use rite_lib::doctor::{self, Finding, Machine, Status};

/// The check id for an old `agile-md` MCP entry.
pub const MCP_OLD_ENTRY: &str = "mcp-old-entry";

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
    let mut all = doctor::check(machine);
    all.extend(mcp(home));
    all
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
