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
#[derive(Clone)]
pub struct Document {
    /// Required stable id (a 1C reference / GUID). Everything that is later
    /// updated or deleted is keyed by this.
    pub doc_id: String,
    pub name: String,
    /// Doc-level metadata (JSON object), echoed back in hits.
    pub meta: serde_json::Value,
    pub segments: Vec<Segment>,
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
    fn test_lock() -> std::sync::MutexGuard<'static, ()> {
        let g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // Fresh state for every test.
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
}
