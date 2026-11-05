//! The shared composer dictation recognizer. One process (`webagent-v3 --voice-service
//! dictation`, started by `host/voice.rs`) holds the multilingual Parakeet v3 model for every
//! dictation and transcribes each phrase when the Silero pause detector closes it; the spoken
//! language is detected per phrase. sherpa-onnx is loaded at run time from the `voice` stage of
//! the Dockerfile, so the workspace builds and tests without its native libraries.
//!
//! Requests arrive one JSON object per line on stdin: `{"op":"open","id":ID}`,
//! `{"op":"audio","id":ID,"pcm":BASE64}` (16 kHz mono s16le), `{"op":"finish","id":ID}` or
//! `{"op":"cancel","id":ID}`. Replies are JSON lines on stdout: `{"ready":true}` once the model
//! has loaded, `{"id":ID,"text":TEXT}` with the transcript so far after each phrase,
//! `{"id":ID,"final":TEXT}` for a finished dictation and `{"id":ID,"error":REASON}`. Audio stays
//! in memory until finish or cancel; nothing is stored and no network is used.
use std::collections::HashMap;
use std::ffi::{c_char, c_float, c_void, CStr, CString};
use std::io::{self, BufRead, Write};
use std::path::Path;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use libloading::Library;
use serde_json::{json, Value};

use crate::settings;

/// Where the `voice` stage installs `lib/` and `models/`.
pub const VOICE_ROOT: &str = "/opt/voice";
/// The only sherpa-onnx release whose C layouts [`ffi`] mirrors.
const LIBRARY_VERSION: &str = "2.0.1";
const SAMPLE_RATE: i32 = 16000;
/// Audio the detector may hold for a phrase in progress, in seconds.
const VAD_BUFFER_SECONDS: f32 = 60.0;

/// Transcribes one dictation's audio phrase by phrase.
pub trait Transcriber {
    /// One dictation's pause detector and phrases so far.
    type Dictation;
    /// A new dictation, or `None` when its detector cannot be created.
    fn open(&self) -> Option<Self::Dictation>;
    /// Add samples in [-1, 1); returns the whole transcript when a phrase was added.
    fn push(&self, dictation: &mut Self::Dictation, samples: &[f32]) -> Option<String>;
    /// Transcribe the phrase still in progress and return the whole transcript.
    fn finish(&self, dictation: Self::Dictation) -> String;
}

/// 16-bit little-endian PCM as samples in [-1, 1); a trailing odd byte is ignored.
pub fn samples(pcm: &[u8]) -> Vec<f32> {
    pcm.chunks_exact(2)
        .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32768.0)
        .collect()
}

/// Answer requests until the input closes. Requests for unknown dictations are ignored, except
/// `finish`, which answers with an error so the caller does not wait.
pub fn serve<T: Transcriber>(
    transcriber: &T,
    input: impl BufRead,
    mut out: impl Write,
) -> io::Result<()> {
    writeln!(out, "{}", json!({"ready": true}))?;
    out.flush()?;
    let mut open = HashMap::new();
    for line in input.lines() {
        let Ok(request) = serde_json::from_str::<Value>(&line?) else {
            continue;
        };
        let id = request["id"].as_str().unwrap_or_default().to_owned();
        let reply = match request["op"].as_str() {
            Some("open") => match transcriber.open() {
                Some(dictation) => {
                    open.insert(id, dictation);
                    None
                }
                None => Some(json!({"id": id, "error": "pause detector unavailable"})),
            },
            Some("audio") => open.get_mut(&id).and_then(|dictation| {
                let pcm = request["pcm"]
                    .as_str()
                    .and_then(|pcm| STANDARD.decode(pcm).ok())
                    .unwrap_or_default();
                transcriber
                    .push(dictation, &samples(&pcm))
                    .map(|text| json!({"id": id, "text": text}))
            }),
            Some("finish") => Some(match open.remove(&id) {
                Some(dictation) => json!({"id": id, "final": transcriber.finish(dictation)}),
                None => json!({"id": id, "error": "unknown dictation"}),
            }),
            Some("cancel") => {
                open.remove(&id);
                None
            }
            _ => None,
        };
        if let Some(reply) = reply {
            writeln!(out, "{reply}")?;
            out.flush()?;
        }
    }
    Ok(())
}

/// Load the model from `root` (`lib/` and `models/`) and serve stdin until it closes.
pub fn run(root: &Path, settings: &settings::Dictation) -> Result<(), String> {
    let parakeet = Parakeet::load(root, settings)?;
    serve(&parakeet, io::stdin().lock(), io::stdout().lock()).map_err(|error| error.to_string())
}

/// C layouts from sherpa-onnx 2.0.1's `c-api.h`, as published in `sherpa-onnx-sys` 2.0.1
/// (Apache-2.0). Configs start zeroed (every field is a pointer, integer or float, for which zero
/// means "unset"); [`Parakeet::load`] refuses any other library version.
#[allow(dead_code)]
mod ffi {
    use std::ffi::{c_char, c_float};

    #[repr(C)]
    pub struct FeatureConfig {
        pub sample_rate: i32,
        pub feature_dim: i32,
    }
    #[repr(C)]
    pub struct HomophoneReplacerConfig {
        pub dict_dir: *const c_char,
        pub lexicon: *const c_char,
        pub rule_fsts: *const c_char,
    }
    #[repr(C)]
    pub struct Transducer {
        pub encoder: *const c_char,
        pub decoder: *const c_char,
        pub joiner: *const c_char,
    }
    #[repr(C)]
    pub struct OneModel {
        pub model: *const c_char,
    }
    #[repr(C)]
    pub struct Whisper {
        pub encoder: *const c_char,
        pub decoder: *const c_char,
        pub language: *const c_char,
        pub task: *const c_char,
        pub tail_paddings: i32,
        pub enable_token_timestamps: i32,
        pub enable_segment_timestamps: i32,
    }
    #[repr(C)]
    pub struct Canary {
        pub encoder: *const c_char,
        pub decoder: *const c_char,
        pub src_lang: *const c_char,
        pub tgt_lang: *const c_char,
        pub use_pnc: i32,
    }
    #[repr(C)]
    pub struct EncoderDecoder {
        pub encoder: *const c_char,
        pub decoder: *const c_char,
    }
    #[repr(C)]
    pub struct Moonshine {
        pub preprocessor: *const c_char,
        pub encoder: *const c_char,
        pub uncached_decoder: *const c_char,
        pub cached_decoder: *const c_char,
        pub merged_decoder: *const c_char,
    }
    #[repr(C)]
    pub struct LanguageModel {
        pub model: *const c_char,
        pub scale: c_float,
    }
    #[repr(C)]
    pub struct SenseVoice {
        pub model: *const c_char,
        pub language: *const c_char,
        pub use_itn: i32,
    }
    #[repr(C)]
    pub struct FunAsrNano {
        pub encoder_adaptor: *const c_char,
        pub llm: *const c_char,
        pub embedding: *const c_char,
        pub tokenizer: *const c_char,
        pub system_prompt: *const c_char,
        pub user_prompt: *const c_char,
        pub max_new_tokens: i32,
        pub temperature: c_float,
        pub top_p: c_float,
        pub seed: i32,
        pub language: *const c_char,
        pub itn: i32,
        pub hotwords: *const c_char,
    }
    #[repr(C)]
    pub struct Qwen3Asr {
        pub conv_frontend: *const c_char,
        pub encoder: *const c_char,
        pub decoder: *const c_char,
        pub tokenizer: *const c_char,
        pub max_total_len: i32,
        pub max_new_tokens: i32,
        pub temperature: c_float,
        pub top_p: c_float,
        pub seed: i32,
        pub hotwords: *const c_char,
    }
    #[repr(C)]
    pub struct CohereTranscribe {
        pub encoder: *const c_char,
        pub decoder: *const c_char,
        pub language: *const c_char,
        pub use_punct: i32,
        pub use_itn: i32,
    }
    /// `SherpaOnnxOfflineModelConfig`, field for field.
    #[repr(C)]
    pub struct OfflineModelConfig {
        pub transducer: Transducer,
        pub paraformer: OneModel,
        pub nemo_ctc: OneModel,
        pub whisper: Whisper,
        pub tdnn: OneModel,
        pub tokens: *const c_char,
        pub num_threads: i32,
        pub debug: i32,
        pub provider: *const c_char,
        pub model_type: *const c_char,
        pub modeling_unit: *const c_char,
        pub bpe_vocab: *const c_char,
        pub telespeech_ctc: *const c_char,
        pub sense_voice: SenseVoice,
        pub moonshine: Moonshine,
        pub fire_red_asr: EncoderDecoder,
        pub dolphin: OneModel,
        pub zipformer_ctc: OneModel,
        pub canary: Canary,
        pub wenet_ctc: OneModel,
        pub omnilingual: OneModel,
        pub medasr: OneModel,
        pub funasr_nano: FunAsrNano,
        pub fire_red_asr_ctc: OneModel,
        pub qwen3_asr: Qwen3Asr,
        pub cohere_transcribe: CohereTranscribe,
    }
    #[repr(C)]
    pub struct CtcFstDecoderConfig {
        pub graph: *const c_char,
        pub max_active: i32,
    }
    /// `SherpaOnnxOfflineRecognizerConfig`, field for field.
    #[repr(C)]
    pub struct OfflineRecognizerConfig {
        pub feat_config: FeatureConfig,
        pub model_config: OfflineModelConfig,
        pub lm_config: LanguageModel,
        pub decoding_method: *const c_char,
        pub max_active_paths: i32,
        pub hotwords_file: *const c_char,
        pub hotwords_score: c_float,
        pub rule_fsts: *const c_char,
        pub rule_fars: *const c_char,
        pub blank_penalty: c_float,
        pub hr: HomophoneReplacerConfig,
        pub ctc_fst_decoder_config: CtcFstDecoderConfig,
    }
    #[repr(C)]
    pub struct VadModel {
        pub model: *const c_char,
        pub threshold: c_float,
        pub min_silence_duration: c_float,
        pub min_speech_duration: c_float,
        pub window_size: i32,
        pub max_speech_duration: c_float,
    }
    /// `SherpaOnnxVadModelConfig`, field for field.
    #[repr(C)]
    pub struct VadModelConfig {
        pub silero_vad: VadModel,
        pub sample_rate: i32,
        pub num_threads: i32,
        pub provider: *const c_char,
        pub debug: i32,
        pub ten_vad: VadModel,
    }
    #[repr(C)]
    pub struct SpeechSegment {
        pub start: i64,
        pub samples: *mut f32,
        pub n: i32,
    }
}

/// An all-zero config, which sherpa-onnx reads as "every option unset".
pub(crate) fn zeroed<T>() -> T {
    // SAFETY: only used for the `ffi` config structs, whose fields are raw pointers, integers and
    // floats; the all-zero bit pattern is a valid null/0 value for each of them.
    unsafe { std::mem::zeroed() }
}

pub(crate) type Handle = *const c_void;

/// The sherpa-onnx entry points used here, resolved once from the loaded library.
struct Api {
    accept_waveform: unsafe extern "C" fn(Handle, i32, *const f32, i32),
    create_stream: unsafe extern "C" fn(Handle) -> Handle,
    decode: unsafe extern "C" fn(Handle, Handle),
    destroy_stream: unsafe extern "C" fn(Handle),
    result_json: unsafe extern "C" fn(Handle) -> *const c_char,
    destroy_result_json: unsafe extern "C" fn(*const c_char),
    create_vad: unsafe extern "C" fn(*const ffi::VadModelConfig, c_float) -> Handle,
    destroy_vad: unsafe extern "C" fn(Handle),
    vad_accept: unsafe extern "C" fn(Handle, *const f32, i32),
    vad_empty: unsafe extern "C" fn(Handle) -> i32,
    vad_front: unsafe extern "C" fn(Handle) -> *const ffi::SpeechSegment,
    vad_pop: unsafe extern "C" fn(Handle),
    vad_flush: unsafe extern "C" fn(Handle),
    destroy_segment: unsafe extern "C" fn(*const ffi::SpeechSegment),
}

/// Parakeet TDT with a Silero pause detector per dictation. The library stays loaded for the
/// process lifetime, so the resolved entry points and the recognizer remain valid.
pub struct Parakeet {
    _library: Library,
    api: Api,
    recognizer: Handle,
    vad: ffi::VadModelConfig,
    /// Owns the strings `vad` points to.
    _vad_strings: Vec<CString>,
}

/// One open dictation: its pause detector and the phrases transcribed so far.
pub struct ParakeetDictation {
    vad: Handle,
    destroy: unsafe extern "C" fn(Handle),
    phrases: Vec<String>,
}

impl Drop for ParakeetDictation {
    fn drop(&mut self) {
        // SAFETY: `vad` came from `create_vad` and is destroyed exactly once, here.
        unsafe { (self.destroy)(self.vad) }
    }
}

pub(crate) fn text(value: &str) -> Result<CString, String> {
    CString::new(value).map_err(|_| "model path contains a NUL byte".to_owned())
}

/// Load a shared library from `root/lib`.
pub(crate) fn library(root: &Path, name: &str) -> Result<Library, String> {
    let path = root.join("lib").join(name);
    // SAFETY: loads a checksum-pinned library from the image (sherpa-onnx, whose initializers
    // only set up ONNX Runtime, or espeak-ng, which has none); nothing else is loaded from there.
    unsafe { Library::new(&path) }
        .map_err(|error| format!("cannot load {}: {error}", path.display()))
}

/// Resolve one entry point; the caller declares `T` with its C signature.
///
/// # Safety
/// `T` must be the symbol's exact function pointer type.
pub(crate) unsafe fn symbol<T: Copy>(library: &Library, name: &str) -> Result<T, String> {
    // SAFETY: the caller guarantees the signature; the copied pointer is used only while
    // `library` stays loaded, which its owners keep for the process lifetime.
    unsafe { library.get::<T>(name.as_bytes()) }
        .map(|symbol| *symbol)
        .map_err(|error| format!("{name}: {error}"))
}

/// sherpa-onnx from `root/lib`, refused unless it is the release [`ffi`] mirrors.
pub(crate) fn sherpa(root: &Path) -> Result<Library, String> {
    let library = library(root, "libsherpa-onnx-c-api.so")?;
    // SAFETY: the version query has this signature in every sherpa-onnx release and returns a
    // static string.
    let found = unsafe {
        let version: unsafe extern "C" fn() -> *const c_char =
            symbol(&library, "SherpaOnnxGetVersionStr")?;
        CStr::from_ptr(version()).to_string_lossy().into_owned()
    };
    if found != LIBRARY_VERSION {
        return Err(format!(
            "sherpa-onnx {found} found; the C layouts match {LIBRARY_VERSION}"
        ));
    }
    Ok(library)
}

impl Parakeet {
    /// Load sherpa-onnx from `root/lib` and the models from `root/models`.
    pub fn load(root: &Path, settings: &settings::Dictation) -> Result<Self, String> {
        let library = sherpa(root)?;
        // SAFETY: each symbol is declared with the signature of sherpa-onnx 2.0.1's C API.
        let api = unsafe {
            macro_rules! symbol {
                ($name:literal) => {
                    symbol(&library, $name)?
                };
            }
            Api {
                accept_waveform: symbol!("SherpaOnnxAcceptWaveformOffline"),
                create_stream: symbol!("SherpaOnnxCreateOfflineStream"),
                decode: symbol!("SherpaOnnxDecodeOfflineStream"),
                destroy_stream: symbol!("SherpaOnnxDestroyOfflineStream"),
                result_json: symbol!("SherpaOnnxGetOfflineStreamResultAsJson"),
                destroy_result_json: symbol!("SherpaOnnxDestroyOfflineStreamResultJson"),
                create_vad: symbol!("SherpaOnnxCreateVoiceActivityDetector"),
                destroy_vad: symbol!("SherpaOnnxDestroyVoiceActivityDetector"),
                vad_accept: symbol!("SherpaOnnxVoiceActivityDetectorAcceptWaveform"),
                vad_empty: symbol!("SherpaOnnxVoiceActivityDetectorEmpty"),
                vad_front: symbol!("SherpaOnnxVoiceActivityDetectorFront"),
                vad_pop: symbol!("SherpaOnnxVoiceActivityDetectorPop"),
                vad_flush: symbol!("SherpaOnnxVoiceActivityDetectorFlush"),
                destroy_segment: symbol!("SherpaOnnxDestroySpeechSegment"),
            }
        };
        let models = root.join("models");
        let model = |name: &str| text(&models.join(name).to_string_lossy());
        let (encoder, decoder, joiner, tokens) = (
            model("parakeet/encoder.int8.onnx")?,
            model("parakeet/decoder.int8.onnx")?,
            model("parakeet/joiner.int8.onnx")?,
            model("parakeet/tokens.txt")?,
        );
        let (model_type, method) = (text("nemo_transducer")?, text("greedy_search")?);
        let mut config: ffi::OfflineRecognizerConfig = zeroed();
        config.feat_config = ffi::FeatureConfig {
            sample_rate: SAMPLE_RATE,
            feature_dim: 80,
        };
        config.model_config.transducer = ffi::Transducer {
            encoder: encoder.as_ptr(),
            decoder: decoder.as_ptr(),
            joiner: joiner.as_ptr(),
        };
        config.model_config.tokens = tokens.as_ptr();
        config.model_config.num_threads = settings.threads;
        config.model_config.model_type = model_type.as_ptr();
        config.decoding_method = method.as_ptr();
        config.max_active_paths = 4;
        // SAFETY: the symbol has this signature in 2.0.1; `config` and the strings it points to
        // outlive the call, which copies what it needs.
        let recognizer = unsafe {
            let create: unsafe extern "C" fn(*const ffi::OfflineRecognizerConfig) -> Handle =
                symbol(&library, "SherpaOnnxCreateOfflineRecognizer")?;
            create(&config)
        };
        if recognizer.is_null() {
            return Err("Parakeet model failed to load".into());
        }
        let silero = model("silero_vad.onnx")?;
        let pauses = &settings.pause_detection;
        let mut vad: ffi::VadModelConfig = zeroed();
        vad.silero_vad = ffi::VadModel {
            model: silero.as_ptr(),
            threshold: pauses.threshold,
            min_silence_duration: pauses.min_silence_seconds,
            min_speech_duration: pauses.min_speech_seconds,
            window_size: 512,
            max_speech_duration: pauses.max_phrase_seconds,
        };
        vad.sample_rate = SAMPLE_RATE;
        vad.num_threads = 1;
        let parakeet = Parakeet {
            _library: library,
            api,
            recognizer,
            vad,
            _vad_strings: vec![silero],
        };
        parakeet.open().ok_or("Silero VAD model failed to load")?;
        Ok(parakeet)
    }

    /// Transcribe one closed phrase.
    fn transcribe(&self, samples: &[f32]) -> String {
        let api = &self.api;
        // SAFETY: the recognizer and stream are live handles of this library; the stream is
        // destroyed after its result JSON is copied and freed, and `samples` outlives the call.
        unsafe {
            let stream = (api.create_stream)(self.recognizer);
            if stream.is_null() {
                return String::new();
            }
            (api.accept_waveform)(stream, SAMPLE_RATE, samples.as_ptr(), samples.len() as i32);
            (api.decode)(self.recognizer, stream);
            let json = (api.result_json)(stream);
            let result = if json.is_null() {
                String::new()
            } else {
                let value = serde_json::from_slice::<Value>(CStr::from_ptr(json).to_bytes())
                    .ok()
                    .and_then(|value| value["text"].as_str().map(|text| text.trim().to_owned()))
                    .unwrap_or_default();
                (api.destroy_result_json)(json);
                value
            };
            (api.destroy_stream)(stream);
            result
        }
    }

    /// Transcribe every phrase the detector has closed; true when the transcript grew.
    fn drain(&self, dictation: &mut ParakeetDictation) -> bool {
        let api = &self.api;
        let mut grew = false;
        // SAFETY: `dictation.vad` is a live detector; each front segment is copied out and
        // destroyed before the detector pops it.
        while unsafe { (api.vad_empty)(dictation.vad) } == 0 {
            // SAFETY: as above; the segment's sample pointer covers `n` floats until it is destroyed.
            let samples = unsafe {
                let segment = (api.vad_front)(dictation.vad);
                let samples = if segment.is_null() || (*segment).samples.is_null() {
                    Vec::new()
                } else {
                    std::slice::from_raw_parts((*segment).samples, (*segment).n.max(0) as usize)
                        .to_vec()
                };
                if !segment.is_null() {
                    (api.destroy_segment)(segment);
                }
                (api.vad_pop)(dictation.vad);
                samples
            };
            let phrase = self.transcribe(&samples);
            if !phrase.is_empty() {
                dictation.phrases.push(phrase);
                grew = true;
            }
        }
        grew
    }
}

impl Transcriber for Parakeet {
    type Dictation = ParakeetDictation;

    fn open(&self) -> Option<ParakeetDictation> {
        // SAFETY: `self.vad` and the model path it points to live as long as `self`.
        let vad = unsafe { (self.api.create_vad)(&self.vad, VAD_BUFFER_SECONDS) };
        (!vad.is_null()).then(|| ParakeetDictation {
            vad,
            destroy: self.api.destroy_vad,
            phrases: Vec::new(),
        })
    }

    fn push(&self, dictation: &mut ParakeetDictation, samples: &[f32]) -> Option<String> {
        // SAFETY: a live detector; `samples` outlives the call, which copies them.
        unsafe { (self.api.vad_accept)(dictation.vad, samples.as_ptr(), samples.len() as i32) };
        self.drain(dictation).then(|| dictation.phrases.join(" "))
    }

    fn finish(&self, mut dictation: ParakeetDictation) -> String {
        // SAFETY: a live detector; flushing closes the phrase in progress.
        unsafe { (self.api.vad_flush)(dictation.vad) };
        self.drain(&mut dictation);
        dictation.phrases.join(" ")
    }
}

#[cfg(test)]
mod tests {
    // Test code may unwrap (the standard's `allow-unwrap-in-tests`).
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// Treats every 1600 samples as one phrase named by its count.
    struct Counting;
    impl Transcriber for Counting {
        type Dictation = (usize, Vec<String>);
        fn open(&self) -> Option<Self::Dictation> {
            Some((0, Vec::new()))
        }
        fn push(&self, d: &mut Self::Dictation, samples: &[f32]) -> Option<String> {
            d.0 += samples.len();
            let before = d.1.len();
            while d.0 >= 1600 {
                d.0 -= 1600;
                d.1.push(format!("Phrase {}.", d.1.len() + 1));
            }
            (d.1.len() > before).then(|| d.1.join(" "))
        }
        fn finish(&self, d: Self::Dictation) -> String {
            d.1.join(" ")
        }
    }

    fn replies(input: &str) -> Vec<Value> {
        let mut out = Vec::new();
        serve(&Counting, input.as_bytes(), &mut out).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn pcm_becomes_unit_range_samples() {
        let pcm: Vec<u8> = [0i16, 16384, -32768, i16::MAX]
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .chain([7])
            .collect();
        assert_eq!(samples(&pcm), vec![0.0, 0.5, -1.0, 32767.0 / 32768.0]);
    }

    #[test]
    fn dictations_are_transcribed_apart_and_unknown_ones_answer_finish() {
        let frame = STANDARD.encode(vec![0u8; 3200]);
        let audio = |id: &str| format!("{}\n", json!({"op": "audio", "id": id, "pcm": frame}));
        let input = [
            json!({"op": "open", "id": "a"}).to_string() + "\n",
            json!({"op": "open", "id": "b"}).to_string() + "\n",
            audio("a"),
            audio("b"),
            audio("a"),
            "not json\n".into(),
            audio("closed"),
            json!({"op": "cancel", "id": "b"}).to_string() + "\n",
            audio("b"),
            json!({"op": "finish", "id": "a"}).to_string() + "\n",
            json!({"op": "finish", "id": "b"}).to_string() + "\n",
        ]
        .concat();
        assert_eq!(
            replies(&input),
            vec![
                json!({"ready": true}),
                json!({"id": "a", "text": "Phrase 1."}),
                json!({"id": "b", "text": "Phrase 1."}),
                json!({"id": "a", "text": "Phrase 1. Phrase 2."}),
                json!({"id": "a", "final": "Phrase 1. Phrase 2."}),
                json!({"id": "b", "error": "unknown dictation"}),
            ]
        );
    }

    /// Loads the real library and models when `WEBAGENT_VOICE_ROOT` names a `voice` stage copy
    /// with the model's sample recordings; the broker's test run has none, so it is skipped there.
    #[test]
    fn parakeet_transcribes_recordings_when_installed() {
        let Ok(root) = std::env::var("WEBAGENT_VOICE_ROOT") else {
            return;
        };
        let defaults = crate::settings::VoiceSettings::default();
        let parakeet = Parakeet::load(Path::new(&root), &defaults.dictation).unwrap();
        for (language, word) in [
            ("en", "country"),
            ("de", "Wurst"),
            ("fr", "pays"),
            ("es", "país"),
        ] {
            let wav = Path::new(&root).join(format!("models/parakeet/test_wavs/{language}.wav"));
            let wav = std::fs::read(wav).unwrap();
            let mut dictation = parakeet.open().unwrap();
            for frame in samples(&wav[44..]).chunks(1600) {
                parakeet.push(&mut dictation, frame);
            }
            let text = parakeet.finish(dictation);
            assert!(text.contains(word), "{language}: {text}");
        }
    }
}
