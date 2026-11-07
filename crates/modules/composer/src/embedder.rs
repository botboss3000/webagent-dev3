//! Sentence embeddings from bge-small-en-v1.5 on the CPU: the normalized `[CLS]` vector of the
//! model's last hidden state. ONNX Runtime is the `voice` stage's library, loaded at run time.
use std::fmt::Display;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use ort::environment::Environment;
use ort::session::Session;
use ort::value::Tensor;

use crate::tokenizer::Tokenizer;

/// ONNX Runtime, installed by the `voice` stage of the Dockerfile.
pub const RUNTIME: &str = "/opt/voice/lib/libonnxruntime.so";
/// The model and its vocabulary, installed by the same stage (pinned and hash-checked there).
pub const MODEL_DIR: &str = "/opt/voice/composer/bge-small-en-v1.5";
/// bge's instruction for short queries matched against longer passages.
pub const QUERY_PREFIX: &str = "Represent this sentence for searching relevant passages: ";
/// The matcher answers one request at a time; two threads keep it light on a shared host.
const THREADS: usize = 2;

/// One loaded model.
pub struct Embedder {
    session: Session,
    tokenizer: Tokenizer,
    _environment: Environment,
}

impl Embedder {
    /// Load the runtime library, the model and its vocabulary.
    pub fn load(runtime: &Path, model_dir: &Path) -> Result<Self> {
        let vocab = std::fs::read_to_string(model_dir.join("vocab.txt"))
            .context("Composer vocabulary unavailable")?;
        let environment = ort::init_from(runtime)
            .map_err(failed)?
            .with_name("composer")
            .build()
            .map_err(failed)?;
        let session = Session::builder(&environment)
            .map_err(failed)?
            .with_intra_threads(THREADS)
            .map_err(failed)?
            .commit_from_file(model_dir.join("model.onnx"))
            .map_err(failed)?;
        Ok(Self {
            session,
            tokenizer: Tokenizer::from_vocab(&vocab)?,
            _environment: environment,
        })
    }

    /// The unit-length embedding of `text`.
    pub fn embed(&mut self, text: &str) -> Result<Vec<f32>> {
        let ids = self.tokenizer.encode(text);
        let shape = [1, ids.len()];
        let tensor = |values: Vec<i64>| Tensor::from_array((shape, values)).map_err(failed);
        let inputs = ort::inputs![
            "input_ids" => tensor(ids.clone())?,
            "attention_mask" => tensor(vec![1; ids.len()])?,
            "token_type_ids" => tensor(vec![0; ids.len()])?,
        ];
        let outputs = self.session.run(inputs).map_err(failed)?;
        let (shape, hidden) = outputs[0].try_extract_tensor::<f32>().map_err(failed)?;
        let width = usize::try_from(shape[2]).map_err(failed)?;
        let first = hidden
            .get(..width)
            .ok_or_else(|| anyhow!("Empty embedding"))?;
        let norm = first
            .iter()
            .map(|x| x * x)
            .sum::<f32>()
            .sqrt()
            .max(f32::EPSILON);
        Ok(first.iter().map(|x| x / norm).collect())
    }
}

/// Cosine similarity of two unit vectors.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn failed(error: impl Display) -> anyhow::Error {
    anyhow!("Composer model: {error}")
}
