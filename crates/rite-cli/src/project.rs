//! `rite project` — the projects in this folder and which one `rite` uses
//! (ticket 093). Runs before any board is opened: it has to work exactly
//! when discovery can't decide on its own.

use std::path::Path;

use anyhow::{Result, bail};

use rite_lib::board::Board;
use rite_lib::current_project::{self, ProjectEntry};
use rite_lib::transition::Event;

use crate::form;

/// `rite project`: list them, marking the current one.
pub fn list() -> Result<()> {
    let dir = current_project::project_dir()?;
    let projects = current_project::projects(&dir);
    if projects.is_empty() {
        println!("no projects in {}", dir.display());
        return Ok(());
    }
    let current = current_project::current(&dir);
    let width = projects.iter().map(|p| p.name.len()).max().unwrap_or(0);
    println!("projects in {}:", dir.display());
    for project in &projects {
        let marker = if current
            .as_deref()
            .is_some_and(|name| rite_lib::names::same(name, &project.name))
        {
            '*'
        } else {
            ' '
        };
        let archived = if rite_lib::archive::is_archived(&Board::in_repo(&project.tickets)) {
            "  (archived)"
        } else {
            ""
        };
        // A display name (ticket 326) beside the name `use` takes.
        let shown = current_project::shown_name(&project.tickets);
        let shown = if shown == project.name {
            String::new()
        } else {
            format!("  ({shown})")
        };
        println!(
            "{marker} {:<width$}  {}{shown}{archived}",
            project.name,
            repositories(&dir, project)
        );
    }
    // Two projects whose names differ only in case (ticket 338).
    for (a, b) in current_project::twins(&dir) {
        eprintln!(
            "warning: {} and {} differ only in case, so a project name can't tell them apart:              keep one",
            a.display(),
            b.display()
        );
    }
    if current.is_none() {
        println!("no current project — choose one with `rite project use <name>`");
    }
    Ok(())
}

/// `rite project use [name]`: make one current; with no name, pick from a
/// list in a terminal.
pub fn use_project(name: Option<String>) -> Result<()> {
    let dir = current_project::project_dir()?;
    let projects = current_project::projects(&dir);
    let names: Vec<String> = projects.iter().map(|p| p.name.clone()).collect();
    if names.is_empty() {
        bail!("no projects in {}", dir.display());
    }
    let name = match name {
        Some(name) => name,
        None if form::available() => {
            let default = current_project::current(&dir).unwrap_or_else(|| names[0].clone());
            form::select("Project", names.clone(), &default)?
        }
        None => bail!(
            "usage: rite project use <name> (one of: {})",
            names.join(", ")
        ),
    };
    // Found by name without case, and recorded as the project is called
    // (ticket 338): `rite project use KanRite` finds `kanrite`.
    let matching: Vec<&String> = names
        .iter()
        .filter(|candidate| rite_lib::names::same(candidate, &name))
        .collect();
    let name = match matching.as_slice() {
        [] => bail!(
            "no project '{name}' in {} (one of: {})",
            dir.display(),
            names.join(", ")
        ),
        [one] => (*one).clone(),
        // Exactly as typed picks between two that differ only in case.
        many => match many.iter().find(|candidate| **candidate == &name) {
            Some(exact) => (*exact).clone(),
            None => bail!(
                "{} differ only in case: say which, exactly as written",
                many.iter()
                    .map(|n| n.as_str())
                    .collect::<Vec<_>>()
                    .join(" and ")
            ),
        },
    };
    current_project::write(&dir, &name)?;
    println!("current project: {name}");
    Ok(())
}

/// `rite project archive|unarchive [name]` (tickets 157, 263): the named
/// project in this folder, else the one rite uses here. Always on GitHub
/// too; archiving also takes it off rite on this machine.
pub fn archive(name: Option<String>, on: bool) -> Result<()> {
    let board = match name {
        Some(name) => {
            let dir = current_project::project_dir()?;
            let projects = current_project::projects(&dir);
            match projects.iter().find(|p| p.name == name).or_else(|| {
                projects
                    .iter()
                    .find(|p| rite_lib::names::same(&p.name, &name))
            }) {
                Some(project) => Board::in_repo(&project.tickets),
                None => bail!(
                    "no project '{name}' in {} (one of: {})",
                    dir.display(),
                    projects
                        .iter()
                        .map(|p| p.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
        }
        None => Board::discover()?,
    };
    if !board.root.is_dir() {
        bail!("no board at {}", board.root.display());
    }
    rite_lib::project_trust::check(&board)?;
    let report = |events: Vec<Event>| {
        for event in events {
            match event {
                Event::Info(text) => println!("{text}"),
                Event::Warn(text) => eprintln!("rite: {text}"),
            }
        }
    };
    report(rite_lib::sync::catch_up(
        &board,
        rite_lib::sync::Purpose::Write,
    )?);
    report(rite_lib::archive::set_archived(
        &board,
        on,
        true,
        &mut rite_lib::archive::gh,
        &mut rite_lib::archive::forget,
    )?);
    // Back on rite here (ticket 263): archiving took it off this machine.
    if !on && !rite_lib::archive::is_archived(&board) {
        let root = board.repo_root().to_path_buf();
        crate::init::register(&root)?;
        let project = rite_lib::current_project::project_name(&root);
        if let Some(dir) = root.parent() {
            let documents = rite_lib::names::find_folder(dir, &format!("{project}-documents"))
                .unwrap_or_else(|| dir.join(format!("{project}-documents")));
            if rite_lib::git::is_repo_dir(&documents) {
                crate::init::register(&documents)?;
            }
        }
    }
    Ok(())
}

/// Says so when `init`/`join` adding `added` to `dir` made it a folder of
/// several projects, and the one already here was kept current.
pub fn keep_existing(dir: &Path, added: &str) -> Result<()> {
    if let Some(kept) = current_project::keep_existing(dir, added)? {
        println!(
            "two projects here now — current is still {kept}; `rite project use {added}` to switch"
        );
    }
    Ok(())
}

/// `kanrite-tickets, kanrite-documents` — relative to the folder, since
/// that's where the list was asked for.
fn repositories(dir: &Path, project: &ProjectEntry) -> String {
    [Some(&project.tickets), project.documents.as_ref()]
        .into_iter()
        .flatten()
        .map(|path| path.strip_prefix(dir).unwrap_or(path).display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
