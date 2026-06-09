//! `grep` — regex search over the *text* of stored segments.
//!
//! Unlike dense `search`, grep needs **no vectors**: it scans the segment text
//! that `accept_index` installs synchronously (Task 1), so it works the instant
//! a doc is accepted — even while the collection's `vector_status` is still
//! `Building`. It is a purely textual, line-oriented operation.
//!
//! ## Engine
//! Matching uses the [`regex`] crate (RE2-style finite automata): linear-time,
//! no backreferences or lookaround, so a user-supplied pattern can never trigger
//! catastrophic backtracking (ReDoS). `RegexBuilder` carries the `ignore_case`
//! and `multiline` flags. A pattern that fails to compile is reported as a
//! *structural* error ([`GrepError::BadPattern`]) — never a panic.
//!
//! ## Line semantics
//! Each segment's text is normalized **CRLF → LF** before scanning. Line numbers
//! are **1-based and inclusive**, counted by `\n` after normalization, relative
//! to each segment's text (segment-local, not document-global). `context_lines`
//! attaches up to N preceding / following lines from the *same* segment.
//!
//! ## Limits
//! `max_matches` caps the total number of returned hits; hitting it sets the
//! top-level `truncated` flag (and `total_found` may then be a lower bound — it
//! counts what we produced up to the cap, see below). `max_per_doc` caps hits
//! per document. Scanning stops early once the global cap is reached.

use std::borrow::Cow;

use regex::RegexBuilder;
use serde_json::Value;

use crate::core::Core;
use crate::filter::MetaFilter;

/// A parsed `grep` request.
pub struct GrepRequest {
    /// The regular expression to search for (RE2 syntax).
    pub pattern: String,
    /// Optional collection scope; `None` ⇒ scan all collections.
    pub collection: Option<String>,
    /// Combinable meta filters over each hit's effective meta (doc ∪ segment).
    pub filter: MetaFilter,
    /// Case-insensitive matching (`RegexBuilder::case_insensitive`).
    pub ignore_case: bool,
    /// Multi-line mode: `^`/`$` match at line boundaries
    /// (`RegexBuilder::multi_line`). Defaults to `true` so anchors behave the way
    /// a line-oriented grep user expects.
    pub multiline: bool,
    /// Lines of context to include before/after each matching line (per side).
    pub context_lines: usize,
    /// Maximum number of hits to return overall. Reaching it sets `truncated`.
    pub max_matches: usize,
    /// Optional cap on hits per document.
    pub max_per_doc: Option<usize>,
}

/// One grep hit: a single matching line plus optional context, located by
/// document + segment and a segment-local 1-based line number.
pub struct GrepHit {
    pub doc_id: String,
    pub name: String,
    pub collection: String,
    pub segment_id: u64,
    /// 1-based, inclusive line number within the segment's (LF-normalized) text.
    pub line_number: usize,
    pub line_text: String,
    /// Up to `context_lines` lines immediately before the match (in order).
    pub context_before: Vec<String>,
    /// Up to `context_lines` lines immediately after the match (in order).
    pub context_after: Vec<String>,
}

/// Result of a grep: hits plus paging/limit metadata.
pub struct GrepResult {
    pub hits: Vec<GrepHit>,
    /// True when `max_matches` was reached and scanning stopped early.
    pub truncated: bool,
    /// Number of matching lines produced (equals `hits.len()`; when `truncated`
    /// it is the capped count, i.e. a lower bound on the true total).
    pub total_found: usize,
}

/// Why a grep could not run. The only failure mode is an uncompilable pattern,
/// surfaced to the dispatcher as a structural `bad_pattern` error.
#[derive(Debug)]
pub enum GrepError {
    /// The supplied pattern failed to compile; carries the engine's message.
    BadPattern(String),
}

/// Run a grep over the store. Compiles the pattern first (a compile failure is a
/// structural [`GrepError::BadPattern`], not a panic), then scans the text of
/// every in-scope segment line by line.
///
/// Borrows `&Core` read-only — the dispatcher holds the read lock. Scanning is
/// linear in total text size and stops as soon as `max_matches` is reached.
pub fn grep(core: &Core, req: &GrepRequest) -> Result<GrepResult, GrepError> {
    // Compile the pattern up front. RE2 → no catastrophic backtracking.
    let re = RegexBuilder::new(&req.pattern)
        .case_insensitive(req.ignore_case)
        .multi_line(req.multiline)
        .build()
        .map_err(|e| GrepError::BadPattern(e.to_string()))?;

    let mut hits: Vec<GrepHit> = Vec::new();
    let mut truncated = false;

    // Deterministic-ish ordering: iterate collections, then docs, then segments.
    // (HashMap order is unspecified, but each doc's segments keep ingest order.)
    'outer: for (cname, coll) in core.collections.iter() {
        if let Some(want) = req.collection.as_ref() {
            if want != cname {
                continue;
            }
        }

        for doc in coll.docs.values() {
            let mut per_doc = 0usize;

            for seg in &doc.segments {
                // grep is a text operation: it needs no vector, so it ignores
                // `seg.vector` entirely and works while vectors are Building.

                // Meta filter over the hit's effective meta (segment overrides
                // doc). Cheap no-op when no filter was supplied.
                if !req.filter.matches_doc_seg(&doc.meta, &seg.meta) {
                    continue;
                }

                // Normalize CRLF → LF, then split into lines for 1-based,
                // \n-counted line numbers. `split('\n')` yields exactly
                // (count of '\n') + 1 pieces, which is the line model we want.
                let normalized = normalize_newlines(&seg.text);
                let lines: Vec<&str> = normalized.split('\n').collect();

                for (idx, line) in lines.iter().enumerate() {
                    if !re.is_match(line) {
                        continue;
                    }

                    // Build context windows from the same segment.
                    let before_start = idx.saturating_sub(req.context_lines);
                    let context_before: Vec<String> = lines[before_start..idx]
                        .iter()
                        .map(|s| s.to_string())
                        .collect();
                    let after_end = (idx + 1 + req.context_lines).min(lines.len());
                    let context_after: Vec<String> = lines[idx + 1..after_end]
                        .iter()
                        .map(|s| s.to_string())
                        .collect();

                    hits.push(GrepHit {
                        doc_id: doc.doc_id.clone(),
                        name: doc.name.clone(),
                        collection: cname.clone(),
                        segment_id: seg.segment_id,
                        line_number: idx + 1, // 1-based, inclusive
                        line_text: line.to_string(),
                        context_before,
                        context_after,
                    });

                    per_doc += 1;

                    // Global cap → stop everything and flag truncation.
                    if hits.len() >= req.max_matches {
                        truncated = true;
                        break 'outer;
                    }
                    // Per-doc cap → move on to the next document.
                    if let Some(max) = req.max_per_doc {
                        if per_doc >= max {
                            break;
                        }
                    }
                }

                // If the per-doc cap was hit mid-segment, skip this doc's
                // remaining segments too.
                if let Some(max) = req.max_per_doc {
                    if per_doc >= max {
                        break;
                    }
                }
            }
        }
    }

    let total_found = hits.len();
    Ok(GrepResult {
        hits,
        truncated,
        total_found,
    })
}

/// Normalize line endings to `\n`: turn CRLF and bare CR into LF so line numbers
/// (counted by `\n`) are stable regardless of the source's newline convention.
///
/// Fast path: a single pass first checks for any `\r`. The common case — text
/// that is already LF-only (every `index_raw` segment, plus any LF source) — has
/// no CR, so we **borrow** the input with zero allocation instead of running two
/// `replace` passes that would each allocate a fresh `String`. Only CR-bearing
/// text takes the owned, two-replace path.
fn normalize_newlines(s: &str) -> Cow<'_, str> {
    if !s.as_bytes().contains(&b'\r') {
        return Cow::Borrowed(s);
    }
    // Replace CRLF first, then any remaining lone CR, so "\r\n" doesn't become
    // two line breaks.
    Cow::Owned(s.replace("\r\n", "\n").replace('\r', "\n"))
}

/// Serialize one grep hit to JSON. `context_before` / `context_after` are only
/// included when non-empty, keeping the common (no-context) payload compact.
pub fn hit_to_json(h: &GrepHit) -> Value {
    let mut obj = serde_json::json!({
        "doc_id": h.doc_id,
        "name": h.name,
        "collection": h.collection,
        "segment_id": h.segment_id,
        "line_number": h.line_number,
        "line_text": h.line_text,
    });
    if !h.context_before.is_empty() {
        obj["context_before"] = serde_json::json!(h.context_before);
    }
    if !h.context_after.is_empty() {
        obj["context_after"] = serde_json::json!(h.context_after);
    }
    obj
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{accept_index, IndexRequest, SegmentInput, VectorStatus, CORE, TEST_LOCK};
    use serde_json::json;

    #[test]
    fn normalize_handles_crlf_and_cr() {
        // CR-bearing input → owned, fully normalized.
        assert_eq!(normalize_newlines("a\r\nb\rc\nd").as_ref(), "a\nb\nc\nd");
        assert!(matches!(normalize_newlines("a\r\nb"), Cow::Owned(_)));
        // LF-only input → borrowed with zero allocation.
        assert!(matches!(normalize_newlines("a\nb\nc"), Cow::Borrowed(_)));
    }

    /// Build a default `GrepRequest` for `pattern`, scoped to `collection`.
    fn req(pattern: &str, collection: Option<&str>) -> GrepRequest {
        GrepRequest {
            pattern: pattern.to_string(),
            collection: collection.map(String::from),
            filter: MetaFilter::default(),
            ignore_case: false,
            multiline: true,
            context_lines: 0,
            max_matches: 200,
            max_per_doc: None,
        }
    }

    #[test]
    fn grep_finds_text_while_collection_is_building() {
        // Ties Task 1 + Task 3 together, deterministically: hold the WRITE lock so
        // the background worker cannot run, install text via `accept_index`, then
        // grep the text while `vector_status` is still `Building` and vectors are
        // still `None`. No `wait_until_ready`, no race.
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // The mutating section runs under the write lock; we drop it before grep
        // (grep takes a read lock) but assert Building/None *while still holding
        // it*, which the blocked worker cannot have changed.
        {
            let mut c = CORE.write().unwrap();
            c.reset();
            accept_index(
                &mut c,
                IndexRequest {
                    collection: "docs".to_string(),
                    doc_id: "b1".to_string(),
                    name: "n".to_string(),
                    meta: json!({}),
                    segments: vec![SegmentInput {
                        text: "needle in the haystack".to_string(),
                        embed_text: None,
                        line_start: None,
                        line_end: None,
                        meta: json!({}),
                    }],
                    description: None,
                },
            );
            let coll = c.collections.get("docs").unwrap();
            assert_eq!(coll.vector_status, VectorStatus::Building);
            let doc = coll.docs.get("b1").unwrap();
            assert!(doc.segments.iter().all(|s| s.vector.is_none()));
        }
        // Now grep under a read lock. Because the prior write section never let
        // the worker apply (it was blocked on the write lock), the collection may
        // *or may not* have flipped Ready by now — either way grep over text works
        // and must find the needle.
        let c = CORE.read().unwrap();
        let res = grep(&c, &req("needle", Some("docs"))).unwrap();
        assert_eq!(res.hits.len(), 1, "grep must find text regardless of vectors");
        assert_eq!(res.hits[0].doc_id, "b1");
        assert_eq!(res.hits[0].line_number, 1);
    }
}
