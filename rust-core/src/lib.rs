//! `rcore` — the Rust search core for the 1C MCP component.
//!
//! ===========================================================================
//! FFI MEMORY-OWNERSHIP CONVENTION (read before touching this module)
//! ===========================================================================
//! Strings cross the C ABI boundary as JSON. The ownership rules are strict and
//! enforced here so the C++ side cannot leak, double-free, or free across CRTs:
//!
//!   1. Every `*const c_char` / `*mut c_char` returned to C is allocated by
//!      Rust via `CString::into_raw`. Rust hands ownership to the caller.
//!
//!   2. The caller MUST return that pointer to `rcore_free_string` exactly once
//!      to release it. `rcore_free_string` reclaims it with `CString::from_raw`
//!      and drops it — freeing on Rust's allocator/CRT, the same one that
//!      allocated it. The C++ side must NEVER call `free`/`delete` on these
//!      pointers (that would be a cross-CRT free → heap corruption).
//!
//!   3. Input pointers (`method`, `payload_json`) are *borrowed* from C. Rust
//!      copies what it needs and never frees them.
//!
//!   4. Null and double-free are guarded: `rcore_free_string(NULL)` is a no-op;
//!      passing a non-null pointer twice is a use-after-free and is the
//!      caller's contract to avoid (the C++ `RustString` RAII wrapper in
//!      RustCore.h enforces single-free on that side).
//!
//! The boundary is also panic-free: every `extern "C"` function wraps its body
//! in `catch_unwind`, because unwinding across the FFI boundary is undefined
//! behaviour. On an unexpected panic we return a structural JSON error.
//! ===========================================================================

mod core;
mod embed;
// The real fastembed-backed embedder is compiled only under the `fastembed`
// feature so the default (mock-only) build never pulls in ort/tokenizers.
#[cfg(feature = "fastembed")]
mod fastembed_embedder;
mod filter;
mod grep;
mod protocol;

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::panic::{self, AssertUnwindSafe};

use serde_json::{json, Value};

use crate::core::{
    self as store, Config, GetSegmentRequest, IndexRequest, RawIndexRequest, SearchMode,
    SearchRequest, SegmentInput, CORE,
};
use crate::filter::MetaFilter;
use crate::grep::GrepRequest;
use crate::protocol::{codes, Envelope};

/// ABI revision of this boundary. Bump when the FFI surface changes shape so the
/// C++ side can detect a mismatch at runtime (via `rcore_version`).
const ABI_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Move an owned `String` across the boundary as a heap `char*` owned by C.
///
/// Returns a pointer that MUST be freed by `rcore_free_string`. If `s` contains
/// an interior NUL byte (which valid JSON never does) we substitute a constant
/// error JSON so we still return a freeable, well-formed C string.
fn into_c_string(s: String) -> *mut c_char {
    match CString::new(s) {
        Ok(cs) => cs.into_raw(),
        Err(_) => CString::new(
            r#"{"ok":false,"error":{"code":"internal","message":"interior NUL in response"}}"#,
        )
        .expect("static error string is NUL-free")
        .into_raw(),
    }
}

/// Borrow a C string as a Rust `&str` without taking ownership. Returns `None`
/// for a null pointer or non-UTF-8 input.
///
/// # Safety
/// `ptr` must be null or point to a valid NUL-terminated C string that stays
/// alive for the duration of the borrow.
unsafe fn borrow_c_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    CStr::from_ptr(ptr).to_str().ok()
}

/// Run `f`, converting any panic into a structural JSON error envelope. This is
/// the single place that keeps panics from crossing the C ABI.
fn guard<F: FnOnce() -> String>(f: F) -> String {
    match panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(s) => s,
        Err(_) => Envelope::err(codes::INTERNAL, "panic in core").to_json_string(),
    }
}

/// Lock-free process-global counter of dispatched calls, surfaced via `stats`.
/// Kept OUT of the `RwLock`-guarded store so a `search`/`grep` never has to take
/// the exclusive write lock merely to bump a counter (which would serialize
/// otherwise-concurrent readers).
static CALLS_HANDLED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Method dispatch (pure Rust, no FFI types — easy to unit-test)
// ---------------------------------------------------------------------------

/// Route a `(method, payload)` pair to its handler and return the JSON response
/// string. Unknown methods yield a structural `unknown_method` error rather
/// than panicking. Stage 1 adds `configure` / `index_segments` / `search` arms.
fn dispatch(method: &str, payload_json: &str) -> String {
    // Parse the payload up front. An empty payload is treated as JSON null so
    // stub methods that ignore their input still work with `""`/`"null"`.
    let payload: Value = if payload_json.trim().is_empty() {
        Value::Null
    } else {
        match serde_json::from_str(payload_json) {
            Ok(v) => v,
            Err(e) => {
                return Envelope::err(codes::BAD_PAYLOAD, format!("invalid JSON payload: {e}"))
                    .to_json_string();
            }
        }
    };

    // Count every dispatched call (surfaced via `stats`) WITHOUT taking the
    // index write lock — a lock-free atomic, so concurrent search/grep calls
    // don't serialize on the exclusive lock just to bump a counter.
    CALLS_HANDLED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let envelope = match method {
        // Liveness probe. Echoes the payload back so the C++ side can verify a
        // full JSON round-trip through the boundary.
        "ping" => Envelope::ok(json!({ "pong": true, "echo": payload })),

        // Report process-singleton state: the cold-start progress surfaced to
        // the 1C form. Per-collection two-axis state + counters, plus totals.
        "stats" => {
            let core = match CORE.read() {
                Ok(c) => c,
                Err(_) => {
                    return Envelope::err(codes::INTERNAL, "core lock poisoned").to_json_string()
                }
            };

            // Per-collection two-axis state machine + progress counters.
            let mut collections = serde_json::Map::new();
            for (name, coll) in core.collections.iter() {
                let mut obj = json!({
                    "text_ready": coll.text_ready,
                    "vector_status": coll.vector_status.as_str(),
                    "embedded": coll.embedded,
                    "failed": coll.failed,
                    "skipped": coll.skipped,
                    "n_docs": coll.docs.len(),
                    "n_segments": coll.n_segments(),
                });
                // `error` only present when a fatal fault occurred.
                if let Some(err) = &coll.error {
                    obj["error"] = json!(err);
                }
                collections.insert(name.clone(), obj);
            }

            let (total_docs, total_segments) = store::totals(&core);
            Envelope::ok(json!({
                "configured": core.configured,
                "dim": store::current_dim(&core),
                "n_docs": total_docs,
                "n_segments": total_segments,
                "collections": collections,
                "callsHandled": CALLS_HANDLED.load(std::sync::atomic::Ordering::Relaxed),
            }))
        }

        // Clear mutable state back to defaults. Idempotent.
        "reset" => {
            match CORE.write() {
                Ok(mut core) => core.reset(),
                Err(_) => {
                    return Envelope::err(codes::INTERNAL, "core lock poisoned").to_json_string()
                }
            }
            Envelope::ok(json!({ "reset": true }))
        }

        // Load/select the embedder and fix `dim`. Idempotent; reconfiguring
        // with a different dim while data is indexed resets the index.
        "configure" => match parse_config(&payload) {
            Ok(config) => {
                let mut core = match CORE.write() {
                    Ok(c) => c,
                    Err(_) => {
                        return Envelope::err(codes::INTERNAL, "core lock poisoned")
                            .to_json_string()
                    }
                };
                let res = store::configure(&mut core, config);
                Envelope::ok(json!({
                    "configured": true,
                    "dim": res.dim,
                    "reset": res.reset_due_to_dim_change,
                    "model": core.config.model,
                    "model_path": core.config.model_path,
                    "normalize": core.config.normalize,
                    "max_seq_len": core.config.max_seq_len,
                    "device": core.config.device,
                    "intra_threads": core.config.intra_threads,
                }))
            }
            Err(e) => Envelope::err(codes::BAD_PAYLOAD, e),
        },

        // Asynchronous ingest: synchronous accept under a short lock, then the
        // background worker embeds off-thread. Returns immediately.
        "index_segments" => match parse_index_request(&payload) {
            Ok(req) => {
                let mut core = match CORE.write() {
                    Ok(c) => c,
                    Err(_) => {
                        return Envelope::err(codes::INTERNAL, "core lock poisoned")
                            .to_json_string()
                    }
                };
                let res = store::accept_index(&mut core, req);
                Envelope::ok(json!({
                    "accepted": true,
                    "collection": res.collection,
                    "segment_count": res.segment_count,
                }))
            }
            Err(e) => Envelope::err(codes::BAD_PAYLOAD, e),
        },

        // Asynchronous raw-document ingest: under a short lock we normalize the
        // text, build a line offset table, store the full text + table, chunk it
        // (line-snapped, by token budget, with overlap) and install the chunks
        // (vector=None); the worker embeds off-thread. Returns immediately, with
        // the (possibly auto-assigned) doc_id so the caller can upsert/delete.
        "index_raw" => match parse_raw_index_request(&payload) {
            Ok(req) => {
                let mut core = match CORE.write() {
                    Ok(c) => c,
                    Err(_) => {
                        return Envelope::err(codes::INTERNAL, "core lock poisoned")
                            .to_json_string()
                    }
                };
                let res = store::accept_index_raw(&mut core, req);
                Envelope::ok(json!({
                    "accepted": true,
                    "collection": res.collection,
                    "doc_id": res.doc_id,
                    "segment_count": res.segment_count,
                }))
            }
            Err(e) => Envelope::err(codes::BAD_PAYLOAD, e),
        },

        // O(1) line-range slice of a raw document's stored full text via the
        // offset table. Works the instant after accept (text+offsets are
        // synchronous). Out-of-range clamps and returns the ACTUAL range used.
        // An atomic index_segments record (no offset table) → no_line_index.
        "get_segment" => match parse_get_segment_request(&payload) {
            Ok(req) => {
                let core = match CORE.read() {
                    Ok(c) => c,
                    Err(_) => {
                        return Envelope::err(codes::INTERNAL, "core lock poisoned")
                            .to_json_string()
                    }
                };
                match store::get_segment(&core, &req) {
                    Ok(res) => Envelope::ok(json!({
                        "doc_id": res.doc_id,
                        "line_start": res.line_start,
                        "line_end": res.line_end,
                        "line_count": res.line_count,
                        "text": res.text,
                    })),
                    Err(store::GetSegmentError::NotFound) => Envelope::err(
                        codes::NOT_FOUND,
                        format!("no document with doc_id '{}'", req.doc_id),
                    ),
                    Err(store::GetSegmentError::NoLineIndex) => Envelope::err(
                        codes::NO_LINE_INDEX,
                        format!(
                            "doc_id '{}' has no line index (not an index_raw document)",
                            req.doc_id
                        ),
                    ),
                }
            }
            Err(e) => Envelope::err(codes::BAD_PAYLOAD, e),
        },

        // Dense top-k search over normalized vectors (dot product).
        "search" => match parse_search_request(&payload) {
            Ok(req) => {
                let include_text = req.include_text;
                let core = match CORE.read() {
                    Ok(c) => c,
                    Err(_) => {
                        return Envelope::err(codes::INTERNAL, "core lock poisoned")
                            .to_json_string()
                    }
                };
                let res = store::search(&core, req);
                let hits: Vec<Value> = res
                    .hits
                    .iter()
                    .map(|h| hit_to_json(h, include_text))
                    .collect();
                Envelope::ok(json!({
                    "hits": hits,
                    "partial": res.partial,
                }))
            }
            Err(e) => Envelope::err(codes::BAD_PAYLOAD, e),
        },

        // Regex search over stored segment TEXT. Needs no vectors, so it works
        // the instant a doc is accepted (even while vectors are still Building).
        // A broken pattern is a structural `bad_pattern` error, never a panic.
        "grep" => match parse_grep_request(&payload) {
            Ok(req) => {
                let core = match CORE.read() {
                    Ok(c) => c,
                    Err(_) => {
                        return Envelope::err(codes::INTERNAL, "core lock poisoned")
                            .to_json_string()
                    }
                };
                match grep::grep(&core, &req) {
                    Ok(res) => {
                        let hits: Vec<Value> = res.hits.iter().map(grep::hit_to_json).collect();
                        Envelope::ok(json!({
                            "hits": hits,
                            "truncated": res.truncated,
                            "total_found": res.total_found,
                        }))
                    }
                    Err(grep::GrepError::BadPattern(msg)) => {
                        Envelope::err(codes::BAD_PATTERN, format!("invalid grep pattern: {msg}"))
                    }
                }
            }
            Err(e) => Envelope::err(codes::BAD_PAYLOAD, e),
        },

        // Incremental delete of a single document (and all its segments) by
        // `doc_id`, atomically under one short write lock. An unknown doc_id is
        // a structural no-op (`deleted:false`), never an error — a benign
        // idempotent retry. Echoes which collection it lived in, how many
        // segments went away, and whether the (now-empty) collection was dropped.
        "delete_document" => match parse_delete_document_request(&payload) {
            Ok(doc_id) => {
                let mut core = match CORE.write() {
                    Ok(c) => c,
                    Err(_) => {
                        return Envelope::err(codes::INTERNAL, "core lock poisoned")
                            .to_json_string()
                    }
                };
                let res = store::delete_document(&mut core, &doc_id);
                Envelope::ok(json!({
                    "deleted": res.deleted,
                    "doc_id": doc_id,
                    "collection": res.collection,
                    "removed_segments": res.removed_segments,
                    "collection_dropped": res.collection_dropped,
                }))
            }
            Err(e) => Envelope::err(codes::BAD_PAYLOAD, e),
        },

        // Incremental delete of an entire collection (all docs + segments),
        // atomically. An unknown collection is a structural no-op
        // (`deleted:false`), consistent with `delete_document`.
        "delete_collection" => match parse_delete_collection_request(&payload) {
            Ok(collection) => {
                let mut core = match CORE.write() {
                    Ok(c) => c,
                    Err(_) => {
                        return Envelope::err(codes::INTERNAL, "core lock poisoned")
                            .to_json_string()
                    }
                };
                let res = store::delete_collection(&mut core, &collection);
                Envelope::ok(json!({
                    "deleted": res.deleted,
                    "collection": collection,
                    "removed_docs": res.removed_docs,
                    "removed_segments": res.removed_segments,
                }))
            }
            Err(e) => Envelope::err(codes::BAD_PAYLOAD, e),
        },

        other => Envelope::err(
            codes::UNKNOWN_METHOD,
            format!("no such method: '{other}'"),
        ),
    };

    envelope.to_json_string()
}

// ---------------------------------------------------------------------------
// Payload parsing (JSON -> typed requests). Kept here so the store module stays
// free of JSON shape concerns. Each returns Err(message) on a bad payload.
// ---------------------------------------------------------------------------

/// Pull an optional string field.
fn opt_str(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
}

/// Pull an optional u64 field.
fn opt_u64(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(|x| x.as_u64())
}

/// Pull an optional bool field with a default.
fn bool_or(v: &Value, key: &str, default: bool) -> bool {
    v.get(key).and_then(|x| x.as_bool()).unwrap_or(default)
}

/// Pull a meta object, defaulting to `{}` if absent. Non-object values are
/// coerced to `{}` so hits always echo a JSON object.
fn meta_or_empty(v: &Value, key: &str) -> Value {
    match v.get(key) {
        Some(m) if m.is_object() => m.clone(),
        _ => json!({}),
    }
}

/// Parse a `configure` payload. All fields optional; a null payload is fine.
///
/// `model` selects a built-in real model (e.g. `"multilingual-e5-small"`) and
/// `model_path` points at offline ONNX + tokenizer files; either one (when the
/// `fastembed` feature is compiled in) selects the real embedder, otherwise the
/// mock is used — see `core::configure`.
///
/// `device` is `"cpu" | "dml" | "auto"` (default `"auto"` = DirectML with
/// automatic CPU fallback). It only affects the real embedder; the mock build
/// parses and echoes it but otherwise ignores it.
fn parse_config(payload: &Value) -> Result<Config, String> {
    Ok(Config {
        model: opt_str(payload, "model"),
        model_path: opt_str(payload, "model_path"),
        normalize: bool_or(payload, "normalize", true),
        max_seq_len: opt_u64(payload, "max_seq_len"),
        // Default `"auto"` = DirectML with ort's automatic CPU fallback (the
        // GPU-acceleration default). Explicit `"cpu"`/`"dml"` force the EP. Only
        // meaningful under the `fastembed` feature; ignored by the mock build.
        device: opt_str(payload, "device").unwrap_or_else(|| "auto".to_string()),
        intra_threads: opt_u64(payload, "intra_threads"),
    })
}

/// Parse an `index_segments` payload. `doc_id` is required and non-empty.
fn parse_index_request(payload: &Value) -> Result<IndexRequest, String> {
    let collection = opt_str(payload, "collection")
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "missing or empty 'collection'".to_string())?;
    let doc_id = opt_str(payload, "doc_id")
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "missing or empty 'doc_id' (required for upsert/delete)".to_string())?;
    let name = opt_str(payload, "name").unwrap_or_default();
    let meta = meta_or_empty(payload, "meta");

    let segments_val = payload
        .get("segments")
        .and_then(|s| s.as_array())
        .ok_or_else(|| "missing 'segments' array".to_string())?;

    let mut segments = Vec::with_capacity(segments_val.len());
    for (i, s) in segments_val.iter().enumerate() {
        let text = opt_str(s, "text")
            .ok_or_else(|| format!("segment[{i}] missing 'text'"))?;
        segments.push(SegmentInput {
            text,
            embed_text: opt_str(s, "embed_text"),
            line_start: opt_u64(s, "line_start"),
            line_end: opt_u64(s, "line_end"),
            meta: meta_or_empty(s, "meta"),
        });
    }

    Ok(IndexRequest {
        collection,
        doc_id,
        name,
        meta,
        segments,
    })
}

/// Parse an `index_raw` payload. `collection`, `name`, and `text` are required;
/// `doc_id` is optional (auto-assigned when absent). `chunk_cfg` is an optional
/// object with `target_tokens` / `max_tokens` / `overlap_lines` overrides.
fn parse_raw_index_request(payload: &Value) -> Result<RawIndexRequest, String> {
    let collection = opt_str(payload, "collection")
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "missing or empty 'collection'".to_string())?;
    // doc_id is optional; a present-but-blank value is treated as absent so the
    // caller still gets an auto-assigned id rather than an unusable "" key.
    let doc_id = opt_str(payload, "doc_id").filter(|s| !s.trim().is_empty());
    let name = opt_str(payload, "name").unwrap_or_default();
    let meta = meta_or_empty(payload, "meta");
    // `text` is required (an empty string is allowed → a single empty chunk).
    let text = payload
        .get("text")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "missing 'text'".to_string())?;

    // Optional chunker overrides under `chunk_cfg`.
    let cfg = payload.get("chunk_cfg");
    let target_tokens = cfg
        .and_then(|c| opt_u64(c, "target_tokens"))
        .map(|v| v as usize);
    let max_tokens = cfg
        .and_then(|c| opt_u64(c, "max_tokens"))
        .map(|v| v as usize);
    let overlap_lines = cfg
        .and_then(|c| opt_u64(c, "overlap_lines"))
        .map(|v| v as usize);

    Ok(RawIndexRequest {
        collection,
        doc_id,
        name,
        meta,
        text,
        target_tokens,
        max_tokens,
        overlap_lines,
    })
}

/// Parse a `get_segment` payload. `doc_id` is required and non-empty;
/// `line_start` / `line_end` default to 1 (the store clamps out-of-range values).
fn parse_get_segment_request(payload: &Value) -> Result<GetSegmentRequest, String> {
    let doc_id = opt_str(payload, "doc_id")
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "missing or empty 'doc_id'".to_string())?;
    // Missing line bounds default to 1; the store clamps to the actual range.
    let line_start = opt_u64(payload, "line_start").unwrap_or(1);
    let line_end = opt_u64(payload, "line_end").unwrap_or(line_start);
    let max_lines = opt_u64(payload, "max_lines").map(|v| v as usize);
    Ok(GetSegmentRequest {
        doc_id,
        line_start,
        line_end,
        max_lines,
    })
}

/// Parse a `delete_document` payload. `doc_id` is required and non-empty.
fn parse_delete_document_request(payload: &Value) -> Result<String, String> {
    opt_str(payload, "doc_id")
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "missing or empty 'doc_id'".to_string())
}

/// Parse a `delete_collection` payload. `collection` is required and non-empty.
fn parse_delete_collection_request(payload: &Value) -> Result<String, String> {
    opt_str(payload, "collection")
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "missing or empty 'collection'".to_string())
}

/// Parse a `search` payload. `query` required; `k` defaults to 10; `mode`
/// defaults to `dense` (an unknown mode also falls back to `dense`).
fn parse_search_request(payload: &Value) -> Result<SearchRequest, String> {
    let query = opt_str(payload, "query")
        .ok_or_else(|| "missing 'query'".to_string())?;
    let k = opt_u64(payload, "k").map(|v| v as usize).unwrap_or(10).max(1);
    let min_score = payload
        .get("min_score")
        .and_then(|x| x.as_f64())
        .map(|x| x as f32);
    let max_per_doc = opt_u64(payload, "max_per_doc").map(|v| v as usize);
    // `mode`: dense | keyword | hybrid. Absent/unknown → dense (back-compat).
    let mode = SearchMode::parse(payload.get("mode").and_then(|x| x.as_str()));
    Ok(SearchRequest {
        query,
        collection: opt_str(payload, "collection"),
        mode,
        k,
        min_score,
        max_per_doc,
        include_text: bool_or(payload, "include_text", true),
        filter: MetaFilter::parse(payload),
    })
}

/// Parse a `grep` payload. `pattern` is required and non-empty. `max_matches`
/// defaults to 200 (and is clamped to at least 1); `multiline` defaults to true;
/// `context_lines` defaults to 0.
fn parse_grep_request(payload: &Value) -> Result<GrepRequest, String> {
    let pattern = opt_str(payload, "pattern")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "missing or empty 'pattern'".to_string())?;
    let max_matches = opt_u64(payload, "max_matches")
        .map(|v| v as usize)
        .unwrap_or(200)
        .max(1);
    let context_lines = opt_u64(payload, "context_lines").map(|v| v as usize).unwrap_or(0);
    let max_per_doc = opt_u64(payload, "max_per_doc").map(|v| v as usize);
    Ok(GrepRequest {
        pattern,
        collection: opt_str(payload, "collection"),
        filter: MetaFilter::parse(payload),
        ignore_case: bool_or(payload, "ignore_case", false),
        multiline: bool_or(payload, "multiline", true),
        context_lines,
        max_matches,
        max_per_doc,
    })
}

/// Build the JSON object for one hit. When `include_text` is false the full text
/// is replaced by a short `preview` so the wire payload stays small.
fn hit_to_json(h: &store::Hit, include_text: bool) -> Value {
    let mut obj = json!({
        "doc_id": h.doc_id,
        "name": h.name,
        "collection": h.collection,
        "meta": h.meta,
        "segment_id": h.segment_id,
        "score": h.score,
    });
    if let Some(ls) = h.line_start {
        obj["line_start"] = json!(ls);
    }
    if let Some(le) = h.line_end {
        obj["line_end"] = json!(le);
    }
    if include_text {
        obj["text"] = json!(h.text);
    } else {
        // Preview: first ~120 chars, on a char boundary.
        let preview: String = h.text.chars().take(120).collect();
        obj["preview"] = json!(preview);
    }
    obj
}

// ---------------------------------------------------------------------------
// C ABI surface
// ---------------------------------------------------------------------------

/// Return a JSON object describing this core: `{"name", "version", "abi"}`.
///
/// The returned pointer is Rust-owned and MUST be freed with
/// `rcore_free_string`.
#[no_mangle]
pub extern "C" fn rcore_version() -> *mut c_char {
    let s = guard(|| {
        json!({
            "name": env!("CARGO_PKG_NAME"),
            "version": env!("CARGO_PKG_VERSION"),
            "abi": ABI_VERSION,
        })
        .to_string()
    });
    into_c_string(s)
}

/// The single generic JSON-in / JSON-out entry point.
///
/// * `method`       — borrowed C string naming the operation (e.g. "ping").
/// * `payload_json` — borrowed C string with the method's JSON arguments;
///                    may be null or empty (treated as JSON null).
///
/// Returns a Rust-owned JSON string that MUST be freed with `rcore_free_string`.
/// Never returns null and never panics across the boundary.
///
/// # Safety
/// `method` and `payload_json` must each be null or a valid NUL-terminated
/// C string that remains valid for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn rcore_dispatch(
    method: *const c_char,
    payload_json: *const c_char,
) -> *mut c_char {
    let s = guard(|| {
        let method = match borrow_c_str(method) {
            Some(m) => m,
            None => {
                return Envelope::err(codes::BAD_PAYLOAD, "method is null or not UTF-8")
                    .to_json_string()
            }
        };
        // A null payload pointer is allowed and means "no arguments".
        let payload = borrow_c_str(payload_json).unwrap_or("");
        dispatch(method, payload)
    });
    into_c_string(s)
}

/// Free a string previously returned by any `rcore_*` function.
///
/// Calling with a null pointer is a safe no-op. Calling more than once on the
/// same non-null pointer is undefined behaviour (double-free) and is the
/// caller's responsibility to avoid.
///
/// # Safety
/// `s` must be either null or a pointer obtained from an `rcore_*` function and
/// not yet freed.
#[no_mangle]
pub unsafe extern "C" fn rcore_free_string(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    // Reclaim ownership and drop, freeing on Rust's allocator (the same one
    // that allocated it in `into_c_string`). catch_unwind guards the (extremely
    // unlikely) drop panic so it never crosses the boundary.
    let _ = panic::catch_unwind(AssertUnwindSafe(|| {
        drop(CString::from_raw(s));
    }));
}

/// Best-effort teardown hook. Intended for `doStopListen` / form-close on the
/// C++ side (NOT `~HttpServerComponent`, since the singleton outlives any one
/// component). Stage 1 will cancel + join background workers here so the DLL can
/// unload cleanly without crashing inside a still-running native thread.
///
/// For Stage 0 there are no workers, so this is an idempotent, safe no-op. It is
/// explicitly safe to call from a C++ destructor path and safe to call multiple
/// times.
#[no_mangle]
pub extern "C" fn rcore_shutdown() {
    let _ = panic::catch_unwind(|| {
        // Signal the background ingest worker to stop and join it, so the DLL
        // can unload without crashing inside a still-running native thread.
        // Idempotent: if no worker is running this is a no-op, and a later
        // ingest will lazily respawn one.
        store::shutdown();
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    /// Helper: take a Rust-owned `*mut c_char`, copy it to a `String`, then free
    /// it through the real `rcore_free_string` path (exercising the free path).
    unsafe fn take_and_free(ptr: *mut c_char) -> String {
        assert!(!ptr.is_null(), "core returned a null pointer");
        let owned = CStr::from_ptr(ptr).to_str().unwrap().to_owned();
        rcore_free_string(ptr);
        owned
    }

    #[test]
    fn ping_round_trips_and_frees() {
        let method = CString::new("ping").unwrap();
        let payload = CString::new(r#"{"hello":"world"}"#).unwrap();

        let out = unsafe { rcore_dispatch(method.as_ptr(), payload.as_ptr()) };
        let s = unsafe { take_and_free(out) };

        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["result"]["pong"], json!(true));
        assert_eq!(v["result"]["echo"]["hello"], json!("world"));
    }

    #[test]
    fn unknown_method_is_structural_error_not_panic() {
        let method = CString::new("does_not_exist").unwrap();
        let out = unsafe { rcore_dispatch(method.as_ptr(), std::ptr::null()) };
        let s = unsafe { take_and_free(out) };

        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["ok"], json!(false));
        assert_eq!(v["error"]["code"], json!(codes::UNKNOWN_METHOD));
    }

    #[test]
    fn bad_payload_is_structural_error() {
        let method = CString::new("ping").unwrap();
        let payload = CString::new("{not json").unwrap();
        let out = unsafe { rcore_dispatch(method.as_ptr(), payload.as_ptr()) };
        let s = unsafe { take_and_free(out) };

        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["ok"], json!(false));
        assert_eq!(v["error"]["code"], json!(codes::BAD_PAYLOAD));
    }

    #[test]
    fn reset_and_stats_share_singleton_state() {
        // Hold the shared singleton lock so a concurrent `configure`/ingest in
        // another test can't flip `configured` between our reset and stats.
        let _g = crate::core::TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let reset = CString::new("reset").unwrap();
        let stats = CString::new("stats").unwrap();

        let r = unsafe { rcore_dispatch(reset.as_ptr(), std::ptr::null()) };
        let _ = unsafe { take_and_free(r) };

        let out = unsafe { rcore_dispatch(stats.as_ptr(), std::ptr::null()) };
        let s = unsafe { take_and_free(out) };
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["result"]["configured"], json!(false));
        // callsHandled is process-global and monotonic; it must be > 0 because
        // these very calls were counted.
        assert!(v["result"]["callsHandled"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn version_reports_name_and_abi() {
        let out = rcore_version();
        let s = unsafe { take_and_free(out) };
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["name"], json!("rcore"));
        assert_eq!(v["abi"], json!(ABI_VERSION));
    }

    #[test]
    fn free_null_is_noop() {
        // Must not crash.
        unsafe { rcore_free_string(std::ptr::null_mut()) };
    }

    #[test]
    fn shutdown_is_idempotent() {
        rcore_shutdown();
        rcore_shutdown();
    }

    // -- Stage 1 end-to-end dispatch tests (full JSON round-trip via FFI) -----

    /// Serialize tests that mutate the shared `CORE` singleton, and reset it to
    /// a clean slate at the start of each. Uses a process-global mutex so these
    /// don't race each other or the store-level tests.
    fn e2e_guard() -> std::sync::MutexGuard<'static, ()> {
        // Share the SAME lock as the store-level tests so the two modules don't
        // race on the global singleton.
        let g = crate::core::TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        // Drain the shared background worker first so a job enqueued by a prior
        // test can't apply to this test's same-named collection and flip it
        // `Ready` prematurely (the worker respawns lazily on the next ingest).
        store::shutdown();
        let reset = CString::new("reset").unwrap();
        let out = unsafe { rcore_dispatch(reset.as_ptr(), std::ptr::null()) };
        let _ = unsafe { take_and_free(out) };
        g
    }

    /// Call a method by name with a JSON payload and parse the response.
    fn call(method: &str, payload: &str) -> Value {
        let m = CString::new(method).unwrap();
        let p = CString::new(payload).unwrap();
        let out = unsafe { rcore_dispatch(m.as_ptr(), p.as_ptr()) };
        let s = unsafe { take_and_free(out) };
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn configure_echoes_dim_and_config() {
        let _g = e2e_guard();
        let v = call(
            "configure",
            r#"{"model_path":"/models/bge","normalize":true,"device":"cpu","max_seq_len":512}"#,
        );
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["result"]["configured"], json!(true));
        assert!(v["result"]["dim"].as_u64().unwrap() > 0);
        assert_eq!(v["result"]["model_path"], json!("/models/bge"));
        assert_eq!(v["result"]["device"], json!("cpu"));
        assert_eq!(v["result"]["max_seq_len"], json!(512));
    }

    #[test]
    fn index_segments_requires_doc_id() {
        let _g = e2e_guard();
        let v = call(
            "index_segments",
            r#"{"collection":"c","segments":[{"text":"x"}]}"#,
        );
        assert_eq!(v["ok"], json!(false));
        assert_eq!(v["error"]["code"], json!(codes::BAD_PAYLOAD));
    }

    #[test]
    fn full_pipeline_index_search_stats() {
        let _g = e2e_guard();
        call("configure", "{}");

        // Accept is immediate.
        let v = call(
            "index_segments",
            r#"{"collection":"docs","doc_id":"d1","name":"DB guide",
                "meta":{"kind":"manual"},
                "segments":[
                  {"text":"database connection pooling and tuning","line_start":1,"line_end":3},
                  {"text":"banana orange apple smoothie"}
                ]}"#,
        );
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["result"]["accepted"], json!(true));
        assert_eq!(v["result"]["collection"], json!("docs"));
        assert_eq!(v["result"]["segment_count"], json!(2));

        // Wait for the worker (deterministic condvar helper).
        assert!(store::wait_until_ready(
            "docs",
            std::time::Duration::from_secs(5)
        ));

        // Search returns the DB segment as top hit with full metadata.
        let v = call(
            "search",
            r#"{"query":"database connection","collection":"docs","k":5,"include_text":true}"#,
        );
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["result"]["partial"], json!(false));
        let hits = v["result"]["hits"].as_array().unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0]["doc_id"], json!("d1"));
        assert_eq!(hits[0]["name"], json!("DB guide"));
        assert_eq!(hits[0]["collection"], json!("docs"));
        assert_eq!(hits[0]["meta"]["kind"], json!("manual"));
        assert!(hits[0]["text"].as_str().unwrap().contains("database"));
        assert_eq!(hits[0]["line_start"], json!(1));

        // include_text:false yields a preview instead of text.
        let v = call(
            "search",
            r#"{"query":"database connection","collection":"docs","k":1,"include_text":false}"#,
        );
        let hits = v["result"]["hits"].as_array().unwrap();
        assert!(hits[0].get("text").is_none());
        assert!(hits[0].get("preview").is_some());

        // stats reflects two-axis state + counters + totals.
        let v = call("stats", "");
        let r = &v["result"];
        assert_eq!(r["configured"], json!(true));
        assert!(r["dim"].as_u64().unwrap() > 0);
        assert_eq!(r["n_docs"], json!(1));
        assert_eq!(r["n_segments"], json!(2));
        let coll = &r["collections"]["docs"];
        assert_eq!(coll["text_ready"], json!(true));
        assert_eq!(coll["vector_status"], json!("ready"));
        assert_eq!(coll["embedded"], json!(2));
        assert_eq!(coll["failed"], json!(0));
        assert_eq!(coll["skipped"], json!(0));
        assert_eq!(coll["n_docs"], json!(1));
        assert_eq!(coll["n_segments"], json!(2));
    }

    // -- Stage 2: meta filters on `search` (Task 2) ---------------------------

    /// Index two docs with distinguishing meta, wait for vectors, then return.
    /// Both share the query tokens so ranking can't accidentally hide a doc; the
    /// meta filter is what selects between them.
    fn seed_meta_docs() {
        call("configure", "{}");
        call(
            "index_segments",
            r#"{"collection":"c","doc_id":"en1","name":"english",
                "meta":{"lang":"en","status":"draft"},
                "segments":[{"text":"shared token alpha beta"}]}"#,
        );
        call(
            "index_segments",
            r#"{"collection":"c","doc_id":"ru1","name":"russian",
                "meta":{"lang":"ru","status":"published"},
                "segments":[{"text":"shared token alpha beta"}]}"#,
        );
        assert!(store::wait_until_ready("c", std::time::Duration::from_secs(5)));
    }

    #[test]
    fn search_meta_filter_all_is_and() {
        let _g = e2e_guard();
        seed_meta_docs();
        // all: lang==en AND status==draft → only en1.
        let v = call(
            "search",
            r#"{"query":"shared token","collection":"c","k":10,
                "filter":{"all":{"lang":"en","status":"draft"}}}"#,
        );
        let hits = v["result"]["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1, "AND filter must select exactly one doc");
        assert_eq!(hits[0]["doc_id"], json!("en1"));
    }

    #[test]
    fn search_meta_filter_any_is_or() {
        let _g = e2e_guard();
        seed_meta_docs();
        // any: lang==ru OR status==draft → both docs (en1 via status, ru1 via lang).
        let v = call(
            "search",
            r#"{"query":"shared token","collection":"c","k":10,
                "filter":{"any":{"lang":"ru","status":"draft"}}}"#,
        );
        let hits = v["result"]["hits"].as_array().unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h["doc_id"].as_str().unwrap()).collect();
        assert_eq!(hits.len(), 2, "OR filter should match both");
        assert!(ids.contains(&"en1") && ids.contains(&"ru1"));
    }

    #[test]
    fn search_meta_filter_combined() {
        let _g = e2e_guard();
        seed_meta_docs();
        // all (lang==en) AND any (status in [draft, review]) → only en1.
        let v = call(
            "search",
            r#"{"query":"shared token","collection":"c","k":10,
                "filter":{"all":{"lang":"en"},"any":{"status":["draft","review"]}}}"#,
        );
        let hits = v["result"]["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["doc_id"], json!("en1"));

        // Same all-clause but an any-clause that excludes en1's status → empty.
        let v = call(
            "search",
            r#"{"query":"shared token","collection":"c","k":10,
                "filter":{"all":{"lang":"en"},"any":{"status":["review"]}}}"#,
        );
        assert!(v["result"]["hits"].as_array().unwrap().is_empty());
    }

    // -- Stage 2: grep (Task 3) ----------------------------------------------

    /// Index a multi-line doc (text installed synchronously) and wait for ready.
    /// The segment text uses JSON `\r\n` escapes (two-char sequences in the wire
    /// JSON) so the stored text really contains CRLF, exercising normalization.
    fn seed_grep_doc() {
        call("configure", "{}");
        // A 4-line segment with CRLF endings to exercise normalization. The
        // raw-string keeps the backslash-r-backslash-n as JSON escapes, not
        // literal control bytes (which would be invalid JSON).
        call(
            "index_segments",
            r#"{"collection":"docs","doc_id":"g1","name":"log",
                "meta":{"kind":"manual"},
                "segments":[{"text":"first line ERROR here\r\nsecond plain line\r\nthird Error again\r\nfourth done"}]}"#,
        );
        assert!(store::wait_until_ready("docs", std::time::Duration::from_secs(5)));
    }

    #[test]
    fn grep_basic_match() {
        let _g = e2e_guard();
        seed_grep_doc();
        let v = call("grep", r#"{"pattern":"ERROR","collection":"docs"}"#);
        assert_eq!(v["ok"], json!(true));
        let hits = v["result"]["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1, "only the exact-case ERROR line matches");
        assert_eq!(hits[0]["doc_id"], json!("g1"));
        assert_eq!(hits[0]["line_number"], json!(1));
        assert_eq!(hits[0]["line_text"], json!("first line ERROR here"));
        assert_eq!(v["result"]["truncated"], json!(false));
        assert_eq!(v["result"]["total_found"], json!(1));
    }

    #[test]
    fn grep_ignore_case() {
        let _g = e2e_guard();
        seed_grep_doc();
        let v = call(
            "grep",
            r#"{"pattern":"error","collection":"docs","ignore_case":true}"#,
        );
        let hits = v["result"]["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 2, "case-insensitive matches both ERROR and Error");
        // Line numbers are 1-based after CRLF normalization.
        assert_eq!(hits[0]["line_number"], json!(1));
        assert_eq!(hits[1]["line_number"], json!(3));
    }

    #[test]
    fn grep_context_lines() {
        let _g = e2e_guard();
        seed_grep_doc();
        let v = call(
            "grep",
            r#"{"pattern":"third","collection":"docs","context_lines":1}"#,
        );
        let hits = v["result"]["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1);
        let h = &hits[0];
        assert_eq!(h["line_number"], json!(3));
        assert_eq!(h["context_before"], json!(["second plain line"]));
        assert_eq!(h["context_after"], json!(["fourth done"]));
    }

    #[test]
    fn grep_max_matches_truncates() {
        let _g = e2e_guard();
        seed_grep_doc();
        // "line" appears on lines 1 and 2; cap at 1 → truncated.
        let v = call(
            "grep",
            r#"{"pattern":"line","collection":"docs","max_matches":1}"#,
        );
        let hits = v["result"]["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1, "max_matches caps hits");
        assert_eq!(v["result"]["truncated"], json!(true));
    }

    #[test]
    fn grep_broken_pattern_is_structural_error() {
        let _g = e2e_guard();
        seed_grep_doc();
        // An unclosed group is an invalid regex → structural bad_pattern error.
        let v = call("grep", r#"{"pattern":"(unclosed","collection":"docs"}"#);
        assert_eq!(v["ok"], json!(false));
        assert_eq!(v["error"]["code"], json!(codes::BAD_PATTERN));
    }

    #[test]
    fn grep_meta_filtered() {
        let _g = e2e_guard();
        call("configure", "{}");
        call(
            "index_segments",
            r#"{"collection":"docs","doc_id":"m1","name":"a","meta":{"kind":"manual"},
                "segments":[{"text":"keyword target one"}]}"#,
        );
        call(
            "index_segments",
            r#"{"collection":"docs","doc_id":"n1","name":"b","meta":{"kind":"note"},
                "segments":[{"text":"keyword target two"}]}"#,
        );
        assert!(store::wait_until_ready("docs", std::time::Duration::from_secs(5)));
        // Filter to kind==manual → only m1 should match the same pattern.
        let v = call(
            "grep",
            r#"{"pattern":"keyword","collection":"docs","filter":{"all":{"kind":"manual"}}}"#,
        );
        let hits = v["result"]["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1, "meta filter must scope grep");
        assert_eq!(hits[0]["doc_id"], json!("m1"));
    }

    // -- Stage 2: index_raw + get_segment (this card) ------------------------

    #[test]
    fn index_raw_returns_doc_id_and_segment_count() {
        let _g = e2e_guard();
        call("configure", "{}");
        // No doc_id → auto-assigned and returned in the ack so the caller can
        // upsert/delete later.
        let v = call(
            "index_raw",
            r#"{"collection":"raw","name":"manual","text":"line a\nline b\nline c"}"#,
        );
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["result"]["accepted"], json!(true));
        assert_eq!(v["result"]["collection"], json!("raw"));
        let doc_id = v["result"]["doc_id"].as_str().unwrap();
        assert!(!doc_id.is_empty(), "auto doc_id must be returned in the ack");
        assert!(v["result"]["segment_count"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn index_raw_get_segment_works_right_after_accept() {
        let _g = e2e_guard();
        call("configure", "{}");
        // CRLF on the wire is normalized to LF on accept; line numbers are
        // 1-based inclusive. We do NOT wait for vectors — text+offsets are
        // synchronous, so get_segment must work immediately.
        let v = call(
            "index_raw",
            r#"{"collection":"raw","doc_id":"d1","name":"manual",
                "text":"first line\r\nsecond line\r\nthird line\r\nfourth line"}"#,
        );
        assert_eq!(v["result"]["doc_id"], json!("d1"));

        let v = call(
            "get_segment",
            r#"{"doc_id":"d1","line_start":2,"line_end":3}"#,
        );
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["result"]["line_start"], json!(2));
        assert_eq!(v["result"]["line_end"], json!(3));
        assert_eq!(v["result"]["line_count"], json!(4));
        // CRLF normalized away; exactly lines 2..=3 returned.
        assert_eq!(v["result"]["text"], json!("second line\nthird line"));
    }

    #[test]
    fn get_segment_clamps_out_of_range_and_returns_actual() {
        let _g = e2e_guard();
        call("configure", "{}");
        call(
            "index_raw",
            r#"{"collection":"raw","doc_id":"d2","name":"n","text":"a\nb\nc"}"#,
        );
        // Request well beyond the end → clamp to [1,3] and report actual range.
        let v = call(
            "get_segment",
            r#"{"doc_id":"d2","line_start":2,"line_end":900}"#,
        );
        assert_eq!(v["result"]["line_start"], json!(2));
        assert_eq!(v["result"]["line_end"], json!(3));
        assert_eq!(v["result"]["text"], json!("b\nc"));

        // max_lines caps the returned line count.
        let v = call(
            "get_segment",
            r#"{"doc_id":"d2","line_start":1,"line_end":3,"max_lines":1}"#,
        );
        assert_eq!(v["result"]["line_start"], json!(1));
        assert_eq!(v["result"]["line_end"], json!(1));
        assert_eq!(v["result"]["text"], json!("a"));
    }

    #[test]
    fn get_segment_on_atomic_record_is_structural_error() {
        let _g = e2e_guard();
        call("configure", "{}");
        // An atomic index_segments doc has no document-wide text/offset table.
        call(
            "index_segments",
            r#"{"collection":"docs","doc_id":"atomic","name":"a",
                "segments":[{"text":"a segment with no document line index"}]}"#,
        );
        let v = call("get_segment", r#"{"doc_id":"atomic","line_start":1,"line_end":1}"#);
        assert_eq!(v["ok"], json!(false));
        assert_eq!(v["error"]["code"], json!(codes::NO_LINE_INDEX));
    }

    #[test]
    fn get_segment_unknown_doc_is_not_found() {
        let _g = e2e_guard();
        call("configure", "{}");
        let v = call("get_segment", r#"{"doc_id":"ghost","line_start":1,"line_end":1}"#);
        assert_eq!(v["ok"], json!(false));
        assert_eq!(v["error"]["code"], json!(codes::NOT_FOUND));
    }

    #[test]
    fn index_raw_doc_is_greppable_immediately_and_searchable_when_ready() {
        let _g = e2e_guard();
        call("configure", "{}");
        // A multi-line raw doc; chunked synchronously on accept.
        call(
            "index_raw",
            r#"{"collection":"raw","doc_id":"big","name":"guide",
                "text":"intro paragraph\ndatabase connection pooling and tuning\nunrelated banana smoothie\nclosing notes"}"#,
        );
        // Greppable immediately (no vectors needed): the text is in the store.
        let v = call("grep", r#"{"pattern":"database","collection":"raw"}"#);
        assert_eq!(v["ok"], json!(true));
        let hits = v["result"]["hits"].as_array().unwrap();
        assert!(!hits.is_empty(), "raw-doc text must be greppable right after accept");
        assert_eq!(hits[0]["doc_id"], json!("big"));

        // Dense-searchable once the worker finishes embedding the chunks.
        assert!(store::wait_until_ready("raw", std::time::Duration::from_secs(5)));
        let v = call(
            "search",
            r#"{"query":"database connection","collection":"raw","k":5}"#,
        );
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["result"]["partial"], json!(false));
        let hits = v["result"]["hits"].as_array().unwrap();
        assert!(!hits.is_empty(), "raw-doc chunks must be dense-searchable when ready");
        assert_eq!(hits[0]["doc_id"], json!("big"));
        // The hit carries the chunk's 1-based inclusive line range.
        assert!(hits[0]["line_start"].as_u64().is_some());
        assert!(hits[0]["line_end"].as_u64().is_some());
    }

    #[test]
    fn index_raw_oversized_line_truncates_embed_but_get_segment_returns_whole() {
        let _g = e2e_guard();
        // Small max_seq_len so a long line is "oversized". The line stays whole
        // in the store (get_segment), but its embed text is truncated to the cap.
        call("configure", r#"{"max_seq_len":5}"#);
        // Build a single very long line (many tokens) surrounded by short ones.
        let long_line = "word ".repeat(60); // ~60 tokens, well over the cap of 5
        let payload = serde_json::json!({
            "collection": "raw",
            "doc_id": "ov",
            "name": "oversized",
            "text": format!("short head\n{}\nshort tail", long_line.trim()),
        });
        let v = call("index_raw", &payload.to_string());
        assert_eq!(v["ok"], json!(true));

        // get_segment must return the oversized line WHOLE (line 2).
        let v = call("get_segment", r#"{"doc_id":"ov","line_start":2,"line_end":2}"#);
        let text = v["result"]["text"].as_str().unwrap();
        assert_eq!(
            text,
            long_line.trim(),
            "get_segment returns the oversized line in full, untruncated"
        );

        // It still embeds (truncated) in the background and reaches Ready.
        assert!(store::wait_until_ready("raw", std::time::Duration::from_secs(5)));
    }

    // -- Stage 2: hybrid search (mode: dense | keyword | hybrid) -------------

    /// Seed three docs whose contents separate the two retrieval channels:
    ///   * `dwin` — semantically close to "database connection" (dense-strong)
    ///     but has none of the exact-id token, so keyword wouldn't surface it.
    ///   * `kwin` — carries the exact identifier `sku7701234567` (keyword-strong)
    ///     but little semantic overlap, so dense alone may bury it under noise.
    ///   * `noise` — unrelated, to give RRF something to outrank.
    fn seed_hybrid_docs() {
        call("configure", "{}");
        call(
            "index_segments",
            r#"{"collection":"c","doc_id":"dwin","name":"db",
                "segments":[{"text":"database connection pooling and tuning guide"}]}"#,
        );
        call(
            "index_segments",
            r#"{"collection":"c","doc_id":"kwin","name":"sku",
                "segments":[{"text":"sku7701234567 inventory record"}]}"#,
        );
        call(
            "index_segments",
            r#"{"collection":"c","doc_id":"noise","name":"x",
                "segments":[{"text":"completely irrelevant banana smoothie"}]}"#,
        );
        assert!(store::wait_until_ready("c", std::time::Duration::from_secs(5)));
    }

    #[test]
    fn keyword_mode_ranks_by_term_match() {
        let _g = e2e_guard();
        call("configure", "{}");
        // doc m has two distinct query terms; doc o has one; doc z has none.
        call(
            "index_segments",
            r#"{"collection":"c","doc_id":"m","name":"m",
                "segments":[{"text":"alpha beta gamma"}]}"#,
        );
        call(
            "index_segments",
            r#"{"collection":"c","doc_id":"o","name":"o",
                "segments":[{"text":"alpha delta epsilon"}]}"#,
        );
        call(
            "index_segments",
            r#"{"collection":"c","doc_id":"z","name":"z",
                "segments":[{"text":"nothing relevant here"}]}"#,
        );
        assert!(store::wait_until_ready("c", std::time::Duration::from_secs(5)));

        let v = call(
            "search",
            r#"{"query":"alpha beta","collection":"c","mode":"keyword","k":10}"#,
        );
        assert_eq!(v["ok"], json!(true));
        // keyword reads only text → never partial.
        assert_eq!(v["result"]["partial"], json!(false));
        let hits = v["result"]["hits"].as_array().unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h["doc_id"].as_str().unwrap()).collect();
        assert_eq!(ids.len(), 2, "only docs containing a query term match");
        assert_eq!(ids[0], "m", "two-term doc outranks one-term doc");
        assert_eq!(ids[1], "o");
        assert!(!ids.contains(&"z"), "non-matching doc is excluded");
    }

    #[test]
    fn keyword_mode_works_while_building() {
        let _g = e2e_guard();
        call("configure", "{}");
        // Index but do NOT wait for vectors — keyword must work immediately.
        call(
            "index_segments",
            r#"{"collection":"c","doc_id":"b1","name":"b",
                "segments":[{"text":"exact token magicword present"}]}"#,
        );
        let v = call(
            "search",
            r#"{"query":"magicword","collection":"c","mode":"keyword","k":5}"#,
        );
        assert_eq!(v["ok"], json!(true));
        let hits = v["result"]["hits"].as_array().unwrap();
        assert!(!hits.is_empty(), "keyword works before vectors are ready");
        assert_eq!(hits[0]["doc_id"], json!("b1"));
    }

    #[test]
    fn hybrid_mode_rrf_surfaces_both_channels() {
        let _g = e2e_guard();
        seed_hybrid_docs();
        // A query blending a semantic phrase (favors dwin via dense) with the
        // exact id token (favors kwin via keyword). RRF must surface BOTH.
        let v = call(
            "search",
            r#"{"query":"database connection sku7701234567","collection":"c","mode":"hybrid","k":10}"#,
        );
        assert_eq!(v["ok"], json!(true));
        let hits = v["result"]["hits"].as_array().unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h["doc_id"].as_str().unwrap()).collect();
        assert!(ids.contains(&"dwin"), "dense-strong doc must surface");
        assert!(ids.contains(&"kwin"), "keyword-only-strong doc must surface");
    }

    #[test]
    fn keyword_and_hybrid_honor_filter_k_and_max_per_doc() {
        let _g = e2e_guard();
        call("configure", "{}");
        // Two docs share the query term; meta distinguishes them. Each doc has
        // two matching segments so max_per_doc is observable.
        call(
            "index_segments",
            r#"{"collection":"c","doc_id":"en1","name":"en","meta":{"lang":"en"},
                "segments":[{"text":"shared keyword one"},{"text":"shared keyword two"}]}"#,
        );
        call(
            "index_segments",
            r#"{"collection":"c","doc_id":"ru1","name":"ru","meta":{"lang":"ru"},
                "segments":[{"text":"shared keyword three"},{"text":"shared keyword four"}]}"#,
        );
        assert!(store::wait_until_ready("c", std::time::Duration::from_secs(5)));

        // keyword + meta filter (lang==en) → only en1's segments.
        let v = call(
            "search",
            r#"{"query":"shared keyword","collection":"c","mode":"keyword","k":10,
                "filter":{"all":{"lang":"en"}}}"#,
        );
        let hits = v["result"]["hits"].as_array().unwrap();
        assert!(!hits.is_empty());
        assert!(
            hits.iter().all(|h| h["doc_id"] == json!("en1")),
            "keyword must respect the meta filter"
        );

        // keyword + k caps total hits.
        let v = call(
            "search",
            r#"{"query":"shared keyword","collection":"c","mode":"keyword","k":1}"#,
        );
        assert_eq!(v["result"]["hits"].as_array().unwrap().len(), 1, "k caps keyword");

        // hybrid + max_per_doc:1 → at most one hit per doc.
        let v = call(
            "search",
            r#"{"query":"shared keyword","collection":"c","mode":"hybrid","k":10,"max_per_doc":1}"#,
        );
        let hits = v["result"]["hits"].as_array().unwrap();
        let mut per_doc = std::collections::HashMap::new();
        for h in hits {
            *per_doc.entry(h["doc_id"].as_str().unwrap()).or_insert(0) += 1;
        }
        assert!(per_doc.values().all(|&n| n <= 1), "hybrid must respect max_per_doc");
    }

    // -- Stage 2: incremental delete ----------------------------------------

    #[test]
    fn delete_document_removes_from_search_grep_and_get_segment() {
        let _g = e2e_guard();
        call("configure", "{}");
        // A raw doc → searchable + greppable + get_segment-able.
        call(
            "index_raw",
            r#"{"collection":"c","doc_id":"d1","name":"guide",
                "text":"database connection tuning\nsecond informative line"}"#,
        );
        // A second doc so the collection survives the first delete.
        call(
            "index_segments",
            r#"{"collection":"c","doc_id":"d2","name":"keep","segments":[{"text":"keep me around"}]}"#,
        );
        assert!(store::wait_until_ready("c", std::time::Duration::from_secs(5)));

        // Present in all three surfaces before the delete.
        assert!(!call("grep", r#"{"pattern":"database","collection":"c"}"#)["result"]["hits"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(!call("search", r#"{"query":"database connection","collection":"c","mode":"keyword","k":10}"#)["result"]["hits"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(
            call("get_segment", r#"{"doc_id":"d1","line_start":1,"line_end":1}"#)["ok"],
            json!(true)
        );

        // Delete d1.
        let v = call("delete_document", r#"{"doc_id":"d1"}"#);
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["result"]["deleted"], json!(true));
        assert_eq!(v["result"]["collection"], json!("c"));
        assert!(v["result"]["removed_segments"].as_u64().unwrap() >= 1);
        assert_eq!(v["result"]["collection_dropped"], json!(false), "d2 keeps it alive");

        // Gone from grep, keyword search, and get_segment (now not_found).
        assert!(call("grep", r#"{"pattern":"database","collection":"c"}"#)["result"]["hits"]
            .as_array()
            .unwrap()
            .is_empty(), "grep no longer finds the deleted doc");
        assert!(call("search", r#"{"query":"database connection","collection":"c","mode":"keyword","k":10}"#)["result"]["hits"]
            .as_array()
            .unwrap()
            .is_empty(), "keyword search no longer finds the deleted doc");
        let g = call("get_segment", r#"{"doc_id":"d1","line_start":1,"line_end":1}"#);
        assert_eq!(g["ok"], json!(false));
        assert_eq!(g["error"]["code"], json!(codes::NOT_FOUND));

        // d2 is untouched.
        assert!(!call("grep", r#"{"pattern":"keep","collection":"c"}"#)["result"]["hits"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn delete_document_drops_empty_collection() {
        let _g = e2e_guard();
        call("configure", "{}");
        call(
            "index_segments",
            r#"{"collection":"solo","doc_id":"only","name":"n","segments":[{"text":"lonely doc"}]}"#,
        );
        assert!(store::wait_until_ready("solo", std::time::Duration::from_secs(5)));

        let v = call("delete_document", r#"{"doc_id":"only"}"#);
        assert_eq!(v["result"]["deleted"], json!(true));
        assert_eq!(v["result"]["collection_dropped"], json!(true), "last doc → drop collection");

        // The empty collection is no longer reported by stats.
        let s = call("stats", "");
        assert!(s["result"]["collections"].get("solo").is_none(), "empty collection gone from stats");
    }

    #[test]
    fn delete_unknown_document_is_structural_false() {
        let _g = e2e_guard();
        call("configure", "{}");
        let v = call("delete_document", r#"{"doc_id":"ghost"}"#);
        // A no-op result, not an error envelope.
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["result"]["deleted"], json!(false));
        assert_eq!(v["result"]["removed_segments"], json!(0));
    }

    #[test]
    fn delete_collection_clears_it_and_unknown_is_false() {
        let _g = e2e_guard();
        call("configure", "{}");
        call(
            "index_segments",
            r#"{"collection":"c","doc_id":"d1","name":"a","segments":[{"text":"one"},{"text":"two"}]}"#,
        );
        call(
            "index_segments",
            r#"{"collection":"c","doc_id":"d2","name":"b","segments":[{"text":"three"}]}"#,
        );
        assert!(store::wait_until_ready("c", std::time::Duration::from_secs(5)));

        let v = call("delete_collection", r#"{"collection":"c"}"#);
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["result"]["deleted"], json!(true));
        assert_eq!(v["result"]["removed_docs"], json!(2));
        assert_eq!(v["result"]["removed_segments"], json!(3));

        // The collection is gone: search and grep find nothing in it.
        assert!(call("search", r#"{"query":"one two three","collection":"c","mode":"keyword","k":10}"#)["result"]["hits"]
            .as_array()
            .unwrap()
            .is_empty());
        let s = call("stats", "");
        assert!(s["result"]["collections"].get("c").is_none());

        // Unknown collection → structural false (no-op), not an error.
        let v = call("delete_collection", r#"{"collection":"nope"}"#);
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["result"]["deleted"], json!(false));
        assert_eq!(v["result"]["removed_docs"], json!(0));
    }

    #[test]
    fn delete_document_requires_doc_id() {
        let _g = e2e_guard();
        let v = call("delete_document", r#"{}"#);
        assert_eq!(v["ok"], json!(false));
        assert_eq!(v["error"]["code"], json!(codes::BAD_PAYLOAD));
    }

    // -- Real fastembed integration test (gated) -----------------------------
    //
    // Compiled and run ONLY under `--features fastembed`. It mirrors the probe:
    // configure the real multilingual-e5-small model, index ru/en/uk docs (two
    // about a contract, one unrelated — a cat), wait for the background worker
    // to embed, then dense-search a contract query and assert the contract docs
    // outrank the unrelated one and that the index dim is 384.
    //
    // This downloads the model from HuggingFace on a cold cache (slow) and loads
    // onnxruntime, so it lives behind the feature flag and is not part of the
    // fast default suite.
    #[cfg(feature = "fastembed")]
    #[test]
    fn fastembed_real_model_ranks_contracts_above_cat() {
        let _g = e2e_guard();

        // Select the real model with `device:"auto"` → ort registers the
        // DirectML EP (best-effort). On a machine with no usable GPU/driver ort
        // logs and falls back to CPU automatically, so this MUST NOT crash either
        // way. With the feature on, e5-small loads at dim 384. The mock test
        // suite never passes `model`, so it stays at dim 64.
        let v = call("configure", r#"{"model":"multilingual-e5-small","device":"auto"}"#);
        assert_eq!(v["ok"], json!(true), "configure failed: {v}");
        // The chosen device is echoed back verbatim (the EP selection — and any
        // DirectML→CPU fallback — happens inside ort and is transparent here).
        assert_eq!(v["result"]["device"], json!("auto"));
        assert_eq!(
            v["result"]["dim"].as_u64().unwrap(),
            384,
            "real e5-small must report dim 384 (got {}). If this is 64 the real \
             model failed to load and we fell back to the mock — inspect the \
             onnxruntime/model-download path.",
            v["result"]["dim"]
        );

        // Index two contract docs (ru + en) and one unrelated doc (uk: a cat on
        // a windowsill). These mirror the probe's verified passages.
        call(
            "index_segments",
            r#"{"collection":"c","doc_id":"ru","name":"ru-contract",
                "segments":[{"text":"Договор поставки товара №123 от 5 июня"}]}"#,
        );
        call(
            "index_segments",
            r#"{"collection":"c","doc_id":"en","name":"en-contract",
                "segments":[{"text":"Contract for the supply of goods"}]}"#,
        );
        call(
            "index_segments",
            r#"{"collection":"c","doc_id":"cat","name":"uk-cat",
                "segments":[{"text":"Кіт сидить на вікні"}]}"#,
        );

        // Real embedding is much slower than the mock; give the worker time.
        assert!(
            store::wait_until_ready("c", std::time::Duration::from_secs(120)),
            "collection did not reach Ready (worker may have failed to embed)"
        );

        // Dense search for a contract query (e5 query prefix is applied inside
        // the embedder). Both contract docs must outrank the cat.
        let v = call(
            "search",
            r#"{"query":"договор на поставку товаров","collection":"c","mode":"dense","k":10,"include_text":true}"#,
        );
        assert_eq!(v["ok"], json!(true), "search failed: {v}");
        assert_eq!(v["result"]["partial"], json!(false));
        let hits = v["result"]["hits"].as_array().unwrap();
        assert!(hits.len() >= 3, "expected all three docs scored, got {}", hits.len());

        // Map doc_id -> score and assert both contracts beat the cat.
        let mut score_of = std::collections::HashMap::new();
        for h in hits {
            score_of.insert(
                h["doc_id"].as_str().unwrap().to_string(),
                h["score"].as_f64().unwrap(),
            );
        }
        let ru = score_of["ru"];
        let en = score_of["en"];
        let cat = score_of["cat"];
        // Surfaced with --nocapture so the verified cosine ranking is visible.
        eprintln!("e5-small cosine scores: ru={ru:.4} en={en:.4} cat={cat:.4}");
        assert!(
            ru > cat && en > cat,
            "contract docs must outrank the cat: ru={ru:.4} en={en:.4} cat={cat:.4}"
        );

        // The top hit is one of the two contracts (not the cat).
        let top = hits[0]["doc_id"].as_str().unwrap();
        assert!(top == "ru" || top == "en", "top hit must be a contract, got '{top}'");

        // dim is surfaced as 384 in stats too.
        let s = call("stats", "");
        assert_eq!(s["result"]["dim"].as_u64().unwrap(), 384);
    }

    // -- Offline / air-gapped local-model init (gated; needs a staged model dir) -
    //
    // Empirically closes §11.4: `configure` with a local `model_path` routes to
    // FastEmbedder::new_local (try_new_from_user_defined over on-disk bytes — no
    // hf-hub). Runs ONLY when RCORE_TEST_MODEL_DIR points at a pre-staged model
    // directory (onnx/model.onnx + the four tokenizer/config files); skipped
    // otherwise, so the normal suite and CI stay unaffected. The runner blocks
    // network (a dead HTTP(S) proxy + HF_HUB_OFFLINE=1), so a PASS proves the
    // local path makes no implicit egress on first `configure` or first embed.
    #[cfg(feature = "fastembed")]
    #[test]
    fn fastembed_offline_local_init() {
        let dir = match std::env::var("RCORE_TEST_MODEL_DIR") {
            Ok(d) if !d.trim().is_empty() => d,
            _ => {
                eprintln!(
                    "RCORE_TEST_MODEL_DIR not set — skipping offline local-init test \
                     (set it to a staged model dir to run)"
                );
                return;
            }
        };
        let _g = e2e_guard();

        // Local path → new_local. dim 384 proves the staged e5-small loaded; a 64
        // would mean new_local failed and we silently fell back to the mock.
        let payload = format!(r#"{{"model_path":{},"device":"cpu"}}"#, json!(dir));
        let v = call("configure", &payload);
        assert_eq!(v["ok"], json!(true), "offline configure failed: {v}");
        assert_eq!(
            v["result"]["dim"].as_u64().unwrap(),
            384,
            "offline local e5-small must report dim 384 (got {}); a 64 means new_local \
             failed to load the staged files and we fell back to the mock",
            v["result"]["dim"]
        );

        // Exercise the real embed path offline (where any hidden egress in
        // try_new_from_user_defined / inference would surface as a failure).
        call(
            "index_segments",
            r#"{"collection":"off","doc_id":"ru","name":"ru-contract",
                "segments":[{"text":"Договор поставки товара №123 от 5 июня"}]}"#,
        );
        call(
            "index_segments",
            r#"{"collection":"off","doc_id":"cat","name":"uk-cat",
                "segments":[{"text":"Кіт сидить на вікні"}]}"#,
        );
        assert!(
            store::wait_until_ready("off", std::time::Duration::from_secs(180)),
            "offline collection did not reach Ready — the worker failed to embed \
             (an unexpected network dependency would surface here)"
        );

        let v = call(
            "search",
            r#"{"query":"договор на поставку товаров","collection":"off","mode":"dense","k":10,"include_text":true}"#,
        );
        assert_eq!(v["ok"], json!(true), "offline search failed: {v}");
        let hits = v["result"]["hits"].as_array().unwrap();
        assert!(!hits.is_empty(), "offline search returned no hits: {v}");
        assert_eq!(
            hits[0]["doc_id"],
            json!("ru"),
            "offline dense search must rank the contract above the cat: {v}"
        );
        eprintln!("offline local-init OK: dim=384, top hit=ru, network blocked");
    }
}
