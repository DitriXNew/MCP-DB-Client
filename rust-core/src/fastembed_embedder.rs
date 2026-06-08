//! Real, production [`Embedder`] backed by **fastembed** (ONNX Runtime + the
//! HuggingFace tokenizer).
//!
//! This module is compiled **only** under the `fastembed` cargo feature. Without
//! that feature the crate never pulls in `ort`/`tokenizers`, so the default
//! `cargo test` stays pure-Rust and fast (mock-only). The real model is selected
//! at runtime by `configure` (see `core::configure`) when the caller asks for it.
//!
//! ## Why two model instances (the deliberate architecture)
//!
//! fastembed's embedding call is **`TextEmbedding::embed(&mut self, …)`** — it
//! requires `&mut self` (the ONNX session is mutated per inference). Our
//! [`Embedder`] trait, by contrast, is `&self + Send + Sync`, because the
//! embedder is shared (`Arc<dyn Embedder>`) and called concurrently from the
//! background ingest worker *and* from query threads. To bridge `&self` →
//! `&mut self` we must wrap each `TextEmbedding` in a `Mutex`.
//!
//! A single shared `Mutex<TextEmbedding>` would mean a long **bulk reindex**
//! (embedding thousands of passages on the worker thread) holds the lock and
//! **blocks every incoming query** for the whole reindex — exactly the UI freeze
//! the async architecture exists to avoid. So we deliberately load **two
//! separate model instances**:
//!
//!   * `bulk`  — locked by [`FastEmbedder::embed_passages`] (the worker's path);
//!   * `query` — locked by [`FastEmbedder::embed_query`] (the latency path).
//!
//! Passage embedding and query embedding therefore never contend on the same
//! lock: a multi-minute reindex on `bulk` leaves query latency untouched. The
//! cost is a second copy of the model in memory; the e5-small ONNX file is small
//! (int8/quantized-friendly, dim 384), so the double load is an acceptable
//! trade for never blocking queries.
//!
//! ## e5 prefixes
//!
//! The multilingual-e5 family is trained with asymmetric prefixes: documents are
//! embedded as `"passage: …"` and queries as `"query: …"`. We prepend these here
//! (verified on this machine: a `"query: договор на поставку"` scores ru/en
//! contract passages ~0.86–0.90 and an unrelated sentence ~0.78). Omitting the
//! prefixes measurably degrades ranking, so they are not optional.

use std::sync::Mutex;

use fastembed::{
    EmbeddingModel, InitOptions, InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles,
    UserDefinedEmbeddingModel,
};

use crate::embed::Embedder;

/// e5 query prefix (prepended to every query before embedding).
const QUERY_PREFIX: &str = "query: ";
/// e5 passage prefix (prepended to every document before embedding).
const PASSAGE_PREFIX: &str = "passage: ";

/// Production embedder: two separately-loaded fastembed models behind their own
/// mutexes (see the module docs for the two-instance rationale).
pub struct FastEmbedder {
    /// Model used for bulk passage embedding (the background-worker path). A long
    /// reindex locks only this, leaving `query` free.
    bulk: Mutex<TextEmbedding>,
    /// Model used for low-latency query embedding (the search path).
    query: Mutex<TextEmbedding>,
    /// Output dimensionality, probed once at construction. Bound to the index.
    dim: usize,
}

impl FastEmbedder {
    /// Build a `FastEmbedder` for the built-in **MultilingualE5Small** model.
    ///
    /// fastembed downloads (first run) and caches the ONNX + tokenizer files from
    /// HuggingFace; subsequent runs load from the cache. This is the path the
    /// integration test verifies. Loading happens **twice** (one model per
    /// instance — see module docs); both share the same on-disk cache, so only
    /// the first incurs a download.
    ///
    /// Returns a descriptive error string (never panics) so the FFI boundary can
    /// surface a clean structural error if model load fails (e.g. offline with a
    /// cold cache, or onnxruntime not loadable).
    pub fn new_builtin() -> Result<Self, String> {
        // Two independent loads of the SAME model (see module docs: this is what
        // decouples bulk-reindex latency from query latency).
        let bulk = Self::load_builtin().map_err(|e| format!("load bulk model: {e}"))?;
        let query = Self::load_builtin().map_err(|e| format!("load query model: {e}"))?;

        let mut me = FastEmbedder {
            bulk: Mutex::new(bulk),
            query: Mutex::new(query),
            // Provisional; replaced by the probe below. The built-in e5-small is
            // 384-dim, but we probe rather than hard-code so the value is always
            // truthful even if the model file changes.
            dim: 0,
        };
        me.dim = me.probe_dim()?;
        Ok(me)
    }

    /// Build a `FastEmbedder` from local ONNX + tokenizer files at `model_path`
    /// (the **offline / air-gapped** path, §11.4). `model_path` is a directory
    /// laid out like the fastembed HF cache snapshot:
    ///
    /// ```text
    ///   <model_path>/onnx/model.onnx        (or <model_path>/model.onnx)
    ///   <model_path>/tokenizer.json
    ///   <model_path>/config.json
    ///   <model_path>/special_tokens_map.json
    ///   <model_path>/tokenizer_config.json
    /// ```
    ///
    /// Pooling is set to `Mean` (what multilingual-e5 uses); change this if you
    /// point it at a CLS-pooled model. NOTE: this path is implemented but, unlike
    /// `new_builtin`, is **not exercised by the integration test** (it needs a
    /// pre-staged model directory). It is here so production can run offline.
    pub fn new_local(model_path: &str) -> Result<Self, String> {
        let bulk = Self::load_local(model_path).map_err(|e| format!("load bulk model: {e}"))?;
        let query = Self::load_local(model_path).map_err(|e| format!("load query model: {e}"))?;

        let mut me = FastEmbedder {
            bulk: Mutex::new(bulk),
            query: Mutex::new(query),
            dim: 0,
        };
        me.dim = me.probe_dim()?;
        Ok(me)
    }

    /// Load one built-in MultilingualE5Small instance (download/cache via HF).
    fn load_builtin() -> Result<TextEmbedding, String> {
        TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::MultilingualE5Small)
                .with_show_download_progress(true),
        )
        .map_err(|e| e.to_string())
    }

    /// Load one user-defined instance from on-disk ONNX + tokenizer files.
    fn load_local(model_path: &str) -> Result<TextEmbedding, String> {
        use std::path::Path;
        let dir = Path::new(model_path);
        let read = |rel: &str| -> Result<Vec<u8>, String> {
            std::fs::read(dir.join(rel)).map_err(|e| format!("read {rel}: {e}"))
        };

        // The ONNX file may live at <dir>/onnx/model.onnx (the HF snapshot layout)
        // or directly at <dir>/model.onnx. Try the nested path first.
        let onnx = match std::fs::read(dir.join("onnx").join("model.onnx")) {
            Ok(bytes) => bytes,
            Err(_) => read("model.onnx")?,
        };

        let tokenizer_files = TokenizerFiles {
            tokenizer_file: read("tokenizer.json")?,
            config_file: read("config.json")?,
            special_tokens_map_file: read("special_tokens_map.json")?,
            tokenizer_config_file: read("tokenizer_config.json")?,
        };

        let model = UserDefinedEmbeddingModel::new(onnx, tokenizer_files)
            // multilingual-e5 is mean-pooled (matches the built-in path).
            .with_pooling(Pooling::Mean);

        TextEmbedding::try_new_from_user_defined(model, InitOptionsUserDefined::new())
            .map_err(|e| e.to_string())
    }

    /// Probe the model's output dimensionality by embedding a single short
    /// passage and measuring the vector length. Done once at construction so
    /// `dim()` is O(1) and always truthful for whatever model was loaded.
    fn probe_dim(&self) -> Result<usize, String> {
        let mut m = self
            .bulk
            .lock()
            .map_err(|_| "embedder mutex poisoned".to_string())?;
        let out = m
            .embed(vec![format!("{PASSAGE_PREFIX}probe")], None)
            .map_err(|e| format!("dim probe embed failed: {e}"))?;
        out.first()
            .map(|v| v.len())
            .ok_or_else(|| "dim probe returned no vector".to_string())
    }

    /// Run a batch through one of the model instances, prepending `prefix` to
    /// each input. On any error (poisoned lock or an ort inference failure) we
    /// return one all-zero vector per input: the ingest/search pipeline already
    /// treats all-zero vectors as "no signal" (skipped, never indexed, never a
    /// hit), so a transient embed failure degrades gracefully instead of
    /// panicking across the FFI boundary.
    fn run(model: &Mutex<TextEmbedding>, prefix: &str, texts: &[String], dim: usize) -> Vec<Vec<f32>> {
        let prefixed: Vec<String> = texts.iter().map(|t| format!("{prefix}{t}")).collect();
        let mut guard = match model.lock() {
            Ok(g) => g,
            Err(_) => return vec![vec![0.0; dim]; texts.len()],
        };
        // `None` batch_size lets fastembed use its default (256).
        match guard.embed(prefixed, None) {
            Ok(vecs) => vecs,
            Err(_) => vec![vec![0.0; dim]; texts.len()],
        }
    }
}

impl Embedder for FastEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed_passages(&self, texts: &[String]) -> Vec<Vec<f32>> {
        // Worker path: lock `bulk` only — never contends with queries.
        Self::run(&self.bulk, PASSAGE_PREFIX, texts, self.dim)
    }

    fn embed_query(&self, text: &str) -> Vec<f32> {
        // Query path: lock `query` only — never blocked by a bulk reindex.
        let one = [text.to_string()];
        Self::run(&self.query, QUERY_PREFIX, &one, self.dim)
            .into_iter()
            .next()
            .unwrap_or_else(|| vec![0.0; self.dim])
    }
}
