//! Optional zstd compression of record and file plaintext before encryption.
//!
//! Settings come from `storage.compression` in the live
//! `config/access/policy.json`, reread at most every ten seconds. Readers need no
//! setting: a zstd frame starts with a magic number no JSON text can start with, so compressed and
//! uncompressed records coexist and turning compression off never breaks reads.
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const POLICY: &str = "config/access/policy.json";
const MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];
/// Decompressed size bound; larger than any record or attachment the app admits.
const MAX_PLAIN: usize = 512 * 1024 * 1024;
const REFRESH: Duration = Duration::from_secs(10);
thread_local! {
    // Per thread: tests run in parallel threads, and one test's fixed settings must not
    // leak into another's.
    static FORCED: std::cell::RefCell<Option<Compression>> = const { std::cell::RefCell::new(None) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Off,
    OnSave,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Compression {
    pub mode: Mode,
    /// Only "zstd" today; named so another algorithm can be added without a format change.
    pub algorithm: String,
    /// zstd level 1..=19; low levels are fastest.
    pub level: i32,
    /// Smaller plaintext is stored as is; compression would not pay for its frame.
    pub min_bytes: usize,
    /// Attachment media types (exact, or a `type/*` prefix) stored as is: already compressed.
    pub skip_media_types: Vec<String>,
    /// Level for repacking the rows of sessions idle `compress_idle_days`: slower to write,
    /// as fast to read.
    #[serde(default = "default_idle_level")]
    pub idle_level: i32,
}

fn default_idle_level() -> i32 {
    19
}

impl Default for Compression {
    fn default() -> Self {
        Self {
            mode: Mode::Off,
            algorithm: "zstd".into(),
            level: 3,
            min_bytes: 256,
            skip_media_types: Vec::new(),
            idle_level: default_idle_level(),
        }
    }
}

#[derive(Deserialize)]
struct PolicyFile {
    #[serde(default)]
    compression: Option<Compression>,
}

impl Compression {
    pub fn validate(&self) -> Result<()> {
        if self.algorithm != "zstd" {
            bail!("Unsupported compression algorithm");
        }
        if !(1..=19).contains(&self.level) || !(1..=19).contains(&self.idle_level) {
            bail!("Compression level and idle_level must be 1..19");
        }
        Ok(())
    }
    /// Parse the whole policy file and return its validated compression section.
    pub fn from_policy(bytes: &[u8]) -> Result<Self> {
        let section = serde_json::from_slice::<PolicyFile>(bytes)?
            .compression
            .unwrap_or_default();
        section.validate()?;
        Ok(section)
    }
    /// Read `storage.compression` from the whole policy document.
    pub fn from_document(bytes: &[u8]) -> Result<Self> {
        let mut value: serde_json::Value = serde_json::from_slice(bytes)?;
        let storage = value
            .get_mut("storage")
            .map(serde_json::Value::take)
            .ok_or_else(|| anyhow!("Policy has no storage section"))?;
        Self::from_policy(&serde_json::to_vec(&storage)?)
    }
    /// Fix the settings for this thread, ignoring the policy file. For tests whose
    /// assertions depend on exact stored sizes.
    pub fn force(settings: Self) {
        FORCED.with(|forced| *forced.borrow_mut() = Some(settings));
    }
    /// Current settings; an unreadable policy keeps the last good settings (initially off).
    pub fn current() -> Self {
        if let Some(forced) = FORCED.with(|forced| forced.borrow().clone()) {
            return forced;
        }
        static CURRENT: Mutex<Option<(Instant, Compression)>> = Mutex::new(None);
        let mut current = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, settings)) = current.as_ref() {
            if at.elapsed() < REFRESH {
                return settings.clone();
            }
        }
        let fresh = webagent_configuration::live::parsed_from(
            &webagent_configuration::live::roots(),
            POLICY,
            64 * 1024,
            Self::from_document,
        )
        .ok()
        .or_else(|| current.as_ref().map(|(_, s)| s.clone()))
        .unwrap_or_default();
        *current = Some((Instant::now(), fresh.clone()));
        fresh
    }
    /// Whether attachment bytes of `media` are compressed before sealing.
    pub fn applies_to_media(&self, media: &str) -> bool {
        !self.skip_media_types.iter().any(|skip| {
            skip == media
                || skip
                    .strip_suffix("/*")
                    .is_some_and(|prefix| media.split('/').next() == Some(prefix))
        })
    }
    pub fn on(&self) -> bool {
        self.mode != Mode::Off
    }
    /// Compressed plaintext when on and worthwhile; otherwise the input unchanged.
    pub fn pack(&self, plain: Vec<u8>) -> Result<Vec<u8>> {
        self.pack_with(plain, None, None)
    }
    /// `pack` with an account's dictionary and a level other than the default, as given.
    pub fn pack_with(
        &self,
        plain: Vec<u8>,
        dictionary: Option<crate::dictionary::Id>,
        level: Option<i32>,
    ) -> Result<Vec<u8>> {
        if self.mode == Mode::Off || plain.len() < self.min_bytes {
            return Ok(plain);
        }
        let level = level.unwrap_or(self.level);
        let packed = match dictionary {
            // A dictionary dropped from memory since `current` named it: pack without one.
            Some(id) => crate::dictionary::compress(&plain, &id, level)
                .or_else(|_| -> Result<Vec<u8>> { Ok(zstd::bulk::compress(&plain, level)?) })?,
            None => zstd::bulk::compress(&plain, level)?,
        };
        Ok(if packed.len() < plain.len() {
            packed
        } else {
            plain
        })
    }
}

/// Undo `pack`: zstd frames are decompressed, anything else is returned as is.
pub fn unpack(plain: Vec<u8>) -> Result<Vec<u8>> {
    if plain.starts_with(&crate::dictionary::MAGIC) {
        return crate::dictionary::decompress(&plain, MAX_PLAIN);
    }
    if !plain.starts_with(&MAGIC) {
        return Ok(plain);
    }
    let mut decoder = zstd::stream::read::Decoder::new(plain.as_slice())?;
    let mut out = Vec::new();
    use std::io::Read;
    (&mut decoder)
        .take(MAX_PLAIN as u64 + 1)
        .read_to_end(&mut out)?;
    if out.len() > MAX_PLAIN {
        return Err(anyhow!("Stored record exceeds the decompression bound"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_threshold_and_legacy() {
        let on = Compression {
            mode: Mode::OnSave,
            skip_media_types: vec!["image/*".into(), "application/zip".into()],
            ..Compression::default()
        };
        let text = serde_json::to_vec(&serde_json::json!({"output":"line\n".repeat(400)})).unwrap();
        let packed = on.pack(text.clone()).unwrap();
        assert!(packed.starts_with(&MAGIC) && packed.len() * 5 < text.len());
        assert_eq!(unpack(packed).unwrap(), text);
        // Small, incompressible and legacy plaintext pass through unchanged.
        assert_eq!(on.pack(b"{}".to_vec()).unwrap(), b"{}");
        let noise: Vec<u8> = (0..4096u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
            .collect();
        assert_eq!(unpack(on.pack(noise.clone()).unwrap()).unwrap(), noise);
        assert_eq!(unpack(text.clone()).unwrap(), text);
        assert_eq!(Compression::default().pack(text.clone()).unwrap(), text);
        assert!(!on.applies_to_media("image/png") && !on.applies_to_media("application/zip"));
        assert!(on.applies_to_media("text/plain") && on.applies_to_media("application/pdf"));
    }

    #[test]
    fn policy_section_parses_and_validates() {
        assert_eq!(
            Compression::from_policy(br#"{"tiers":{}}"#).unwrap().mode,
            Mode::Off
        );
        let on = br#"{"compression":{"mode":"on_save","algorithm":"zstd","level":3,"min_bytes":256,"skip_media_types":["image/*"]}}"#;
        assert_eq!(Compression::from_policy(on).unwrap().mode, Mode::OnSave);
        for bad in [
            r#"{"compression":{"mode":"on_save","algorithm":"lz4","level":3,"min_bytes":0,"skip_media_types":[]}}"#,
            r#"{"compression":{"mode":"on_save","algorithm":"zstd","level":0,"min_bytes":0,"skip_media_types":[]}}"#,
            r#"{"compression":{"mode":"sometimes","algorithm":"zstd","level":3,"min_bytes":0,"skip_media_types":[]}}"#,
        ] {
            assert!(Compression::from_policy(bad.as_bytes()).is_err(), "{bad}");
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        Compression::from_document(&std::fs::read(root.join(POLICY)).unwrap()).unwrap();
    }
}
