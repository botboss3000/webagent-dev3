//! Agent voice replies: Kokoro v1.0 through sherpa-onnx. sherpa-onnx 2.0 speaks only the words in
//! its lexicon and drops the rest (numbers, names, code words), so the text is turned into
//! phonemes here with espeak-ng 1.52, the release the former Python speaker used, following the
//! same punctuation rules. One process (`webagent-v3 --voice-service speech`, started by
//! `host/voice.rs`) holds the model; both libraries are loaded at run time from the `voice` stage
//! of the Dockerfile.
//!
//! Requests arrive one JSON object per line on stdin: `{"op":"speak","id":ID,"voice":VOICE,
//! "chunks":[TEXT,...]}` (prepared by [`crate::speech`]) or `{"op":"cancel","id":ID}`. Replies are
//! JSON lines on stdout: `{"ready":true}` once the model has loaded, `{"id":ID,"audio":BASE64}`
//! with a 16-bit mono WAV for each chunk as it is synthesized, then `{"id":ID,"end":true}` or
//! `{"id":ID,"error":REASON}`. A cancelled reply stops after its current chunk and sends nothing
//! more. Nothing is stored and no network is used.
use std::collections::{HashSet, VecDeque};
use std::ffi::{c_char, c_float, c_void, CStr, CString};
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::mpsc;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use libloading::Library;
use serde_json::{json, Value};

use crate::recognizer::{library, sherpa, symbol, text as c_text, zeroed, Handle};
use crate::settings;
use crate::speech::VOICES;

/// Punctuation kept between phonemized runs of words, as the former speaker's phonemizer did.
const MARKS: &str = ";:,.!?¡¿—…\"«»“”(){}[]";
/// Phonemes Kokoro takes in one pass; longer input is split at a pause or word.
const MAX_PHONEMES: usize = 500;

/// Synthesizes one chunk of prepared text as a WAV file.
pub trait Synthesizer {
    fn speak(&mut self, text: &str, voice: &str) -> Result<Vec<u8>, String>;
}

/// Answer requests until the input closes. Requests are read on their own thread, so a cancel
/// reaches a reply that is being synthesized; other requests wait their turn.
pub fn serve<S: Synthesizer>(
    synthesizer: &mut S,
    input: impl BufRead + Send + 'static,
    mut out: impl Write,
) -> io::Result<()> {
    writeln!(out, "{}", json!({"ready": true}))?;
    out.flush()?;
    let (sender, requests) = mpsc::channel::<Value>();
    std::thread::spawn(move || {
        for line in input.lines().map_while(Result::ok) {
            if let Ok(request) = serde_json::from_str::<Value>(&line) {
                if sender.send(request).is_err() {
                    break;
                }
            }
        }
    });
    let id_of = |request: &Value| request["id"].as_str().unwrap_or_default().to_owned();
    let (mut waiting, mut cancelled) = (VecDeque::new(), HashSet::new());
    loop {
        let Some(request) = waiting.pop_front().or_else(|| requests.recv().ok()) else {
            return Ok(());
        };
        let id = id_of(&request);
        if request["op"] == "cancel" {
            cancelled.insert(id);
            continue;
        }
        if request["op"] != "speak" || cancelled.remove(&id) {
            continue;
        }
        let voice = request["voice"].as_str().unwrap_or_default();
        let chunks = request["chunks"].as_array().cloned().unwrap_or_default();
        let mut last = Some(json!({"id": id, "end": true}));
        for chunk in chunks {
            while let Ok(next) = requests.try_recv() {
                if next["op"] == "cancel" {
                    cancelled.insert(id_of(&next));
                } else {
                    waiting.push_back(next);
                }
            }
            if cancelled.remove(&id) {
                last = None;
                break;
            }
            match synthesizer.speak(chunk.as_str().unwrap_or_default(), voice) {
                Ok(wav) => writeln!(out, "{}", json!({"id": id, "audio": STANDARD.encode(wav)}))?,
                Err(error) => {
                    last = Some(json!({"id": id, "error": error}));
                    break;
                }
            }
            out.flush()?;
        }
        if let Some(last) = last {
            writeln!(out, "{last}")?;
            out.flush()?;
        }
    }
}

/// Load the model from `root` and serve stdin until it closes.
pub fn run(root: &Path, settings: &settings::Speech) -> Result<(), String> {
    let mut kokoro = Kokoro::load(root, settings)?;
    serve(
        &mut kokoro,
        io::BufReader::new(io::stdin()),
        io::stdout().lock(),
    )
    .map_err(|error| error.to_string())
}

/// Phonemes for `text` in the manner of phonemizer's espeak backend with punctuation preserved:
/// the runs of words between punctuation marks go to `espeak` and the marks are kept between
/// them, except a `.` or `,` between digits, which belongs to the number. The result keeps only
/// `vocabulary` characters, with whitespace collapsed.
pub fn phonemize(
    text: &str,
    mut espeak: impl FnMut(&str) -> String,
    vocabulary: &HashSet<char>,
) -> String {
    let chars: Vec<char> = text.chars().collect();
    let is_mark = |i: usize| {
        let c = chars[i];
        let decimal = matches!(c, '.' | ',')
            && i > 0
            && chars[i - 1].is_ascii_digit()
            && chars.get(i + 1).is_some_and(char::is_ascii_digit);
        MARKS.contains(c) && !decimal
    };
    let mut out = String::new();
    let mut words = String::new();
    let mut i = 0;
    while i < chars.len() {
        if !is_mark(i) {
            words.push(chars[i]);
            i += 1;
            continue;
        }
        // A mark run takes the whitespace around it, as phonemizer's `(\s*[marks]+\s*)+` does.
        let mut mark: String = words
            .chars()
            .rev()
            .take_while(|c| c.is_whitespace())
            .collect();
        let phonemes = espeak_words(words.trim(), &mut espeak);
        out.push_str(&phonemes);
        words.clear();
        while i < chars.len() && (is_mark(i) || chars[i].is_whitespace()) {
            mark.push(chars[i]);
            i += 1;
        }
        out.push_str(&mark);
        out.push(' ');
    }
    out.push_str(&espeak_words(words.trim(), &mut espeak));
    let kept: String = out.chars().filter(|c| vocabulary.contains(c)).collect();
    kept.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// espeak's phonemes for a run of words: the `_` between phones dropped, one space between words.
fn espeak_words(words: &str, espeak: &mut impl FnMut(&str) -> String) -> String {
    if words.is_empty() {
        return String::new();
    }
    espeak(words)
        .replace('_', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Phonemes as Kokoro passes of at most [`MAX_PHONEMES`], split after the last pause mark that
/// fits, else the last space, else hard.
pub fn passes(phonemes: &str) -> Vec<Vec<char>> {
    let mut rest: Vec<char> = phonemes.chars().collect();
    let mut out = Vec::new();
    while rest.len() > MAX_PHONEMES {
        let window = &rest[..MAX_PHONEMES];
        let cut = window
            .iter()
            .rposition(|c| ".!?;:,—…".contains(*c))
            .or_else(|| window.iter().rposition(|c| *c == ' '))
            .map_or(MAX_PHONEMES, |at| at + 1);
        let tail = rest.split_off(cut);
        out.push(rest);
        rest = tail.into_iter().skip_while(|c| *c == ' ').collect();
    }
    if !rest.is_empty() {
        out.push(rest);
    }
    out
}

/// A 16-bit mono WAV of `samples` in [-1, 1].
pub fn wav(samples: &[f32], rate: u32) -> Vec<u8> {
    let data = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    for field in [16u32, 1 | (1 << 16), rate, rate * 2, 2 | (16 << 16)] {
        out.extend_from_slice(&field.to_le_bytes());
    }
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for sample in samples {
        let sample = (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

/// C layouts from sherpa-onnx 2.0.1's `c-api.h`, as published in `sherpa-onnx-sys` 2.0.1
/// (Apache-2.0). Only the Kokoro fields are set; the rest stay zero ("unset").
#[allow(dead_code)]
mod ffi {
    use std::ffi::{c_char, c_float};

    #[repr(C)]
    pub struct Vits {
        pub model: *const c_char,
        pub lexicon: *const c_char,
        pub tokens: *const c_char,
        pub data_dir: *const c_char,
        pub noise_scale: c_float,
        pub noise_scale_w: c_float,
        pub length_scale: c_float,
        pub dict_dir: *const c_char,
    }
    #[repr(C)]
    pub struct Matcha {
        pub acoustic_model: *const c_char,
        pub vocoder: *const c_char,
        pub lexicon: *const c_char,
        pub tokens: *const c_char,
        pub data_dir: *const c_char,
        pub noise_scale: c_float,
        pub length_scale: c_float,
        pub dict_dir: *const c_char,
    }
    #[repr(C)]
    pub struct Kokoro {
        pub model: *const c_char,
        pub voices: *const c_char,
        pub tokens: *const c_char,
        pub data_dir: *const c_char,
        pub length_scale: c_float,
        pub dict_dir: *const c_char,
        pub lexicon: *const c_char,
        pub lang: *const c_char,
    }
    #[repr(C)]
    pub struct Kitten {
        pub model: *const c_char,
        pub voices: *const c_char,
        pub tokens: *const c_char,
        pub data_dir: *const c_char,
        pub length_scale: c_float,
        pub lexicon: *const c_char,
    }
    #[repr(C)]
    pub struct Zipvoice {
        pub tokens: *const c_char,
        pub encoder: *const c_char,
        pub decoder: *const c_char,
        pub vocoder: *const c_char,
        pub data_dir: *const c_char,
        pub lexicon: *const c_char,
        pub feat_scale: c_float,
        pub t_shift: c_float,
        pub target_rms: c_float,
        pub guidance_scale: c_float,
    }
    #[repr(C)]
    pub struct Pocket {
        pub lm_flow: *const c_char,
        pub lm_main: *const c_char,
        pub encoder: *const c_char,
        pub decoder: *const c_char,
        pub text_conditioner: *const c_char,
        pub vocab_json: *const c_char,
        pub token_scores_json: *const c_char,
        pub voice_embedding_cache_capacity: i32,
    }
    #[repr(C)]
    pub struct Supertonic {
        pub duration_predictor: *const c_char,
        pub text_encoder: *const c_char,
        pub vector_estimator: *const c_char,
        pub vocoder: *const c_char,
        pub tts_json: *const c_char,
        pub unicode_indexer: *const c_char,
        pub voice_style: *const c_char,
    }
    #[repr(C)]
    pub struct ModelConfig {
        pub vits: Vits,
        pub num_threads: i32,
        pub debug: i32,
        pub provider: *const c_char,
        pub matcha: Matcha,
        pub kokoro: Kokoro,
        pub kitten: Kitten,
        pub zipvoice: Zipvoice,
        pub pocket: Pocket,
        pub supertonic: Supertonic,
    }
    #[repr(C)]
    pub struct TtsConfig {
        pub model: ModelConfig,
        pub rule_fsts: *const c_char,
        pub max_num_sentences: i32,
        pub rule_fars: *const c_char,
        pub silence_scale: c_float,
    }
    #[repr(C)]
    pub struct GeneratedAudio {
        pub samples: *const f32,
        pub n: i32,
        pub sample_rate: i32,
    }
    #[repr(C)]
    pub struct GenerationConfig {
        pub silence_scale: c_float,
        pub speed: c_float,
        pub sid: i32,
        pub reference_audio: *const f32,
        pub reference_audio_len: i32,
        pub reference_sample_rate: i32,
        pub reference_text: *const c_char,
        pub num_steps: i32,
        pub extra: *const c_char,
        pub phoneme_codepoints: *const i32,
        pub phoneme_codepoints_lens: *const i32,
        pub phoneme_codepoints_num_sentences: i32,
        pub tokens: *const *const c_char,
        pub tokens_lens: *const i32,
        pub tokens_num_sentences: i32,
    }
}

type Progress = Option<unsafe extern "C" fn(*const f32, i32, c_float, *mut c_void) -> i32>;

/// Kokoro with espeak-ng phonemes. Both libraries stay loaded for the process lifetime, so the
/// resolved entry points and the model handle remain valid.
pub struct Kokoro {
    _sherpa: Library,
    _espeak: Library,
    tts: Handle,
    generate: unsafe extern "C" fn(
        Handle,
        *const c_char,
        *const ffi::GenerationConfig,
        Progress,
        *mut c_void,
    ) -> *const ffi::GeneratedAudio,
    destroy_audio: unsafe extern "C" fn(*const ffi::GeneratedAudio),
    set_voice: unsafe extern "C" fn(*const c_char) -> i32,
    to_phonemes: unsafe extern "C" fn(*mut *const c_void, i32, i32) -> *const c_char,
    vocabulary: HashSet<char>,
    /// The espeak voice set last.
    accent: String,
}

impl Kokoro {
    /// Load sherpa-onnx and espeak-ng from `root/lib`, espeak's data from
    /// `root/espeak-ng-data` and the model from `root/models/kokoro`.
    pub fn load(root: &Path, settings: &settings::Speech) -> Result<Self, String> {
        let sherpa = sherpa(root)?;
        let espeak = library(root, "libespeak-ng.so.1")?;
        let models = root.join("models/kokoro");
        let model = |name: &str| c_text(&models.join(name).to_string_lossy());
        let (onnx, voices, tokens) = (
            model("model.onnx")?,
            model("voices.bin")?,
            model("tokens.txt")?,
        );
        let vocabulary = std::fs::read_to_string(models.join("tokens.txt"))
            .map_err(|error| format!("tokens.txt: {error}"))?
            .lines()
            .filter_map(|line| {
                line.rsplit_once(' ')
                    .and_then(|(token, _)| token.chars().next())
            })
            .collect();
        let mut config: ffi::TtsConfig = zeroed();
        config.model.kokoro.model = onnx.as_ptr();
        config.model.kokoro.voices = voices.as_ptr();
        config.model.kokoro.tokens = tokens.as_ptr();
        config.model.kokoro.length_scale = 1.0;
        config.model.num_threads = settings.threads;
        config.max_num_sentences = 1;
        config.silence_scale = 0.2;
        let data = c_text(&root.to_string_lossy())?;
        // SAFETY: each symbol is declared with its C signature (sherpa-onnx 2.0.1, checked by
        // `sherpa`, and espeak-ng's stable `speak_lib.h`); `config` and the strings it points
        // to outlive the calls, which copy what they need.
        unsafe {
            let create: unsafe extern "C" fn(*const ffi::TtsConfig) -> Handle =
                symbol(&sherpa, "SherpaOnnxCreateOfflineTts")?;
            let tts = create(&config);
            if tts.is_null() {
                return Err("Kokoro model failed to load".into());
            }
            let initialize: unsafe extern "C" fn(i32, i32, *const c_char, i32) -> i32 =
                symbol(&espeak, "espeak_Initialize")?;
            // Synchronous output: espeak only converts text, it never plays audio.
            if initialize(2, 0, data.as_ptr(), 0) < 0 {
                return Err("espeak-ng data failed to load".into());
            }
            let mut kokoro = Self {
                generate: symbol(&sherpa, "SherpaOnnxOfflineTtsGenerateWithConfig")?,
                destroy_audio: symbol(&sherpa, "SherpaOnnxDestroyOfflineTtsGeneratedAudio")?,
                set_voice: symbol(&espeak, "espeak_SetVoiceByName")?,
                to_phonemes: symbol(&espeak, "espeak_TextToPhonemes")?,
                _sherpa: sherpa,
                _espeak: espeak,
                tts,
                vocabulary,
                accent: String::new(),
            };
            // espeak has no voice until one is set; phonemes need one.
            kokoro.set_accent("gmw/en-US")?;
            Ok(kokoro)
        }
    }

    /// Switch espeak to the voice `accent` (`gmw/en-US` or `gmw/en`) unless it is already set.
    fn set_accent(&mut self, accent: &str) -> Result<(), String> {
        if self.accent != accent {
            let name = CString::new(accent).map_err(|_| "bad accent")?;
            // SAFETY: a NUL-terminated voice name that espeak copies.
            if unsafe { (self.set_voice)(name.as_ptr()) } != 0 {
                return Err(format!("espeak voice {accent} unavailable"));
            }
            self.accent = accent.into();
        }
        Ok(())
    }

    /// espeak's IPA for `words` in the current accent, clause by clause, phones separated by `_`.
    fn espeak(&self, words: &str) -> String {
        let Ok(words) = CString::new(words) else {
            return String::new();
        };
        let mut next = words.as_ptr().cast::<c_void>();
        let mut clauses = Vec::new();
        // UTF-8 text; IPA output with `_` between phones (phonemizer's mode).
        const IPA_PHONES: i32 = ((b'_' as i32) << 8) | 0x02;
        while !next.is_null() {
            // SAFETY: `next` points into `words` (or is advanced by espeak within it, then set
            // to null at the end); the returned clause is copied before the next call.
            let clause = unsafe { (self.to_phonemes)(&mut next, 1, IPA_PHONES) };
            if !clause.is_null() {
                // SAFETY: a NUL-terminated string valid until the next espeak call.
                clauses.push(
                    unsafe { CStr::from_ptr(clause) }
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
        clauses.join(" ")
    }
}

impl Synthesizer for Kokoro {
    fn speak(&mut self, text: &str, voice: &str) -> Result<Vec<u8>, String> {
        let sid = VOICES
            .iter()
            .position(|(id, _)| *id == voice)
            .ok_or("unknown voice")?;
        // phonemizer's `en-gb` and `en-us` resolve to these espeak voice identifiers.
        self.set_accent(if voice.starts_with('b') {
            "gmw/en"
        } else {
            "gmw/en-US"
        })?;
        let phonemes = phonemize(text, |words| self.espeak(words), &self.vocabulary);
        let passes = passes(&phonemes);
        if passes.is_empty() {
            return Ok(wav(&[], 24000));
        }
        let flat: Vec<i32> = passes.iter().flatten().map(|c| *c as i32).collect();
        let lengths: Vec<i32> = passes.iter().map(|pass| pass.len() as i32).collect();
        let (empty, extra) = (c_text("")?, c_text("{}")?);
        let mut config: ffi::GenerationConfig = zeroed();
        config.silence_scale = 0.2;
        config.speed = 1.0;
        config.sid = sid as i32;
        config.extra = extra.as_ptr();
        config.phoneme_codepoints = flat.as_ptr();
        config.phoneme_codepoints_lens = lengths.as_ptr();
        config.phoneme_codepoints_num_sentences = lengths.len() as i32;
        // SAFETY: a live model handle; the config and the arrays it points to outlive the call,
        // and the returned audio is copied before it is destroyed exactly once.
        unsafe {
            let audio = (self.generate)(
                self.tts,
                empty.as_ptr(),
                &config,
                None,
                std::ptr::null_mut(),
            );
            if audio.is_null() || (*audio).samples.is_null() {
                return Err("synthesis failed".into());
            }
            let samples = std::slice::from_raw_parts((*audio).samples, (*audio).n.max(0) as usize);
            let out = wav(samples, (*audio).sample_rate.max(1) as u32);
            (self.destroy_audio)(audio);
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    // Test code may unwrap (the standard's `allow-unwrap-in-tests`).
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// Writes each request's text length as a one-byte "WAV"; fails on "bad".
    struct Lengths;
    impl Synthesizer for Lengths {
        fn speak(&mut self, text: &str, _voice: &str) -> Result<Vec<u8>, String> {
            if text == "bad" {
                return Err("synthesis failed".into());
            }
            Ok(vec![text.len() as u8])
        }
    }

    fn replies(input: &str) -> Vec<Value> {
        let mut out = Vec::new();
        serve(&mut Lengths, io::Cursor::new(input.to_owned()), &mut out).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn replies_stream_per_chunk_and_cancelled_ones_stay_silent() {
        let speak = |id: &str, chunks: &[&str]| {
            json!({"op": "speak", "id": id, "voice": "af_heart", "chunks": chunks}).to_string()
                + "\n"
        };
        let input = [
            json!({"op": "cancel", "id": "b"}).to_string() + "\n",
            speak("a", &["ab", "abc"]),
            speak("b", &["never"]),
            speak("c", &["ok", "bad", "unsent"]),
        ]
        .concat();
        let audio = |id: &str, byte: u8| json!({"id": id, "audio": STANDARD.encode([byte])});
        assert_eq!(
            replies(&input),
            vec![
                json!({"ready": true}),
                audio("a", 2),
                audio("a", 3),
                json!({"id": "a", "end": true}),
                audio("c", 2),
                json!({"id": "c", "error": "synthesis failed"}),
            ]
        );
    }

    #[test]
    fn punctuation_survives_between_phonemized_word_runs() {
        let vocabulary: HashSet<char> = "abcdefghijklmnopqrstuvwxyz ,.!?:;—".chars().collect();
        let mut seen = Vec::new();
        let espeak = |words: &str| {
            seen.push(words.to_owned());
            let letters = words
                .to_lowercase()
                .replace(|c: char| c.is_ascii_digit() || c == '.', "");
            format!("{letters} _x_y")
        };
        let phonemes = phonemize("Hello , world! It costs 3.50, ok?", espeak, &vocabulary);
        assert_eq!(phonemes, "hello xy , world xy! it costs xy, ok xy?");
        assert_eq!(seen, vec!["Hello", "world", "It costs 3.50", "ok"]);
    }

    #[test]
    fn long_phonemes_split_at_a_pause_then_a_space() {
        let sentence = format!("{}. ", "a".repeat(299));
        let split = passes(&sentence.repeat(2));
        assert_eq!(split.len(), 2);
        assert_eq!(split[0].len(), 300);
        let words = passes(&"abcd ".repeat(150));
        assert!(words.iter().all(|pass| pass.len() <= MAX_PHONEMES));
        assert_eq!(*words[0].last().unwrap(), ' ');
        assert_eq!(passes(&"x".repeat(1001)).len(), 3);
    }

    #[test]
    fn wav_headers_describe_mono_16_bit_samples() {
        let file = wav(&[0.0, 1.0, -2.0], 24000);
        assert_eq!(&file[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(file[24..28].try_into().unwrap()), 24000);
        assert_eq!(u32::from_le_bytes(file[40..44].try_into().unwrap()), 6);
        assert_eq!(&file[44..], &[0, 0, 0xff, 0x7f, 0x01, 0x80]);
    }

    /// Loads the real libraries and model when `WEBAGENT_INFERENCE_ROOT` names a copy of /opt/inference;
    /// the broker's test run has none, so it is skipped there.
    #[test]
    fn kokoro_reads_numbers_and_unusual_words_when_installed() {
        let Ok(root) = std::env::var("WEBAGENT_INFERENCE_ROOT") else {
            return;
        };
        let defaults = crate::settings::VoiceSettings::default();
        let mut kokoro = Kokoro::load(Path::new(&root), &defaults.speech).unwrap();
        let phonemes = phonemize(
            "It costs 1969 dollars.",
            |w| kokoro.espeak(w),
            &kokoro.vocabulary,
        );
        assert!(phonemes.contains("nˈaɪntiːn"), "{phonemes}");
        for voice in ["af_heart", "bm_george"] {
            let file = kokoro
                .speak("Rebuild the Kubernetes cluster at 10:30.", voice)
                .unwrap();
            // More than a second of 24 kHz audio.
            assert!(file.len() > 44 + 48_000, "{voice}: {} bytes", file.len());
        }
        if let Ok(dir) = std::env::var("WEBAGENT_VOICE_SAMPLES") {
            let text = "Hi, I'm Heart. Version 2.0 shipped on May 5th, with 37 fixes.";
            std::fs::write(
                Path::new(&dir).join("heart.wav"),
                kokoro.speak(text, "af_heart").unwrap(),
            )
            .unwrap();
        }
    }
}
