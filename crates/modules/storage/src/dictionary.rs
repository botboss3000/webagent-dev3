//! Per-account compression dictionaries. A dictionary trained on one account's own records
//! lets each small record compress against what records share (field names, tool output
//! shapes, repeated text): about a quarter smaller than plain zstd on measured data.
//!
//! Each account's dictionaries are files in `user_data/<user>/dictionaries/`, sealed with the
//! installation key and bound to the account. A record packed with one starts with `MAGIC`
//! and the dictionary's 8-byte id, so readers load the right one on demand, in either
//! process. Old dictionaries are kept: records written with them stay readable, each is about
//! 110 KB, and a new one is trained only after the account's records grow fourfold.
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

/// Starts plaintext packed with a dictionary; JSON text and zstd frames never start with it.
pub const MAGIC: [u8; 4] = *b"WDz1";
/// Trained dictionary size: zstd's usual default.
pub const DICTIONARY_BYTES: usize = 112 * 1024;
const MARKER: &str = "current.json";
/// Each process rereads an account's current dictionary this often, so one trained in the
/// other process is adopted for writes within a minute.
const REFRESH: Duration = Duration::from_secs(60);

pub type Id = [u8; 8];

/// A record names a dictionary this process has not loaded.
#[derive(Debug)]
pub struct Missing(pub Id);
impl std::fmt::Display for Missing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Compression dictionary {} is not loaded", hex(&self.0))
    }
}
impl std::error::Error for Missing {}

struct Loaded {
    raw: Arc<Vec<u8>>,
    decoder: Arc<zstd::dict::DecoderDictionary<'static>>,
}

fn loaded() -> &'static RwLock<HashMap<Id, Loaded>> {
    static LOADED: OnceLock<RwLock<HashMap<Id, Loaded>>> = OnceLock::new();
    LOADED.get_or_init(Default::default)
}

fn current_cache() -> &'static Mutex<HashMap<PathBuf, (Instant, Option<Id>)>> {
    static CURRENT: OnceLock<Mutex<HashMap<PathBuf, (Instant, Option<Id>)>>> = OnceLock::new();
    CURRENT.get_or_init(Default::default)
}

pub fn id_of(raw: &[u8]) -> Id {
    let digest = Sha256::digest(raw);
    let mut id = [0; 8];
    id.copy_from_slice(&digest[..8]);
    id
}

pub fn hex(id: &Id) -> String {
    id.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn parse_hex(text: &str) -> Option<Id> {
    if text.len() != 16 {
        return None;
    }
    let mut id = [0; 8];
    for (index, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(id)
}

pub fn folder(root: &Path, user: &str) -> PathBuf {
    root.join("user_data").join(user).join("dictionaries")
}

/// Binds a sealed dictionary file to its account.
pub fn aad(user: &str) -> Vec<u8> {
    format!("v3-dictionary-v1:{user}").into_bytes()
}

/// Make `raw` usable in this process; returns its id.
pub fn register(raw: Vec<u8>) -> Id {
    let id = id_of(&raw);
    let mut map = loaded().write().unwrap_or_else(|e| e.into_inner());
    map.entry(id).or_insert_with(|| Loaded {
        decoder: Arc::new(zstd::dict::DecoderDictionary::copy(&raw)),
        raw: Arc::new(raw),
    });
    id
}

fn raw(id: &Id) -> Option<Arc<Vec<u8>>> {
    loaded()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(id)
        .map(|entry| entry.raw.clone())
}

/// Load the account's dictionary `id` from its sealed file; `open` unseals the file bytes.
pub fn load(
    root: &Path,
    user: &str,
    id: &Id,
    open: impl Fn(&[u8]) -> Result<Vec<u8>>,
) -> Result<()> {
    if raw(id).is_some() {
        return Ok(());
    }
    let sealed = std::fs::read(folder(root, user).join(format!("{}.zdict", hex(id))))
        .map_err(|_| anyhow!("Compression dictionary {} is missing", hex(id)))?;
    let bytes = open(&sealed)?;
    if id_of(&bytes) != *id {
        return Err(anyhow!(
            "Compression dictionary {} does not match its name",
            hex(id)
        ));
    }
    register(bytes);
    Ok(())
}

/// The account's training marker: `{"id":hex,"rows":n,"created_at":t}`.
pub fn marker(root: &Path, user: &str) -> Option<Value> {
    let bytes = std::fs::read(folder(root, user).join(MARKER)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The account's current dictionary, loaded; `None` before its first training or when its
/// file cannot be read (records are then packed without one).
pub fn current(root: &Path, user: &str, open: impl Fn(&[u8]) -> Result<Vec<u8>>) -> Option<Id> {
    let dir = folder(root, user);
    {
        let cache = current_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, id)) = cache.get(&dir) {
            if at.elapsed() < REFRESH {
                return *id;
            }
        }
    }
    let id = marker(root, user)
        .and_then(|marker| marker["id"].as_str().and_then(parse_hex))
        .filter(|id| load(root, user, id, open).is_ok());
    current_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(dir, (Instant::now(), id));
    id
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    use webagent_platform::fs::OpenOptionsExt;
    let temporary = path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

/// Save a newly trained dictionary, sealed by `seal`, and make it the account's current one.
pub fn install(
    root: &Path,
    user: &str,
    raw: Vec<u8>,
    rows: u64,
    now: u64,
    seal: impl Fn(&[u8]) -> Result<Vec<u8>>,
) -> Result<Id> {
    let dir = folder(root, user);
    std::fs::create_dir_all(&dir)?;
    let id = id_of(&raw);
    write_atomically(&dir.join(format!("{}.zdict", hex(&id))), &seal(&raw)?)?;
    let marker = json!({"id":hex(&id),"rows":rows,"created_at":now});
    write_atomically(&dir.join(MARKER), &serde_json::to_vec(&marker)?)?;
    register(raw);
    current_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(dir, (Instant::now(), Some(id)));
    Ok(id)
}

/// Train a dictionary of at most `bytes` from sample records.
pub fn train(samples: &[Vec<u8>], bytes: usize) -> Result<Vec<u8>> {
    Ok(zstd::dict::from_samples(samples, bytes)?)
}

/// `plain` packed with dictionary `id` at `level`: `MAGIC`, the id, then the zstd frame.
pub fn compress(plain: &[u8], id: &Id, level: i32) -> Result<Vec<u8>> {
    let raw = raw(id).ok_or(Missing(*id))?;
    // Built per call: a prepared encoder holds level-sized tables per account in memory.
    let frame = zstd::bulk::Compressor::with_dictionary(level, &raw)?.compress(plain)?;
    let mut packed = Vec::with_capacity(12 + frame.len());
    packed.extend_from_slice(&MAGIC);
    packed.extend_from_slice(id);
    packed.extend_from_slice(&frame);
    Ok(packed)
}

/// Undo `compress`, refusing output past `max` bytes. A dictionary this process has not
/// loaded is reported as `Missing`, so the account's store can load it and read again.
pub fn decompress(packed: &[u8], max: usize) -> Result<Vec<u8>> {
    if packed.len() < 12 || packed[..4] != MAGIC {
        return Err(anyhow!("Invalid dictionary record"));
    }
    let mut id = [0; 8];
    id.copy_from_slice(&packed[4..12]);
    let decoder = loaded()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(&id)
        .map(|entry| entry.decoder.clone())
        .ok_or(Missing(id))?;
    let mut reader =
        zstd::stream::read::Decoder::with_prepared_dictionary(&packed[12..], &decoder)?;
    let mut out = Vec::new();
    (&mut reader).take(max as u64 + 1).read_to_end(&mut out)?;
    if out.len() > max {
        return Err(anyhow!("Stored record exceeds the decompression bound"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples() -> Vec<Vec<u8>> {
        (0..2000)
            .map(|n| {
                serde_json::to_vec(&json!({"type":"function_call_output","call_id":format!("call_{n}"),
                    "output":format!("{{\"status\":\"ok\",\"path\":\"/code/src/file_{n}.rs\",\"lines\":{n}}}")}))
                .unwrap()
            })
            .collect()
    }

    #[test]
    fn records_round_trip_and_a_missing_dictionary_is_named() {
        let raw = train(&samples(), 4096).unwrap();
        let id = id_of(&raw);
        let record =
            serde_json::to_vec(&json!({"type":"function_call_output","call_id":"call_9999",
            "output":"{\"status\":\"ok\",\"path\":\"/code/src/file_9999.rs\",\"lines\":9999}"}))
            .unwrap();
        // Unknown until registered: the error names the dictionary.
        let unknown = compress(&record, &id, 3).unwrap_err();
        assert!(unknown.downcast_ref::<Missing>().is_some());
        register(raw.clone());
        let packed = compress(&record, &id, 3).unwrap();
        assert!(packed.starts_with(&MAGIC));
        assert!(packed.len() < zstd::bulk::compress(&record, 3).unwrap().len());
        assert_eq!(decompress(&packed, 1 << 20).unwrap(), record);
        assert!(decompress(&packed, 10).is_err());
        let mut other = packed.clone();
        other[4] ^= 0xff;
        assert!(decompress(&other, 1 << 20)
            .unwrap_err()
            .downcast_ref::<Missing>()
            .is_some());
    }

    #[test]
    fn installed_dictionaries_load_from_their_sealed_files() {
        let root = tempfile::tempdir().unwrap();
        // A stand-in seal: reversible, and it proves the file is not stored as is.
        let seal = |bytes: &[u8]| Ok(bytes.iter().map(|b| b ^ 0x5a).collect::<Vec<_>>());
        let raw = train(&samples()[..300], 2048).unwrap();
        let id = install(root.path(), "u1", raw.clone(), 1234, 7, seal).unwrap();
        assert_eq!(marker(root.path(), "u1").unwrap()["rows"], 1234);
        let file =
            std::fs::read(folder(root.path(), "u1").join(format!("{}.zdict", hex(&id)))).unwrap();
        assert_ne!(file, raw);
        assert_eq!(current(root.path(), "u1", seal), Some(id));
        assert_eq!(current(root.path(), "u2", seal), None);
        // A file whose bytes do not match its name is refused.
        let wrong = [9u8; 8];
        std::fs::write(
            folder(root.path(), "u1").join(format!("{}.zdict", hex(&wrong))),
            &file,
        )
        .unwrap();
        assert!(load(root.path(), "u1", &wrong, seal).is_err());
        assert_eq!(parse_hex(&hex(&id)), Some(id));
    }
}
