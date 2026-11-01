//! Streamed dictation capture: 16 kHz mono 16-bit PCM frames with replay-safe sequencing and a
//! length limit. The host streams accepted frames to the local speech recognizer.
use sha2::{Digest, Sha256};

/// Capture sample rate in Hz.
pub const SAMPLE_RATE: u32 = 16000;
/// Largest accepted PCM frame in bytes.
pub const MAX_FRAME_BYTES: usize = 64 * 1024;
/// Longest dictation: five minutes of PCM plus ten seconds of slack for client timer drift.
pub const MAX_RECORDING_BYTES: usize = 16000 * 2 * 310;

/// True when `frame` is a non-empty whole number of samples within [`MAX_FRAME_BYTES`].
pub fn valid_frame(frame: &[u8]) -> bool {
    !frame.is_empty() && frame.len().is_multiple_of(2) && frame.len() <= MAX_FRAME_BYTES
}

/// One dictation in progress.
pub struct Capture {
    /// Client-visible capture identity.
    pub id: String,
    seq: u64,
    last_hash: String,
    bytes: usize,
}

/// Result of offering one frame to a capture.
#[derive(Debug, PartialEq, Eq)]
pub enum Chunk {
    /// The frame repeats the last accepted one; nothing changed.
    Replayed {
        /// The last accepted sequence number.
        seq: u64,
    },
    /// The frame is new and must be passed on.
    Accepted,
}

/// A frame out of sequence, or one that would exceed [`MAX_RECORDING_BYTES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceRejected;

impl Capture {
    /// An empty capture.
    pub fn new(id: String) -> Self {
        Self {
            id,
            seq: 0,
            last_hash: String::new(),
            bytes: 0,
        }
    }

    /// Offer frame `sequence`; the caller has already checked it with [`valid_frame`]. An
    /// identical retry of the last frame is replayed, anything else must be the next number.
    pub fn accept(&mut self, sequence: u64, frame: &[u8]) -> Result<Chunk, SequenceRejected> {
        let hash = hex::encode(Sha256::digest(frame));
        if sequence == self.seq && hash == self.last_hash {
            return Ok(Chunk::Replayed { seq: self.seq });
        }
        if sequence != self.seq + 1 || self.bytes + frame.len() > MAX_RECORDING_BYTES {
            return Err(SequenceRejected);
        }
        self.bytes += frame.len();
        self.seq = sequence;
        self.last_hash = hash;
        Ok(Chunk::Accepted)
    }
}
