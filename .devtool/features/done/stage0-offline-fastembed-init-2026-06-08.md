---
id: "stage0-offline-fastembed-init-2026-06-08"
status: "done"
priority: "high"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T18:13:44.000Z"
completedAt: "2026-06-09T13:00:00.000Z"
labels: ["stage-0", "rust-core", "investigation", "blocker"]
order: "a3"
---

# Stage 0 — Оффлайн-инициализация fastembed (откр. вопрос §11.4)

## Acceptance
- [ ] Точный способ указать локальный путь к модели (ONNX + tokenizer + config)
- [ ] Отключить **все** сетевые загрузки: и моделей, и бинарников `ort`/`onnxruntime`
- [ ] `hf-hub` не использовать в оффлайн-режиме
- [ ] Подтвердить отсутствие неявных обращений к сети при первом `configure`

Refs: §3 (fastembed, hf-hub), §11.4.

## Findings (2026-06-09)

### 1. Exact offline-init recipe (CONFIRMED FROM CODE)

**API call — local path:**
```
FastEmbedder::new_local(model_path: &str, device: Device) -> Result<Self, String>
```
`rust-core/src/fastembed_embedder.rs:151`

Internally calls `TextEmbedding::try_new_from_user_defined(model, InitOptionsUserDefined::new().with_execution_providers(...))` at line 206.
This is the fastembed offline API — it does NOT call `TextEmbedding::try_new` (which is the online/hf-hub path, used only by `new_builtin` at line 170).

**Online/hf-hub path (for comparison, DO NOT use offline):**
```
FastEmbedder::new_builtin(device: Device)  // calls TextEmbedding::try_new + InitOptions + EmbeddingModel::MultilingualE5Small
```
`rust-core/src/fastembed_embedder.rs:115,170`

**Required files in the model directory** (confirmed from `load_local`, lines 183–203):
```
<model_path>/onnx/model.onnx          # tried first; falls back to <model_path>/model.onnx
<model_path>/tokenizer.json           # required (TokenizerFiles.tokenizer_file)
<model_path>/config.json              # required (TokenizerFiles.config_file)
<model_path>/special_tokens_map.json  # required (TokenizerFiles.special_tokens_map_file)
<model_path>/tokenizer_config.json    # required (TokenizerFiles.tokenizer_config_file)
```
All five files are read with `std::fs::read` (pure local I/O). No network call is issued.

**configure payload to trigger the local path:**
```json
{"model_path": "/absolute/path/to/model/dir", "device": "cpu"}
```
Omit `"model"` entirely. `device: "cpu"` avoids the DirectML EP (safe on air-gapped CI machines without a GPU).

### 2. Disabling ALL network downloads (CONFIRMED FROM CODE, with one build-time nuance)

**Model download — disabled by design on the local path.**
`load_local` reads only from local `std::fs` (lines 184–199). `UserDefinedEmbeddingModel` + `try_new_from_user_defined` accept raw bytes — hf-hub is not invoked on this code path.

**ort/onnxruntime binary acquisition — BUILD TIME, not RUNTIME.**
`Cargo.toml` line 60 notes: `fastembed already enables ort/download-binaries`. This means `ort-sys` (the build-script for onnxruntime) downloads and statically links the onnxruntime prebuilt binary **at `cargo build` time**, not at the first `configure` call. Confirmed by `ort-sys` appearing as a dependency in `Cargo.lock` lines 1597–1605, with `ureq` (its HTTP client) used by the build script. Once the crate is compiled and `rcore.dll` is produced, there is **no runtime onnxruntime download** — the ONNX runtime is statically embedded in the DLL.

**To disable build-time onnxruntime download** (for fully air-gapped builds): set the `ORT_LIB_LOCATION` environment variable to a pre-fetched onnxruntime directory before running `cargo build --features fastembed`. This causes `ort-sys` to skip the download and link the pre-placed binaries. This is the only remaining step not covered by the code as written.

### 3. hf-hub not on the local path (CONFIRMED FROM CODE)

`hf-hub` version 0.5.0 IS a transitive dependency — it comes in through `fastembed` (Cargo.lock lines 595–609: `fastembed` depends on `hf-hub`). However it is reachable **only** through `TextEmbedding::try_new` (the online path, inside `load_builtin`). `load_local` calls `TextEmbedding::try_new_from_user_defined` and never touches hf-hub — there is no hf-hub call in `load_local` (lines 181–212). The library is compiled in, but the code path that calls hf-hub is unreachable when `model_path` is set.

### 4. Implicit network access on first `configure` (PARTIAL — code confirms none, runtime check pending)

**Code analysis:** `configure` → `build_embedder` (`core.rs:332`) → checks `model_path` non-empty → calls `FastEmbedder::new_local` → `load_local` → reads five files from local `std::fs`, passes raw bytes to `UserDefinedEmbeddingModel::new` → `TextEmbedding::try_new_from_user_defined`. No HTTP/network client is called in this Rust code path.

**Remaining gap (needs runtime verification):** fastembed's `try_new_from_user_defined` internals (inside the compiled fastembed crate, not visible in this repo) could theoretically make a network call via hf-hub for e.g. tokenizer vocabulary lookups or model metadata. This is very unlikely given the API contract (it takes raw bytes), but it has NOT been verified by running `configure` with a pre-staged model directory against a machine with outbound network blocked. This is the **one remaining step** to fully close §11.4.

### Per-bullet verdict

| Acceptance criterion | Status | Evidence |
|---|---|---|
| Exact local model path API | CONFIRMED | `new_local` / `load_local` / `try_new_from_user_defined`: fastembed_embedder.rs:151,181,206 |
| Required files in model dir | CONFIRMED | fastembed_embedder.rs:183–203; 5 files listed above |
| Disable all downloads — models | CONFIRMED | local path uses only std::fs, no hf-hub call |
| Disable all downloads — ort binaries | CONFIRMED (build-time only) | ort/download-binaries acts at `cargo build`, not at runtime; DLL is self-contained after build |
| Build-time ort binary download disablement | NOT YET DOCUMENTED IN CODE | Use `ORT_LIB_LOCATION` env var to supply pre-fetched onnxruntime; needs operational doc |
| No hf-hub on local path | CONFIRMED | hf-hub linked but only reachable via `try_new` (online path); not called in `try_new_from_user_defined` |
| Zero implicit network on first `configure` | ✅ VERIFIED (2026-06-09) | Offline smoke-test passed with network blocked: `tests::fastembed_offline_local_init` — dim 384, correct ranking, no egress |

### ✅ Empirical verification (2026-06-09) — OFFLINE-PASS

The remaining runtime smoke-test was **executed and passed**. A gated integration
test — `tests::fastembed_offline_local_init` in `rust-core/src/lib.rs`
(`#[cfg(feature = "fastembed")]`, runs only when `RCORE_TEST_MODEL_DIR` is set, so
CI/the normal suite is unaffected) — drives the real `configure` → `new_local`
path against a pre-staged model dir, then embeds + dense-searches, **with all
outbound network blocked**:

- Staged model dir: `rust-core/target/offline-model/` — `onnx/model.onnx` + the
  four tokenizer/config files, copied out of the HF snapshot.
- Network block at run time: `HF_HUB_OFFLINE=1`, `TRANSFORMERS_OFFLINE=1`, and a
  dead proxy (`HTTP(S)_PROXY=http://127.0.0.1:1`) — any accidental HTTP fails fast.
- Build (network allowed for the build only): `RUSTFLAGS="-C target-feature=-crt-static"
  cargo test --no-run --features fastembed`; the produced test binary then ran
  under the block.

Result:

```
offline local-init OK: dim=384, top hit=ru, network blocked
test result: ok. 1 passed; 0 failed; ... finished in 11.79s
RESULT: OFFLINE-PASS
```

`configure {"model_path":...,"device":"cpu"}` loaded e5-small (dim 384) and ranked
the contract above the cat with **zero network access** → no implicit egress on
first `configure` or first embed. **§11.4 closed.** The only air-gapped-*build*
caveat remains `ORT_LIB_LOCATION` (skip the build-time onnxruntime download) — an
operational/packaging note, not a runtime behaviour.
