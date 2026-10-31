//! `rite docs` (ticket 179): the project's documents from the command line —
//! the start page, a search, the page tree the start page links to, and
//! your own pinned documents (ticket 232). Otherwise reading only: the
//! documents repository is a clone like any other, so changes are made
//! there directly.

use anyhow::{Context, Result, bail};

use rite_lib::board::Board;
use rite_lib::documents::{self, Kind};
use rite_lib::documents_edit;

/// What `rite docs` was asked to do.
pub enum Action {
    Start { path: bool },
    Search { query: String },
    Tree,
    Pins,
    Pin { document: String, on: bool },
}

pub fn run(action: Action) -> Result<()> {
    let board = Board::discover()?;
    let sources = documents::sources(&board);
    let own = sources
        .iter()
        .find(|s| s.kind == Kind::Own)
        .context("this project has no documents repository")?;
    let own_root = || {
        own.root
            .clone()
            .with_context(|| format!("{} isn't cloned on this machine", own.name))
    };
    match action {
        Action::Start { path } => {
            let root = own_root()?;
            let Some(start) = documents_edit::start_page(&root) else {
                bail!(
                    "{} has no start page: pin one in riteui, or add a README.md",
                    own.name
                );
            };
            if path {
                println!("{}", root.join(&start).display());
            } else {
                print!("{}", documents::read(&root, &start)?);
            }
            Ok(())
        }
        Action::Search { query } => {
            let mut found = 0;
            for source in &sources {
                let Some(root) = &source.root else { continue };
                for hit in documents_edit::search(root, &query, 3)? {
                    found += 1;
                    if hit.line == 0 {
                        println!("{}/{}: {}", source.name, hit.path, hit.title);
                    } else {
                        println!("{}/{}:{}: {}", source.name, hit.path, hit.line, hit.text);
                    }
                }
            }
            if found == 0 {
                eprintln!("nothing matches '{}'", query.trim());
            }
            Ok(())
        }
        Action::Tree => {
            let root = own_root()?;
            let Some(start) = documents_edit::start_page(&root) else {
                bail!(
                    "{} has no start page: pin one in riteui, or add a README.md",
                    own.name
                );
            };
            for page in documents_edit::link_tree(&root, &start)? {
                println!("{}{}  {}", "  ".repeat(page.depth), page.title, page.path);
            }
            Ok(())
        }
        Action::Pins => {
            let root = own_root()?;
            let me = rite_lib::doc_pins::me(&root)?;
            let pins = rite_lib::doc_pins::pins(&root, &me);
            if pins.is_empty() {
                println!("nothing pinned — `rite docs pin <repository>/<path>` pins one");
            }
            for pin in pins {
                let there = sources
                    .iter()
                    .find(|s| s.name == pin.source)
                    .and_then(|s| s.root.as_ref())
                    .is_some_and(|r| r.join(&pin.path).is_file());
                let gone = if there { "" } else { "  (not there)" };
                println!("{}/{}{gone}", pin.source, pin.path);
            }
            Ok(())
        }
        Action::Pin { document, on } => {
            let root = own_root()?;
            let me = rite_lib::doc_pins::me(&root)?;
            let Some((source, path)) = document.split_once('/') else {
                bail!(
                    "name it as <repository>/<path>, e.g. {}/docs/guide.md",
                    own.name
                );
            };
            if on {
                let there = sources
                    .iter()
                    .find(|s| s.name == source)
                    .with_context(|| {
                        format!("{source} isn't one of this project's documents repositories")
                    })?
                    .root
                    .as_ref()
                    .is_some_and(|r| documents::resolve(r, path).is_ok_and(|p| p.is_file()));
                if !there {
                    bail!("{document} isn't a document here");
                }
            }
            let done = rite_lib::doc_pins::set(&root, &me, source, path, on)?;
            if let Some(warning) = &done.warning {
                eprintln!("rite: {warning}");
            }
            println!("{}", done.message);
            Ok(())
        }
    }
}
