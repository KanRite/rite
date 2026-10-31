//! Which speech models there are, where they live, and fetching one
//! (ticket 187). Nothing is downloaded unless the user asks, in
//! Preferences; removing a model deletes its folder and leaves nothing
//! behind.

use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

/// A Whisper model riteui can use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    /// `tiny.en`, quantised to 8 bits (candle's own build): 42 MB.
    TinyEn,
    /// `base.en`, OpenAI's full-precision weights: 290 MB.
    BaseEn,
}

/// One file of a model: where it comes from, and its name here.
pub struct File {
    pub url: &'static str,
    pub name: &'static str,
}

impl Model {
    pub const ALL: [Model; 2] = [Model::TinyEn, Model::BaseEn];

    pub fn id(self) -> &'static str {
        match self {
            Model::TinyEn => "tiny.en",
            Model::BaseEn => "base.en",
        }
    }

    pub fn from_id(id: &str) -> Option<Model> {
        Model::ALL.into_iter().find(|m| m.id() == id)
    }

    /// Roughly how much it downloads, for Preferences to say.
    pub fn size_mb(self) -> u32 {
        match self {
            Model::TinyEn => 42,
            Model::BaseEn => 290,
        }
    }

    pub fn quantized(self) -> bool {
        matches!(self, Model::TinyEn)
    }

    pub fn files(self) -> [File; 3] {
        match self {
            Model::TinyEn => [
                File {
                    url: "https://huggingface.co/lmz/candle-whisper/resolve/main/model-tiny-en-q80.gguf",
                    name: "model.gguf",
                },
                File {
                    url: "https://huggingface.co/lmz/candle-whisper/resolve/main/config-tiny-en.json",
                    name: "config.json",
                },
                File {
                    url: "https://huggingface.co/lmz/candle-whisper/resolve/main/tokenizer-tiny-en.json",
                    name: "tokenizer.json",
                },
            ],
            Model::BaseEn => [
                File {
                    url: "https://huggingface.co/openai/whisper-base.en/resolve/main/model.safetensors",
                    name: "model.safetensors",
                },
                File {
                    url: "https://huggingface.co/openai/whisper-base.en/resolve/main/config.json",
                    name: "config.json",
                },
                File {
                    url: "https://huggingface.co/openai/whisper-base.en/resolve/main/tokenizer.json",
                    name: "tokenizer.json",
                },
            ],
        }
    }

    /// `$XDG_DATA_HOME/kanrite/speech/<id>`.
    pub fn dir(self) -> PathBuf {
        speech_dir().join(self.id())
    }

    /// All its files are here (a half-finished download isn't).
    pub fn installed(self) -> bool {
        self.files()
            .iter()
            .all(|f| self.dir().join(f.name).is_file())
    }

    /// Fetches every file, each into a `.part` that's renamed only once it's
    /// complete, so a failed or interrupted download leaves nothing that
    /// looks installed. `progress(done, total)` counts bytes across all the
    /// files, `total` growing as each file's size becomes known.
    pub fn download(self, mut progress: impl FnMut(u64, u64)) -> Result<()> {
        let dir = self.dir();
        fs::create_dir_all(&dir).with_context(|| format!("making {}", dir.display()))?;
        let mut done = 0u64;
        let mut total = 0u64;
        for file in self.files() {
            let target = dir.join(file.name);
            if target.is_file() {
                continue;
            }
            let part = dir.join(format!("{}.part", file.name));
            let response = ureq::get(file.url)
                .call()
                .with_context(|| format!("downloading {}", file.url))?;
            let length: u64 = response
                .headers()
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            total += length;
            let mut reader = response.into_body().into_reader();
            let mut out =
                fs::File::create(&part).with_context(|| format!("writing {}", part.display()))?;
            let mut buf = vec![0u8; 256 * 1024];
            let mut got = 0u64;
            loop {
                let n = reader
                    .read(&mut buf)
                    .with_context(|| format!("downloading {}", file.url))?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n])?;
                got += n as u64;
                progress(done + got, total.max(done + got));
            }
            out.flush()?;
            drop(out);
            if length > 0 && got != length {
                let _ = fs::remove_file(&part);
                bail!("{} stopped at {got} of {length} bytes", file.url);
            }
            fs::rename(&part, &target)?;
            done += got;
        }
        Ok(())
    }

    /// Deletes the model's folder.
    pub fn remove(self) -> Result<()> {
        let dir = self.dir();
        if dir.exists() {
            fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
        }
        // The speech folder goes too once it's empty, and KanRite's own
        // data folder if speech was all it held: nothing left behind.
        let speech = speech_dir();
        if fs::remove_dir(&speech).is_ok()
            && let Some(parent) = speech.parent()
        {
            let _ = fs::remove_dir(parent);
        }
        Ok(())
    }
}

pub(crate) fn speech_dir() -> PathBuf {
    rite_lib::places::data()
        .unwrap_or_else(|| PathBuf::from(".local/share").join(rite_lib::places::NAME))
        .join("speech")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_and_each_model_has_its_three_files() {
        for model in Model::ALL {
            assert_eq!(Model::from_id(model.id()), Some(model));
            let names: Vec<&str> = model.files().iter().map(|f| f.name).collect();
            assert!(names.contains(&"config.json") && names.contains(&"tokenizer.json"));
            // In the data folder, kanrite or (until it's moved) agile-md.
            assert!(model.dir().ends_with(format!("speech/{}", model.id())));
        }
        assert_eq!(Model::from_id("large"), None);
        assert!(Model::TinyEn.quantized() && !Model::BaseEn.quantized());
    }
}
