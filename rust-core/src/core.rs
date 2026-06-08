//! Process-global singleton store + the async ingest worker.
//!
//! A single [`Core`] instance is shared across *all* component instances in the
//! host process. The 1C runtime can open and close the MCP form many times (each
//! time constructing a fresh `HttpServerComponent` via the `AddComponent`
//! factory), but the search core — the loaded embedding model and the in-memory
//! index — must be built once and outlive any single component. It lives for the
//! lifetime of the process and dies with it.
//!
//! ## Concurrency model (Stage 1)
//!
//! The store is a `Lazy<RwLock<Core>>`:
//!   * readers = `search` / `stats`;
//!   * writers = ingest-*apply* / `reset` / `configure`.
//!
//! The expensive part — embedding (15–60 s with a real model) — happens
//! **outside** the index lock, on a background worker thread. The write lock is
//! held only for the short synchronous accept (store text, mark Building) and
//! for the short apply (swap in finished vectors). This is the architectural
//! heart of the slice: a `index_segments` call from BSL returns immediately and
//! never freezes the 1C UI thread.
//!
//! The worker is a `std::thread` fed by an `mpsc` job queue. It is spawned
//! lazily on the first ingest and lives until [`shutdown`] signals it to stop
//! and joins it.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::JoinHandle;

use once_cell::sync::Lazy;

use crate::embed::{dot, Embedder, MockEmbedder};
use crate::filter::MetaFilter;

// ===========================================================================
// Data model: collection → documents → segments
// ===========================================================================

/// One indexed segment: a chunk of text plus its (lazily computed) vector.
///
/// `embed_text` and segment-level `meta` are retained verbatim from ingest:
/// they are part of the stable data model (the real model card and future
/// meta-filter / re-embed cards read them) even though this dense-only slice
/// doesn't surface them yet.
#[allow(dead_code)]
#[derive(Clone)]
pub struct Segment {
    /// Stable id, unique within the store. Lets callers reference a hit later.
    pub segment_id: u64,
    /// The human-facing text shown in search results.
    pub text: String,
    /// Optional alternate text to embed (e.g. a summary). Falls back to `text`.
    pub embed_text: Option<String>,
    /// Optional source line range (offset table), echoed back in hits.
    pub line_start: Option<u64>,
    pub line_end: Option<u64>,
    /// Arbitrary per-segment metadata (JSON object), echoed back in hits.
    pub meta: serde_json::Value,
    /// The normalized embedding. `None` until the worker fills it; stays `None`
    /// for skipped (blank/non-finite) segments, which search simply ignores.
    pub vector: Option<Vec<f32>>,
}

/// One document: a `doc_id`-keyed bundle of segments with doc-level metadata.
///
/// For `index_raw` documents we additionally retain the **full normalized text**
/// plus a **line offset table** so `get_segment` can do an O(1) line-range slice.
/// Atomic `index_segments` records leave both `None` — they have no document-wide
/// text/line model, so `get_segment` returns a structural `no_line_index` error.
#[derive(Clone)]
pub struct Document {
    /// Required stable id (a 1C reference / GUID). Everything that is later
    /// updated or deleted is keyed by this.
    pub doc_id: String,
    pub name: String,
    /// Doc-level metadata (JSON object), echoed back in hits.
    pub meta: serde_json::Value,
    pub segments: Vec<Segment>,
    /// Full CRLF→LF-normalized document text. `Some` only for `index_raw` docs.
    pub full_text: Option<String>,
    /// Byte offset of the start of each line in `full_text` (1-based line N is at
    /// `line_offsets[N-1]`). Parallel to `full_text`; `Some` only for raw docs.
    /// Has exactly `line_count` entries (the number of `\n`-delimited lines).
    pub line_offsets: Option<Vec<usize>>,
}

/// Two-axis vector state. `text_ready` is a separate boolean on the collection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VectorStatus {
    /// Text accepted; vectors still being computed by the worker.
    Building,
    /// All enqueued segments embedded (or skipped); vectors are complete.
    Ready,
}

impl VectorStatus {
    /// Lower-case wire name used in JSON (`stats` / `search`).
    pub fn as_str(self) -> &'static str {
        match self {
            VectorStatus::Building => "building",
            VectorStatus::Ready => "ready",
        }
    }
}

/// A named collection of documents plus its build state machine.
pub struct Collection {
    /// Documents keyed by `doc_id` for atomic upsert/delete.
    pub docs: HashMap<String, Document>,
    /// Axis 1: text is queryable/visible the moment ingest is accepted.
    pub text_ready: bool,
    /// Axis 2: vectors transition Building → Ready as the worker finishes.
    pub vector_status: VectorStatus,
    /// Set only on a *fatal* fault. Per-segment problems do NOT set this.
    pub error: Option<String>,
    /// How many jobs for this collection are still queued/in-flight. When it
    /// drops to zero the collection flips to `Ready`.
    pub pending_jobs: u32,
    // ---- progress counters (the cold-start progress surfaced by `stats`) ----
    /// Segments that got a valid vector.
    pub embedded: u64,
    /// Segments that produced a non-finite/invalid vector.
    pub failed: u64,
    /// Segments skipped because their text was blank.
    pub skipped: u64,
}

impl Collection {
    fn new() -> Self {
        Collection {
            docs: HashMap::new(),
            text_ready: false,
            vector_status: VectorStatus::Building,
            error: None,
            pending_jobs: 0,
            embedded: 0,
            failed: 0,
            skipped: 0,
        }
    }

    /// Total segment count across all docs.
    pub fn n_segments(&self) -> usize {
        self.docs.values().map(|d| d.segments.len()).sum()
    }
}

// ===========================================================================
// Configuration echoed back by `configure` / `stats`
// ===========================================================================

/// Echo of the knobs passed to `configure`. In mock mode `model_path` is
/// accepted but a [`MockEmbedder`] is instantiated regardless.
#[derive(Clone, Default)]
pub struct Config {
    pub model_path: Option<String>,
    pub normalize: bool,
    pub max_seq_len: Option<u64>,
    pub device: String,
    pub intra_threads: Option<u64>,
}

// ===========================================================================
// Background worker plumbing
// ===========================================================================

/// A unit of embedding work handed to the worker thread.
///
/// The doc's text segments are *already installed* in the store at accept time
/// (synchronously, with `vector: None`). This job carries only what the worker
/// needs to embed *outside* the lock and then, *inside* a short lock, fill the
/// already-present segments' vectors in place — matched by `(doc_id, segment_id)`:
///   * the collection + doc id to locate the segments;
///   * the stable `segment_id` of each segment, parallel to `embed_texts`;
///   * the text to embed for each segment (empty string marks a blank → skip).
struct EmbedJob {
    collection: String,
    doc_id: String,
    /// Stable segment ids to fill, parallel to `embed_texts`. A stale id (the
    /// doc was re-ingested while this job was in flight) simply won't be found
    /// and is skipped — this is what keeps re-ingest atomic.
    segment_ids: Vec<u64>,
    /// Per-segment text to embed, parallel to `segment_ids`. Empty strings mark
    /// blank segments to skip.
    embed_texts: Vec<String>,
}

/// Messages on the worker queue.
enum WorkerMsg {
    Job(EmbedJob),
    /// Asks the worker to drain and exit. Sent by [`shutdown`].
    Stop,
}

/// Owns the worker thread + its job sender. Lives inside [`Core`].
struct Worker {
    tx: Sender<WorkerMsg>,
    handle: Option<JoinHandle<()>>,
}

// ===========================================================================
// The core
// ===========================================================================

/// The process-global core state.
pub struct Core {
    /// Whether `configure` has been called.
    pub configured: bool,
    /// The active embedder (mock for this slice). `None` until configured;
    /// ingest auto-configures with a default mock if needed.
    pub embedder: Option<Arc<dyn Embedder>>,
    /// Echoed config from the last `configure`.
    pub config: Config,
    /// Named collections of indexed documents.
    pub collections: HashMap<String, Collection>,
    /// Monotonic counter of dispatched calls — surfaced via `stats`.
    pub calls_handled: u64,
    /// Source of stable, unique segment ids.
    next_segment_id: u64,
    /// The background worker, spawned lazily on first ingest.
    worker: Option<Worker>,
    /// Condvar signalled whenever a collection's `vector_status`/counters change,
    /// so tests (and callers) can `wait_until_ready` without polling/sleeping.
    progress: Arc<(Mutex<()>, Condvar)>,
}

impl Default for Core {
    fn default() -> Self {
        Core {
            configured: false,
            embedder: None,
            config: Config::default(),
            collections: HashMap::new(),
            calls_handled: 0,
            next_segment_id: 1,
            worker: None,
            progress: Arc::new((Mutex::new(()), Condvar::new())),
        }
    }
}

impl Core {
    /// Reset all mutable index state back to defaults. Used by `reset` and by
    /// `configure` when the embedding space changes (dim mismatch).
    ///
    /// Note: does NOT tear down the worker thread — that is `shutdown`'s job and
    /// must not happen on a routine `reset`. Any jobs still in flight for the
    /// cleared collections become harmless: their apply step recreates the
    /// (now-empty) collection only if it still… see `apply_job` for the guard.
    pub fn reset(&mut self) {
        self.configured = false;
        self.embedder = None;
        self.config = Config::default();
        self.collections.clear();
        self.next_segment_id = 1;
        // `calls_handled` is a lifetime-of-process counter; not cleared.
        // Wake any waiters so a `wait_until_ready` on a cleared collection
        // returns promptly instead of hanging to its timeout.
        self.progress.1.notify_all();
    }

    /// Allocate the next stable segment id.
    fn alloc_segment_id(&mut self) -> u64 {
        let id = self.next_segment_id;
        self.next_segment_id = self.next_segment_id.saturating_add(1);
        id
    }

    /// Clone the progress condvar handle (shared with the worker thread).
    pub fn progress_handle(&self) -> Arc<(Mutex<()>, Condvar)> {
        Arc::clone(&self.progress)
    }
}

/// The process-global singleton. Acquire `.read()` for queries and `.write()`
/// for mutations. Never hold a lock across an FFI return.
pub static CORE: Lazy<RwLock<Core>> = Lazy::new(|| RwLock::new(Core::default()));

// ===========================================================================
// configure
// ===========================================================================

/// Outcome of [`configure`], so the caller can build the JSON echo.
pub struct ConfigureResult {
    pub dim: usize,
    pub reset_due_to_dim_change: bool,
}

/// Apply a `configure` request. Idempotent. If a different-dim embedder is
/// chosen while data is already indexed, the index is reset (old vectors live in
/// a different space and are invalid).
///
/// In this slice we always instantiate a [`MockEmbedder`]; `model_path` is
/// accepted and echoed but does not load anything.
pub fn configure(core: &mut Core, config: Config) -> ConfigureResult {
    let new_embedder = MockEmbedder::new();
    let new_dim = new_embedder.dim();

    // If we already had an embedder of a different dim AND there is indexed
    // data, the existing vectors are invalid in the new space → full reset.
    let old_dim = core.embedder.as_ref().map(|e| e.dim());
    let had_data = !core.collections.is_empty();
    let dim_changed = matches!(old_dim, Some(d) if d != new_dim);
    let reset_due_to_dim_change = dim_changed && had_data;

    if reset_due_to_dim_change {
        // Drop indexed collections; in-flight jobs for them become no-ops
        // because their target collections no longer exist (see `apply_job`).
        core.collections.clear();
        core.next_segment_id = 1;
    }

    core.embedder = Some(Arc::new(new_embedder));
    core.config = config;
    core.configured = true;
    core.progress.1.notify_all();

    ConfigureResult {
        dim: new_dim,
        reset_due_to_dim_change,
    }
}

/// Ensure an embedder exists, auto-configuring a default mock if `configure` was
/// never called. Returns the embedder handle. Keeps ingest usable without a
/// mandatory explicit configure step (the dim is then the mock default).
fn ensure_embedder(core: &mut Core) -> Arc<dyn Embedder> {
    if core.embedder.is_none() {
        core.embedder = Some(Arc::new(MockEmbedder::new()));
        // Do NOT flip `configured` — that flag tracks an explicit configure.
    }
    Arc::clone(core.embedder.as_ref().expect("just set"))
}

// ===========================================================================
// index_segments — the async accept
// ===========================================================================

/// A parsed segment request from the `index_segments` payload (pre-embedding).
pub struct SegmentInput {
    pub text: String,
    pub embed_text: Option<String>,
    pub line_start: Option<u64>,
    pub line_end: Option<u64>,
    pub meta: serde_json::Value,
}

/// A parsed `index_segments` request.
pub struct IndexRequest {
    pub collection: String,
    pub doc_id: String,
    pub name: String,
    pub meta: serde_json::Value,
    pub segments: Vec<SegmentInput>,
}

/// Outcome of the synchronous accept.
pub struct AcceptResult {
    pub collection: String,
    pub segment_count: usize,
}

/// Synchronous accept for `index_segments`. Does the cheap work under a short
/// write lock — allocate ids, **install the doc's text segments into the store
/// with `vector: None`** (atomic upsert by `doc_id`), mark the collection
/// `text_ready=true` / `vector_status=Building`, bump `pending_jobs` — then
/// enqueues one [`EmbedJob`] for the background worker. Returns immediately;
/// embedding happens off-thread.
///
/// This is the carried-forward fix from Stage 1 (§4.4 / Правка-2): text-only
/// operations (`grep` / `get_segment` / keyword) can see a doc's text the moment
/// accept returns, because the segments are present in the store *before* the
/// worker runs. The worker fills ONLY the vectors afterwards (matched by
/// `segment_id`), so dense search is the only operation that waits for vectors.
///
/// Blank-text segments are counted as `skipped` right here at accept time (they
/// never get embedded) so they don't keep the collection in Building forever.
pub fn accept_index(core: &mut Core, req: IndexRequest) -> AcceptResult {
    let embedder = ensure_embedder(core);
    let dim = embedder.dim();

    // Build the doc + the parallel list of (segment_id, text-to-embed). A blank
    // text is skipped immediately (counter bumped after we have the collection).
    let mut doc = Document {
        doc_id: req.doc_id.clone(),
        name: req.name,
        meta: req.meta,
        segments: Vec::with_capacity(req.segments.len()),
        // Atomic `index_segments` records carry no document-wide text/offset
        // table; `get_segment` is intentionally unsupported for them.
        full_text: None,
        line_offsets: None,
    };
    let mut segment_ids: Vec<u64> = Vec::with_capacity(req.segments.len());
    let mut embed_texts: Vec<String> = Vec::with_capacity(req.segments.len());
    let mut skipped_now: u64 = 0;

    for s in req.segments {
        let segment_id = core.alloc_segment_id();
        // Choose embed_text-or-text; blank → mark skip (empty embed string).
        let chosen = s.embed_text.clone().unwrap_or_else(|| s.text.clone());
        let is_blank = chosen.trim().is_empty();
        if is_blank {
            skipped_now += 1;
        }
        segment_ids.push(segment_id);
        embed_texts.push(if is_blank { String::new() } else { chosen });
        doc.segments.push(Segment {
            segment_id,
            text: s.text,
            embed_text: s.embed_text,
            line_start: s.line_start,
            line_end: s.line_end,
            meta: s.meta,
            vector: None,
        });
    }

    let segment_count = doc.segments.len();

    // --- short write-locked accept: install text, mark Building ---
    let coll = core
        .collections
        .entry(req.collection.clone())
        .or_insert_with(Collection::new);
    coll.text_ready = true;
    coll.vector_status = VectorStatus::Building;
    coll.error = None;
    coll.pending_jobs = coll.pending_jobs.saturating_add(1);
    coll.skipped = coll.skipped.saturating_add(skipped_now);

    // Atomic upsert by doc_id: install the doc's text segments NOW (vectors are
    // `None`). Re-ingesting the same doc_id replaces all of its segments here, in
    // one map insert — so text-readers never see a torn doc, and any embed job
    // still in flight for the *previous* generation of this doc has stale
    // `segment_id`s that simply won't match (see `apply_job`).
    coll.docs.insert(req.doc_id.clone(), doc);

    let _ = dim; // dim is fixed by the embedder; kept for clarity/future use.

    // Enqueue the embed job for the worker (spawns it lazily). It carries only
    // the ids + texts needed to fill vectors in place after embedding.
    enqueue_job(
        core,
        EmbedJob {
            collection: req.collection.clone(),
            doc_id: req.doc_id,
            segment_ids,
            embed_texts,
        },
    );

    AcceptResult {
        collection: req.collection,
        segment_count,
    }
}

/// Lazily spawn the worker thread (if needed) and push a job onto its queue.
fn enqueue_job(core: &mut Core, job: EmbedJob) {
    if core.worker.is_none() {
        core.worker = Some(spawn_worker(core.progress_handle()));
    }
    if let Some(w) = core.worker.as_ref() {
        // If the receiver is gone (worker stopped), the job is dropped and the
        // collection would stay Building; that only happens post-shutdown.
        let _ = w.tx.send(WorkerMsg::Job(job));
    }
}

/// Spawn the background worker thread. It pulls jobs, embeds OUTSIDE the lock,
/// then applies finished vectors under a short write lock and updates the state
/// machine / counters. `progress` is the shared condvar used to wake waiters.
fn spawn_worker(progress: Arc<(Mutex<()>, Condvar)>) -> Worker {
    let (tx, rx): (Sender<WorkerMsg>, Receiver<WorkerMsg>) = mpsc::channel();
    let handle = std::thread::Builder::new()
        .name("rcore-ingest".to_string())
        .spawn(move || worker_loop(rx, progress))
        .expect("failed to spawn ingest worker");
    Worker {
        tx,
        handle: Some(handle),
    }
}

/// The worker thread body. Blocks on the queue; for each job it embeds outside
/// the lock and then applies under a short lock. Exits on `Stop` or when the
/// sender is dropped.
fn worker_loop(rx: Receiver<WorkerMsg>, progress: Arc<(Mutex<()>, Condvar)>) {
    while let Ok(msg) = rx.recv() {
        match msg {
            WorkerMsg::Stop => break,
            WorkerMsg::Job(job) => {
                // --- embed OUTSIDE the index lock (the expensive part) ---
                // Grab a handle to the embedder under a brief read lock, then
                // release it before doing the heavy embedding work.
                let embedder = match CORE.read() {
                    Ok(c) => c.embedder.clone(),
                    Err(_) => None,
                };
                let embedder = match embedder {
                    Some(e) => e,
                    None => {
                        // No embedder (e.g. reset mid-flight) — apply with the
                        // collection still bookkept so it doesn't hang Building.
                        apply_job(job, None);
                        notify(&progress);
                        continue;
                    }
                };

                // Only embed the non-blank texts; blanks already counted as
                // skipped at accept. We embed all in one batch call.
                let vectors = embedder.embed_passages(&job.embed_texts);

                // --- apply finished vectors under a SHORT write lock ---
                apply_job(job, Some(vectors));
                notify(&progress);
            }
        }
    }
}

/// Notify all waiters that progress changed.
fn notify(progress: &Arc<(Mutex<()>, Condvar)>) {
    let _guard = progress.0.lock().expect("progress mutex");
    progress.1.notify_all();
}

/// Apply one finished job under a short write lock: fill the *already-installed*
/// segments' vectors in place (matched by `segment_id`), update counters, and
/// flip the collection to `Ready` when it has no more pending jobs.
///
/// The doc's text was installed synchronously at accept time, so here we only
/// fill vectors — we never insert/replace segments. `vectors`, when `Some`, is
/// parallel to `job.segment_ids` / `job.embed_texts`:
///   * a blank embed text (empty string) → skip (vector stays `None`);
///   * a non-finite / all-zero vector → fail;
///   * a missing segment id (the doc was re-ingested while this job ran, so the
///     id is stale) → silently skipped, which is what makes re-ingest atomic;
///   * otherwise the vector is installed and `embedded` bumped.
fn apply_job(job: EmbedJob, vectors: Option<Vec<Vec<f32>>>) {
    let mut core = match CORE.write() {
        Ok(c) => c,
        Err(_) => return, // poisoned lock: nothing safe to do
    };

    let EmbedJob {
        collection,
        doc_id,
        segment_ids,
        embed_texts,
    } = job;

    // If the collection was cleared (reset / dim-change) while this job was in
    // flight, drop the result on the floor. The job is now meaningless.
    let coll = match core.collections.get_mut(&collection) {
        Some(c) => c,
        None => return,
    };

    let mut embedded = 0u64;
    let mut failed = 0u64;

    if let Some(vectors) = vectors {
        // Locate the (possibly re-ingested) doc once; if it's gone, every id is
        // stale and we just fall through to the bookkeeping below.
        if let Some(doc) = coll.docs.get_mut(&doc_id) {
            // Index this doc's current segments by id for an O(1) lookup, so a
            // re-ingest that shuffled/replaced segments still fills correctly and
            // stale ids from a superseded generation are simply absent.
            let mut by_id: HashMap<u64, &mut Segment> = doc
                .segments
                .iter_mut()
                .map(|s| (s.segment_id, s))
                .collect();

            for (i, &sid) in segment_ids.iter().enumerate() {
                // Blank text was marked skip at accept; leave vector None.
                if embed_texts.get(i).map(|t| t.is_empty()).unwrap_or(true) {
                    continue;
                }
                // Stale id (doc re-ingested) → segment absent → skip silently.
                let seg = match by_id.get_mut(&sid) {
                    Some(s) => s,
                    None => continue,
                };
                match vectors.get(i) {
                    Some(v) if v.iter().all(|x| x.is_finite()) && v.iter().any(|&x| x != 0.0) => {
                        seg.vector = Some(v.clone());
                        embedded += 1;
                    }
                    _ => {
                        // Non-finite / all-zero vector → fail, but keep going.
                        failed += 1;
                    }
                }
            }
        }
    }

    coll.embedded = coll.embedded.saturating_add(embedded);
    coll.failed = coll.failed.saturating_add(failed);
    coll.pending_jobs = coll.pending_jobs.saturating_sub(1);
    if coll.pending_jobs == 0 {
        coll.vector_status = VectorStatus::Ready;
    }
}

// ===========================================================================
// index_raw — normalize + offset table + chunker + async accept
// ===========================================================================

/// Default token target per chunk when the request omits `chunk_cfg`. The
/// chunker aims for chunks around this many estimated tokens, snapping to whole
/// lines, so a chunk is usually slightly under or over this figure.
pub const DEFAULT_CHUNK_TARGET_TOKENS: usize = 300;

/// Default number of overlapping lines carried into the next chunk so a match
/// straddling a chunk boundary is still wholly present in at least one chunk.
const DEFAULT_OVERLAP_LINES: usize = 2;

/// Estimate the token count of a piece of text.
///
/// IMPORTANT — heuristic placeholder. There is no real tokenizer in this slice
/// (the [`MockEmbedder`] is a bag-of-hashed-tokens model with no vocabulary). We
/// approximate a transformer subword count with `max(whitespace_words, chars/4)`:
/// whitespace words give a sane floor for normal prose, while `chars/4` (the
/// common "~4 chars per token" rule of thumb) dominates for long/CJK/no-space
/// runs so we never wildly under-count. This is the SINGLE place that defines
/// "token count"; when the real embedder + tokenizer land, swap this body for a
/// call into the tokenizer and everything downstream (budget, hard cap, oversized
/// detection) keeps working unchanged.
pub fn estimate_tokens(text: &str) -> usize {
    let words = text.split_whitespace().count();
    let chars = text.chars().count();
    words.max(chars / 4)
}

/// Knobs controlling how [`chunk_text`] splits a document. All optional on the
/// wire (`chunk_cfg`); sensible defaults are filled in by [`ChunkConfig::resolve`].
#[derive(Clone, Copy)]
pub struct ChunkConfig {
    /// Soft per-chunk token budget the chunker aims for (snapped to line ends).
    pub target_tokens: usize,
    /// Hard cap on tokens per chunk; defaults to the configured `max_seq_len`.
    /// A single line over this cap becomes one *oversized* chunk on its own.
    pub max_tokens: usize,
    /// Number of trailing lines of one chunk repeated at the start of the next.
    pub overlap_lines: usize,
}

impl ChunkConfig {
    /// Resolve a (possibly partial) request config against the store's
    /// `max_seq_len`, clamping to keep invariants: target ≥ 1, max ≥ target,
    /// overlap < target so chunking always makes forward progress.
    pub fn resolve(
        target_tokens: Option<usize>,
        max_tokens: Option<usize>,
        overlap_lines: Option<usize>,
        config_max_seq_len: Option<u64>,
    ) -> ChunkConfig {
        let target = target_tokens.unwrap_or(DEFAULT_CHUNK_TARGET_TOKENS).max(1);
        // Hard cap = explicit value, else configured max_seq_len, else fall back
        // to the target itself. Never below the target (that would make every
        // chunk oversized).
        let cap = max_tokens
            .or_else(|| config_max_seq_len.map(|v| v as usize))
            .unwrap_or(target)
            .max(target);
        // Overlap must stay strictly below the target line count so successive
        // chunks always advance; we cap it defensively here too.
        let overlap = overlap_lines.unwrap_or(DEFAULT_OVERLAP_LINES);
        ChunkConfig {
            target_tokens: target,
            max_tokens: cap,
            overlap_lines: overlap,
        }
    }
}

/// One chunk produced by [`chunk_text`]: its 1-based inclusive line range, the
/// exact text of those lines, and whether it is an *oversized* single line.
pub struct Chunk {
    /// 1-based inclusive first line of the chunk.
    pub line_start: u64,
    /// 1-based inclusive last line of the chunk.
    pub line_end: u64,
    /// The chunk's text (the joined lines, LF-separated, no trailing newline).
    pub text: String,
    /// True when this chunk is a single line that alone exceeds `max_tokens`.
    /// Such a chunk is truncated for *embedding* but returned whole by
    /// `get_segment`; we surface the flag so the caller (accept) can truncate
    /// only the embed text and keep the stored segment text intact.
    pub oversized: bool,
}

/// Build the line offset table for `text`: the byte offset at which each
/// `\n`-delimited line begins. `text` MUST already be CRLF→LF normalized.
///
/// The table has exactly one entry per line, where "line count" follows the same
/// model grep uses: `split('\n')` yields `(number of '\n') + 1` pieces. So a
/// trailing `\n` produces a final empty line with its own offset (== text.len()).
/// This lets `get_segment` map a 1-based line number to a byte range in O(1).
pub fn build_line_offsets(text: &str) -> Vec<usize> {
    let mut offsets = vec![0usize]; // line 1 always starts at byte 0
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            // The next line starts right after this '\n'.
            offsets.push(i + 1);
        }
    }
    offsets
}

/// Split a normalized document into line-snapped chunks by token budget.
///
/// Algorithm (greedy, line-granular, with overlap):
///   * Walk the document's lines. Accumulate lines into the current chunk while
///     the running token estimate stays under `target_tokens`.
///   * When adding a line would exceed the target, the current chunk is emitted
///     (snapped to whole line boundaries — we never split mid-line), and the next
///     chunk *backs up* `overlap_lines` lines so consecutive chunks overlap.
///   * Edge case — a single line that alone exceeds `max_tokens` is emitted as
///     its own **oversized** chunk (it cannot be combined or split further at the
///     line granularity). The caller truncates only its *embed* text to the cap;
///     `get_segment` still returns it whole from the stored full text.
///
/// Returns chunks with 1-based inclusive line ranges. An empty document yields a
/// single empty chunk covering line 1 so the doc is always representable.
pub fn chunk_text(text: &str, cfg: &ChunkConfig) -> Vec<Chunk> {
    // Line model matches `build_line_offsets` / grep: split on '\n'. This keeps
    // line numbers consistent across chunking, the offset table, and get_segment.
    let lines: Vec<&str> = text.split('\n').collect();
    let n = lines.len();

    let mut chunks: Vec<Chunk> = Vec::new();
    if n == 0 {
        return chunks; // unreachable (split always yields ≥1), but be safe.
    }

    let mut start = 0usize; // 0-based index of the current chunk's first line
    while start < n {
        let mut end = start; // 0-based inclusive index of last line included
        let mut tokens = estimate_tokens(lines[start]);

        // A single first line already over the hard cap → oversized chunk of
        // exactly that line. Don't try to grow it.
        let oversized = tokens > cfg.max_tokens;
        if !oversized {
            // Grow the chunk one line at a time while we stay within the target
            // budget. We always keep at least the starting line (end == start).
            while end + 1 < n {
                let next_tokens = estimate_tokens(lines[end + 1]);
                if tokens + next_tokens > cfg.target_tokens {
                    break;
                }
                tokens += next_tokens;
                end += 1;
            }
        }

        // Materialize the chunk text from the joined lines (LF-separated).
        let text = lines[start..=end].join("\n");
        chunks.push(Chunk {
            line_start: (start + 1) as u64, // 1-based inclusive
            line_end: (end + 1) as u64,
            text,
            oversized,
        });

        // Advance. Normally we step to `end + 1`, but back up `overlap_lines` so
        // consecutive chunks share context. Never let overlap stall progress:
        // the next start must be strictly greater than the current start.
        let next_start = (end + 1).saturating_sub(cfg.overlap_lines);
        start = next_start.max(start + 1);
    }

    chunks
}

/// A parsed `index_raw` request (the raw-document ingest path).
pub struct RawIndexRequest {
    pub collection: String,
    /// Caller-supplied id; `None` ⇒ auto-assign a stable id and return it.
    pub doc_id: Option<String>,
    pub name: String,
    pub meta: serde_json::Value,
    /// The raw document text (any newline convention; normalized on accept).
    pub text: String,
    /// Optional chunker overrides (`target_tokens` / `max_tokens` / `overlap_lines`).
    pub target_tokens: Option<usize>,
    pub max_tokens: Option<usize>,
    pub overlap_lines: Option<usize>,
}

/// Outcome of the synchronous `index_raw` accept. `doc_id` is always populated
/// (auto-assigned when the request omitted it) so the caller can upsert/delete.
pub struct RawAcceptResult {
    pub collection: String,
    pub doc_id: String,
    pub segment_count: usize,
}

/// Auto-assign a stable, collision-resistant `doc_id` for a fire-and-forget
/// `index_raw` with no caller id. Reuses the monotonic segment-id source so the
/// value is unique within the process run. The `raw:` prefix makes auto-ids
/// distinguishable from caller GUIDs in logs/debugging.
fn auto_doc_id(core: &mut Core) -> String {
    format!("raw:{}", core.alloc_segment_id())
}

/// Synchronous accept for `index_raw`. Under the short write lock it:
///   1. normalizes the text CRLF→LF;
///   2. builds the line offset table for the full document;
///   3. stores the full normalized text + offset table on the doc (so
///      `get_segment` works the instant this returns — before any embedding);
///   4. chunks the text (line-snapped, by token budget, with overlap) and
///      installs each chunk as a text segment with `vector: None`;
///   5. marks the collection `text_ready` / `Building` and enqueues ONE embed
///      job for the worker (which later fills only the vectors).
///
/// Returns immediately; embedding happens off-thread, exactly like
/// `index_segments`. The doc is greppable and `get_segment`-able right away.
pub fn accept_index_raw(core: &mut Core, req: RawIndexRequest) -> RawAcceptResult {
    let _embedder = ensure_embedder(core);

    // (1) Normalize newlines once, up front. Everything downstream — offsets,
    // chunk line numbers, stored text — is in terms of this LF-only text.
    let full_text = normalize_newlines(&req.text);

    // (2) Offset table over the full normalized document.
    let line_offsets = build_line_offsets(&full_text);

    // Resolve chunk config against the configured max_seq_len (the hard cap).
    let cfg = ChunkConfig::resolve(
        req.target_tokens,
        req.max_tokens,
        req.overlap_lines,
        core.config.max_seq_len,
    );

    // (4) Chunk the normalized text into line-snapped segments.
    let chunks = chunk_text(&full_text, &cfg);

    // doc_id: caller-supplied or auto-assigned (returned in the ack).
    let doc_id = match req.doc_id {
        Some(id) => id,
        None => auto_doc_id(core),
    };

    // Build the doc's segments + the parallel (segment_id, embed_text) lists.
    let mut segments: Vec<Segment> = Vec::with_capacity(chunks.len());
    let mut segment_ids: Vec<u64> = Vec::with_capacity(chunks.len());
    let mut embed_texts: Vec<String> = Vec::with_capacity(chunks.len());
    let mut skipped_now: u64 = 0;

    for chunk in chunks {
        let segment_id = core.alloc_segment_id();
        // Oversized single line → truncate ONLY the embed text to the hard cap.
        // The stored segment text stays whole (get_segment returns it intact).
        let embed_full = if chunk.oversized {
            truncate_to_tokens(&chunk.text, cfg.max_tokens)
        } else {
            chunk.text.clone()
        };
        let is_blank = embed_full.trim().is_empty();
        if is_blank {
            skipped_now += 1;
        }
        segment_ids.push(segment_id);
        embed_texts.push(if is_blank { String::new() } else { embed_full });
        segments.push(Segment {
            segment_id,
            text: chunk.text,
            embed_text: None,
            line_start: Some(chunk.line_start),
            line_end: Some(chunk.line_end),
            meta: serde_json::json!({}),
            vector: None,
        });
    }

    let segment_count = segments.len();

    // (3) Build the doc carrying the full text + offset table.
    let doc = Document {
        doc_id: doc_id.clone(),
        name: req.name,
        meta: req.meta,
        segments,
        full_text: Some(full_text),
        line_offsets: Some(line_offsets),
    };

    // (5) Short write-locked accept: install text+offsets, mark Building, upsert.
    let coll = core
        .collections
        .entry(req.collection.clone())
        .or_insert_with(Collection::new);
    coll.text_ready = true;
    coll.vector_status = VectorStatus::Building;
    coll.error = None;
    coll.pending_jobs = coll.pending_jobs.saturating_add(1);
    coll.skipped = coll.skipped.saturating_add(skipped_now);
    // Atomic upsert by doc_id (same semantics as index_segments).
    coll.docs.insert(doc_id.clone(), doc);

    // Enqueue the embed job for the worker (fills vectors off-thread).
    enqueue_job(
        core,
        EmbedJob {
            collection: req.collection.clone(),
            doc_id: doc_id.clone(),
            segment_ids,
            embed_texts,
        },
    );

    RawAcceptResult {
        collection: req.collection,
        doc_id,
        segment_count,
    }
}

/// Truncate `text` to roughly `max_tokens` for *embedding only*. Uses the same
/// "~4 chars per token" rule of thumb as [`estimate_tokens`]; cut on a char
/// boundary so we never split a UTF-8 sequence. The original line is preserved
/// elsewhere (stored segment text) — this only bounds what we hand the embedder.
fn truncate_to_tokens(text: &str, max_tokens: usize) -> String {
    let max_chars = max_tokens.saturating_mul(4);
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect()
}

// ===========================================================================
// get_segment — O(1) line-range slice over the offset table
// ===========================================================================

/// A parsed `get_segment` request.
pub struct GetSegmentRequest {
    pub doc_id: String,
    pub line_start: u64,
    pub line_end: u64,
    /// Optional hard cap on how many lines are returned (after clamping).
    pub max_lines: Option<usize>,
}

/// Why `get_segment` could not produce a slice. Surfaced as a structural error.
pub enum GetSegmentError {
    /// No document with that `doc_id` exists in any collection.
    NotFound,
    /// The document exists but has no full-text/offset table (an atomic
    /// `index_segments` record). Line slicing is not supported for it.
    NoLineIndex,
}

/// Successful `get_segment` result: the sliced text plus the *actual* (clamped)
/// 1-based inclusive line range that was returned.
pub struct GetSegmentResult {
    pub doc_id: String,
    pub line_start: u64,
    pub line_end: u64,
    pub text: String,
    /// Total number of lines in the document (so the caller can page).
    pub line_count: u64,
}

/// Return the text of lines `[line_start, line_end]` (1-based inclusive) from a
/// document's stored full text via its offset table — O(1) per boundary, then a
/// single substring copy.
///
/// Clamping: an out-of-range request is clamped to `[1, line_count]` and the
/// *actual* range used is returned. `max_lines`, when set, further caps the
/// returned line count (trimming from the end). A reversed request
/// (`line_start > line_end`) is normalized to a single line at the start.
///
/// Errors: an unknown `doc_id` → [`GetSegmentError::NotFound`]; a document with
/// no offset table (atomic `index_segments` record) → [`GetSegmentError::NoLineIndex`].
pub fn get_segment(core: &Core, req: &GetSegmentRequest) -> Result<GetSegmentResult, GetSegmentError> {
    // Find the doc across all collections (doc_id is unique per ingest path; the
    // first match wins). We scan collections because doc_id is the addressing
    // key and the caller doesn't pass a collection here.
    let doc = core
        .collections
        .values()
        .find_map(|coll| coll.docs.get(&req.doc_id))
        .ok_or(GetSegmentError::NotFound)?;

    // Must be a raw doc with a full-text/offset table.
    let (full_text, offsets) = match (&doc.full_text, &doc.line_offsets) {
        (Some(t), Some(o)) => (t, o),
        _ => return Err(GetSegmentError::NoLineIndex),
    };

    let line_count = offsets.len() as u64; // ≥ 1 (offsets always has line 1)

    // Clamp the requested range to [1, line_count]. A 0 or reversed request is
    // normalized: start is clamped to ≥1, end to ≥start, both to ≤ line_count.
    let mut start = req.line_start.clamp(1, line_count);
    let mut end = req.line_end.clamp(start, line_count);

    // Respect max_lines (cap the returned line count, trimming from the end).
    if let Some(max) = req.max_lines {
        let max = max.max(1) as u64;
        if end - start + 1 > max {
            end = start + max - 1;
        }
    }
    let _ = &mut start; // start is final; kept mutable above for clarity.

    // O(1) byte range: line N starts at offsets[N-1]; the slice runs from the
    // start of `start` to the end of `end`. The end of line `end` is the start
    // of line `end+1` minus its '\n' (i.e. offsets[end] - 1), or text.len() for
    // the last line.
    let byte_start = offsets[(start - 1) as usize];
    let byte_end = if (end as usize) < offsets.len() {
        // Start of the next line minus the separating '\n'.
        offsets[end as usize] - 1
    } else {
        full_text.len()
    };
    let text = full_text[byte_start..byte_end].to_string();

    Ok(GetSegmentResult {
        doc_id: req.doc_id.clone(),
        line_start: start,
        line_end: end,
        text,
        line_count,
    })
}

/// Normalize line endings to `\n` (CRLF and bare CR → LF). Mirrors the grep
/// module's normalization so the line model is identical everywhere: chunking,
/// the offset table, grep, and get_segment all agree on what a "line" is.
fn normalize_newlines(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

// ===========================================================================
// search (dense)
// ===========================================================================

/// Parsed `search` request.
pub struct SearchRequest {
    pub query: String,
    pub collection: Option<String>,
    pub k: usize,
    pub min_score: Option<f32>,
    pub max_per_doc: Option<usize>,
    pub include_text: bool,
    /// Combinable meta filters over the hit's effective meta (empty ⇒ no-op).
    pub filter: MetaFilter,
}

/// One search hit.
pub struct Hit {
    pub doc_id: String,
    pub name: String,
    pub collection: String,
    pub meta: serde_json::Value,
    pub segment_id: u64,
    pub line_start: Option<u64>,
    pub line_end: Option<u64>,
    pub score: f32,
    pub text: String,
}

/// Result of a search: ranked hits + a partiality flag (true if any searched
/// collection is still Building, so vectors are still filling in).
pub struct SearchResult {
    pub hits: Vec<Hit>,
    pub partial: bool,
}

/// Dense search: embed the query, score every vectorized segment by dot product
/// (== cosine, normalized), top-k with `min_score` and `max_per_doc` applied.
///
/// `collection` scopes the search; when omitted, all collections are searched.
pub fn search(core: &Core, req: SearchRequest) -> SearchResult {
    let embedder = match core.embedder.as_ref() {
        Some(e) => e,
        // Not configured and nothing indexed → empty, non-partial result.
        None => {
            return SearchResult {
                hits: Vec::new(),
                partial: false,
            }
        }
    };
    let qvec = embedder.embed_query(&req.query);

    // Which collections are in scope, and is any of them still Building?
    let mut partial = false;
    let mut scored: Vec<Hit> = Vec::new();

    for (cname, coll) in core.collections.iter() {
        if let Some(want) = req.collection.as_ref() {
            if want != cname {
                continue;
            }
        }
        if coll.vector_status == VectorStatus::Building {
            partial = true;
        }
        for doc in coll.docs.values() {
            for seg in &doc.segments {
                let v = match &seg.vector {
                    Some(v) => v,
                    None => continue, // not embedded (or skipped) → ignore
                };
                // Meta filter over the hit's effective meta (doc ∪ segment,
                // segment wins). Cheap no-op when no filter was supplied.
                if !req.filter.matches_doc_seg(&doc.meta, &seg.meta) {
                    continue;
                }
                let score = dot(&qvec, v);
                if let Some(min) = req.min_score {
                    if score < min {
                        continue;
                    }
                }
                scored.push(Hit {
                    doc_id: doc.doc_id.clone(),
                    name: doc.name.clone(),
                    collection: cname.clone(),
                    meta: doc.meta.clone(),
                    segment_id: seg.segment_id,
                    line_start: seg.line_start,
                    line_end: seg.line_end,
                    score,
                    text: seg.text.clone(),
                });
            }
        }
    }

    // Rank by descending score (stable on ties by keeping insertion order via
    // a total comparator that treats NaN as lowest — vectors are finite so this
    // is just defensive).
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Apply max_per_doc, then truncate to k.
    let hits = if let Some(max_per_doc) = req.max_per_doc {
        let mut per_doc: HashMap<(String, String), usize> = HashMap::new();
        let mut kept: Vec<Hit> = Vec::new();
        for hit in scored {
            let key = (hit.collection.clone(), hit.doc_id.clone());
            let count = per_doc.entry(key).or_insert(0);
            if *count < max_per_doc {
                *count += 1;
                kept.push(hit);
            }
            if kept.len() >= req.k {
                break;
            }
        }
        kept
    } else {
        scored.into_iter().take(req.k).collect()
    };

    SearchResult { hits, partial }
}

// ===========================================================================
// stats helpers
// ===========================================================================

/// Aggregate totals for `stats`: (total_docs, total_segments).
pub fn totals(core: &Core) -> (usize, usize) {
    let mut docs = 0usize;
    let mut segs = 0usize;
    for coll in core.collections.values() {
        docs += coll.docs.len();
        segs += coll.n_segments();
    }
    (docs, segs)
}

/// The embedder dim, or 0 if not configured.
pub fn current_dim(core: &Core) -> usize {
    core.embedder.as_ref().map(|e| e.dim()).unwrap_or(0)
}

// ===========================================================================
// wait_until_ready — deterministic test/caller helper
// ===========================================================================

/// Block until `collection`'s `vector_status` is `Ready` (or it vanishes / hits
/// `error`), or until `timeout` elapses. Returns `true` if it observed Ready.
///
/// Uses the shared progress condvar so it wakes the instant the worker applies a
/// job — no polling, no sleeps. Safe to call from tests and from callers that
/// want a synchronous "indexing done" point. (Used by the test suite today; a
/// future `await_collection` dispatch arm can expose it over FFI.)
#[cfg_attr(not(test), allow(dead_code))]
pub fn wait_until_ready(collection: &str, timeout: std::time::Duration) -> bool {
    // Snapshot the condvar handle without holding the index lock across waits.
    let progress = {
        match CORE.read() {
            Ok(c) => c.progress_handle(),
            Err(_) => return false,
        }
    };
    let deadline = std::time::Instant::now() + timeout;
    let (lock, cvar) = &*progress;
    let mut guard = lock.lock().expect("progress mutex");
    loop {
        // Check current state under the index read lock.
        if let Ok(core) = CORE.read() {
            match core.collections.get(collection) {
                Some(c) if c.vector_status == VectorStatus::Ready => return true,
                Some(c) if c.error.is_some() => return false,
                None => return false, // gone (reset) → never going to be ready
                _ => {}
            }
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return false;
        }
        let remaining = deadline - now;
        let (g, timed_out) = cvar
            .wait_timeout(guard, remaining)
            .expect("progress condvar wait");
        guard = g;
        if timed_out.timed_out() {
            // Final check after timeout before giving up.
            if let Ok(core) = CORE.read() {
                if let Some(c) = core.collections.get(collection) {
                    return c.vector_status == VectorStatus::Ready;
                }
            }
            return false;
        }
    }
}

// ===========================================================================
// shutdown
// ===========================================================================

/// Stop and join the background worker. Idempotent and safe to call from a C++
/// teardown path. After this the worker is gone; a later ingest spawns a fresh
/// one lazily.
pub fn shutdown() {
    // Take the worker out of the singleton under the write lock, then join it
    // OUTSIDE the lock (joining holds no index lock, so a job applying mid-join
    // can still acquire the write lock and finish cleanly).
    let worker = match CORE.write() {
        Ok(mut c) => c.worker.take(),
        Err(_) => return,
    };
    if let Some(mut w) = worker {
        let _ = w.tx.send(WorkerMsg::Stop);
        if let Some(handle) = w.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Process-global mutex shared by ALL test modules (this one and `lib.rs`).
/// Because every test mutates the single `CORE` singleton, they must run
/// serialized; using one shared lock prevents cross-module races (a `reset` in
/// one test wiping another test's in-flight collection).
#[cfg(test)]
pub static TEST_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    /// Acquire the global test mutex so the shared `CORE` singleton isn't raced
    /// by tests running in parallel. Each test resets state at the start.
    ///
    /// Crucially we also **drain the background worker** (`shutdown` joins it)
    /// BEFORE resetting: the worker is a process-global thread shared by every
    /// test, so a job enqueued by a *previous* test could otherwise apply to a
    /// later test's same-named collection and flip it `Ready` prematurely (a
    /// latent cross-test race). Draining first guarantees no stale job survives
    /// into the next test; the worker respawns lazily on the next ingest. This is
    /// test-harness hygiene only — production behaviour is unchanged.
    fn test_lock() -> std::sync::MutexGuard<'static, ()> {
        let g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // Join any in-flight worker so no stale job bleeds into this test, then
        // reset to a clean slate.
        shutdown();
        if let Ok(mut c) = CORE.write() {
            c.reset();
        }
        g
    }

    fn seg(text: &str) -> SegmentInput {
        SegmentInput {
            text: text.to_string(),
            embed_text: None,
            line_start: None,
            line_end: None,
            meta: json!({}),
        }
    }

    fn index(collection: &str, doc_id: &str, texts: &[&str]) {
        let req = IndexRequest {
            collection: collection.to_string(),
            doc_id: doc_id.to_string(),
            name: format!("doc {doc_id}"),
            meta: json!({}),
            segments: texts.iter().map(|t| seg(t)).collect(),
        };
        let mut c = CORE.write().unwrap();
        accept_index(&mut c, req);
    }

    #[test]
    fn configure_sets_dim() {
        let _g = test_lock();
        let mut c = CORE.write().unwrap();
        let res = configure(&mut c, Config::default());
        assert_eq!(res.dim, MockEmbedder::DEFAULT_DIM);
        assert!(c.configured);
        assert!(!res.reset_due_to_dim_change);
    }

    #[test]
    fn reconfigure_different_dim_resets() {
        let _g = test_lock();
        // Force a small-dim embedder + some data, then reconfigure to default.
        {
            let mut c = CORE.write().unwrap();
            c.embedder = Some(Arc::new(MockEmbedder::with_dim(8)));
            let coll = c.collections.entry("k".to_string()).or_insert_with(Collection::new);
            coll.text_ready = true;
            coll.docs.insert(
                "d1".to_string(),
                Document {
                    doc_id: "d1".to_string(),
                    name: "n".to_string(),
                    meta: json!({}),
                    segments: vec![],
                    full_text: None,
                    line_offsets: None,
                },
            );
        }
        let mut c = CORE.write().unwrap();
        let res = configure(&mut c, Config::default());
        assert!(res.reset_due_to_dim_change, "dim 8 → 64 with data must reset");
        assert!(c.collections.is_empty(), "index must be cleared on dim change");
    }

    #[test]
    fn index_returns_immediately_then_becomes_ready() {
        let _g = test_lock();
        index("docs", "d1", &["semantic search over vectors", "another segment here"]);
        // Right after accept the collection exists and is Building.
        {
            let c = CORE.read().unwrap();
            let coll = c.collections.get("docs").unwrap();
            assert!(coll.text_ready);
            assert_eq!(coll.vector_status, VectorStatus::Building);
        }
        // After the worker finishes it flips to Ready.
        assert!(
            wait_until_ready("docs", Duration::from_secs(5)),
            "collection should reach Ready"
        );
        let c = CORE.read().unwrap();
        let coll = c.collections.get("docs").unwrap();
        assert_eq!(coll.vector_status, VectorStatus::Ready);
        assert_eq!(coll.embedded, 2);
        assert_eq!(coll.docs.len(), 1);
    }

    #[test]
    fn text_is_installed_synchronously_at_accept() {
        // Carried-forward fix (Task 1): the doc's TEXT segments must be present
        // in the store the instant accept returns — BEFORE the worker embeds.
        let _g = test_lock();
        {
            // Hold the WRITE lock across accept + inspection. The worker needs the
            // write lock to apply vectors, so while we hold it the worker is
            // blocked — making "text present, vectors still None, Building" a
            // deterministic observation rather than a race.
            let mut c = CORE.write().unwrap();
            let req = IndexRequest {
                collection: "docs".to_string(),
                doc_id: "d1".to_string(),
                name: "doc d1".to_string(),
                meta: json!({}),
                segments: vec![seg("alpha text here"), seg("beta text here")],
            };
            accept_index(&mut c, req);

            let coll = c.collections.get("docs").unwrap();
            assert!(coll.text_ready, "text must be ready at accept");
            let doc = coll.docs.get("d1").expect("doc installed synchronously");
            assert_eq!(doc.segments.len(), 2, "both text segments present");
            assert!(doc.segments.iter().any(|s| s.text == "alpha text here"));
            assert!(doc.segments.iter().any(|s| s.text == "beta text here"));
            // Vectors are NOT yet filled — that is the worker's job.
            assert!(
                doc.segments.iter().all(|s| s.vector.is_none()),
                "vectors must still be None right after accept"
            );
            // The collection is Building until the worker finishes.
            assert_eq!(coll.vector_status, VectorStatus::Building);
        } // write lock released here → worker can now apply.

        // And the worker still fills vectors in place afterwards.
        assert!(wait_until_ready("docs", Duration::from_secs(5)));
        let c = CORE.read().unwrap();
        let doc = c.collections.get("docs").unwrap().docs.get("d1").unwrap();
        assert!(
            doc.segments.iter().all(|s| s.vector.is_some()),
            "worker must fill the already-installed segments' vectors"
        );
    }

    #[test]
    fn search_finds_shared_token_doc() {
        let _g = test_lock();
        index("docs", "db", &["database connection pool tuning"]);
        index("docs", "fruit", &["banana orange apple smoothie"]);
        assert!(wait_until_ready("docs", Duration::from_secs(5)));

        let c = CORE.read().unwrap();
        let res = search(
            &c,
            SearchRequest {
                query: "database connection".to_string(),
                collection: Some("docs".to_string()),
                k: 10,
                min_score: None,
                max_per_doc: None,
                include_text: true,
                filter: MetaFilter::default(),
            },
        );
        assert!(!res.partial);
        assert!(!res.hits.is_empty());
        assert_eq!(res.hits[0].doc_id, "db", "db doc should rank first");
    }

    #[test]
    fn search_respects_k_min_score_max_per_doc() {
        let _g = test_lock();
        index(
            "docs",
            "d1",
            &["alpha beta gamma", "alpha beta delta", "alpha epsilon"],
        );
        index("docs", "d2", &["alpha beta gamma"]);
        assert!(wait_until_ready("docs", Duration::from_secs(5)));
        let c = CORE.read().unwrap();

        // k limits total hits.
        let res = search(
            &c,
            SearchRequest {
                query: "alpha beta gamma".to_string(),
                collection: None,
                k: 2,
                min_score: None,
                max_per_doc: None,
                include_text: true,
                filter: MetaFilter::default(),
            },
        );
        assert_eq!(res.hits.len(), 2, "k must cap hits");

        // max_per_doc limits per document.
        let res = search(
            &c,
            SearchRequest {
                query: "alpha beta gamma".to_string(),
                collection: None,
                k: 10,
                min_score: None,
                max_per_doc: Some(1),
                include_text: true,
                filter: MetaFilter::default(),
            },
        );
        let mut counts: HashMap<String, usize> = HashMap::new();
        for h in &res.hits {
            *counts.entry(h.doc_id.clone()).or_insert(0) += 1;
        }
        assert!(counts.values().all(|&n| n <= 1), "max_per_doc must hold");

        // min_score filters out low scorers.
        let res = search(
            &c,
            SearchRequest {
                query: "alpha beta gamma".to_string(),
                collection: None,
                k: 10,
                min_score: Some(0.99),
                max_per_doc: None,
                include_text: true,
                filter: MetaFilter::default(),
            },
        );
        assert!(
            res.hits.iter().all(|h| h.score >= 0.99),
            "all hits must meet min_score"
        );
    }

    #[test]
    fn search_during_building_is_partial() {
        let _g = test_lock();
        // Make the collection exist + Building without letting the worker run by
        // constructing the Building state directly (no job enqueued).
        {
            let mut c = CORE.write().unwrap();
            c.embedder = Some(Arc::new(MockEmbedder::new()));
            let coll = c.collections.entry("docs".to_string()).or_insert_with(Collection::new);
            coll.text_ready = true;
            coll.vector_status = VectorStatus::Building;
            coll.pending_jobs = 1;
        }
        let c = CORE.read().unwrap();
        let res = search(
            &c,
            SearchRequest {
                query: "anything".to_string(),
                collection: Some("docs".to_string()),
                k: 5,
                min_score: None,
                max_per_doc: None,
                include_text: true,
                filter: MetaFilter::default(),
            },
        );
        assert!(res.partial, "search over a Building collection must be partial");
    }

    #[test]
    fn upsert_replaces_doc_segments() {
        let _g = test_lock();
        index("docs", "d1", &["original unique_alpha_token content"]);
        assert!(wait_until_ready("docs", Duration::from_secs(5)));
        // Re-ingest the same doc_id with different text.
        index("docs", "d1", &["replaced unique_beta_token content"]);
        assert!(wait_until_ready("docs", Duration::from_secs(5)));

        let c = CORE.read().unwrap();
        let coll = c.collections.get("docs").unwrap();
        assert_eq!(coll.docs.len(), 1, "still one doc");
        let texts: Vec<&str> = coll
            .docs
            .get("d1")
            .unwrap()
            .segments
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        assert!(
            texts.iter().any(|t| t.contains("unique_beta_token")),
            "new segment present"
        );
        assert!(
            !texts.iter().any(|t| t.contains("unique_alpha_token")),
            "old segment gone"
        );
    }

    #[test]
    fn blank_segment_is_skipped_and_collection_still_ready() {
        let _g = test_lock();
        let req = IndexRequest {
            collection: "docs".to_string(),
            doc_id: "d1".to_string(),
            name: "n".to_string(),
            meta: json!({}),
            segments: vec![seg("real content here"), seg("   "), seg("")],
        };
        {
            let mut c = CORE.write().unwrap();
            accept_index(&mut c, req);
        }
        assert!(wait_until_ready("docs", Duration::from_secs(5)));
        let c = CORE.read().unwrap();
        let coll = c.collections.get("docs").unwrap();
        assert_eq!(coll.vector_status, VectorStatus::Ready);
        assert_eq!(coll.skipped, 2, "two blank segments skipped");
        assert_eq!(coll.embedded, 1, "one real segment embedded");
    }

    #[test]
    fn stats_helpers_reflect_state() {
        let _g = test_lock();
        index("a", "d1", &["one two three", "alpha beta"]);
        index("b", "d2", &["four five"]);
        assert!(wait_until_ready("a", Duration::from_secs(5)));
        assert!(wait_until_ready("b", Duration::from_secs(5)));
        let c = CORE.read().unwrap();
        let (docs, segs) = totals(&c);
        assert_eq!(docs, 2);
        assert_eq!(segs, 3);
        assert_eq!(current_dim(&c), MockEmbedder::DEFAULT_DIM);
    }

    #[test]
    fn shutdown_then_index_still_works() {
        let _g = test_lock();
        index("docs", "d1", &["hello world"]);
        assert!(wait_until_ready("docs", Duration::from_secs(5)));
        shutdown();
        // Worker is gone; a new ingest must spawn a fresh one and still finish.
        index("docs", "d2", &["second document"]);
        assert!(
            wait_until_ready("docs", Duration::from_secs(5)),
            "ingest after shutdown should respawn worker and complete"
        );
    }

    // ===================================================================
    // index_raw: chunker + offset table (pure-function unit tests)
    // ===================================================================

    /// Build a config with a known small target/cap/overlap for deterministic
    /// chunking tests independent of the default 300-token budget.
    fn cfg(target: usize, cap: usize, overlap: usize) -> ChunkConfig {
        ChunkConfig::resolve(Some(target), Some(cap), Some(overlap), None)
    }

    #[test]
    fn estimate_tokens_uses_max_of_words_and_chars_over_4() {
        // Five short words → 5 words; chars/4 is smaller, so words wins.
        assert_eq!(estimate_tokens("a b c d e"), 5);
        // One long no-space run: words=1 but chars/4 dominates so we don't
        // wildly under-count a single huge token.
        let long = "x".repeat(40);
        assert_eq!(estimate_tokens(&long), 10); // 40 chars / 4
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn offset_table_maps_lines_to_byte_starts() {
        // 3 lines, no trailing newline.
        let text = "ab\ncde\nf";
        let off = build_line_offsets(text);
        assert_eq!(off, vec![0, 3, 7]); // "ab\n"=0..3, "cde\n"=3..7, "f"=7
        // A trailing newline yields a final empty line with its own offset.
        let text2 = "ab\ncd\n";
        let off2 = build_line_offsets(text2);
        assert_eq!(off2, vec![0, 3, 6]); // last (empty) line starts at len()
    }

    #[test]
    fn chunker_snaps_to_lines_and_overlaps() {
        // Six single-word lines; target 3 tokens means ~3 lines per chunk.
        // overlap=1 → the last line of a chunk repeats as the first of the next.
        let text = "l1\nl2\nl3\nl4\nl5\nl6";
        let chunks = chunk_text(text, &cfg(3, 100, 1));
        // Each chunk is at most 3 lines (the target), snapped to whole lines.
        assert!(chunks.len() >= 2, "must split into multiple chunks");
        for ch in &chunks {
            assert!(ch.line_end >= ch.line_start, "range well-formed");
            assert!(!ch.oversized);
            // The chunk text is exactly the joined source lines.
            let all_lines: Vec<&str> = text.split('\n').collect();
            let joined =
                all_lines[(ch.line_start - 1) as usize..=(ch.line_end - 1) as usize].join("\n");
            assert_eq!(ch.text, joined);
        }
        // Overlap: consecutive chunks must share at least `overlap` line(s) — the
        // next chunk starts no later than the previous chunk's last line.
        for w in chunks.windows(2) {
            assert!(
                w[1].line_start <= w[0].line_end,
                "consecutive chunks must overlap (got {} then start {})",
                w[0].line_end,
                w[1].line_start
            );
        }
        // Coverage: line 1 is in the first chunk, the last line in the last.
        assert_eq!(chunks.first().unwrap().line_start, 1);
        assert_eq!(chunks.last().unwrap().line_end, 6);
    }

    #[test]
    fn chunker_oversized_single_line_is_one_chunk() {
        // A single line whose token estimate exceeds the hard cap (cap=3) must
        // become exactly one oversized chunk covering just that line.
        let huge = "tok ".repeat(50); // ~50 whitespace tokens, one line
        let text = format!("short before\n{}\nshort after", huge.trim());
        let chunks = chunk_text(&text, &cfg(3, 5, 1));
        // Find the oversized chunk: it must be exactly one line, whole.
        let oversized: Vec<&Chunk> = chunks.iter().filter(|c| c.oversized).collect();
        assert_eq!(oversized.len(), 1, "exactly one oversized chunk");
        let o = oversized[0];
        assert_eq!(o.line_start, o.line_end, "oversized chunk is a single line");
        assert!(o.text.starts_with("tok"), "oversized chunk holds the whole line");
        assert_eq!(o.text, huge.trim(), "stored oversized text is the full line");
    }

    #[test]
    fn truncate_to_tokens_bounds_embed_text_on_char_boundary() {
        let s = "a".repeat(100);
        let t = truncate_to_tokens(&s, 5); // 5 tokens * 4 = 20 chars
        assert_eq!(t.chars().count(), 20);
        // Short text is returned unchanged.
        assert_eq!(truncate_to_tokens("hi", 5), "hi");
    }

    // ===================================================================
    // get_segment: O(1) slice, clamping, structural errors
    // ===================================================================

    /// Accept a raw doc synchronously under the write lock and return its doc_id.
    fn index_raw_doc(collection: &str, doc_id: Option<&str>, text: &str) -> String {
        let mut c = CORE.write().unwrap();
        let res = accept_index_raw(
            &mut c,
            RawIndexRequest {
                collection: collection.to_string(),
                doc_id: doc_id.map(String::from),
                name: "raw doc".to_string(),
                meta: json!({}),
                text: text.to_string(),
                target_tokens: Some(3),
                max_tokens: Some(8),
                overlap_lines: Some(1),
            },
        );
        res.doc_id
    }

    #[test]
    fn get_segment_basic_slice_and_immediate_after_accept() {
        let _g = test_lock();
        // CRLF on the wire → normalized to LF; get_segment works immediately,
        // before any vectors are computed (we never wait_until_ready here).
        let id = index_raw_doc("docs", Some("r1"), "line one\r\nline two\r\nline three");
        assert_eq!(id, "r1");
        let c = CORE.read().unwrap();
        let res = get_segment(
            &c,
            &GetSegmentRequest {
                doc_id: "r1".to_string(),
                line_start: 2,
                line_end: 3,
                max_lines: None,
            },
        )
        .unwrap_or_else(|_| panic!("slice must succeed"));
        // CRLF was normalized away; the slice is exactly lines 2..=3.
        assert_eq!(res.text, "line two\nline three");
        assert_eq!(res.line_start, 2);
        assert_eq!(res.line_end, 3);
        assert_eq!(res.line_count, 3);
    }

    #[test]
    fn get_segment_out_of_range_clamps_and_returns_actual() {
        let _g = test_lock();
        index_raw_doc("docs", Some("r2"), "a\nb\nc");
        let c = CORE.read().unwrap();
        // Request lines 0..=99 → clamp to 1..=3 and report the actual range.
        let res = get_segment(
            &c,
            &GetSegmentRequest {
                doc_id: "r2".to_string(),
                line_start: 0,
                line_end: 99,
                max_lines: None,
            },
        )
        .unwrap_or_else(|_| panic!("clamped slice must succeed"));
        assert_eq!(res.line_start, 1);
        assert_eq!(res.line_end, 3);
        assert_eq!(res.text, "a\nb\nc");
    }

    #[test]
    fn get_segment_respects_max_lines() {
        let _g = test_lock();
        index_raw_doc("docs", Some("r3"), "a\nb\nc\nd\ne");
        let c = CORE.read().unwrap();
        let res = get_segment(
            &c,
            &GetSegmentRequest {
                doc_id: "r3".to_string(),
                line_start: 2,
                line_end: 5,
                max_lines: Some(2),
            },
        )
        .unwrap_or_else(|_| panic!("max_lines slice must succeed"));
        assert_eq!(res.line_start, 2);
        assert_eq!(res.line_end, 3, "max_lines caps the returned range");
        assert_eq!(res.text, "b\nc");
    }

    #[test]
    fn get_segment_on_atomic_record_is_no_line_index() {
        let _g = test_lock();
        // An index_segments doc has no full text / offset table.
        index("docs", "atomic1", &["just a segment, no document text"]);
        let c = CORE.read().unwrap();
        let err = get_segment(
            &c,
            &GetSegmentRequest {
                doc_id: "atomic1".to_string(),
                line_start: 1,
                line_end: 1,
                max_lines: None,
            },
        );
        assert!(matches!(err, Err(GetSegmentError::NoLineIndex)));
    }

    #[test]
    fn get_segment_unknown_doc_is_not_found() {
        let _g = test_lock();
        let c = CORE.read().unwrap();
        let err = get_segment(
            &c,
            &GetSegmentRequest {
                doc_id: "nope".to_string(),
                line_start: 1,
                line_end: 1,
                max_lines: None,
            },
        );
        assert!(matches!(err, Err(GetSegmentError::NotFound)));
    }

    #[test]
    fn index_raw_auto_assigns_doc_id() {
        let _g = test_lock();
        // No doc_id supplied → a stable id is auto-assigned and returned.
        let id = index_raw_doc("docs", None, "auto id document body");
        assert!(!id.is_empty(), "auto doc_id must be returned");
        let c = CORE.read().unwrap();
        assert!(
            c.collections
                .get("docs")
                .unwrap()
                .docs
                .contains_key(&id),
            "auto-assigned doc is addressable by the returned id"
        );
    }

    #[test]
    fn index_raw_doc_embeds_in_background() {
        // `test_lock()` already drains the shared worker, so no stale job can
        // flip our collection Ready before our own embed job applies.
        let _g = test_lock();
        index_raw_doc("docs", Some("rb"), "alpha beta\ngamma delta\nepsilon zeta");
        // The worker fills vectors off-thread; collection reaches Ready.
        assert!(wait_until_ready("docs", Duration::from_secs(5)));
        let c = CORE.read().unwrap();
        let doc = c.collections.get("docs").unwrap().docs.get("rb").unwrap();
        assert!(
            doc.segments.iter().any(|s| s.vector.is_some()),
            "raw-doc chunks must get vectors from the worker"
        );
        // Full text + offset table are retained on the raw doc.
        assert!(doc.full_text.is_some());
        assert!(doc.line_offsets.is_some());
    }
}
