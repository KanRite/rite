//! Turning 16 kHz speech into text with Whisper on candle (ticket 187), all
//! on this machine. Follows candle's own Whisper example: log-mel
//! spectrogram, the encoder once, then the decoder greedily one token at a
//! time. Tokens become text through `tokenizer.json`'s vocabulary and
//! GPT-2's byte mapping, so no tokenizer library is needed.

use std::collections::HashMap;

use anyhow::{Context, Result, anyhow, bail};
use candle_core::{D, Device, IndexOp, Tensor};
use candle_transformers::models::whisper::{self as w, Config};

use super::dsp;
use super::model::Model;

enum Net {
    Full(w::model::Whisper),
    Quantized(w::quantized_model::Whisper),
}

impl Net {
    fn encode(&mut self, mel: &Tensor) -> candle_core::Result<Tensor> {
        match self {
            Net::Full(m) => m.encoder.forward(mel, true),
            Net::Quantized(m) => m.encoder.forward(mel, true),
        }
    }

    fn decode(
        &mut self,
        tokens: &Tensor,
        audio: &Tensor,
        flush: bool,
    ) -> candle_core::Result<Tensor> {
        match self {
            Net::Full(m) => m.decoder.forward(tokens, audio, flush),
            Net::Quantized(m) => m.decoder.forward(tokens, audio, flush),
        }
    }

    fn logits(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        match self {
            Net::Full(m) => m.decoder.final_linear(x),
            Net::Quantized(m) => m.decoder.final_linear(x),
        }
    }
}

/// A loaded model, ready to transcribe.
pub struct Engine {
    pub model: Model,
    net: Net,
    config: Config,
    filters: Vec<f32>,
    words: Words,
    /// Added to the logits: minus infinity for tokens never to produce.
    suppress: Tensor,
    sot: u32,
    eot: u32,
    /// `<|nospeech|>` (or `<|nocaptions|>`): how sure Whisper is there's no
    /// speech at all.
    no_speech: Option<u32>,
    no_timestamps: u32,
    device: Device,
}

impl Engine {
    /// Loads an installed model (a few hundred milliseconds).
    pub fn load(model: Model) -> Result<Engine> {
        let dir = model.dir();
        let device = Device::Cpu;
        let config: Config = serde_json::from_str(
            &std::fs::read_to_string(dir.join("config.json"))
                .context("reading the model's config")?,
        )
        .context("reading the model's config")?;
        let words = Words::load(
            &std::fs::read_to_string(dir.join("tokenizer.json"))
                .context("reading the model's tokenizer")?,
        )?;
        let net = if model.quantized() {
            let vb = candle_transformers::quantized_var_builder::VarBuilder::from_gguf(
                dir.join("model.gguf"),
                &device,
            )?;
            Net::Quantized(w::quantized_model::Whisper::load(&vb, config.clone())?)
        } else {
            // Safety: the file is ours, and nothing writes it while it's mapped.
            let vb = unsafe {
                candle_nn::VarBuilder::from_mmaped_safetensors(
                    &[dir.join("model.safetensors")],
                    w::DTYPE,
                    &device,
                )?
            };
            Net::Full(w::model::Whisper::load(&vb, config.clone())?)
        };
        let special = |name: &str| {
            words
                .special(name)
                .ok_or_else(|| anyhow!("the tokenizer has no {name}"))
        };
        let sot = special(w::SOT_TOKEN)?;
        let eot = special(w::EOT_TOKEN)?;
        let no_timestamps = special(w::NO_TIMESTAMPS_TOKEN)?;
        let no_speech = w::NO_SPEECH_TOKENS.iter().find_map(|t| words.special(t));
        // The tokens Whisper's config says never to produce, and with no
        // timestamps asked for, the timestamp tokens after <|notimestamps|>.
        let suppress: Vec<f32> = (0..config.vocab_size as u32)
            .map(|t| {
                if config.suppress_tokens.contains(&t) || t > no_timestamps {
                    f32::NEG_INFINITY
                } else {
                    0.0
                }
            })
            .collect();
        let suppress = Tensor::new(suppress.as_slice(), &device)?;
        Ok(Engine {
            model,
            net,
            filters: dsp::mel_filters(config.num_mel_bins, w::N_FFT),
            config,
            words,
            suppress,
            sot,
            eot,
            no_speech,
            no_timestamps,
            device,
        })
    }

    /// The words in `pcm` (16 kHz mono, at most 30 seconds).
    pub fn transcribe(&mut self, pcm: &[f32]) -> Result<String> {
        if pcm.is_empty() {
            return Ok(String::new());
        }
        let pcm = &pcm[..pcm.len().min(w::N_SAMPLES)];
        let mel = w::audio::pcm_to_mel(&self.config, pcm, &self.filters);
        let frames = mel.len() / self.config.num_mel_bins;
        let mel = Tensor::from_vec(mel, (1, self.config.num_mel_bins, frames), &self.device)?;
        let mel = mel.narrow(2, 0, frames.min(w::N_FRAMES))?;
        let audio = self.net.encode(&mel)?;
        // English-only models start with just these two (no language or task).
        let mut tokens = vec![self.sot, self.no_timestamps];
        let first = tokens.len();
        let limit = self.config.max_target_positions / 2;
        for i in 0..limit {
            let input = Tensor::new(tokens.as_slice(), &self.device)?.unsqueeze(0)?;
            let ys = self.net.decode(&input, &audio, i == 0)?;
            let (_, seq, _) = ys.dims3()?;
            // Whisper's own "no speech" judgement, read at the first step
            // from the start-of-transcript position, as OpenAI's code does:
            // silence or room noise gives nothing rather than a guess
            // like "you".
            if i == 0
                && let Some(no_speech) = self.no_speech
            {
                let first = self.net.logits(&ys.i((..1, 0..1))?)?.i(0)?.i(0)?;
                let probs = candle_nn::ops::softmax_last_dim(&first)?;
                let p = probs.i(no_speech as usize)?.to_scalar::<f32>()?;
                if f64::from(p) > w::NO_SPEECH_THRESHOLD {
                    return Ok(String::new());
                }
            }
            let logits = self.net.logits(&ys.i((..1, seq - 1..))?)?.i(0)?.i(0)?;
            let logits = logits.broadcast_add(&self.suppress)?;
            let next = logits.argmax(D::Minus1)?.to_scalar::<u32>()?;
            if next == self.eot {
                break;
            }
            tokens.push(next);
        }
        Ok(self.words.decode(&tokens[first..]).trim().to_string())
    }
}

/// The tokenizer's vocabulary: each token's text in GPT-2's byte-to-letter
/// form, and the special tokens by name.
struct Words {
    by_id: HashMap<u32, String>,
    special: HashMap<String, u32>,
}

impl Words {
    fn load(json: &str) -> Result<Words> {
        let doc: serde_json::Value =
            serde_json::from_str(json).context("reading the model's tokenizer")?;
        let mut by_id = HashMap::new();
        let vocab = doc
            .pointer("/model/vocab")
            .and_then(|v| v.as_object())
            .ok_or_else(|| anyhow!("the tokenizer has no vocabulary"))?;
        for (text, id) in vocab {
            if let Some(id) = id.as_u64() {
                by_id.insert(id as u32, text.clone());
            }
        }
        let mut special = HashMap::new();
        for token in doc
            .get("added_tokens")
            .and_then(|t| t.as_array())
            .into_iter()
            .flatten()
        {
            if let (Some(text), Some(id)) = (
                token.get("content").and_then(|c| c.as_str()),
                token.get("id").and_then(|i| i.as_u64()),
            ) {
                special.insert(text.to_string(), id as u32);
                by_id.entry(id as u32).or_insert_with(|| text.to_string());
            }
        }
        if by_id.is_empty() {
            bail!("the tokenizer's vocabulary is empty");
        }
        Ok(Words { by_id, special })
    }

    fn special(&self, name: &str) -> Option<u32> {
        self.special.get(name).copied()
    }

    /// Tokens to text: special tokens dropped, GPT-2's letters back to
    /// bytes, bytes to UTF-8.
    fn decode(&self, tokens: &[u32]) -> String {
        let unicode_to_byte = byte_decoder();
        let mut bytes = Vec::new();
        for t in tokens {
            let Some(text) = self.by_id.get(t) else {
                continue;
            };
            if self.special.contains_key(text) {
                continue;
            }
            for c in text.chars() {
                if let Some(b) = unicode_to_byte.get(&c) {
                    bytes.push(*b);
                }
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// GPT-2's map from the printable letters its vocabulary uses back to the
/// bytes they stand for.
fn byte_decoder() -> HashMap<char, u8> {
    let mut printable: Vec<u32> = (u32::from(b'!')..=u32::from(b'~'))
        .chain(0xA1..=0xAC)
        .chain(0xAE..=0xFF)
        .collect();
    let mut letters: Vec<u32> = printable.clone();
    let mut extra = 0;
    for b in 0..=255u32 {
        if !printable.contains(&b) {
            printable.push(b);
            letters.push(256 + extra);
            extra += 1;
        }
    }
    printable
        .into_iter()
        .zip(letters)
        .filter_map(|(b, c)| Some((char::from_u32(c)?, b as u8)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words() -> Words {
        Words::load(
            r#"{"model":{"vocab":{"Ġhello":0,"Ġworld":1,"!":2,"Ã©":3}},
               "added_tokens":[{"id":50256,"content":"<|endoftext|>"},{"id":50257,"content":"<|startoftranscript|>"}]}"#,
        )
        .unwrap()
    }

    #[test]
    fn tokens_decode_through_gpt2s_byte_letters() {
        let w = words();
        assert_eq!(w.decode(&[0, 1, 2]), " hello world!");
        assert_eq!(
            w.decode(&[50257, 0, 50256]),
            " hello",
            "special tokens dropped"
        );
        assert_eq!(
            w.decode(&[3]),
            "é",
            "two byte-letters make one UTF-8 character"
        );
        assert_eq!(w.special("<|endoftext|>"), Some(50256));
    }

    #[test]
    fn the_byte_map_covers_every_byte_once() {
        let map = byte_decoder();
        assert_eq!(map.len(), 256);
        assert_eq!(map[&'Ġ'], b' ');
        assert_eq!(map[&'A'], b'A');
    }

    /// Needs tiny.en installed and `RITE_SPEECH_WAV` naming a mono 16-bit WAV
    /// of someone saying the words in `RITE_SPEECH_EXPECT`, e.g. ALSA's own
    /// `/usr/share/sounds/alsa/Front_Center.wav` and "front center".
    #[test]
    #[ignore]
    fn transcribes_a_recording() {
        let (Ok(wav), Ok(expect)) = (
            rite_lib::renamed::var("RITE_SPEECH_WAV"),
            rite_lib::renamed::var("RITE_SPEECH_EXPECT"),
        ) else {
            return;
        };
        let bytes = std::fs::read(wav).unwrap();
        let rate = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
        // A plain 44-byte header, then 16-bit samples.
        let pcm: Vec<f32> = bytes[44..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| f32::from(i16::from_le_bytes(*b)) / 32768.0)
            .collect();
        let pcm = dsp::resample(&pcm, rate, dsp::WHISPER_RATE);
        let mut engine = Engine::load(Model::TinyEn).unwrap();
        let heard = engine.transcribe(&pcm).unwrap();
        // Words only: Whisper adds its own punctuation ("front, center.").
        let words = |t: &str| {
            t.to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| !w.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert!(words(&heard).contains(&words(&expect)), "{heard}");
    }
}
