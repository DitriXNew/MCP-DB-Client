//! Embedding seam.
//!
//! Stage 1 builds the entire store / ingest / search pipeline against an
//! [`Embedder`] *trait* rather than a concrete model. This is deliberate: the
//! real model integration (ONNX + tokenizer via fastembed/ort) is a separate
//! later card and drags in heavy native dependencies. By coding to the trait we
//! keep this whole slice pure-Rust and fully unit-testable now, and the real
//! impl slots in later by just implementing [`Embedder`].
//!
//! Contract for every implementation:
//!   * vectors are **L2-normalized**, so cosine similarity reduces to a plain
//!     dot product (this is what `search` relies on);
//!   * `embed_query` and `embed_passages` use the *same* scheme, so a query and
//!     a passage that share tokens land close together in the vector space.

/// A text → vector embedder. The store/search pipeline only ever sees this
/// trait, never a concrete model.
///
/// `Send + Sync` is required because the embedder lives inside the process
/// singleton and is called from the background ingest worker thread.
pub trait Embedder: Send + Sync {
    /// Dimensionality of the produced vectors. Fixed for the life of the
    /// embedder and bound to the index (changing it invalidates all vectors).
    fn dim(&self) -> usize;

    /// Embed a batch of passages (the documents being indexed). Returns one
    /// L2-normalized vector per input, in the same order.
    fn embed_passages(&self, texts: &[String]) -> Vec<Vec<f32>>;

    /// Embed a batch of passages on behalf of a *pinned* worker thread.
    ///
    /// `slot` is the stable worker-thread index (0-based, assigned at spawn and
    /// fixed for the thread's lifetime). A multi-session implementation maps the
    /// slot onto one of its model sessions so each worker thread always uses the
    /// *same* session — eliminating the head-of-line blocking a shared/rotating
    /// cursor can cause (a worker being assigned a session currently locked by a
    /// slow sibling while other sessions sit idle). The default simply forwards
    /// to [`Embedder::embed_passages`] (correct for any single-session embedder,
    /// including the mock).
    fn embed_passages_at(&self, _slot: usize, texts: &[String]) -> Vec<Vec<f32>> {
        self.embed_passages(texts)
    }

    /// Embed a single query string. Same scheme as [`Embedder::embed_passages`]
    /// so cosine similarity between a query and a passage is meaningful.
    fn embed_query(&self, text: &str) -> Vec<f32>;

    /// How many bulk embeds this embedder can run concurrently — i.e. how many
    /// independent model sessions it holds. The ingest worker spawns this many
    /// threads. Defaults to 1 (the mock and any single-session embedder).
    fn bulk_concurrency(&self) -> usize {
        1
    }

    /// The most recent bulk/query embed failure, for diagnostics via `stats`.
    ///
    /// Inside the 1C host process there is no console, so an `eprintln!` on the
    /// embed path goes nowhere; implementations that can fail (the real ONNX
    /// embedder) record the last error string here instead so the operator can
    /// see it in the `stats` payload. `None` means no failure has been observed
    /// (the default, and always the answer for the infallible mock).
    fn last_error(&self) -> Option<String> {
        None
    }

    /// Release the heavyweight BULK (indexing) sessions to give RAM back after
    /// an ingest completes — each real bulk session is a full model copy. The
    /// query session stays loaded (search latency must not pay a model reload);
    /// the next bulk embed lazily re-creates what was dropped. Default no-op
    /// (the mock holds no sessions).
    fn trim_bulk(&self) {}
}

/// L2-normalize a vector in place. A zero (or non-finite) vector is left as an
/// all-zeros vector — callers treat such vectors as "no signal" and skip them.
fn l2_normalize(v: &mut [f32]) {
    let norm_sq: f32 = v.iter().map(|x| x * x).sum();
    let norm = norm_sq.sqrt();
    if norm > 0.0 && norm.is_finite() {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Deterministic, dependency-free embedder used for the whole Stage 1 slice and
/// its tests.
///
/// Scheme: lowercase the text, split on ASCII whitespace, hash each token into a
/// bucket in `[0, dim)`, accumulate a count per bucket, then L2-normalize. This
/// is a bag-of-hashed-tokens model: two texts that share tokens map onto
/// overlapping buckets and therefore have a higher dot product (== cosine, since
/// normalized). That property is exactly what makes search ranking testable.
pub struct MockEmbedder {
    dim: usize,
}

impl MockEmbedder {
    /// Default dimensionality for the mock. Small on purpose — keeps test
    /// vectors cheap while still leaving enough buckets to avoid pathological
    /// collisions for short token sets.
    pub const DEFAULT_DIM: usize = 64;

    /// Create a mock embedder with the default dimensionality.
    pub fn new() -> Self {
        Self::with_dim(Self::DEFAULT_DIM)
    }

    /// Create a mock embedder with an explicit dimensionality (used by tests
    /// that exercise reconfigure-with-different-dim).
    pub fn with_dim(dim: usize) -> Self {
        // A zero dim would make every vector empty and every score zero; clamp
        // to at least 1 so the type stays well-behaved.
        Self { dim: dim.max(1) }
    }

    /// Hash a single token to a bucket in `[0, dim)` using a small, stable
    /// FNV-1a hash. Stable across runs/platforms because it is pure arithmetic
    /// over the token bytes (no `DefaultHasher`, whose seed can vary).
    fn bucket(&self, token: &str) -> usize {
        let mut hash: u64 = 0xcbf29ce484222325; // FNV offset basis
        for b in token.bytes() {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b3); // FNV prime
        }
        (hash % self.dim as u64) as usize
    }

    /// Core of the scheme: text → raw bucket counts → L2-normalized vector.
    fn embed_one(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0.0f32; self.dim];
        for token in text.to_lowercase().split_whitespace() {
            let idx = self.bucket(token);
            v[idx] += 1.0;
        }
        l2_normalize(&mut v);
        v
    }
}

impl Default for MockEmbedder {
    fn default() -> Self {
        Self::new()
    }
}

impl Embedder for MockEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed_passages(&self, texts: &[String]) -> Vec<Vec<f32>> {
        texts.iter().map(|t| self.embed_one(t)).collect()
    }

    fn embed_query(&self, text: &str) -> Vec<f32> {
        self.embed_one(text)
    }
}

/// Dot product of two equal-length vectors. Because vectors are L2-normalized
/// this equals cosine similarity. Mismatched lengths (which should never happen
/// once `dim` is fixed) yield `0.0` rather than panicking.
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    // Manual 8-lane chunking gives LLVM a trivially auto-vectorizable shape: 8
    // independent f32 accumulators with no cross-lane dependency, so it can lower
    // the inner loop to packed SIMD multiply-adds. The previous `.zip().map().sum()`
    // form is a single serial f32 accumulator — one long dependency chain LLVM
    // will not vectorize (reassociating f32 adds changes results, which the
    // optimizer is not allowed to do on its own). Vectors are L2-normalized, so
    // this still equals cosine; only the summation grouping changes (negligible,
    // and ranking is robust to it).
    const LANES: usize = 8;
    let mut acc = [0.0f32; LANES];
    let mut ca = a.chunks_exact(LANES);
    let mut cb = b.chunks_exact(LANES);
    for (xa, xb) in ca.by_ref().zip(cb.by_ref()) {
        for l in 0..LANES {
            acc[l] += xa[l] * xb[l];
        }
    }
    let mut sum: f32 = acc.iter().sum();
    // Tail (len % 8) — at most 7 elements.
    for (x, y) in ca.remainder().iter().zip(cb.remainder().iter()) {
        sum += x * y;
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    /// L2 norm of a vector, for asserting normalization.
    fn norm(v: &[f32]) -> f32 {
        v.iter().map(|x| x * x).sum::<f32>().sqrt()
    }

    #[test]
    fn embeddings_are_deterministic() {
        let e = MockEmbedder::new();
        let a = e.embed_query("Hello World foo");
        let b = e.embed_query("Hello World foo");
        assert_eq!(a, b, "same input must produce identical vectors");
    }

    #[test]
    fn vectors_are_l2_normalized() {
        let e = MockEmbedder::new();
        let v = e.embed_query("alpha beta gamma");
        assert!((norm(&v) - 1.0).abs() < 1e-5, "expected unit norm, got {}", norm(&v));
    }

    #[test]
    fn query_and_passage_use_same_scheme() {
        let e = MockEmbedder::new();
        let q = e.embed_query("shared token text");
        let p = e.embed_passages(&["shared token text".to_string()]);
        assert_eq!(q, p[0], "query and passage of same text must match");
    }

    #[test]
    fn shared_tokens_score_higher() {
        let e = MockEmbedder::new();
        let query = e.embed_query("database connection pool");
        // This passage shares 2/3 tokens with the query.
        let close = e.embed_passages(&["database connection settings".to_string()]);
        // This one shares nothing.
        let far = e.embed_passages(&["banana orange apple".to_string()]);

        let s_close = dot(&query, &close[0]);
        let s_far = dot(&query, &far[0]);
        assert!(
            s_close > s_far,
            "shared-token passage ({s_close}) must outscore unrelated one ({s_far})"
        );
        assert!(s_close > 0.0, "shared tokens must give positive similarity");
    }

    #[test]
    fn case_insensitive() {
        let e = MockEmbedder::new();
        assert_eq!(e.embed_query("Hello"), e.embed_query("hello"));
    }

    #[test]
    fn empty_text_is_zero_vector() {
        let e = MockEmbedder::new();
        let v = e.embed_query("   ");
        assert!(v.iter().all(|&x| x == 0.0), "blank text must give zero vector");
        assert_eq!(norm(&v), 0.0);
    }

    #[test]
    fn dim_is_respected() {
        let e = MockEmbedder::with_dim(32);
        assert_eq!(e.dim(), 32);
        assert_eq!(e.embed_query("x").len(), 32);
    }
}
