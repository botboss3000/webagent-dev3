//! Voice settings: the live file `crates/modules/media/voice.json` (listed in `live-files.json`).
//! Values are read when they are used, so an edit applies to the next dictation or reply;
//! `threads` and `pause_detection` apply when a model next loads. An invalid edit keeps serving
//! the last good version, and the built-in defaults (the same values as the file) apply only
//! when no good version was ever read.
use serde::{Deserialize, Serialize};

/// The live settings file, relative to the checkout.
pub const LIVE_VOICE: &str = "crates/modules/media/voice.json";

/// Both voice directions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceSettings {
    /// Always 1.
    pub schema_version: u32,
    /// Editing notes for people; ignored.
    #[serde(
        default,
        rename = "_instructions",
        skip_serializing_if = "String::is_empty"
    )]
    pub instructions: String,
    pub dictation: Dictation,
    pub speech: Speech,
}

/// Composer dictation (Parakeet with a Silero pause detector).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dictation {
    /// Unload the model after this many minutes with no open dictation; 0 keeps it loaded.
    pub idle_unload_minutes: u32,
    /// Dictations open at once across all users.
    pub max_concurrent: usize,
    /// A dictation with no audio for this long was abandoned and is dropped.
    pub idle_capture_seconds: u64,
    /// Longest wait for the last phrase after stop.
    pub finish_timeout_seconds: u64,
    /// Longest dictation; the browser stops recording here.
    pub max_recording_seconds: u32,
    /// Decoder threads.
    pub threads: i32,
    pub pause_detection: PauseDetection,
}

/// Silero pause detection, which closes a phrase for transcription.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PauseDetection {
    /// Speech probability above which audio counts as speech.
    pub threshold: f32,
    /// Silence that ends a phrase.
    pub min_silence_seconds: f32,
    /// Shorter speech is ignored.
    pub min_speech_seconds: f32,
    /// Unbroken speech is cut into phrases of at most this length so text keeps coming.
    pub max_phrase_seconds: f32,
    /// Audio before each detected phrase that is transcribed with it, so a soft first word
    /// (heard before the detector is sure of speech) is kept.
    pub lead_in_seconds: f32,
}

/// Agent voice replies (Kokoro).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Speech {
    /// Unload the model after this many minutes without a reply; 0 keeps it loaded.
    pub idle_unload_minutes: u32,
    /// Synthesis threads.
    pub threads: i32,
    /// Most characters of a reply read aloud.
    pub max_chars: usize,
    /// One reply is synthesized at a time; a later one waits at most this long.
    pub queue_wait_seconds: u64,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            schema_version: 1,
            instructions: String::new(),
            dictation: Dictation {
                idle_unload_minutes: 10,
                max_concurrent: 4,
                idle_capture_seconds: 90,
                finish_timeout_seconds: 15,
                max_recording_seconds: 300,
                threads: 2,
                pause_detection: PauseDetection {
                    threshold: 0.5,
                    min_silence_seconds: 0.4,
                    min_speech_seconds: 0.25,
                    max_phrase_seconds: 10.0,
                    lead_in_seconds: 0.5,
                },
            },
            speech: Speech {
                idle_unload_minutes: 10,
                threads: 4,
                max_chars: 1800,
                queue_wait_seconds: 120,
            },
        }
    }
}

impl VoiceSettings {
    /// The live settings, or the defaults when the file has never been readable.
    pub fn live() -> Self {
        webagent_configuration::live::parsed(LIVE_VOICE, 16 * 1024, Self::parse).unwrap_or_default()
    }

    /// Decode and range-check one version of the file.
    pub fn parse(bytes: &[u8]) -> anyhow::Result<Self> {
        let settings: Self = serde_json::from_slice(bytes)?;
        settings.validate().map_err(anyhow::Error::msg)?;
        Ok(settings)
    }

    fn validate(&self) -> Result<(), String> {
        let (d, p, s) = (
            &self.dictation,
            &self.dictation.pause_detection,
            &self.speech,
        );
        let checks = [
            (self.schema_version == 1, "schema_version must be 1"),
            (
                d.idle_unload_minutes <= 1440,
                "dictation.idle_unload_minutes is 0-1440",
            ),
            (
                (1..=16).contains(&d.max_concurrent),
                "dictation.max_concurrent is 1-16",
            ),
            (
                (10..=600).contains(&d.idle_capture_seconds),
                "dictation.idle_capture_seconds is 10-600",
            ),
            (
                (5..=120).contains(&d.finish_timeout_seconds),
                "dictation.finish_timeout_seconds is 5-120",
            ),
            (
                (10..=900).contains(&d.max_recording_seconds),
                "dictation.max_recording_seconds is 10-900",
            ),
            ((1..=8).contains(&d.threads), "dictation.threads is 1-8"),
            (
                (0.05..=0.95).contains(&p.threshold),
                "pause_detection.threshold is 0.05-0.95",
            ),
            (
                (0.1..=3.0).contains(&p.min_silence_seconds),
                "pause_detection.min_silence_seconds is 0.1-3",
            ),
            (
                (0.05..=2.0).contains(&p.min_speech_seconds),
                "pause_detection.min_speech_seconds is 0.05-2",
            ),
            (
                (2.0..=30.0).contains(&p.max_phrase_seconds),
                "pause_detection.max_phrase_seconds is 2-30",
            ),
            (
                (0.0..=2.0).contains(&p.lead_in_seconds),
                "pause_detection.lead_in_seconds is 0-2",
            ),
            (
                s.idle_unload_minutes <= 1440,
                "speech.idle_unload_minutes is 0-1440",
            ),
            ((1..=8).contains(&s.threads), "speech.threads is 1-8"),
            (
                (100..=8000).contains(&s.max_chars),
                "speech.max_chars is 100-8000",
            ),
            (
                (5..=600).contains(&s.queue_wait_seconds),
                "speech.queue_wait_seconds is 5-600",
            ),
        ];
        match checks.iter().find(|(ok, _)| !ok) {
            Some((_, problem)) => Err((*problem).to_owned()),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    // Test code may unwrap (the standard's `allow-unwrap-in-tests`).
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_live_file_holds_the_defaults() {
        let bytes = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/voice.json")).unwrap();
        let mut file = VoiceSettings::parse(&bytes).unwrap();
        file.instructions.clear();
        assert_eq!(file, VoiceSettings::default());
    }

    #[test]
    fn out_of_range_and_unknown_fields_are_rejected() {
        let good = serde_json::to_value(VoiceSettings::default()).unwrap();
        let mut zero_threads = good.clone();
        zero_threads["speech"]["threads"] = 0.into();
        let mut unknown = good.clone();
        unknown["dictation"]["spare"] = true.into();
        let mut version = good.clone();
        version["schema_version"] = 2.into();
        for bad in [zero_threads, unknown, version] {
            assert!(
                VoiceSettings::parse(bad.to_string().as_bytes()).is_err(),
                "{bad}"
            );
        }
        assert!(VoiceSettings::parse(good.to_string().as_bytes()).is_ok());
    }
}
