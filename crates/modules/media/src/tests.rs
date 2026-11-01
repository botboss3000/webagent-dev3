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
use crate::speech::{sample_text, speech_text, voice_catalog, VoiceOutput, DEFAULT_VOICE, VOICES};
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
fn frames_must_be_whole_samples_within_bounds() {
    assert!(!capture::valid_frame(&[]));
    assert!(!capture::valid_frame(&[0; 3]));
    assert!(capture::valid_frame(&[0; 2]));
    assert!(capture::valid_frame(&[0; capture::MAX_FRAME_BYTES]));
    assert!(!capture::valid_frame(&[0; capture::MAX_FRAME_BYTES + 2]));
}

#[test]
fn capture_replays_identical_retries_and_rejects_gaps() {
    let mut capture = Capture::new("c".into());
    assert_eq!(capture.accept(2, &pcm(0, 16)), Err(SequenceRejected));
    assert_eq!(capture.accept(1, &pcm(0, 16)), Ok(Chunk::Accepted));
    assert_eq!(
        capture.accept(1, &pcm(0, 16)),
        Ok(Chunk::Replayed { seq: 1 })
    );
    // Same number, different bytes is not a replay.
    assert_eq!(capture.accept(1, &pcm(5, 16)), Err(SequenceRejected));
    assert_eq!(capture.accept(2, &pcm(0, 16)), Ok(Chunk::Accepted));
}

#[test]
fn a_dictation_stops_at_the_length_limit() {
    let mut capture = Capture::new("c".into());
    let frame = pcm(8000, 16000);
    let mut seq = 0;
    while capture.accept(seq + 1, &frame).is_ok() {
        seq += 1;
    }
    assert_eq!(seq as usize * frame.len(), capture::MAX_RECORDING_BYTES);
    assert_eq!(capture.accept(seq + 1, &pcm(0, 1)), Err(SequenceRejected));
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
    for spoken in [
        "Cancel.",
        "Please, cancel the task.",
        "Stop, please.",
        "OK, stop.",
        "stop it",
        "Hold on a second.",
        "Wait a second.",
        "Stop\u{2026}",
    ] {
        let text = utterance::words(spoken);
        assert_eq!(utterance::control(&text), Some(Control::Cancel), "{spoken}");
    }
    assert_eq!(
        utterance::control(&utterance::words("Okay, continue the run!")),
        Some(Control::Resume)
    );
    for spoken in [
        "don't stop",
        "Don\u{2019}t stop.",
        "stop the music in the song lyrics you wrote",
        "wait",
        "ok",
        "hold onto it",
    ] {
        assert_eq!(
            utterance::control(&utterance::words(spoken)),
            None,
            "{spoken}"
        );
    }
    // A lone "cancel" is also a rejection; voice_route lets a pending proposal take it first.
    assert!(utterance::no(&utterance::words("Cancel.")));
    assert_eq!(utterance::words("OK, stop."), "ok stop");
    assert!(utterance::yes(&utterance::words("OK!")));
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
         [redacted] [redacted] sk-short."
    );
    assert_eq!(speech_text("  a \n b  "), "a. b.");
    assert_eq!(speech_text("Done!\nNext: step\n\n"), "Done! Next: step.");
    let tick = "\u{60}";
    let markdown = format!(
        "# Results\n\n## **Bold** heading\n- Run {tick}cargo test{tick} first\n\
         > quoted [the docs](https://example.test/a) and <https://example.test/x>\n\
         * keep [brackets] and a < b"
    );
    assert_eq!(
        speech_text(&markdown),
        "Results. Bold heading. Run cargo test first. quoted the docs and link omitted. \
         keep [brackets] and a < b."
    );
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

#[test]
fn voice_output_accepts_only_listed_voices_and_exact_fields() {
    assert_eq!(
        VoiceOutput::from_value(&json!(null)).unwrap(),
        VoiceOutput::default()
    );
    assert!(!VoiceOutput::default().enabled);
    let saved = VoiceOutput::from_value(&json!({"enabled": true, "voice": "bm_george"})).unwrap();
    assert_eq!(saved.voice, "bm_george");
    assert_eq!(VoiceOutput::from_value(&saved.to_value()).unwrap(), saved);
    for bad in [
        json!({"enabled": true, "voice": "zf_xiaobei"}),
        json!({"enabled": "yes", "voice": "af_sarah"}),
        json!({"enabled": true}),
        json!({"enabled": true, "voice": "af_sarah", "speed": 2}),
        json!("af_sarah"),
    ] {
        assert!(VoiceOutput::from_value(&bad).is_err(), "{bad}");
    }
}

#[test]
fn voice_catalog_lists_every_voice_with_its_group() {
    let catalog = voice_catalog();
    assert_eq!(catalog.as_array().unwrap().len(), VOICES.len());
    assert!(VOICES.iter().any(|(id, _)| *id == DEFAULT_VOICE));
    assert_eq!(catalog[0]["group"], "American female");
    assert_eq!(catalog[VOICES.len() - 1]["group"], "British male");
    assert!(sample_text("bf_emma").unwrap().contains("Emma"));
    assert_eq!(sample_text("../etc"), None);
}
