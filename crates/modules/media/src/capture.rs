//! Streamed voice capture: 16 kHz mono 16-bit PCM frames, replay-safe sequencing, a simple
//! energy-based speech/turn-end detector and WAV framing for transcription.
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Capture sample rate in Hz.
pub const SAMPLE_RATE: u32 = 16000;
/// Largest accepted PCM frame in bytes.
pub const MAX_FRAME_BYTES: usize = 64 * 1024;
/// Longest streamed recording: 30 seconds of PCM.
pub const MAX_RECORDING_BYTES: usize = 16000 * 2 * 30;
/// A partial transcript is offered after each further 3 seconds of speech.
const PARTIAL_INTERVAL_BYTES: usize = 16000 * 2 * 3;
/// Frame RMS at or above this level counts as voiced.
const VOICED_RMS: f64 = 0.018;
/// Voiced time that starts speech.
const SPEECH_START_MS: f64 = 120.;
/// Silence after speech that ends the turn.
const TURN_END_SILENCE_MS: f64 = 700.;

/// True when `frame` is a non-empty whole number of samples within [`MAX_FRAME_BYTES`].
pub fn valid_frame(frame: &[u8]) -> bool {
    !frame.is_empty() && frame.len().is_multiple_of(2) && frame.len() <= MAX_FRAME_BYTES
}

/// Wrap mono 16-bit PCM at [`SAMPLE_RATE`] in a 44-byte WAV header.
pub fn wav(pcm: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pcm.len() + 44);
    out.extend(b"RIFF");
    out.extend(((pcm.len() + 36) as u32).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(SAMPLE_RATE.to_le_bytes());
    out.extend(32000u32.to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend((pcm.len() as u32).to_le_bytes());
    out.extend(pcm);
    out
}

/// One recording in progress.
pub struct Capture {
    /// Client-visible capture identity.
    pub id: String,
    /// The pinned voice lane run used to transcribe this capture.
    pub run: Value,
    seq: u64,
    last_hash: String,
    bytes: Vec<u8>,
    speaking: bool,
    voiced_ms: f64,
    silence_ms: f64,
    partial_bytes: usize,
}

/// Result of offering one frame to a capture.
#[derive(Debug, PartialEq)]
pub enum Chunk {
    /// The frame repeats the last accepted one; nothing changed.
    Replayed {
        /// The last accepted sequence number.
        seq: u64,
    },
    /// The frame was appended.
    Accepted {
        /// Whether speech has started.
        speaking: bool,
        /// Whether the turn ended (silence after speech, or the length limit).
        turn_end: bool,
        /// WAV of everything so far when a partial transcript is due.
        partial: Option<Vec<u8>>,
    },
}

/// A frame out of sequence, or one that would exceed [`MAX_RECORDING_BYTES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceRejected;

impl Capture {
    /// An empty capture for `run`.
    pub fn new(id: String, run: Value) -> Self {
        Self {
            id,
            run,
            seq: 0,
            last_hash: String::new(),
            bytes: vec![],
            speaking: false,
            voiced_ms: 0.,
            silence_ms: 0.,
            partial_bytes: 0,
        }
    }

    /// Whether speech has started.
    pub fn speaking(&self) -> bool {
        self.speaking
    }

    /// The whole recording so far as WAV.
    pub fn wav(&self) -> Vec<u8> {
        wav(&self.bytes)
    }

    /// Offer frame `sequence`; the caller has already checked it with [`valid_frame`]. An
    /// identical retry of the last frame is replayed, anything else must be the next number.
    pub fn accept(&mut self, sequence: u64, frame: Vec<u8>) -> Result<Chunk, SequenceRejected> {
        let hash = hex::encode(Sha256::digest(&frame));
        if sequence == self.seq && hash == self.last_hash {
            return Ok(Chunk::Replayed { seq: self.seq });
        }
        if sequence != self.seq + 1 || self.bytes.len() + frame.len() > MAX_RECORDING_BYTES {
            return Err(SequenceRejected);
        }
        let ms = frame.len() as f64 / 32.;
        let rms = (frame
            .chunks_exact(2)
            .map(|v| {
                let sample = i16::from_le_bytes([v[0], v[1]]) as f64 / 32768.;
                sample * sample
            })
            .sum::<f64>()
            / (frame.len() / 2) as f64)
            .sqrt();
        if !self.speaking {
            self.voiced_ms = if rms >= VOICED_RMS {
                self.voiced_ms + ms
            } else {
                0.
            };
            if self.voiced_ms >= SPEECH_START_MS {
                self.speaking = true;
            }
        } else {
            self.silence_ms = if rms >= VOICED_RMS {
                0.
            } else {
                self.silence_ms + ms
            };
        }
        self.bytes.extend(frame);
        self.seq = sequence;
        self.last_hash = hash;
        let turn_end = self.speaking && self.silence_ms >= TURN_END_SILENCE_MS
            || self.bytes.len() >= MAX_RECORDING_BYTES;
        let partial = if self.speaking
            && !turn_end
            && self.bytes.len().saturating_sub(self.partial_bytes) >= PARTIAL_INTERVAL_BYTES
        {
            self.partial_bytes = self.bytes.len();
            Some(wav(&self.bytes))
        } else {
            None
        };
        Ok(Chunk::Accepted {
            speaking: self.speaking,
            turn_end,
            partial,
        })
    }
}
