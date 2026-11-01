//! The update bar's backend (ticket 192): asks GitHub for KanRite's
//! releases, through `rite_lib::update`, which decides what to say and
//! remembers "Later". rite-lib has no HTTP client, so the network is here.
//!
//! Silent about failure, as `update::check` is: offline, rate limited or a
//! garbled answer is "nothing to report", never an error on screen.
//! `RITE_UPDATE_RELEASES=file://…` reads the release list (and `file://`
//! downloads) from disk instead, as `rite update` does, for testing.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rite_lib::settings::Config;
use rite_lib::update::{self, Release, ReleaseSource, Version};
use rite_lib::update_apply::{self, Applied, Fetch, Install};
use rite_ui_model::{ManualCheck, ReleaseNotes, UpdateInfo, UpdateResult};

/// KanRite's releases, newest first; 30 is far more than a build is behind.
const RELEASES: &str = "https://api.github.com/repos/kanrite/rite/releases?per_page=30";

/// GitHub, as `update::check`'s source.
struct GitHub;

impl ReleaseSource for GitHub {
    fn releases(&self) -> anyhow::Result<Vec<Release>> {
        // As `rite update`: the tests point the list at a file.
        if let Ok(list) = rite_lib::renamed::var("RITE_UPDATE_RELEASES")
            && let Some(path) = list.strip_prefix("file://")
        {
            return update::parse_releases(&std::fs::read_to_string(path)?);
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .into();
        let body = agent
            .get(RELEASES)
            .header("User-Agent", concat!("riteui/", env!("CARGO_PKG_VERSION")))
            .header("Accept", "application/vnd.github+json")
            .call()?
            .into_body()
            .read_to_string()?;
        update::parse_releases(&body)
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// The release to offer, if any: none when the check is switched off
/// (`update-check = off`, `RITE_UPDATE_CHECK=0`), this build is current, or
/// it was put off with Later in the last day. GitHub is asked at most daily.
pub fn status() -> Option<UpdateInfo> {
    if !update::enabled(&Config::load()) {
        return None;
    }
    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let found = update::check(&GitHub, current, now(), &update::state_path()?)?;
    Some(UpdateInfo {
        version: found.version,
        notes_url: found.notes_url,
        behind: found.behind,
        current: current.to_string(),
    })
}

/// A check asked for by hand (ticket 304): GitHub now (at most once a
/// minute), even with automatic checks off, clearing a Later.
pub fn check_now() -> ManualCheck {
    let Some(current) = Version::parse(env!("CARGO_PKG_VERSION")) else {
        return ManualCheck::Failed {
            why: "this build has no release version".into(),
        };
    };
    let Some(path) = update::state_path() else {
        return ManualCheck::Failed {
            why: "no home folder to remember updates in".into(),
        };
    };
    match update::check_now(&GitHub, current, now(), &path) {
        update::Manual::Newer(found) => ManualCheck::Newer {
            info: UpdateInfo {
                version: found.version,
                notes_url: found.notes_url,
                behind: found.behind,
                current: current.to_string(),
            },
        },
        update::Manual::Current => ManualCheck::Current {
            current: current.to_string(),
        },
        update::Manual::Failed(why) => ManualCheck::Failed {
            why: format!("couldn't reach GitHub: {why}"),
        },
    }
}

/// What's new (ticket 336): the notes of every release newer than this
/// build, newest first, asked of GitHub now. Unlike the bar's check this
/// says why it failed, since someone asked to read them.
pub fn whats_new() -> Result<Vec<ReleaseNotes>, String> {
    let current = Version::parse(env!("CARGO_PKG_VERSION"))
        .ok_or_else(|| "this build has no release version".to_string())?;
    let releases = GitHub
        .releases()
        .map_err(|e| format!("couldn't reach GitHub: {e:#}"))?;
    Ok(update::newer_than(current, &releases)
        .into_iter()
        .map(|release| ReleaseNotes {
            version: release.version.to_string(),
            html: crate::documents::render_release_notes(&release.notes),
            url: release.notes_url.clone(),
        })
        .collect())
}

/// When GitHub last answered a check (unix seconds), if it's known.
pub fn last_checked() -> Option<i64> {
    update::last_checked(&update::state_path()?)
}

/// "Later": hides `version` for a day, across restarts. A newer release
/// still shows at once.
pub fn later(version: &str) -> anyhow::Result<()> {
    let path = update::state_path().ok_or_else(|| anyhow::anyhow!("no home folder"))?;
    update::snooze(&path, version, now())
}

/// Downloads for an update (ticket 191): a release zip is about 9 MB.
struct Download(ureq::Agent);

impl Fetch for Download {
    fn get(&self, url: &str) -> anyhow::Result<Vec<u8>> {
        if let Some(path) = url.strip_prefix("file://") {
            return Ok(std::fs::read(path)?);
        }
        Ok(self
            .0
            .get(url)
            .header("User-Agent", concat!("riteui/", env!("CARGO_PKG_VERSION")))
            .call()?
            .into_body()
            .with_config()
            .limit(200 * 1024 * 1024)
            .read_to_vec()?)
    }
}

/// The update bar's Update (ticket 191): replaces rite and riteui with the
/// newest release's, one release at a time, through
/// `rite_lib::update_apply`, which touches nothing else. An install
/// riteui doesn't replace says why instead.
pub fn apply() -> UpdateResult {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let failed = |why: String| UpdateResult::Failed {
        why,
        now: current.clone(),
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => return failed(format!("couldn't find riteui: {err}")),
    };
    let dir = match update_apply::install_dir(&exe) {
        Install::Managed(dir) => dir,
        Install::Unmanaged { why, reinstall } => {
            return UpdateResult::Unmanaged {
                why,
                install: reinstall.then(|| update_apply::INSTALL_COMMAND.to_string()),
            };
        }
    };
    let Some(version) = Version::parse(&current) else {
        return failed("this build has no release version".into());
    };
    let releases = match GitHub.releases() {
        Ok(releases) => releases,
        Err(err) => return failed(format!("couldn't reach GitHub: {err:#}")),
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(300)))
        .build()
        .into();
    match update_apply::apply(&dir, version, &releases, &Download(agent), &mut |_| {}) {
        Applied {
            reached: Some(v),
            failed: None,
        } => UpdateResult::Updated {
            version: v.to_string(),
        },
        Applied {
            reached,
            failed: Some((at, why)),
        } => UpdateResult::Failed {
            why: format!("stopped at {at}: {why}"),
            now: reached.map_or(current.clone(), |v| v.to_string()),
        },
        Applied {
            reached: None,
            failed: None,
        } => failed("there's nothing newer to install".into()),
    }
}
