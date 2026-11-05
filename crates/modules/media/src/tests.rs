//! Media-owned rules without the root App: capture, speech text and image receipts. Root tests
//! cover HTTP composition, model calls and attachment publication.
// Test code may unwrap (the standard's `allow-unwrap-in-tests`; no clippy.toml carries it yet).
#![allow(clippy::unwrap_used)]
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::capture::{self, Capture, Chunk, SequenceRejected};
use crate::image;
use crate::speech::{
    sample_text, speech_chunks, voice_catalog, VoiceOutput, DEFAULT_VOICE, VOICES,
};

fn speech_text(text: &str) -> String {
    crate::speech::speech_text(text, 1800)
}

fn pcm(sample: i16, samples: usize) -> Vec<u8> {
    (0..samples).flat_map(|_| sample.to_le_bytes()).collect()
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
    let mut capture = Capture::new("c".into(), capture::recording_limit(300));
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
    let limit = capture::recording_limit(300);
    assert_eq!(limit, 16000 * 2 * 310);
    let mut capture = Capture::new("c".into(), limit);
    let frame = pcm(8000, 16000);
    let mut seq = 0;
    while capture.accept(seq + 1, &frame).is_ok() {
        seq += 1;
    }
    assert_eq!(seq as usize * frame.len(), limit);
    assert_eq!(capture.accept(seq + 1, &pcm(0, 1)), Err(SequenceRejected));
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
    assert_eq!(crate::speech::speech_text("abcdef", 3), "abc");
}

#[test]
fn speech_is_sent_in_sentence_groups_starting_short() {
    let sentence = "This sentence is about forty characters.";
    let text = [sentence; 14].join(" ");
    let chunks = speech_chunks(&text);
    let lengths: Vec<usize> = chunks.iter().map(|c| c.chars().count()).collect();
    // 120 characters fit two sentences plus a space; later groups fit nine.
    assert_eq!(lengths, vec![81, 368, 122]);
    assert_eq!(chunks.join(" "), text);
    assert_eq!(
        speech_chunks("One long sentence without a stop"),
        vec!["One long sentence without a stop"]
    );
    assert_eq!(speech_chunks("Wait:  now?\nYes"), vec!["Wait: now? Yes"]);
    assert!(speech_chunks("").is_empty());
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
