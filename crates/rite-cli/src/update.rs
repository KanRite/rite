//! `rite update` (ticket 191): replaces the installed `rite` and `riteui` with
//! the newest release's, one release at a time, through
//! `rite_lib::update_apply`, which never touches anything else. Only an
//! installer's release install updates itself; anything else is told how.
//!
//! The network is here, not in rite-lib: GitHub's releases API and its
//! downloads, through `ureq`. `RITE_UPDATE_RELEASES` points the release list
//! somewhere else (a `file://` path, as the tests use), and `file://`
//! download URLs are read from disk.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use rite_lib::update::{self, Release, Version};
use rite_lib::update_apply::{self, Applied, Fetch, Install, Step};

const RELEASES: &str = "https://api.github.com/repos/kanrite/rite/releases?per_page=100";

struct Net {
    agent: ureq::Agent,
}

impl Net {
    fn new() -> Net {
        Net {
            agent: ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(120)))
                .build()
                .into(),
        }
    }
}

impl Fetch for Net {
    fn get(&self, url: &str) -> Result<Vec<u8>> {
        if let Some(path) = url.strip_prefix("file://") {
            return std::fs::read(path).with_context(|| format!("reading {path}"));
        }
        Ok(self
            .agent
            .get(url)
            .header("User-Agent", concat!("rite/", env!("CARGO_PKG_VERSION")))
            .call()?
            .into_body()
            .with_config()
            .limit(200 * 1024 * 1024)
            .read_to_vec()?)
    }
}

/// Every published release, with its files, fresh from GitHub.
fn releases(net: &Net) -> Result<Vec<Release>> {
    let url =
        rite_lib::renamed::var("RITE_UPDATE_RELEASES").unwrap_or_else(|_| RELEASES.to_string());
    let body = net
        .get(&url)
        .context("asking GitHub for KanRite's releases")?;
    update::parse_releases(&String::from_utf8_lossy(&body))
}

fn current() -> Result<Version> {
    Version::parse(env!("CARGO_PKG_VERSION"))
        .ok_or_else(|| anyhow!("this build has no release version"))
}

/// `rite update [--check] [--rollback]`.
pub fn run(check: bool, rollback: bool) -> Result<()> {
    let exe = std::env::current_exe().context("finding this rite")?;
    let current = current()?;
    if rollback {
        let Install::Managed(dir) = update_apply::install_dir(&exe) else {
            bail!(
                "rite here isn't a release install, so there's no previous version to go back to"
            );
        };
        let restored = update_apply::rollback(&dir)?;
        println!("put back the previous {}", restored.join(" and "));
        return Ok(());
    }
    let net = Net::new();
    let all = releases(&net)?;
    let steps = update_apply::steps(current, &all);
    let Some(newest) = steps.last().map(|r| r.version) else {
        println!("rite {current} is the newest release");
        return Ok(());
    };
    if check {
        println!(
            "rite {current}; KanRite {newest} is out{}: `rite update` installs it",
            if steps.len() > 1 {
                format!(" ({} releases)", steps.len())
            } else {
                String::new()
            }
        );
        return Ok(());
    }
    let dir = match update_apply::install_dir(&exe) {
        Install::Managed(dir) => dir,
        Install::Unmanaged {
            why,
            reinstall: true,
        } => bail!("{why}:\n  {}", update_apply::INSTALL_COMMAND),
        Install::Unmanaged { why, .. } => bail!("{why}"),
    };
    let applied = update_apply::apply(&dir, current, &all, &net, &mut |step| match step {
        Step::Downloading(v) => println!("downloading {v}"),
        Step::Installing(v) => println!("installing {v}"),
        Step::Installed(v) => println!("updated to {v}"),
    });
    match applied {
        Applied {
            reached: Some(v),
            failed: None,
        } => {
            println!(
                "rite is now {v}; the one before is kept (`rite update --rollback` goes back). Restart riteui if it's open."
            );
            Ok(())
        }
        Applied {
            reached,
            failed: Some((at, why)),
        } => bail!(
            "stopped at {at}: {why}. rite is {}",
            reached.map_or(current.to_string(), |v| v.to_string())
        ),
        Applied {
            reached: None,
            failed: None,
        } => {
            println!("rite {current} is the newest release");
            Ok(())
        }
    }
}
