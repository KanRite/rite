//! Building, checking and installing KanRite from a clone, the same way on
//! Linux, macOS and Windows (ticket 318).
//!
//! ```text
//! cargo xtask build [--release] [rite] [riteui]
//! cargo xtask check
//! cargo xtask install [--dir DIR]
//! ```
//!
//! The commands it runs are the ones CI runs (.github/workflows/ci.yml), so
//! `cargo xtask check` passing here is a good sign it will pass there.

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

const USAGE: &str = "\
Usage: cargo xtask <task>

Tasks:
  build [--release] [rite] [riteui]
          Build rite and riteui (both when neither is named). The desktop
          board's page is built with Trunk first; without Trunk and the
          wasm target, riteui is skipped with a note unless it was named.
  check   Everything CI checks: the board's page, cargo fmt, clippy (also
          as WebAssembly), the unit tests and, except on Windows, the CLI
          tests (tests/test.sh).
  install [--dir DIR]
          Build a release and install it. On Linux and macOS this is
          `install.sh --from-source`, which adds riteui's launcher and the
          uninstaller. On Windows it copies rite.exe and riteui.exe, and
          amd.exe and amdui.exe (the old names), into DIR, by default
          Cargo's bin folder (on the PATH wherever rustup is).
";

const BOARD_TOOLS: &str = "\
riteui needs Trunk and the wasm target to build its page:
  cargo install --locked trunk
  rustup target add wasm32-unknown-unknown
and on Linux WebKitGTK and ALSA (libwebkit2gtk-4.1-dev libasound2-dev).";

type Result<T = ()> = std::result::Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let (task, rest) = match args.split_first() {
        Some((task, rest)) => (task.as_str(), rest),
        None => ("help", &[][..]),
    };
    let result = match task {
        "build" => build(rest),
        "check" => check(rest),
        "install" => install(rest),
        "help" | "-h" | "--help" => {
            print!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown task `{other}`\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("xtask: {message}");
            ExitCode::FAILURE
        }
    }
}

fn build(args: &[String]) -> Result {
    let args = words(args, &["--release", "rite", "riteui"])?;
    let release = args.contains("--release");
    let rite = args.contains("rite");
    let riteui = args.contains("riteui");
    match (rite, riteui) {
        (true, false) => build_rite(release),
        (false, true) => build_riteui(release),
        _ => {
            build_rite(release)?;
            // Named, riteui's tools are required; left to the default, a
            // machine without them still gets rite.
            if riteui || board_tools() {
                build_riteui(release)
            } else {
                eprintln!("Skipped riteui. {BOARD_TOOLS}");
                Ok(())
            }
        }
    }
}

fn check(args: &[String]) -> Result {
    words(args, &[])?;
    board_page()?;
    run(cargo(["fmt", "--all", "--check"]))?;
    run(cargo([
        "clippy",
        "--all-targets",
        "--all-features",
        "--",
        "-D",
        "warnings",
    ]))?;
    run(cargo([
        "clippy",
        "-p",
        "rite-ui-web",
        "-p",
        "rite-ui-model",
        "--target",
        "wasm32-unknown-unknown",
        "--",
        "-D",
        "warnings",
    ]))?;
    run(cargo(["test", "--locked", "--workspace", "--all-features"]))?;
    // The CLI tests are a bash script, and don't pass on Windows yet
    // (ticket 276).
    if cfg!(windows) {
        eprintln!("Skipped the CLI tests (tests/test.sh): not on Windows yet, ticket 276.");
    } else {
        run(command("bash", ["tests/test.sh"]))?;
    }
    println!("All checks passed.");
    Ok(())
}

fn install(args: &[String]) -> Result {
    if !cfg!(windows) {
        // install.sh already does the rest for Linux and macOS (the
        // replace-while-running rename, riteui's launcher, rite-uninstall and
        // the install marker `rite update` reads), and builds through
        // `cargo xtask build`.
        let mut sh = command("bash", ["install.sh", "--from-source"]);
        sh.args(args);
        return run(sh);
    }
    let mut dir = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--dir" => dir = Some(PathBuf::from(rest.next().ok_or("--dir needs a folder")?)),
            other => return Err(format!("install: unknown option `{other}`")),
        }
    }
    let dir = match dir {
        Some(dir) => dir,
        None => cargo_bin()?,
    };

    build_rite(true)?;
    let board = board_tools();
    if board {
        build_riteui(true)?;
    } else {
        eprintln!("Installing rite without riteui. {BOARD_TOOLS}");
    }

    fs::create_dir_all(&dir).map_err(|e| format!("couldn't create {}: {e}", dir.display()))?;
    let release = root().join("target").join("release");
    let names: &[&str] = if board {
        &["rite", "riteui"]
    } else {
        &["rite"]
    };
    for name in names {
        let file = format!("{name}{}", env::consts::EXE_SUFFIX);
        replace(&release.join(&file), &dir.join(&file))?;
        println!("Installed {name} -> {}", dir.join(&file).display());
        // The old name, amd.exe or amdui.exe, as a copy: Windows has no
        // symlinks without developer mode (ticket 309).
        let old = format!("{}{}", old_name(name), env::consts::EXE_SUFFIX);
        replace(&release.join(&file), &dir.join(&old))?;
    }
    let on_path =
        env::var_os("PATH").is_some_and(|path| env::split_paths(&path).any(|p| same_dir(&p, &dir)));
    if !on_path {
        println!("Note: {} is not on your PATH.", dir.display());
    }
    Ok(())
}

/// The 4.x name a program keeps as a shim through 5.x (ticket 309).
fn old_name(name: &str) -> &'static str {
    if name == "riteui" { "amdui" } else { "amd" }
}

fn build_rite(release: bool) -> Result {
    run(cargo_build("rite-cli", release))
}

fn build_riteui(release: bool) -> Result {
    if !board_tools() {
        return Err(BOARD_TOOLS.to_string());
    }
    board_page()?;
    run(cargo_build("rite-ui", release))
}

fn cargo_build(package: &str, release: bool) -> Command {
    let mut cmd = cargo(["build", "--locked", "-p", package]);
    if release {
        cmd.arg("--release");
    }
    cmd
}

/// The desktop board's page, compiled to WebAssembly by Trunk. riteui embeds
/// it at compile time, so it comes before anything builds rite-ui. Always a
/// release build, as in CI and the release workflow.
fn board_page() -> Result {
    if !board_tools() {
        return Err(BOARD_TOOLS.to_string());
    }
    let mut trunk = command("trunk", ["build", "--release"]);
    trunk.current_dir(root().join("crates").join("rite-ui-web"));
    run(trunk)
}

/// Whether this machine has what the board's page needs: Trunk, and the
/// wasm32 target in rustup.
fn board_tools() -> bool {
    let quiet = |mut cmd: Command| {
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        cmd.status().is_ok_and(|s| s.success())
    };
    if !quiet(command("trunk", ["--version"])) {
        return false;
    }
    Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .is_ok_and(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|line| line.trim() == "wasm32-unknown-unknown")
        })
}

/// Puts SRC in place as DEST while DEST may be running. Windows won't
/// overwrite a running .exe but does let it be renamed, so the old one moves
/// aside to `<name>.old` first; it can't be deleted until it exits, so the
/// next install clears it.
fn replace(src: &Path, dest: &Path) -> Result {
    let mut old = dest.as_os_str().to_owned();
    old.push(".old");
    let old = PathBuf::from(old);
    if dest.exists() {
        let _ = fs::remove_file(&old);
        fs::rename(dest, &old)
            .map_err(|e| format!("couldn't move {} aside: {e}", dest.display()))?;
    }
    fs::copy(src, dest)
        .map_err(|e| format!("couldn't copy {} to {}: {e}", src.display(), dest.display()))?;
    let _ = fs::remove_file(&old);
    Ok(())
}

/// Cargo's bin folder: `$CARGO_HOME/bin`, by default `~/.cargo/bin`.
fn cargo_bin() -> Result<PathBuf> {
    if let Some(home) = env::var_os("CARGO_HOME") {
        return Ok(PathBuf::from(home).join("bin"));
    }
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(|home| PathBuf::from(home).join(".cargo").join("bin"))
        .ok_or_else(|| "no home folder to install into; give one with --dir".to_string())
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The task's arguments as a set, refusing any not in ALLOWED.
fn words<'a>(args: &'a [String], allowed: &[&str]) -> Result<BTreeSet<&'a str>> {
    args.iter()
        .map(|arg| {
            if allowed.contains(&arg.as_str()) {
                Ok(arg.as_str())
            } else {
                Err(format!("unknown argument `{arg}`\n\n{USAGE}"))
            }
        })
        .collect()
}

/// The workspace root: this crate is crates/xtask.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/xtask is two folders below the workspace root")
        .to_path_buf()
}

/// The cargo running this task (so the same toolchain), in the workspace
/// root.
fn cargo<const N: usize>(args: [&str; N]) -> Command {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    command(cargo, args)
}

fn command<const N: usize>(program: impl AsRef<OsStr>, args: [&str; N]) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(root());
    cmd
}

/// Runs CMD, saying what it is first, and fails when it does.
fn run(mut cmd: Command) -> Result {
    // The program by name (`cargo`, not the full path $CARGO gives), then
    // its arguments as they are.
    let program = Path::new(cmd.get_program());
    let shown = std::iter::once(program.file_stem().unwrap_or(program.as_os_str()))
        .chain(cmd.get_args())
        .map(|part| part.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    println!("> {shown}");
    let status = cmd
        .status()
        .map_err(|e| format!("couldn't run `{shown}`: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{shown}` failed ({status})"))
    }
}
