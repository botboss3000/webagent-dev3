//! Media-owned rules without the root App: lanes, capture, utterance routing, speech text and
//! image receipts. Root tests cover HTTP composition, model calls and attachment publication.
// Test code may unwrap (the standard's `allow-unwrap-in-tests`; no clippy.toml carries it yet).
#![allow(clippy::unwrap_used)]
use std::collections::HashMap;

use serde_json::json;
use sha2::{Digest, Sha256};

use crate::capture::{self, Capture, Chunk, SequenceRejected};
use crate::image;
use crate::lanes::{self, LaneError, Lanes};
use crate::speech::speech_text;
use crate::utterance::{self, Control};

fn pcm(sample: i16, samples: usize) -> Vec<u8> {
    (0..samples).flat_map(|_| sample.to_le_bytes()).collect()
}

#[test]
fn removed_voice_lane_cannot_be_recreated_by_an_old_result() {
    let mut all = HashMap::new();
    let old = lanes::get_lane(&mut all, "test", 100)
        .unwrap()
        .generation
        .clone();
    all.remove("test");
    assert!(lanes::existing_lane(&mut all, "test", &old).is_err());
    assert!(all.is_empty());
    let fresh = lanes::get_lane(&mut all, "test", 100)
        .unwrap()
        .generation
        .clone();
    assert_ne!(fresh, old);
    assert_eq!(
        lanes::existing_lane(&mut all, "test", &old).err(),
        Some(LaneError::Gone)
    );
    assert!(lanes::existing_lane(&mut all, "test", &fresh).is_ok());
}

#[test]
fn lane_capacity_refuses_new_callers_but_keeps_existing_and_busy_lanes() {
    let mut all = Lanes::new();
    for index in 0..lanes::MAX_LANES {
        lanes::get_lane(&mut all, &format!("lane-{index}"), 100).unwrap();
    }
    assert_eq!(
        lanes::get_lane(&mut all, "extra", 100).err(),
        Some(LaneError::Busy)
    );
    assert_eq!(
        LaneError::Busy.to_string(),
        "Voice is busy; try again shortly"
    );
    assert!(lanes::get_lane(&mut all, "lane-0", 100).is_ok());
    // Every lane expires except the busy one, which is never dropped while it works.
    all.get_mut("lane-1").unwrap().busy = true;
    let later = 100 + lanes::LANE_TTL_SECS;
    let lane = lanes::get_lane(&mut all, "extra", later).unwrap();
    assert_eq!(lane.expires, later + lanes::LANE_TTL_SECS);
    assert_eq!(all.len(), 2);
    assert!(all.contains_key("lane-1"));
}

#[test]
fn lane_keys_separate_users_and_history_keeps_the_newest_messages() {
    assert_eq!(lanes::lane_key("u", "s"), "u\0s");
    assert_ne!(lanes::lane_key("u", "s"), lanes::lane_key("us", ""));
    let mut lane = lanes::Lane::default();
    for turn in 0..8 {
        lane.remember_exchange(&format!("q{turn}"), &format!("a{turn}"));
    }
    assert_eq!(lane.history.len(), lanes::HISTORY_LIMIT);
    assert_eq!(lane.history[0], json!({"role": "user", "content": "q2"}));
    assert_eq!(
        lane.history[11],
        json!({"role": "assistant", "content": "a7"})
    );
}

#[test]
fn wav_header_describes_sixteen_khz_mono_pcm() {
    let out = capture::wav(&[1, 2, 3, 4]);
    assert_eq!(out.len(), 48);
    assert_eq!(&out[..4], b"RIFF");
    assert_eq!(u32::from_le_bytes(out[4..8].try_into().unwrap()), 40);
    assert_eq!(&out[8..16], b"WAVEfmt ");
    assert_eq!(u32::from_le_bytes(out[24..28].try_into().unwrap()), 16000);
    assert_eq!(u32::from_le_bytes(out[28..32].try_into().unwrap()), 32000);
    assert_eq!(&out[36..40], b"data");
    assert_eq!(u32::from_le_bytes(out[40..44].try_into().unwrap()), 4);
    assert_eq!(&out[44..], &[1, 2, 3, 4]);
}

#[test]
fn frames_must_be_whole_samples_within_bounds() {
    assert!(!capture::valid_frame(&[]));
    assert!(!capture::valid_frame(&[0; 3]));
    assert!(capture::valid_frame(&[0; 2]));
    assert!(capture::valid_frame(&[0; capture::MAX_FRAME_BYTES]));
    assert!(!capture::valid_frame(&[0; capture::MAX_FRAME_BYTES + 2]));
}

#[test]
fn capture_replays_identical_retries_and_rejects_gaps() {
    let mut capture = Capture::new("c".into(), json!({}));
    assert_eq!(capture.accept(2, pcm(0, 16)), Err(SequenceRejected));
    let first = capture.accept(1, pcm(0, 16)).unwrap();
    assert!(matches!(
        first,
        Chunk::Accepted {
            speaking: false,
            turn_end: false,
            partial: None
        }
    ));
    assert_eq!(
        capture.accept(1, pcm(0, 16)),
        Ok(Chunk::Replayed { seq: 1 })
    );
    // Same number, different bytes is not a replay.
    assert_eq!(capture.accept(1, pcm(5, 16)), Err(SequenceRejected));
    assert!(capture.accept(2, pcm(0, 16)).is_ok());
}

#[test]
fn speech_starts_on_voiced_audio_and_the_turn_ends_after_silence() {
    let mut capture = Capture::new("c".into(), json!({}));
    // 4000 bytes is 125 ms, enough voiced time to start speech.
    let chunk = capture.accept(1, pcm(8000, 2000)).unwrap();
    assert!(matches!(
        chunk,
        Chunk::Accepted {
            speaking: true,
            turn_end: false,
            ..
        }
    ));
    assert!(capture.speaking());
    let mut seq = 1;
    let mut ended = false;
    // 1024-byte frames are 32 ms; 700 ms of silence takes 22 of them.
    for _ in 0..22 {
        seq += 1;
        let Chunk::Accepted { turn_end, .. } = capture.accept(seq, pcm(0, 512)).unwrap() else {
            panic!("a new frame is accepted");
        };
        ended = turn_end;
    }
    assert!(ended);
}

#[test]
fn partials_arrive_every_three_seconds_and_the_length_limit_ends_the_turn() {
    let mut capture = Capture::new("c".into(), json!({}));
    let frame = pcm(8000, 16000);
    let mut partials = 0;
    let mut seq = 0;
    loop {
        seq += 1;
        match capture.accept(seq, frame.clone()) {
            Ok(Chunk::Accepted {
                turn_end, partial, ..
            }) => {
                if let Some(wav) = partial {
                    assert_eq!(wav.len(), seq as usize * frame.len() + 44);
                    partials += 1;
                }
                if turn_end {
                    break;
                }
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(seq as usize * frame.len(), capture::MAX_RECORDING_BYTES);
    assert_eq!(partials, 9);
    assert_eq!(capture.accept(seq + 1, pcm(0, 1)), Err(SequenceRejected));
    assert_eq!(capture.wav().len(), capture::MAX_RECORDING_BYTES + 44);
}

#[test]
fn utterances_classify_confirmations_controls_and_requests() {
    let text = utterance::words("  Go ahead! ");
    assert_eq!(text, "go ahead");
    assert!(utterance::yes(&text));
    assert!(utterance::no(&utterance::words("Never mind.")));
    assert!(!utterance::yes("yes please"));

    assert_eq!(utterance::control("stop"), Some(Control::Cancel));
    assert_eq!(
        utterance::control("please pause the run"),
        Some(Control::Cancel)
    );
    assert_eq!(
        utterance::control("continue the task"),
        Some(Control::Resume)
    );
    assert_eq!(utterance::control("hold on"), Some(Control::Cancel));
    assert_eq!(utterance::control("stop the music"), None);
    assert_eq!(utterance::control(""), None);
    assert_eq!(Control::Resume.as_str(), "resume");

    assert!(utterance::work("please build a page"));
    assert!(utterance::work("could you please fix it"));
    assert!(!utterance::work("tell me a joke"));
    assert!(utterance::question("how does it work"));
    assert!(utterance::question("tell me more"));
    assert!(!utterance::question("build it"));

    assert!(utterance::status_query("what's the status"));
    assert!(utterance::status_query("are you done"));
    assert!(!utterance::status_query("are you sure"));
    assert!(utterance::modifies_work("use postgres instead"));
    assert!(!utterance::modifies_work("thanks"));
}

#[test]
fn speech_text_omits_code_links_and_secrets() {
    let fence = "\u{60}\u{60}\u{60}";
    let text = format!(
        "Run {fence}rm -rf /{fence} then see https://example.test now. \
         Bearer abc123 sk-0123456789abc api_key=x token=y sk-short"
    );
    assert_eq!(
        speech_text(&text),
        "Run Code omitted. then see link omitted now. [redacted] [redacted] [redacted] \
         [redacted] [redacted] sk-short"
    );
    assert_eq!(speech_text("  a \n b  "), "a b");
    assert_eq!(speech_text(&"x".repeat(5000)).chars().count(), 1800);
}

#[test]
fn image_receipts_fix_the_attachment_identity() {
    let receipt = image::receipt_key("s", "r", "p");
    let expected_digest = hex::encode(Sha256::digest(br#"["r","p"]"#));
    assert_eq!(receipt, format!("receipt:core_image:s:{expected_digest}"));
    let id = image::attachment_id(&receipt);
    let hash = Sha256::digest(serde_json::to_vec(&json!(receipt)).unwrap());
    assert_eq!(id, uuid::Uuid::from_slice(&hash[..16]).unwrap().to_string());
    assert_eq!(id, image::attachment_id(&receipt));
    assert_ne!(id, image::attachment_id("other"));

    assert!(image::prompt_is_valid("a cat"));
    assert!(!image::prompt_is_valid("   "));
    assert!(image::prompt_is_valid(&"x".repeat(image::MAX_PROMPT_BYTES)));
    assert!(!image::prompt_is_valid(
        &"x".repeat(image::MAX_PROMPT_BYTES + 1)
    ));
    assert_eq!(
        image::not_sent("no"),
        json!({"status": "error", "output": "no"})
    );
    assert_eq!(
        image::uncertain("k", "m"),
        json!({"status": "uncertain", "uncertain": true, "output": {"receipt": "k", "message": "m"}})
    );
    assert_eq!(
        image::generated_filename("image/png"),
        Some("generated.png")
    );
    assert_eq!(
        image::generated_filename("image/jpeg"),
        Some("generated.jpg")
    );
    assert_eq!(image::generated_filename("image/svg+xml"), None);
}
