//! The folders amd keeps per-machine files in (ticket 310): config
//! (`~/.config/kanrite`: settings, the registry, project trust), state
//! (`~/.local/state/kanrite`: the update check, screen notes, recovered
//! tickets) and data (`~/.local/share/kanrite`: speech models and Piper).
//! `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `XDG_DATA_HOME` move them, when
//! they're absolute paths, as the XDG spec says.
//!
//! **On Windows** (ticket 278) the folders are `%APPDATA%\kanrite` (config)
//! and `%LOCALAPPDATA%\kanrite` (state and data), unless an `XDG_*` variable
//! says otherwise. 5.0.0 used the Unix layout under `%USERPROFILE%`
//! (`.config\kanrite` and so on): [`move_old_folders`] renames those into
//! place, with no link (a symlink needs Developer Mode or admin there), and
//! until it has, [`config`], [`state`] and [`data`] use such a folder while
//! it's the only one.
//!
//! Before 5.0 each was called `agile-md`. [`move_old_folders`], run first
//! thing by `rite` and `riteui`, renames an `agile-md` folder to `kanrite`
//! and leaves a symlink at the old name, so a 4.x build on the same machine
//! still finds its config. Renamed, never copied: the speech models are
//! large. Until it has run (or where it couldn't), [`config`], [`state`]
//! and [`data`] use the old folder when it's the only one there, so nothing
//! is ever lost track of.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

/// The folders' name since 5.0.
pub const NAME: &str = "kanrite";
/// Their name before (ticket 310).
pub const OLD_NAME: &str = "agile-md";

/// `$XDG_CONFIG_HOME/kanrite`, else `%APPDATA%\kanrite` on Windows, else
/// `~/.config/kanrite`.
pub fn config() -> Option<PathBuf> {
    folder(Place::Config)
}

/// `$XDG_STATE_HOME/kanrite`, else `%LOCALAPPDATA%\kanrite` on Windows, else
/// `~/.local/state/kanrite`.
pub fn state() -> Option<PathBuf> {
    folder(Place::State)
}

/// `$XDG_DATA_HOME/kanrite`, else `%LOCALAPPDATA%\kanrite` on Windows, else
/// `~/.local/share/kanrite`.
pub fn data() -> Option<PathBuf> {
    folder(Place::Data)
}

#[derive(Clone, Copy)]
enum Place {
    Config,
    State,
    Data,
}

impl Place {
    const ALL: [Place; 3] = [Place::Config, Place::State, Place::Data];

    /// The XDG variable that moves it.
    fn xdg(self) -> &'static str {
        match self {
            Place::Config => "XDG_CONFIG_HOME",
            Place::State => "XDG_STATE_HOME",
            Place::Data => "XDG_DATA_HOME",
        }
    }

    /// Where it is under the home folder when nothing says otherwise.
    fn under_home(self) -> &'static str {
        match self {
            Place::Config => ".config",
            Place::State => ".local/state",
            Place::Data => ".local/share",
        }
    }

    /// The variable Windows keeps it under.
    fn windows(self) -> &'static str {
        match self {
            Place::Config => "APPDATA",
            Place::State | Place::Data => "LOCALAPPDATA",
        }
    }
}

/// The folder a place's `kanrite` folder goes in, and on Windows the one
/// 5.0.0 used instead (under the home folder), if it's a different one.
struct Bases {
    base: PathBuf,
    legacy: Option<PathBuf>,
}

/// [`Bases`] for a place, given the platform, the environment and the home
/// folder, so the Windows answer can be tested anywhere. A variable counts
/// only when it's an absolute path.
fn bases(
    place: Place,
    windows: bool,
    var: impl Fn(&str) -> Option<OsString>,
    home: Option<PathBuf>,
) -> Option<Bases> {
    let absolute = |name: &str| var(name).map(PathBuf::from).filter(|p| p.is_absolute());
    let under_home = home.map(|home| home.join(place.under_home()));
    if let Some(base) = absolute(place.xdg()) {
        return Some(Bases { base, legacy: None });
    }
    if windows && let Some(base) = absolute(place.windows()) {
        let legacy = under_home.filter(|legacy| *legacy != base);
        return Some(Bases { base, legacy });
    }
    under_home.map(|base| Bases { base, legacy: None })
}

fn here(place: Place) -> Option<Bases> {
    bases(
        place,
        cfg!(windows),
        |name| std::env::var_os(name),
        crate::home::dir(),
    )
}

fn folder(place: Place) -> Option<PathBuf> {
    here(place).map(|bases| resolve(&bases))
}

/// The folder in use: the one in the base, else (on Windows) a 5.0.0 folder
/// that's the only one there.
fn resolve(bases: &Bases) -> PathBuf {
    let current = within(&bases.base);
    if !current.exists()
        && let Some(legacy) = &bases.legacy
    {
        let old = within(legacy);
        if old.is_dir() {
            return old;
        }
    }
    current
}

/// `base/kanrite`, or `base/agile-md` while that's the only one there.
fn within(base: &Path) -> PathBuf {
    let new = base.join(NAME);
    let old = base.join(OLD_NAME);
    if !new.exists() && old.is_dir() {
        old
    } else {
        new
    }
}

/// A per-machine folder that is still where an older version kept it
/// (ticket 385): what `rite doctor` reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stranded {
    /// Where it is.
    pub old: PathBuf,
    /// Where it belongs.
    pub new: PathBuf,
    /// `true` when the new folder is there too, so the old one is ignored
    /// and left behind; `false` when nothing has moved it yet.
    pub left_behind: bool,
}

/// The old folders still in place: an `agile-md` one beside (or without) a
/// `kanrite` one, and on Windows a 5.0.0 folder under the home folder.
/// [`move_old_folders`] clears the second kind of case at the next start;
/// what's here is what it couldn't or hasn't yet.
pub fn stranded() -> Vec<Stranded> {
    Place::ALL
        .into_iter()
        .filter_map(here)
        .flat_map(|bases| stranded_in(&bases))
        .collect()
}

fn stranded_in(bases: &Bases) -> Vec<Stranded> {
    let is_real_dir = |p: &Path| fs::symlink_metadata(p).is_ok_and(|m| m.is_dir());
    let new = bases.base.join(NAME);
    let mut out = Vec::new();
    let mut note = |old: PathBuf| {
        out.push(Stranded {
            left_behind: fs::symlink_metadata(&new).is_ok(),
            old,
            new: new.clone(),
        });
    };
    let old = bases.base.join(OLD_NAME);
    if is_real_dir(&old) {
        note(old);
    }
    if let Some(legacy) = &bases.legacy {
        for name in [NAME, OLD_NAME] {
            let old = legacy.join(name);
            if is_real_dir(&old) {
                note(old);
            }
        }
    }
    out
}

/// Renames each `agile-md` folder that has no `kanrite` beside it, leaving
/// a symlink at the old name (on Windows, which had no 4.x release, just
/// the rename). On Windows it also moves a 5.0.0 folder under the home
/// folder to `%APPDATA%` or `%LOCALAPPDATA%` (no link there either).
/// Returns the folders moved, as `(old, new)`. A failure leaves that folder
/// where it was, still used ([`resolve`]).
pub fn move_old_folders() -> Vec<(PathBuf, PathBuf)> {
    let mut moved = Vec::new();
    for place in Place::ALL {
        let Some(bases) = here(place) else { continue };
        moved.extend(move_in(&bases.base));
        if let Some(legacy) = &bases.legacy {
            moved.extend(move_across(legacy, &bases.base));
        }
    }
    moved
}

/// [`move_old_folders`], saying on stderr what moved. `rite` and `riteui`
/// call it first thing.
pub fn move_old_folders_and_say() {
    for (old, new) in move_old_folders() {
        let link = if fs::symlink_metadata(&old).is_ok() {
            " (the old name stays as a link, for older versions)"
        } else {
            ""
        };
        eprintln!("moved {} to {}{link}", old.display(), new.display());
    }
}

/// [`move_old_folders`] for one base folder.
fn move_in(base: &Path) -> Option<(PathBuf, PathBuf)> {
    let old = base.join(OLD_NAME);
    let new = base.join(NAME);
    let is_real_dir = fs::symlink_metadata(&old).is_ok_and(|m| m.is_dir());
    if !is_real_dir || fs::symlink_metadata(&new).is_ok() {
        return None;
    }
    fs::rename(&old, &new).ok()?;
    #[cfg(unix)]
    if std::os::unix::fs::symlink(NAME, &old).is_err() {
        // Without the link a 4.x build would start afresh: put it back.
        let _ = fs::rename(&new, &old);
        return None;
    }
    Some((old, new))
}

/// Moves the folder 5.0.0 kept in `from` (a `kanrite` one, else an
/// `agile-md` one) to `to/kanrite`, unless that is there already. No link.
fn move_across(from: &Path, to: &Path) -> Option<(PathBuf, PathBuf)> {
    let is_real_dir = |p: &Path| fs::symlink_metadata(p).is_ok_and(|m| m.is_dir());
    let old = [from.join(NAME), from.join(OLD_NAME)]
        .into_iter()
        .find(|p| is_real_dir(p))?;
    let new = to.join(NAME);
    if fs::symlink_metadata(&new).is_ok() {
        return None;
    }
    fs::create_dir_all(to).ok()?;
    fs::rename(&old, &new).ok()?;
    Some((old, new))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_old_folder_is_used_until_it_is_moved_then_linked() {
        let base = tempfile::tempdir().unwrap();
        let old = base.path().join(OLD_NAME);
        let new = base.path().join(NAME);
        // Nothing yet: the new name.
        assert_eq!(within(base.path()), new);
        // Only the old one: used as it is.
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("config"), "zoom = 120\n").unwrap();
        assert_eq!(within(base.path()), old);
        // Moved: the new name, with the old one a link to it.
        assert_eq!(move_in(base.path()), Some((old.clone(), new.clone())));
        assert_eq!(within(base.path()), new);
        assert_eq!(
            fs::read_to_string(new.join("config")).unwrap(),
            "zoom = 120\n"
        );
        #[cfg(unix)]
        {
            assert!(fs::symlink_metadata(&old).unwrap().is_symlink());
            assert_eq!(
                fs::read_to_string(old.join("config")).unwrap(),
                "zoom = 120\n"
            );
        }
        // Once only.
        assert_eq!(move_in(base.path()), None);
    }

    #[test]
    fn an_old_folder_with_nothing_moved_it_is_stranded_not_left_behind() {
        let base = tempfile::tempdir().unwrap();
        let bases = Bases {
            base: base.path().to_path_buf(),
            legacy: None,
        };
        assert_eq!(stranded_in(&bases), []);
        fs::create_dir_all(base.path().join(OLD_NAME)).unwrap();
        assert_eq!(
            stranded_in(&bases),
            [Stranded {
                old: base.path().join(OLD_NAME),
                new: base.path().join(NAME),
                left_behind: false,
            }]
        );
        // The new one there too: the old is left behind.
        fs::create_dir_all(base.path().join(NAME)).unwrap();
        assert!(stranded_in(&bases)[0].left_behind);
    }

    #[test]
    fn a_5_0_0_folder_under_home_is_stranded_on_windows_layouts() {
        let tmp = tempfile::tempdir().unwrap();
        let bases = Bases {
            base: tmp.path().join("Roaming"),
            legacy: Some(tmp.path().join("me/.config")),
        };
        fs::create_dir_all(tmp.path().join("me/.config").join(NAME)).unwrap();
        let found = stranded_in(&bases);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].old, tmp.path().join("me/.config").join(NAME));
        assert!(!found[0].left_behind);
        fs::create_dir_all(tmp.path().join("Roaming").join(NAME)).unwrap();
        assert!(stranded_in(&bases)[0].left_behind);
        // A symlink at the old name isn't a folder to move or clean up.
        let linked = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(NAME, linked.path().join(OLD_NAME)).unwrap();
            let bases = Bases {
                base: linked.path().to_path_buf(),
                legacy: None,
            };
            assert_eq!(stranded_in(&bases), []);
        }
    }

    #[test]
    fn both_folders_there_are_left_alone() {
        let base = tempfile::tempdir().unwrap();
        fs::create_dir_all(base.path().join(OLD_NAME)).unwrap();
        fs::create_dir_all(base.path().join(NAME)).unwrap();
        assert_eq!(move_in(base.path()), None);
        assert_eq!(within(base.path()), base.path().join(NAME));
        assert!(base.path().join(OLD_NAME).is_dir());
    }

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| OsString::from(v))
        }
    }

    fn at(
        place: Place,
        windows: bool,
        pairs: &[(&str, &str)],
        home: &Path,
    ) -> (PathBuf, Option<PathBuf>) {
        let b = bases(place, windows, env(pairs), Some(home.to_path_buf())).unwrap();
        (b.base, b.legacy)
    }

    #[test]
    fn unix_keeps_the_home_layout() {
        let home = Path::new("/h");
        let vars = [("APPDATA", "/a"), ("LOCALAPPDATA", "/l")];
        assert_eq!(
            at(Place::Config, false, &vars, home),
            (home.join(".config"), None)
        );
        assert_eq!(
            at(Place::State, false, &vars, home),
            (home.join(".local/state"), None)
        );
        assert_eq!(
            at(Place::Data, false, &vars, home),
            (home.join(".local/share"), None)
        );
    }

    #[test]
    fn windows_uses_appdata_and_remembers_the_5_0_0_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, l) = (tmp.path().join("Roaming"), tmp.path().join("Local"));
        let (a_s, l_s) = (a.to_str().unwrap(), l.to_str().unwrap());
        let vars = [("APPDATA", a_s), ("LOCALAPPDATA", l_s)];
        let home = tmp.path().join("me");
        assert_eq!(
            at(Place::Config, true, &vars, &home),
            (a.clone(), Some(home.join(".config")))
        );
        assert_eq!(
            at(Place::State, true, &vars, &home),
            (l.clone(), Some(home.join(".local/state")))
        );
        assert_eq!(
            at(Place::Data, true, &vars, &home),
            (l, Some(home.join(".local/share")))
        );
    }

    #[test]
    fn xdg_wins_everywhere_and_a_relative_variable_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let x = tmp.path().join("x");
        let vars = [
            ("XDG_CONFIG_HOME", x.to_str().unwrap()),
            ("APPDATA", tmp.path().to_str().unwrap()),
            ("XDG_STATE_HOME", "relative"),
        ];
        let home = tmp.path().join("me");
        assert_eq!(at(Place::Config, true, &vars, &home), (x.clone(), None));
        // Relative: skipped, so Windows falls through to LOCALAPPDATA (unset
        // here) and then the home layout.
        assert_eq!(
            at(Place::State, true, &vars, &home),
            (home.join(".local/state"), None)
        );
    }

    #[test]
    fn a_5_0_0_windows_folder_is_used_until_it_is_moved() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("Roaming");
        let legacy = tmp.path().join("me").join(".config");
        fs::create_dir_all(legacy.join(NAME)).unwrap();
        fs::write(
            legacy.join(NAME).join("repos"),
            "x
",
        )
        .unwrap();
        let bases = Bases {
            base: base.clone(),
            legacy: Some(legacy.clone()),
        };
        // Only the old one: used where it is.
        assert_eq!(resolve(&bases), legacy.join(NAME));
        // Moved, with no link left behind.
        assert_eq!(
            move_across(&legacy, &base),
            Some((legacy.join(NAME), base.join(NAME)))
        );
        assert_eq!(resolve(&bases), base.join(NAME));
        assert_eq!(
            fs::read_to_string(base.join(NAME).join("repos")).unwrap(),
            "x
"
        );
        assert!(!legacy.join(NAME).exists());
        // Once only.
        assert_eq!(move_across(&legacy, &base), None);
    }

    #[test]
    fn both_windows_folders_there_are_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("Roaming");
        let legacy = tmp.path().join("me").join(".config");
        fs::create_dir_all(legacy.join(NAME)).unwrap();
        fs::create_dir_all(base.join(NAME)).unwrap();
        assert_eq!(move_across(&legacy, &base), None);
        assert!(legacy.join(NAME).is_dir());
        let bases = Bases {
            base: base.clone(),
            legacy: Some(legacy),
        };
        assert_eq!(resolve(&bases), base.join(NAME));
    }

    #[test]
    fn nothing_anywhere_is_the_new_name_in_the_base() {
        let tmp = tempfile::tempdir().unwrap();
        let bases = Bases {
            base: tmp.path().join("Roaming"),
            legacy: Some(tmp.path().join("me").join(".config")),
        };
        assert_eq!(resolve(&bases), tmp.path().join("Roaming").join(NAME));
    }
}
