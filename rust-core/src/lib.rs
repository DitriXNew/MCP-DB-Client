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
mod protocol;

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::panic::{self, AssertUnwindSafe};

use serde_json::{json, Value};

use crate::core::{
    self as store, Config, IndexRequest, SearchRequest, SegmentInput, CORE,
};
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

    // Count every dispatched call so `stats` observes real shared state.
    if let Ok(mut core) = CORE.write() {
        core.calls_handled = core.calls_handled.saturating_add(1);
    }

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
                "callsHandled": core.calls_handled,
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
fn parse_config(payload: &Value) -> Result<Config, String> {
    Ok(Config {
        model_path: opt_str(payload, "model_path"),
        normalize: bool_or(payload, "normalize", true),
        max_seq_len: opt_u64(payload, "max_seq_len"),
        device: opt_str(payload, "device").unwrap_or_else(|| "cpu".to_string()),
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

/// Parse a `search` payload. `query` required; `k` defaults to 10.
fn parse_search_request(payload: &Value) -> Result<SearchRequest, String> {
    let query = opt_str(payload, "query")
        .ok_or_else(|| "missing 'query'".to_string())?;
    let k = opt_u64(payload, "k").map(|v| v as usize).unwrap_or(10).max(1);
    let min_score = payload
        .get("min_score")
        .and_then(|x| x.as_f64())
        .map(|x| x as f32);
    let max_per_doc = opt_u64(payload, "max_per_doc").map(|v| v as usize);
    Ok(SearchRequest {
        query,
        collection: opt_str(payload, "collection"),
        k,
        min_score,
        max_per_doc,
        include_text: bool_or(payload, "include_text", true),
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
}
