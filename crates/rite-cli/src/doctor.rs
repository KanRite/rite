//! `rite doctor [--fix] [--json]` (ticket 351): rite-lib's checks of this
//! machine (`rite_lib::doctor::check_all`, the list riteui's Preferences
//! shows too, ticket 385), printed as text or JSON.

use std::path::Path;

use anyhow::{Context, Result};
use rite_lib::doctor::{self, Finding, Machine};

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
    doctor::check_all(machine, home)
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
    fn the_report_says_what_fix_would_do() {
        let home = tempfile::tempdir().unwrap();
        let said = text(
            &rite_lib::doctor::mcp::check(Some(home.path()), None),
            &[],
            false,
        );
        assert!(said.starts_with("ok "), "{said}");
        assert!(said.ends_with("0 problems, 0 warnings\n"), "{said}");
        let said = text(&[], &["took x off".to_string()], true);
        assert!(said.starts_with("Fixed:\n  took x off\n"), "{said}");
    }
}
