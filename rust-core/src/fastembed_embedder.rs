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
//!   * `bulk`  — a pool of one or more sessions used by the ingest workers:
//!     each worker thread is *pinned* to one session via
//!     [`Embedder::embed_passages_at`] (slot → session), with
//!     [`FastEmbedder::embed_passages`] round-robining as the fallback for
//!     callers that have no stable worker slot;
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

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use fastembed::{
    EmbeddingModel, ExecutionProviderDispatch, InitOptions, InitOptionsUserDefined, Pooling,
    TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
};

use crate::embed::Embedder;

/// e5 query prefix (prepended to every query before embedding).
const QUERY_PREFIX: &str = "query: ";
/// e5 passage prefix (prepended to every document before embedding).
const PASSAGE_PREFIX: &str = "passage: ";

/// Whitelist of built-in models this core can load, as
/// `(canonical wire name, fastembed variant)` pairs — the source of truth for
/// [`resolve_model`]. The first entry is the default
/// ([`crate::core::DEFAULT_MODEL`]).
///
/// The NAMES are duplicated in [`crate::core::SUPPORTED_MODEL_NAMES`] on
/// purpose: that list must exist without this feature compiled in (it powers
/// `list_models` and configure-time validation in the mock/lite build), while
/// the `EmbeddingModel` type only exists here. The
/// `supported_models_match_core_whitelist` test pins the two lists together so
/// they can never drift. All three multilingual-e5 variants share the
/// `query:`/`passage:` prefix convention and mean pooling, so the rest of this
/// module needs no per-model branches.
pub fn supported_models() -> &'static [(&'static str, EmbeddingModel)] {
    &[
        ("multilingual-e5-small", EmbeddingModel::MultilingualE5Small),
        ("multilingual-e5-base", EmbeddingModel::MultilingualE5Base),
        ("multilingual-e5-large", EmbeddingModel::MultilingualE5Large),
    ]
}

/// Resolve a `configure`-time model name to its fastembed variant.
///
///   * empty/whitespace → the default ([`crate::core::DEFAULT_MODEL`]);
///   * otherwise trimmed + ASCII-case-insensitive match against
///     [`supported_models`];
///   * unknown → `Err` with the same `unknown model '<name>'; supported: <list>`
///     message shape as [`crate::core::validate_model_name`] (the dispatcher
///     normally rejects unknown names with `bad_model` before this is reached).
pub fn resolve_model(name: &str) -> Result<EmbeddingModel, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return resolve_model(crate::core::DEFAULT_MODEL);
    }
    let lower = trimmed.to_ascii_lowercase();
    supported_models()
        .iter()
        .find(|(n, _)| *n == lower)
        .map(|(_, m)| m.clone())
        .ok_or_else(|| {
            format!(
                "unknown model '{trimmed}'; supported: {}",
                supported_models()
                    .iter()
                    .map(|(n, _)| *n)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

/// Which onnxruntime execution provider to request for inference.
///
/// ort registers EPs **best-effort**: CPU is the always-present default, so
/// requesting DirectML (`DirectML`/`Auto`) and finding no usable GPU/driver makes
/// ort **log and silently fall back to CPU** — no manual fallback code is needed.
/// `Auto` is the recommended default: try the GPU, transparently degrade to CPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    /// Force CPU: register no GPU EP (empty list ⇒ ort's default CPU EP).
    Cpu,
    /// Request DirectML explicitly (still auto-falls-back to CPU if unavailable).
    DirectML,
    /// Try DirectML, transparently fall back to CPU. Same EP list as `DirectML`;
    /// distinct variant so callers/echoes can tell an explicit `dml` from `auto`.
    Auto,
}

impl Device {
    /// Build the ort execution-provider list this device selects.
    ///
    ///   * `Cpu`              → `vec![]` (empty ⇒ ort's default CPU EP only).
    ///   * `DirectML`/`Auto`  → `vec![DirectML::default().build()]` — registered
    ///     best-effort; ort falls back to CPU automatically if DirectML can't
    ///     initialize (no GPU/driver). fastembed, seeing a DirectML EP, also sets
    ///     the DML-required session opts (memory_pattern + parallel_execution off).
    fn execution_providers(self) -> Vec<ExecutionProviderDispatch> {
        match self {
            Device::Cpu => vec![],
            Device::DirectML | Device::Auto => vec![ort::ep::DirectML::default().build()],
        }
    }
}

/// Per-session ONNX tuning. The two model instances are tuned for *different*
/// workloads, not just split across locks:
///   * **bulk** (background reindex): the configured device (GPU/DirectML when
///     asked) and the bulk of the cores.
///   * **query** (search path): **CPU** + a small intra-op pool. A single short
///     query is latency-bound, and DirectML routinely loses to CPU on it because
///     of host↔device copy overhead; a small thread pool also avoids spin-up cost.
///
/// Thread budgets are sized so `bulk + query ≤ ncpu` (no oversubscription, which
/// is what let reindex and queries fight over cores under the old all-cores-each
/// default).
#[derive(Clone, Copy)]
struct SessionTuning {
    device: Device,
    threads: usize,
}

/// Resolve the bulk worker/session count from the `embed_workers` config knob:
/// `None` ⇒ 1 (single worker, the default); `Some(0)` ⇒ auto — `ncpu/2` clamped
/// to `1..=4`; `Some(n)` ⇒ exactly `n` (≥1).
///
/// Why the auto cap at 4: measured scaling of multi-session CPU embedding
/// saturates around ~1.4× (the workload is memory-bandwidth bound, not
/// compute bound), so on a big machine an uncapped `ncpu/2` would load extra
/// model copies (one per session, RAM cost) for no additional throughput. An
/// explicit `Some(n)` still gets exactly what it asked for.
fn worker_count(embed_workers: Option<u64>) -> usize {
    let ncpu = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    match embed_workers {
        None => 1,
        Some(0) => (ncpu / 2).clamp(1, 4),
        Some(n) => (n as usize).max(1),
    }
}

/// Derive (per-bulk-session, query) tunings for `m_workers` bulk sessions. Query
/// gets `min(2, ncpu)` CPU threads. Each bulk session gets the explicit intra-op
/// override if given, else the remaining cores split across the `m_workers`
/// sessions (`(ncpu - query) / m`, ≥1) — so `m × per_session + query ≈ ncpu`,
/// no oversubscription whether you run one big session or several small ones.
///
/// An explicit `intra_threads` is a *per-session* knob: with `m` sessions the
/// total thread demand is `m × t`, so an innocent-looking `intra_threads: 8`
/// under 4 workers would ask for 32 threads. To keep the no-oversubscription
/// invariant we clamp the explicit value to `(ncpu / m).max(1)` whenever
/// `m > 1`; a single session keeps the historical `min(t, ncpu)` behavior.
fn tunings(
    device: Device,
    intra_threads: Option<u64>,
    m_workers: usize,
) -> (SessionTuning, SessionTuning) {
    let ncpu = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let query_threads = ncpu.min(2).max(1);
    let m = m_workers.max(1);
    let bulk_threads = match intra_threads {
        // Explicit override: single session caps at ncpu; multi-session clamps
        // to the per-session fair share so m sessions never oversubscribe.
        Some(t) if t >= 1 && m <= 1 => (t as usize).min(ncpu),
        Some(t) if t >= 1 => (t as usize).min((ncpu / m).max(1)),
        // Single worker: one big session, leave the query threads free. Multiple
        // workers: spread ALL cores across the `m` sessions (≈ ncpu/m each) so the
        // task-parallel pool actually saturates the CPU.
        _ if m <= 1 => ncpu.saturating_sub(query_threads).max(1),
        _ => (ncpu / m).max(1),
    };
    (
        SessionTuning {
            device,
            threads: bulk_threads,
        },
        SessionTuning {
            device: Device::Cpu,
            threads: query_threads,
        },
    )
}

/// How to re-create a dropped bulk session (see [`Embedder::trim_bulk`]): the
/// same inputs `new_builtin` / `new_local` used for the original load.
enum ReloadSpec {
    Builtin {
        model: EmbeddingModel,
        cache_dir: Option<String>,
    },
    Local {
        model_path: String,
    },
}

/// Production embedder: two separately-loaded fastembed models behind their own
/// mutexes (see the module docs for the two-instance rationale).
pub struct FastEmbedder {
    /// One or more model sessions for bulk passage embedding (the background-
    /// worker path). A long reindex locks only these, leaving `query` free. With
    /// >1 session the ingest worker runs that many threads, each **pinned** to
    /// its own session via [`Embedder::embed_passages_at`] (worker slot →
    /// session index), so jobs embed concurrently with no cross-session lock
    /// contention (cost: one model copy in RAM per session).
    ///
    /// `None` = the slot was trimmed ([`Embedder::trim_bulk`]) to give RAM back
    /// after an ingest; the next bulk embed on that slot lazily reloads it via
    /// `reload` + `bulk_tuning`.
    bulk_pool: Vec<Mutex<Option<TextEmbedding>>>,
    /// Inputs needed to lazily re-create a trimmed bulk session.
    reload: ReloadSpec,
    /// Tuning the bulk sessions were (and will be re-) created with.
    bulk_tuning: SessionTuning,
    /// Round-robin cursor selecting a bulk session for the *fallback* path —
    /// [`Embedder::embed_passages`] callers that have no stable worker slot
    /// (e.g. ad-hoc/test callers). The pinned worker path never touches it.
    bulk_cursor: AtomicUsize,
    /// Model used for low-latency query embedding (the search path).
    query: Mutex<TextEmbedding>,
    /// Most recent embed failure (poisoned session mutex or an ort inference
    /// error), kept for diagnostics. `eprintln!` goes nowhere inside the 1C
    /// host process, so `stats` surfaces this via [`Embedder::last_error`].
    last_error: Mutex<Option<String>>,
    /// Output dimensionality, probed once at construction. Bound to the index.
    dim: usize,
    /// ONNX inference sub-batch size handed to fastembed. The real constraint
    /// is *peak* inference memory: attention materializes tensors proportional
    /// to `sub_batch × seq_len²` per layer, and ort's BFC arena retains that
    /// high-water mark for the session's lifetime — it never returns the memory
    /// to the OS. At the fastembed default (256) one session padding 512-token
    /// passages grows to many GB and stays there until the session is dropped.
    /// A flat 32 keeps the peak around ~1 GB per session for e5-small at seq
    /// 512 (scale by session count for the pool total), and the CPU throughput
    /// loss vs 256 is negligible — intra-op threading dominates, not batch
    /// amortization.
    embed_batch: usize,
}

impl FastEmbedder {
    /// Build a `FastEmbedder` for a built-in `model` (one of
    /// [`supported_models`], resolved from its wire name by [`resolve_model`];
    /// the default is **MultilingualE5Small**).
    ///
    /// fastembed downloads (first run) and caches the ONNX + tokenizer files
    /// from HuggingFace — into `cache_dir` when given (created on demand), else
    /// into fastembed's default cache location; subsequent runs load from that
    /// cache. This is the path the integration test verifies. Loading happens
    /// once per session (bulk pool + query — see module docs); all share the
    /// same on-disk cache, so only the first incurs a download.
    ///
    /// `device` selects the execution provider (CPU, or DirectML with automatic
    /// CPU fallback — see [`Device`]).
    ///
    /// Returns a descriptive error string (never panics) so the FFI boundary can
    /// surface a clean structural error if model load fails (e.g. offline with a
    /// cold cache, or onnxruntime not loadable).
    pub fn new_builtin(
        model: EmbeddingModel,
        cache_dir: Option<&str>,
        device: Device,
        intra_threads: Option<u64>,
        embed_workers: Option<u64>,
    ) -> Result<Self, String> {
        // `m` independent bulk sessions + one query session (see module docs).
        // The bulk sessions get the configured device + a per-session intra-op
        // pool; query gets CPU + a few threads — see `tunings`.
        let m = worker_count(embed_workers);
        // Multi-worker is a CPU task-parallel strategy: DirectML is a single
        // device and N concurrent sessions on it error out (and a GPU already
        // parallelizes within ONE session). So force CPU for the bulk pool when
        // running more than one worker; a single worker keeps the requested device.
        let bulk_device = if m > 1 { Device::Cpu } else { device };
        let (bulk_t, query_t) = tunings(bulk_device, intra_threads, m);
        let mut bulk_pool = Vec::with_capacity(m);
        for _ in 0..m {
            bulk_pool.push(Mutex::new(Some(
                Self::load_builtin(model.clone(), cache_dir, bulk_t)
                    .map_err(|e| format!("load bulk model: {e}"))?,
            )));
        }
        let query = Self::load_builtin(model.clone(), cache_dir, query_t)
            .map_err(|e| format!("load query model: {e}"))?;

        let mut me = FastEmbedder {
            bulk_pool,
            reload: ReloadSpec::Builtin {
                model,
                cache_dir: cache_dir.map(str::to_string),
            },
            bulk_tuning: bulk_t,
            bulk_cursor: AtomicUsize::new(0),
            query: Mutex::new(query),
            last_error: Mutex::new(None),
            // Provisional; replaced by the probe below. The built-in e5 family
            // is 384/768/1024-dim (small/base/large), but we probe rather than
            // hard-code so the value is always truthful for whatever loaded.
            dim: 0,
            embed_batch: 32,
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
    /// point it at a CLS-pooled model. `device` selects the execution provider
    /// (see [`Device`]). NOTE: this path is implemented but, unlike
    /// `new_builtin`, is **not exercised by the integration test** (it needs a
    /// pre-staged model directory). It is here so production can run offline.
    pub fn new_local(
        model_path: &str,
        device: Device,
        intra_threads: Option<u64>,
        embed_workers: Option<u64>,
    ) -> Result<Self, String> {
        let m = worker_count(embed_workers);
        // Multi-worker = CPU task-parallel strategy (see `new_builtin`): N
        // concurrent DirectML sessions error out, so force CPU when m > 1.
        let bulk_device = if m > 1 { Device::Cpu } else { device };
        let (bulk_t, query_t) = tunings(bulk_device, intra_threads, m);
        let mut bulk_pool = Vec::with_capacity(m);
        for _ in 0..m {
            bulk_pool.push(Mutex::new(Some(
                Self::load_local(model_path, bulk_t).map_err(|e| format!("load bulk model: {e}"))?,
            )));
        }
        let query =
            Self::load_local(model_path, query_t).map_err(|e| format!("load query model: {e}"))?;

        let mut me = FastEmbedder {
            bulk_pool,
            reload: ReloadSpec::Local {
                model_path: model_path.to_string(),
            },
            bulk_tuning: bulk_t,
            bulk_cursor: AtomicUsize::new(0),
            query: Mutex::new(query),
            last_error: Mutex::new(None),
            dim: 0,
            embed_batch: 32,
        };
        me.dim = me.probe_dim()?;
        Ok(me)
    }

    /// Load one built-in `model` instance (download/cache via HF — rooted at
    /// `cache_dir` when given, else fastembed's default cache), requesting
    /// `device`'s execution providers (empty ⇒ CPU; DirectML otherwise, with
    /// ort's automatic CPU fallback).
    fn load_builtin(
        model: EmbeddingModel,
        cache_dir: Option<&str>,
        tuning: SessionTuning,
    ) -> Result<TextEmbedding, String> {
        let mut opts = InitOptions::new(model)
            .with_show_download_progress(true)
            .with_intra_threads(tuning.threads)
            .with_execution_providers(tuning.device.execution_providers());
        if let Some(dir) = cache_dir {
            // Caller-chosen download/cache root (e.g. next to the component so
            // an admin can pre-stage or wipe it). fastembed/hf-hub creates the
            // directory on demand.
            opts = opts.with_cache_dir(std::path::PathBuf::from(dir));
        }
        TextEmbedding::try_new(opts).map_err(|e| e.to_string())
    }

    /// Load one user-defined instance from on-disk ONNX + tokenizer files,
    /// requesting `device`'s execution providers (same CPU/DirectML semantics as
    /// [`Self::load_builtin`]).
    fn load_local(model_path: &str, tuning: SessionTuning) -> Result<TextEmbedding, String> {
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

        TextEmbedding::try_new_from_user_defined(
            model,
            InitOptionsUserDefined::new()
                .with_intra_threads(tuning.threads)
                .with_execution_providers(tuning.device.execution_providers()),
        )
        .map_err(|e| e.to_string())
    }

    /// Probe the model's output dimensionality by embedding a single short
    /// passage and measuring the vector length. Done once at construction so
    /// `dim()` is O(1) and always truthful for whatever model was loaded.
    fn probe_dim(&self) -> Result<usize, String> {
        let mut slot = self.bulk_pool[0]
            .lock()
            .map_err(|_| "embedder mutex poisoned".to_string())?;
        let m = slot
            .as_mut()
            .ok_or_else(|| "dim probe: bulk session missing".to_string())?;
        let out = m
            .embed(vec![format!("{PASSAGE_PREFIX}probe")], None)
            .map_err(|e| format!("dim probe embed failed: {e}"))?;
        out.first()
            .map(|v| v.len())
            .ok_or_else(|| "dim probe returned no vector".to_string())
    }

    /// Re-create one bulk session from the stored [`ReloadSpec`] (after a
    /// [`Embedder::trim_bulk`] released it).
    fn reload_bulk_session(&self) -> Result<TextEmbedding, String> {
        match &self.reload {
            ReloadSpec::Builtin { model, cache_dir } => {
                Self::load_builtin(model.clone(), cache_dir.as_deref(), self.bulk_tuning)
            }
            ReloadSpec::Local { model_path } => Self::load_local(model_path, self.bulk_tuning),
        }
    }

    /// Record an embed failure so [`Embedder::last_error`] (and therefore the
    /// `stats` payload) can surface it — `eprintln!` is invisible inside the 1C
    /// host process. Last writer wins: we only keep the most recent failure.
    fn note_error(last_error: &Mutex<Option<String>>, msg: String) {
        if let Ok(mut g) = last_error.lock() {
            *g = Some(msg);
        }
    }

    /// Run a batch through one of the model instances, prepending `prefix` to
    /// each input. On any error (poisoned lock or an ort inference failure) we
    /// return one all-zero vector per input: the ingest/search pipeline already
    /// treats all-zero vectors as "no signal" (skipped, never indexed, never a
    /// hit), so a transient embed failure degrades gracefully instead of
    /// panicking across the FFI boundary. Failures are additionally recorded in
    /// `last_error` (see [`Self::note_error`]) so they are visible via `stats`.
    fn run(
        model: &Mutex<TextEmbedding>,
        last_error: &Mutex<Option<String>>,
        prefix: &str,
        texts: &[String],
        dim: usize,
        batch: usize,
    ) -> Vec<Vec<f32>> {
        let prefixed: Vec<String> = texts.iter().map(|t| format!("{prefix}{t}")).collect();
        let mut guard = match model.lock() {
            Ok(g) => g,
            Err(_) => {
                eprintln!("rcore: embed skipped — model mutex poisoned");
                Self::note_error(
                    last_error,
                    "embed skipped — model mutex poisoned".to_string(),
                );
                return vec![vec![0.0; dim]; texts.len()];
            }
        };
        // Explicit sub-batch keeps concurrent-session peak memory bounded.
        match guard.embed(prefixed, Some(batch)) {
            Ok(vecs) => vecs,
            Err(e) => {
                // Surface the underlying ort/tokenizer error (was silently
                // swallowed). All-zero vectors still signal "no signal" downstream.
                let msg = format!("embed failed for {} texts: {e}", texts.len());
                eprintln!("rcore: {msg}");
                Self::note_error(last_error, msg);
                vec![vec![0.0; dim]; texts.len()]
            }
        }
    }

    /// Bulk-path variant of [`Self::run`] over an `Option`al session slot:
    /// a trimmed slot ([`Embedder::trim_bulk`]) is lazily re-created from the
    /// stored [`ReloadSpec`] before embedding. Same zero-vector degradation on
    /// any failure (lock poison, reload error, ort error).
    fn run_bulk(&self, idx: usize, texts: &[String]) -> Vec<Vec<f32>> {
        let prefixed: Vec<String> = texts
            .iter()
            .map(|t| format!("{PASSAGE_PREFIX}{t}"))
            .collect();
        let mut guard = match self.bulk_pool[idx].lock() {
            Ok(g) => g,
            Err(_) => {
                eprintln!("rcore: embed skipped — model mutex poisoned");
                Self::note_error(
                    &self.last_error,
                    "embed skipped — model mutex poisoned".to_string(),
                );
                return vec![vec![0.0; self.dim]; texts.len()];
            }
        };
        if guard.is_none() {
            // The slot was trimmed after a previous ingest — reload in place.
            match self.reload_bulk_session() {
                Ok(session) => *guard = Some(session),
                Err(e) => {
                    let msg = format!("bulk session reload failed: {e}");
                    eprintln!("rcore: {msg}");
                    Self::note_error(&self.last_error, msg);
                    return vec![vec![0.0; self.dim]; texts.len()];
                }
            }
        }
        let model = guard.as_mut().expect("just ensured Some");
        match model.embed(prefixed, Some(self.embed_batch)) {
            Ok(vecs) => vecs,
            Err(e) => {
                let msg = format!("embed failed for {} texts: {e}", texts.len());
                eprintln!("rcore: {msg}");
                Self::note_error(&self.last_error, msg);
                vec![vec![0.0; self.dim]; texts.len()]
            }
        }
    }
}

impl Embedder for FastEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed_passages(&self, texts: &[String]) -> Vec<Vec<f32>> {
        // Fallback bulk path for callers WITHOUT a stable worker slot (the
        // pinned worker threads use `embed_passages_at` instead). Round-robin
        // across the bulk session pool so even ad-hoc concurrent callers spread
        // over distinct sessions, never touching the `query` session.
        let n = self.bulk_pool.len();
        let idx = if n <= 1 {
            0
        } else {
            self.bulk_cursor.fetch_add(1, Ordering::Relaxed) % n
        };
        self.run_bulk(idx, texts)
    }

    fn embed_passages_at(&self, slot: usize, texts: &[String]) -> Vec<Vec<f32>> {
        // Pinned worker path: worker thread `slot` always uses session
        // `slot % n`, so a slow job on one session never blocks a sibling
        // worker whose own session is idle (the head-of-line blocking the old
        // shared round-robin cursor allowed). The `% n` guard matters: after a
        // pool resize a *retired* worker thread with a high slot index may
        // briefly run one last drained job against a smaller new pool — the
        // modulo keeps that in bounds instead of panicking.
        let n = self.bulk_pool.len().max(1);
        self.run_bulk(slot % n, texts)
    }

    fn bulk_concurrency(&self) -> usize {
        self.bulk_pool.len()
    }

    fn trim_bulk(&self) {
        // Drop every bulk session (a full model copy each) to give RAM back
        // after an ingest. The query session is intentionally untouched. A
        // poisoned slot is skipped — nothing to free safely there anyway.
        for slot in &self.bulk_pool {
            if let Ok(mut guard) = slot.lock() {
                *guard = None;
            }
        }
    }

    fn embed_query(&self, text: &str) -> Vec<f32> {
        // Query path: lock `query` only — never blocked by a bulk reindex.
        let one = [text.to_string()];
        Self::run(
            &self.query,
            &self.last_error,
            QUERY_PREFIX,
            &one,
            self.dim,
            self.embed_batch,
        )
        .into_iter()
        .next()
        .unwrap_or_else(|| vec![0.0; self.dim])
    }

    fn last_error(&self) -> Option<String> {
        // A poisoned slot only means a panicking writer; the stored string (if
        // any) is still the best diagnostic we have, so recover it.
        match self.last_error.lock() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        }
    }
}

#[cfg(test)]
mod model_tests {
    use super::*;

    /// Empty/whitespace resolves to the default; matching is trimmed and
    /// case-insensitive. (Fast: name resolution only, no model load.)
    #[test]
    fn resolve_model_default_and_case_insensitive() {
        assert_eq!(
            resolve_model("").unwrap(),
            EmbeddingModel::MultilingualE5Small
        );
        assert_eq!(
            resolve_model("   ").unwrap(),
            EmbeddingModel::MultilingualE5Small
        );
        assert_eq!(
            resolve_model("Multilingual-E5-BASE").unwrap(),
            EmbeddingModel::MultilingualE5Base
        );
        assert_eq!(
            resolve_model("  multilingual-e5-large  ").unwrap(),
            EmbeddingModel::MultilingualE5Large
        );
    }

    /// Unknown names error with a message that names the offender and lists
    /// every supported model (what the dispatcher's `bad_model` error carries).
    #[test]
    fn resolve_model_unknown_lists_supported() {
        let err = resolve_model("bge-zzz").unwrap_err();
        assert!(err.contains("unknown model 'bge-zzz'"), "got: {err}");
        for (name, _) in supported_models() {
            assert!(err.contains(name), "error must list '{name}': {err}");
        }
    }

    /// Pin this module's name→variant table to the feature-independent name
    /// list in `core` (which powers `list_models` / mock-build validation):
    /// same names, same order, and the core default resolves to the default
    /// variant. This is what lets the two lists exist without drifting.
    #[test]
    fn supported_models_match_core_whitelist() {
        let names: Vec<&str> = supported_models().iter().map(|(n, _)| *n).collect();
        assert_eq!(names, crate::core::SUPPORTED_MODEL_NAMES);
        assert_eq!(
            resolve_model(crate::core::DEFAULT_MODEL).unwrap(),
            EmbeddingModel::MultilingualE5Small
        );
        for name in crate::core::SUPPORTED_MODEL_NAMES {
            assert!(crate::core::validate_model_name(name).is_ok());
            assert!(resolve_model(name).is_ok(), "core name '{name}' must resolve");
        }
    }
}

#[cfg(test)]
mod conc_tests {
    use super::*;
    use std::sync::Arc;

    /// Reproduce concurrent multi-session embedding to surface any error (the 1C
    /// run failed 7500/7561 under 6 workers). Forces CPU to isolate from DirectML.
    /// Needs the offline model staged at target/offline-model-mini (skips if
    /// absent). Run:
    ///   cargo test --release --features fastembed concurrent_multi_session -- --ignored --nocapture
    #[test]
    #[ignore]
    fn concurrent_multi_session_embed() {
        let path = "target/offline-model-mini";
        if !std::path::Path::new(path).exists() {
            eprintln!("model not staged at {path}; skipping");
            return;
        }
        let workers = 6usize;
        let emb = Arc::new(
            FastEmbedder::new_local(path, Device::Cpu, None, Some(workers as u64))
                .expect("load model pool"),
        );
        eprintln!("bulk_concurrency = {}", emb.bulk_concurrency());
        let mut handles = vec![];
        for t in 0..workers {
            let e = Arc::clone(&emb);
            handles.push(std::thread::spawn(move || {
                // Long passages (~like real Gherkin scenarios) so this exercises
                // the concurrent-session PEAK-MEMORY path, not just short strings.
                let long = "Когда я открываю список документов и проверяю фильтр по компании \
                    Тогда в списке только документы выбранной компании и колонки заполнены "
                    .repeat(8);
                let texts: Vec<String> = (0..500)
                    .map(|i| format!("scenario {t} segment {i} unique{i} {long}"))
                    .collect();
                let v = e.embed_passages(&texts);
                let zeros = v.iter().filter(|x| x.iter().all(|&f| f == 0.0)).count();
                eprintln!("thread {t}: {} vecs, {} all-zero (failed)", v.len(), zeros);
                zeros
            }));
        }
        let total_zeros: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
        eprintln!("TOTAL all-zero (failed) = {total_zeros} / {}", workers * 500);
    }
}
