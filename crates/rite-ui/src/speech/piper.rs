//! A natural voice for read aloud (ticket 287): Piper, set up behind
//! speech-dispatcher on Linux, so `spd-say` (and every other program that
//! speaks through speech-dispatcher) uses it.
//!
//! Piper stays a separate program: it needs espeak-ng, which is GPL, so it's
//! never linked into riteui. It's installed from PyPI (`piper-tts`) into a
//! virtual environment in riteui's own data folder, with `uv` when there is
//! one, else Python's `venv`; voices come from Piper's own downloader. Then a
//! marked block in the user's `speechd.conf` (copied from the system's when
//! there's none) makes a `kanrite-piper` module the default. Remove undoes
//! every part of it. Before 5.0 the module was `agile-md-piper`
//! ([`adopt_new_name`] renames a setup made then, ticket 310).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use super::say::on_path;

/// A voice on offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Voice {
    pub id: &'static str,
    pub label: &'static str,
}

/// The voices Preferences offers: English ones, a mix of accents.
pub const VOICES: [Voice; 6] = [
    Voice {
        id: "en_GB-alba-medium",
        label: "Alba (British English, female)",
    },
    Voice {
        id: "en_GB-northern_english_male-medium",
        label: "Northern English (male)",
    },
    Voice {
        id: "en_GB-cori-high",
        label: "Cori (British English, female, high quality)",
    },
    Voice {
        id: "en_US-lessac-medium",
        label: "Lessac (American English, female)",
    },
    Voice {
        id: "en_US-amy-medium",
        label: "Amy (American English, female)",
    },
    Voice {
        id: "en_US-ryan-high",
        label: "Ryan (American English, male, high quality)",
    },
];

/// The speech-dispatcher module riteui adds.
pub const MODULE: &str = "kanrite-piper";
const BEGIN: &str =
    "# kanrite: Piper voice (begin). Preferences → Read aloud → Remove takes it out.";
const END: &str = "# kanrite: Piper voice (end)";
/// The first line of a `speechd.conf` riteui made (from the system's).
const CREATED: &str = "# Made by kanrite from the system's speechd.conf, to add a Piper voice.";

/// The same, as made before 5.0 (ticket 310): still recognised, removed and
/// renamed by [`adopt_new_name`].
const OLD_MODULE: &str = "agile-md-piper";
const OLD_BEGIN: &str =
    "# agile-md: Piper voice (begin). Preferences → Read aloud → Remove takes it out.";
const OLD_END: &str = "# agile-md: Piper voice (end)";
const OLD_CREATED: &str =
    "# Made by agile-md from the system's speechd.conf, to add a Piper voice.";

pub fn voice(id: &str) -> Option<Voice> {
    VOICES.iter().copied().find(|v| v.id == id)
}

/// How Piper gets installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Installer {
    Uv,
    Venv,
}

impl Installer {
    /// `uv` if it's on the PATH, else Python 3 with `venv` and `ensurepip`
    /// (Ubuntu splits those into `python3-venv`).
    pub fn find() -> Option<Installer> {
        if on_path("uv") {
            return Some(Installer::Uv);
        }
        let ok = Command::new("python3")
            .args(["-c", "import venv, ensurepip"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        ok.then_some(Installer::Venv)
    }

    pub fn name(&self) -> &'static str {
        match self {
            Installer::Uv => "uv",
            Installer::Venv => "Python's venv",
        }
    }
}

/// Where Piper, its environment and its voices live.
pub fn root() -> PathBuf {
    super::model::speech_dir().join("piper")
}

fn venv(root: &Path) -> PathBuf {
    root.join("venv")
}

fn voices_dir(root: &Path) -> PathBuf {
    root.join("voices")
}

fn python(root: &Path) -> PathBuf {
    venv(root).join("bin").join("python")
}

fn piper_program(root: &Path) -> PathBuf {
    venv(root).join("bin").join("piper")
}

/// speech-dispatcher's per-user folder.
fn speechd_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| rite_lib::home::dir().map(|h| h.join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("speech-dispatcher")
}

fn module_file(dir: &Path) -> PathBuf {
    dir.join("modules").join(format!("{MODULE}.conf"))
}

fn old_module_file(dir: &Path) -> PathBuf {
    dir.join("modules").join(format!("{OLD_MODULE}.conf"))
}

const SYSTEM_CONF: &str = "/etc/speech-dispatcher/speechd.conf";

/// Can this machine have it: Linux with speech-dispatcher.
pub fn supported() -> bool {
    cfg!(target_os = "linux") && on_path("spd-say")
}

/// The voice set up now, if Piper is set up.
pub fn installed() -> Option<String> {
    installed_in(&speechd_dir(), &root())
}

fn installed_in(speechd: &Path, root: &Path) -> Option<String> {
    let conf = fs::read_to_string(module_file(speechd))
        .or_else(|_| fs::read_to_string(old_module_file(speechd)))
        .ok()?;
    if !piper_program(root).exists() {
        return None;
    }
    conf.lines()
        .find_map(|l| l.strip_prefix("DefaultVoice "))
        .map(|v| v.trim().trim_matches('"').to_string())
}

/// What's happening, for Preferences.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Installing,
    Voice,
    Configuring,
}

/// Sets Piper up with `voice`, or switches to it: installs Piper unless it's
/// there, fetches the voice (dropping any other), points speech-dispatcher
/// at it and restarts it.
pub fn install(voice: Voice, stage: &mut dyn FnMut(Stage)) -> Result<()> {
    if !supported() {
        bail!("a Piper voice needs Linux with speech-dispatcher (spd-say)");
    }
    let root = root();
    if !piper_program(&root).exists() {
        let installer = Installer::find().ok_or_else(|| {
            anyhow::anyhow!(
                "installing Piper needs uv or Python 3 with venv (on Ubuntu: sudo apt install python3-venv)"
            )
        })?;
        stage(Stage::Installing);
        install_piper(&root, &installer)?;
    }
    stage(Stage::Voice);
    let model = fetch_voice(&root, voice)?;
    stage(Stage::Configuring);
    let rate = sample_rate(&model)?;
    let player = Player::find().ok_or_else(|| {
        anyhow::anyhow!("no audio player found: install alsa-utils (aplay) or pipewire (pw-play)")
    })?;
    configure(
        &speechd_dir(),
        &piper_program(&root),
        &model,
        voice,
        rate,
        player,
    )?;
    restart_speech_dispatcher();
    Ok(())
}

/// Takes it all away again: the module, the `speechd.conf` block (or the
/// whole file when riteui made it and nothing else changed), Piper and its
/// voices.
pub fn remove() -> Result<()> {
    unconfigure(&speechd_dir())?;
    restart_speech_dispatcher();
    let root = root();
    if root.exists() {
        fs::remove_dir_all(&root).with_context(|| format!("removing {}", root.display()))?;
    }
    let speech = super::model::speech_dir();
    if fs::remove_dir(&speech).is_ok()
        && let Some(parent) = speech.parent()
    {
        let _ = fs::remove_dir(parent);
    }
    Ok(())
}

fn run(command: &mut Command, what: &str) -> Result<()> {
    let out = command
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("{what}: couldn't start {:?}", command.get_program()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().rev().take(4).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        bail!("{what} failed: {}", tail.join(" / "));
    }
    Ok(())
}

fn install_piper(root: &Path, installer: &Installer) -> Result<()> {
    fs::create_dir_all(root).with_context(|| format!("making {}", root.display()))?;
    let venv = venv(root);
    // A half-made environment from a failed attempt starts again.
    if venv.exists() {
        fs::remove_dir_all(&venv).with_context(|| format!("removing {}", venv.display()))?;
    }
    for (program, args) in install_commands(installer, &venv) {
        run(Command::new(&program).args(&args), "installing Piper")?;
    }
    if !piper_program(root).exists() {
        bail!("installing Piper finished, but there's no piper program in it");
    }
    Ok(())
}

/// The commands that make the environment and install `piper-tts` into it.
fn install_commands(installer: &Installer, venv: &Path) -> Vec<(String, Vec<String>)> {
    let venv_s = venv.to_string_lossy().to_string();
    let python = venv
        .join("bin")
        .join("python")
        .to_string_lossy()
        .to_string();
    match installer {
        Installer::Uv => vec![
            ("uv".into(), vec!["venv".into(), "--quiet".into(), venv_s]),
            (
                "uv".into(),
                vec![
                    "pip".into(),
                    "install".into(),
                    "--quiet".into(),
                    "--python".into(),
                    python,
                    "piper-tts".into(),
                ],
            ),
        ],
        Installer::Venv => vec![
            ("python3".into(), vec!["-m".into(), "venv".into(), venv_s]),
            (
                python,
                vec![
                    "-m".into(),
                    "pip".into(),
                    "install".into(),
                    "--quiet".into(),
                    "--disable-pip-version-check".into(),
                    "piper-tts".into(),
                ],
            ),
        ],
    }
}

/// Downloads `voice` with Piper's own downloader, leaving only it.
fn fetch_voice(root: &Path, voice: Voice) -> Result<PathBuf> {
    let dir = voices_dir(root);
    fs::create_dir_all(&dir).with_context(|| format!("making {}", dir.display()))?;
    let model = dir.join(format!("{}.onnx", voice.id));
    let config = dir.join(format!("{}.onnx.json", voice.id));
    if !(model.exists() && config.exists()) {
        run(
            Command::new(python(root))
                .args(["-m", "piper.download_voices", "--download-dir"])
                .arg(&dir)
                .arg(voice.id),
            "downloading the voice",
        )?;
    }
    if !model.exists() {
        bail!("the voice {} didn't download", voice.id);
    }
    // Only the voice in use is kept.
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with(&format!("{}.", voice.id)) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    Ok(model)
}

/// The voice's sample rate, from its `.onnx.json`.
fn sample_rate(model: &Path) -> Result<u32> {
    let path = PathBuf::from(format!("{}.json", model.display()));
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let json: serde_json::Value =
        serde_json::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
    json.pointer("/audio/sample_rate")
        .and_then(serde_json::Value::as_u64)
        .map(|r| r as u32)
        .ok_or_else(|| anyhow::anyhow!("{} has no sample rate", path.display()))
}

/// What plays Piper's raw audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Player {
    Aplay,
    PwPlay,
}

impl Player {
    fn find() -> Option<Player> {
        if on_path("aplay") {
            Some(Player::Aplay)
        } else if on_path("pw-play") {
            Some(Player::PwPlay)
        } else {
            None
        }
    }

    fn command(self, rate: u32) -> String {
        match self {
            Player::Aplay => format!("aplay -q -r {rate} -f S16_LE -t raw -c 1 -"),
            Player::PwPlay => format!("pw-play --rate {rate} --format s16 --channels 1 -"),
        }
    }
}

/// The module's configuration for speech-dispatcher's generic module.
fn module_conf(piper: &Path, model: &Path, voice: Voice, rate: u32, player: Player) -> String {
    format!(
        "# kanrite: Piper for speech-dispatcher. Made by riteui; Preferences →\n\
         # Read aloud → Remove takes it away.\n\
         GenericExecuteSynth \"printf %s \\'$DATA\\' | {piper} --model {model} --output-raw 2>/dev/null | {play}\"\n\
         GenericCmdDependency \"{program}\"\n\
         GenericLanguage \"en\" \"en\" \"utf-8\"\n\
         AddVoice \"en\" \"FEMALE1\" \"{id}\"\n\
         DefaultVoice \"{id}\"\n",
        piper = shell_quote(&piper.to_string_lossy()),
        model = shell_quote(&model.to_string_lossy()),
        play = player.command(rate),
        program = match player {
            Player::Aplay => "aplay",
            Player::PwPlay => "pw-play",
        },
        id = voice.id,
    )
}

/// A path for the shell speech-dispatcher runs, inside its double-quoted
/// setting: single quotes, escaped the way its other settings escape them.
fn shell_quote(path: &str) -> String {
    format!("\\'{}\\'", path.replace('\'', ""))
}

/// `conf` with riteui's block (replacing any earlier one) at the end.
fn with_block(conf: &str) -> String {
    let mut out = without_block(conf);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!(
        "{BEGIN}\nAddModule \"{MODULE}\" \"sd_generic\" \"{MODULE}.conf\"\nDefaultModule {MODULE}\n{END}\n"
    ));
    out
}

/// `conf` without riteui's block.
fn without_block(conf: &str) -> String {
    let mut out = String::with_capacity(conf.len());
    let mut inside = false;
    for line in conf.split_inclusive('\n') {
        let bare = line.trim_end();
        if bare == BEGIN || bare == OLD_BEGIN {
            inside = true;
            continue;
        }
        if inside {
            if bare == END || bare == OLD_END {
                inside = false;
            }
            continue;
        }
        out.push_str(line);
    }
    out
}

fn configure(
    dir: &Path,
    piper: &Path,
    model: &Path,
    voice: Voice,
    rate: u32,
    player: Player,
) -> Result<()> {
    let module = module_file(dir);
    if let Some(parent) = module.parent() {
        fs::create_dir_all(parent).with_context(|| format!("making {}", parent.display()))?;
    }
    fs::write(&module, module_conf(piper, model, voice, rate, player))
        .with_context(|| format!("writing {}", module.display()))?;
    let conf_path = dir.join("speechd.conf");
    let conf = match fs::read_to_string(&conf_path) {
        Ok(text) => text,
        // Without a file of its own speech-dispatcher reads the system's;
        // a file here replaces it, so it starts as a copy.
        Err(_) => {
            let system = fs::read_to_string(SYSTEM_CONF).unwrap_or_default();
            format!("{CREATED}\n{system}")
        }
    };
    fs::write(&conf_path, with_block(&conf))
        .with_context(|| format!("writing {}", conf_path.display()))
}

fn unconfigure(dir: &Path) -> Result<()> {
    for module in [module_file(dir), old_module_file(dir)] {
        if module.exists() {
            fs::remove_file(&module).with_context(|| format!("removing {}", module.display()))?;
        }
    }
    let conf_path = dir.join("speechd.conf");
    let Ok(conf) = fs::read_to_string(&conf_path) else {
        return Ok(());
    };
    let rest = without_block(&conf);
    let system = fs::read_to_string(SYSTEM_CONF).unwrap_or_default();
    if rest == format!("{CREATED}\n{system}") || rest == format!("{OLD_CREATED}\n{system}") {
        fs::remove_file(&conf_path).with_context(|| format!("removing {}", conf_path.display()))?;
    } else if rest != conf {
        fs::write(&conf_path, rest).with_context(|| format!("writing {}", conf_path.display()))?;
    }
    // Empty folders riteui made go too.
    let _ = fs::remove_dir(dir.join("modules"));
    let _ = fs::remove_dir(dir);
    Ok(())
}

/// Renames a Piper setup made before 5.0 (ticket 310): the module file
/// becomes `kanrite-piper.conf`, pointing into the data folder by its new
/// name, and `speechd.conf`'s block names the new module. Nothing for any
/// other setup. Returns whether it renamed one, and restarts
/// speech-dispatcher then so the next `spd-say` reads it.
pub fn adopt_new_name() -> bool {
    let renamed = adopt_new_name_in(&speechd_dir()).unwrap_or(false);
    if renamed {
        restart_speech_dispatcher();
    }
    renamed
}

fn adopt_new_name_in(dir: &Path) -> Result<bool> {
    let old = old_module_file(dir);
    let new = module_file(dir);
    let Ok(module) = fs::read_to_string(&old) else {
        return Ok(false);
    };
    if new.exists() {
        return Ok(false);
    }
    let module = module
        .replace(
            &format!("/{}/", rite_lib::places::OLD_NAME),
            &format!("/{}/", rite_lib::places::NAME),
        )
        .replacen("# agile-md: Piper", "# kanrite: Piper", 1);
    fs::write(&new, module).with_context(|| format!("writing {}", new.display()))?;
    let conf_path = dir.join("speechd.conf");
    if let Ok(conf) = fs::read_to_string(&conf_path) {
        let conf = match conf.strip_prefix(OLD_CREATED) {
            Some(rest) => format!("{CREATED}{rest}"),
            None => conf,
        };
        fs::write(&conf_path, with_block(&conf))
            .with_context(|| format!("writing {}", conf_path.display()))?;
    }
    fs::remove_file(&old).with_context(|| format!("removing {}", old.display()))?;
    Ok(true)
}

/// Ends this user's speech-dispatcher so the next `spd-say` starts it with
/// the new settings (it starts itself on demand).
fn restart_speech_dispatcher() {
    // `pkill -x` matches the kernel's 15-character name.
    let uid = Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if uid.is_empty() {
        return;
    }
    let _ = Command::new("pkill")
        .args(["-x", "-u", &uid, "speech-dispatch"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    std::thread::sleep(std::time::Duration::from_millis(300));
}

// Piper is offered on Linux only (`available`), and its paths are a Linux
// virtual environment's (`venv/bin/python`); not run elsewhere (ticket 280).
#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn the_block_is_added_once_and_taken_out_cleanly() {
        let system = "# system\nAddModule \"x\" \"sd_x\" \"x.conf\"\n";
        let once = with_block(system);
        assert!(once.starts_with(system));
        assert!(once.contains("DefaultModule kanrite-piper"));
        assert_eq!(with_block(&once), once, "added twice is still once");
        assert_eq!(without_block(&once), system);
        // A file without a final newline still gets the block on its own line.
        assert!(with_block("a").starts_with("a\n# kanrite"));
    }

    #[test]
    fn configure_then_unconfigure_leaves_nothing_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("speech-dispatcher");
        let voice = VOICES[0];
        configure(
            &dir,
            Path::new("/data/piper"),
            Path::new("/data/v.onnx"),
            voice,
            22050,
            Player::Aplay,
        )
        .unwrap();
        let conf = fs::read_to_string(dir.join("speechd.conf")).unwrap();
        assert!(conf.starts_with(CREATED));
        assert!(conf.contains("DefaultModule kanrite-piper"));
        let module = fs::read_to_string(module_file(&dir)).unwrap();
        assert!(module.contains("--output-raw"));
        assert!(module.contains("aplay -q -r 22050"));
        assert!(module.contains(&format!("DefaultVoice \"{}\"", voice.id)));

        unconfigure(&dir).unwrap();
        assert!(!dir.exists(), "made by riteui, so all of it goes");
    }

    #[test]
    fn a_setup_from_before_5_0_is_renamed_and_still_removed_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("speech-dispatcher");
        fs::create_dir_all(dir.join("modules")).unwrap();
        // What riteui 4.x wrote.
        fs::write(
            old_module_file(&dir),
            "# agile-md: Piper for speech-dispatcher.\nGenericExecuteSynth \"/h/.local/share/agile-md/speech/piper/venv/bin/piper\"\nDefaultVoice \"en_GB-alba-medium\"\n",
        )
        .unwrap();
        let system = fs::read_to_string(SYSTEM_CONF).unwrap_or_default();
        fs::write(
            dir.join("speechd.conf"),
            format!("{OLD_CREATED}\n{system}{OLD_BEGIN}\nAddModule \"{OLD_MODULE}\" \"sd_generic\" \"{OLD_MODULE}.conf\"\nDefaultModule {OLD_MODULE}\n{OLD_END}\n"),
        )
        .unwrap();
        assert!(adopt_new_name_in(&dir).unwrap());
        assert!(!old_module_file(&dir).exists());
        let module = fs::read_to_string(module_file(&dir)).unwrap();
        assert!(module.contains("/share/kanrite/speech/piper/"), "{module}");
        let conf = fs::read_to_string(dir.join("speechd.conf")).unwrap();
        assert!(conf.starts_with(CREATED), "{conf}");
        assert!(
            conf.contains("DefaultModule kanrite-piper") && !conf.contains(OLD_MODULE),
            "{conf}"
        );
        // Once only.
        assert!(!adopt_new_name_in(&dir).unwrap());
        // Remove still takes all of it away.
        unconfigure(&dir).unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn a_users_own_speechd_conf_keeps_their_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("speech-dispatcher");
        fs::create_dir_all(&dir).unwrap();
        let mine = "# mine\nDefaultRate 20\n";
        fs::write(dir.join("speechd.conf"), mine).unwrap();
        configure(
            &dir,
            Path::new("/p"),
            Path::new("/v.onnx"),
            VOICES[3],
            16000,
            Player::PwPlay,
        )
        .unwrap();
        assert!(
            fs::read_to_string(dir.join("speechd.conf"))
                .unwrap()
                .starts_with(mine)
        );
        unconfigure(&dir).unwrap();
        assert_eq!(fs::read_to_string(dir.join("speechd.conf")).unwrap(), mine);
    }

    #[test]
    fn the_installed_voice_is_read_from_the_module() {
        let tmp = tempfile::tempdir().unwrap();
        let (speechd, root) = (tmp.path().join("sd"), tmp.path().join("piper"));
        assert_eq!(installed_in(&speechd, &root), None);
        let program = piper_program(&root);
        fs::create_dir_all(program.parent().unwrap()).unwrap();
        fs::write(&program, "").unwrap();
        configure(
            &speechd,
            &program,
            Path::new("/v.onnx"),
            VOICES[1],
            22050,
            Player::Aplay,
        )
        .unwrap();
        assert_eq!(installed_in(&speechd, &root).as_deref(), Some(VOICES[1].id));
    }

    #[test]
    fn paths_are_quoted_for_speech_dispatchers_shell() {
        assert_eq!(shell_quote("/a b/piper"), "\\'/a b/piper\\'");
        let conf = module_conf(
            Path::new("/a b/piper"),
            Path::new("/v.onnx"),
            VOICES[0],
            22050,
            Player::Aplay,
        );
        assert!(
            conf.contains("| \\'/a b/piper\\' --model \\'/v.onnx\\'"),
            "{conf}"
        );
    }

    #[test]
    fn install_commands_use_the_environment_they_make() {
        let venv = Path::new("/d/venv");
        let uv = install_commands(&Installer::Uv, venv);
        assert_eq!(uv[0].0, "uv");
        assert!(uv[1].1.contains(&"/d/venv/bin/python".to_string()));
        let py = install_commands(&Installer::Venv, venv);
        assert_eq!(
            py[0],
            (
                "python3".into(),
                vec!["-m".into(), "venv".into(), "/d/venv".into()]
            )
        );
        assert_eq!(py[1].0, "/d/venv/bin/python");
        assert!(py[1].1.ends_with(&["piper-tts".to_string()]));
    }

    #[test]
    fn the_sample_rate_comes_from_the_voices_json() {
        let tmp = tempfile::tempdir().unwrap();
        let model = tmp.path().join("v.onnx");
        fs::write(
            tmp.path().join("v.onnx.json"),
            r#"{"audio":{"sample_rate":16000}}"#,
        )
        .unwrap();
        assert_eq!(sample_rate(&model).unwrap(), 16000);
    }

    /// The whole set-up for real, into temporary folders: installs Piper
    /// (network), fetches a voice, checks Piper speaks into the configured
    /// pipeline's first half, then removes it all. `cargo test -p rite-ui
    /// --lib piper -- --ignored`.
    #[test]
    #[ignore = "downloads Piper and a voice"]
    fn install_for_real_then_remove() {
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: an ignored test, run on its own.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path().join("data"));
            std::env::set_var("XDG_CONFIG_HOME", tmp.path().join("config"));
        }
        let mut stages = Vec::new();
        install(VOICES[3], &mut |s| stages.push(s)).unwrap();
        assert_eq!(
            stages,
            [Stage::Installing, Stage::Voice, Stage::Configuring]
        );
        assert_eq!(installed().as_deref(), Some(VOICES[3].id));
        let root = root();
        let model = voices_dir(&root).join(format!("{}.onnx", VOICES[3].id));
        let out = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "printf %s 'Hello there.' | '{}' --model '{}' --output-raw 2>/dev/null | wc -c",
                piper_program(&root).display(),
                model.display()
            ))
            .output()
            .unwrap();
        let bytes: usize = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        assert!(bytes > 10_000, "Piper made {bytes} bytes of audio");

        // Switching voice keeps Piper and swaps the voice.
        install(VOICES[0], &mut |_| {}).unwrap();
        assert_eq!(installed().as_deref(), Some(VOICES[0].id));
        assert!(!model.exists(), "the old voice is gone");

        remove().unwrap();
        assert_eq!(installed(), None);
        assert!(!root.exists());
        assert!(!speechd_dir().exists());
    }

    /// Writes the configuration for an existing Piper into
    /// `$RITE_PIPER_CONF_DIR`, to try it with a speech-dispatcher of its own.
    #[test]
    #[ignore = "a manual check with a real speech-dispatcher"]
    fn write_conf_for_a_manual_check() {
        let (Ok(dir), Ok(piper), Ok(model)) = (
            rite_lib::renamed::var("RITE_PIPER_CONF_DIR"),
            rite_lib::renamed::var("RITE_PIPER_BIN"),
            rite_lib::renamed::var("RITE_PIPER_MODEL"),
        ) else {
            return;
        };
        let rate = sample_rate(Path::new(&model)).unwrap();
        configure(
            Path::new(&dir),
            Path::new(&piper),
            Path::new(&model),
            VOICES[0],
            rate,
            Player::Aplay,
        )
        .unwrap();
    }
}
