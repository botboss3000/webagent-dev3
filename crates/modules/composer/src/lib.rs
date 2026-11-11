//! Composer: the configuration composer's matcher. It maps a plain-language request made on a
//! settings page to one setting from the descriptor catalog (`webagent_configuration::catalog`)
//! and either a proposed value (a card) or a short pick list. Scoring only, no generative model:
//! bge-small-en-v1.5 embeddings rank the page's settings, and plain rules read the value and
//! decide whether the evidence is strong enough to pre-select a card. Every change still needs
//! the user's confirmation; permission, layer writes and current values belong to the caller.
//!
//! The matcher runs as its own process (`webagent-v3 --composer-service`), one JSON request per
//! line on stdin and one reply per line on stdout; see [`service`]. `webagent-v3 --composer-eval
//! FILE...` scores it against the request sets in `eval/`.
pub mod embedder;
pub mod matcher;
pub mod service;
pub mod tokenizer;
pub mod values;

pub use service::{evaluate, load_embedder, serve, Ranker};
