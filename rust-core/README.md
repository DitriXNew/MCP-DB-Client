# rcore — Rust search core

`rcore` provides semantic + keyword + hybrid search, regex (`grep`), and line
slicing (`get_segment`) for the 1C MCP component, over a JSON-in / JSON-out C
ABI.

It compiles to a **cdylib** (`rcore.dll`) that the C++ component loads at
**runtime** (`LoadLibrary` + `GetProcAddress`, see
[`http-1c-dll/src/RustCore.h`](../http-1c-dll/src/RustCore.h)) — it is **not**
linked into the component. This is deliberate: ort/onnxruntime require the
dynamic CRT (`/MD`), but the C++ 1C component can't compile under `/MD`
(`std::basic_stringstream<char16_t>` hits MSVC C2491), so the search core ships
as a separate `/MD` DLL with a self-contained static onnxruntime. One
`libhttp1cWin.dll` then serves both distributions:

- **lite** — `rcore.dll` absent → the `search`/`grep`/`get_segment`/
  `list_collections` tools return a structured `rag_not_installed` result.
- **full** — `rcore.dll` present (next to the component) + `DirectML.dll` → real
  search.

## Crate layout

```
rust-core/
├── Cargo.toml             # cdylib; pinned deps; feature-gated `fastembed`
├── .cargo/config.toml     # +crt-static for the MOCK/staticlib path (see CRT note)
└── src/
    ├── lib.rs                 # C ABI + JSON dispatch + request parsing + e2e tests
    ├── core.rs                # process-global store (Lazy<RwLock<Core>>) + async worker
    ├── embed.rs               # Embedder trait + deterministic MockEmbedder (tests)
    ├── fastembed_embedder.rs  # real FastEmbedder (feature `fastembed`; DirectML/CPU)
    ├── filter.rs              # combinable meta filters (`filter` field: any/all/tags)
    ├── grep.rs                # RE2-style regex scan over stored segment text
    └── protocol.rs            # JSON success/error envelope + error codes
```

## FFI contract

All data crosses as **JSON strings**. Four `extern "C"` exports:

| Function | Signature |
|----------|-----------|
| `rcore_version` | `char* rcore_version(void)` → `{"name","version","abi"}` |
| `rcore_dispatch` | `char* rcore_dispatch(const char* method, const char* payload_json)` → envelope |
| `rcore_free_string` | `void rcore_free_string(char* s)` |
| `rcore_shutdown` | `void rcore_shutdown(void)` |

`rcore_dispatch` is the single generic entry point. Envelope: success
`{"ok":true,"result":...}`, failure `{"ok":false,"error":{"code","message"}}`.
Unknown method → `unknown_method`; bad JSON → `bad_payload`; bad regex →
`bad_pattern`. Panics are caught at the boundary (`catch_unwind`) and surface as
`internal` — they never unwind across the C ABI.

**Methods:** `configure`, `index_segments`, `index_raw`, `search`
(`mode: dense|keyword|hybrid`, single / comma-list / all collections), `grep`,
`get_segment`, `stats`, `list_collections`, `list_models`, `reset`,
`delete_document`, `delete_collection`. Ingest is **async** (returns
immediately; a background worker pool embeds; collections expose a two-axis
`text_ready` / `vector_status` state, polled via `stats` /
`list_collections`).

**Chunking (`index_raw`):** the optional `chunk_cfg` object overrides the
line-granular token-budget chunker — `target_tokens` (default 300),
`max_tokens`, `overlap_lines` (default 2) — and `boundary_regex` makes
chunking **structure-aware**: a line matching the regex always starts a new
chunk, and no overlap is carried across the boundary, so a chunk never glues
the tail of one section to the head of the next. The VA plugin uses this to
chunk `.feature` files per Gherkin scenario. A region bigger than the token
budget still splits into several chunks with normal overlap *inside* it. An
invalid `boundary_regex` fails the whole call with `bad_pattern` before any
state is touched.

**Hit meta:** `search` hits echo the **effective** meta — the document-level
`meta` overlaid by the segment-level `meta`, segment winning on collision. It
is the same view the `filter` clauses match against, so what filtered a hit in
is exactly what the hit reports back (segment-level labels stay visible).

**Collection registry:** each collection carries an optional `description`,
set via the `collection_description` field on `index_segments` **or**
`index_raw` (same semantics on both: last non-empty wins);
`list_collections` returns `{name, description, n_docs, n_segments,
vector_status, text_ready}` so a caller can discover what is searchable and
scope `search` to one or several collections.

## Embedder: mock (tests) vs real (`fastembed` feature)

The `Embedder` trait has two impls:

- **`MockEmbedder`** (dim 64, deterministic token hash) — used by `cargo test`
  and as a fallback. **Test-only**; not a production search backend.
- **`FastEmbedder`** (feature `fastembed`) — fastembed 5.16 + ort 2.0.0-rc.12 +
  the multilingual-e5 family (L2-normalized, `query:`/`passage:` prefixes).
  A separate `query` model instance plus a **bulk pool** of `embed_workers`
  model sessions, because `fastembed::embed` is `&mut self` — a shared mutex
  would let a bulk reindex block queries. Multiple bulk workers embed jobs
  concurrently (CPU is forced when `embed_workers > 1`, since concurrent
  DirectML sessions are unsupported); query embeddings stay prioritized.

`configure` selects the backend: `model` / `model_path` non-empty (with the
feature compiled in) → `FastEmbedder`, else `MockEmbedder`. `embed_workers`
sizes the bulk pool: absent ⇒ **1**; `0` ⇒ auto (`ncpu/2` clamped to 1..=4);
`n` ⇒ exactly n. **Each bulk worker is a separate model session — a full model
copy in RAM** (e5-small fp32 ≈ 0.5–1.5 GB per session under load, plus one
query session), so `auto` on a big server can cost ~5 model copies. The ONNX
inference sub-batch is a flat **32 texts per session**: peak inference memory
scales with `batch × seq_len²` (attention tensors) and onnxruntime's arena
keeps that high-water mark for the session's lifetime, so a small fixed batch
bounds the peak at roughly ~1 GB per session with negligible CPU throughput
loss. There is no hard memory quota in the core; size the pool to your RAM.

**`trim_memory`** drops the bulk sessions to give that RAM back once an ingest
is done (call it when `stats` reports `vector_status: ready`): the query
session stays loaded, and the next ingest lazily re-creates the bulk sessions
from the same model/config. Idempotent; `{"ok":true,"result":{"trimmed":bool}}`
(`false` = no embedder yet). A no-op on the mock backend.

**Model whitelist:** `model` must be one of the built-in names (matched
case-insensitively; empty/absent ⇒ the default):

| name | dim |
|------|-----|
| `multilingual-e5-small` (default) | 384 |
| `multilingual-e5-base` | 768 |
| `multilingual-e5-large` | 1024 |

`list_models` returns the same list —
`{"ok":true,"result":{"default":"multilingual-e5-small","models":[...]}}` — and
works before `configure` and in the lite/mock build, so a client can render a
model dropdown unconditionally. An unknown `model` name makes `configure` fail
with a structural `bad_model` error (message lists the supported names) before
any state is touched; this validation is identical with or without the
`fastembed` feature. A non-empty `model_path` (offline local files) bypasses
the whitelist — local files are loaded by path, not by name. The directory must
contain exactly these files (flat layout; the hf-hub *cache* layout
`models--*/blobs|snapshots` is NOT accepted):

```
<model_path>/onnx/model.onnx        (or <model_path>/model.onnx)
<model_path>/tokenizer.json
<model_path>/config.json
<model_path>/special_tokens_map.json
<model_path>/tokenizer_config.json
```

A quantized ONNX works too — just name it `model.onnx`; no other file names are
probed.

**Model cache:** `configure`'s optional `cache_dir` sets the directory the
built-in model is downloaded into / loaded from (the fastembed/hf-hub cache
root, created on demand). Empty/absent ⇒ fastembed's default cache location.
Only meaningful for the builtin `model` path; `model_path` reads its files
directly and never touches a cache. `cache_dir` is echoed back by `configure`
alongside `model` / `model_path`.
**GPU:** `configure` `device: cpu | dml | auto` (default `auto`) registers the
DirectML execution provider with **automatic CPU fallback** (ort registers it
best-effort; no GPU/driver → CPU). The full package therefore ships
`DirectML.dll` (a hard import of the onnxruntime prebuilt).

## Memory-ownership rule (critical)

> Every `char*` returned to C is allocated by **Rust** (`CString::into_raw`) and
> must be freed **only** by `rcore_free_string`. **Never** call C's
> `free`/`delete` — that is a cross-CRT / cross-DLL free and corrupts the heap.

Input pointers are borrowed (never freed by Rust). The C++ `RustString` RAII
wrapper frees via the **loaded** `rcore_free_string`, keeping ownership inside
`rcore.dll`.

## CRT: mock `/MT` vs fastembed `/MD`

- The **mock** path keeps the static CRT (`+crt-static` in
  [`.cargo/config.toml`](.cargo/config.toml)) to match the C++ component's `/MT`.
- The **fastembed** `rcore.dll` needs the **dynamic** CRT (`/MD`): ort's prebuilt
  onnxruntime is `/MD`, so `+crt-static` fails to link (`__imp_*` unresolved).
  Build it with `RUSTFLAGS="-C target-feature=-crt-static"` (which CMake's
  `RCORE_FASTEMBED=ON` path sets automatically). onnxruntime is statically linked
  into `rcore.dll` — there is **no** `onnxruntime.dll`.

## Build

```sh
# Mock unit tests (fast, no ort):
cargo test

# Real embedder tests (compiles ort; needs the dynamic CRT + network for the model):
RUSTFLAGS="-C target-feature=-crt-static" cargo test --features fastembed

# Production rcore.dll (cdylib) — usually via CMake's full build:
#   cmake .. -DRCORE_FASTEMBED=ON      (drops rcore.dll next to the component)
RUSTFLAGS="-C target-feature=-crt-static" \
  cargo build --release --features fastembed --target x86_64-pc-windows-msvc
# → target/x86_64-pc-windows-msvc/release/rcore.dll
```
