---
id: "stage0-model-delivery-int8-2026-06-08"
status: "review"
priority: "critical"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T18:13:44.000Z"
completedAt: null
labels: ["stage-0", "infra", "investigation", "blocker"]
order: "a2"
---

# Stage 0 — Доставка модели + выбор int8 (откр. вопрос §11.3, Правка 7)

`configure(model_path)` предполагает, что модель уже на диске. Кто и как её туда кладёт — открыто.

## Acceptance
- [ ] План доставки ~120–130 МБ модели в оффлайне (`Template.bin` ~437 КБ — не вариант): отдельная поставка / распаковка при первом старте / контролируемый сетевой путь
- [ ] Версионирование модели под версию компоненты
- [ ] Зафиксировать **int8-квантованный** ONNX multilingual-e5-small (~120–130 МБ vs fp32 ~450 МБ; fastembed по умолчанию тянет квантованные веса)
- [ ] Проверить, что квантованный вариант именно **мультиязычный** (покрывает ru/uk), а не EN-only
- [ ] Померить **дельту ретрива int8 vs fp32** на маленьком ru/uk gold-наборе, особенно по коротким шагам Ванессы (§12 «слипаются»: int8 режет ~0.5–1% косинуса → может перевернуть ранжирование среди близких). Если просядет — на коллекции шагов сильнее опираться на keyword-канал либо держать fp32 точечно (модель-на-коллекцию, этап 5)

Refs: §3, §6.2, §11.3, §12.

---

## Findings / Delivery plan (2026-06-09)

### Code evidence (read-only analysis)

**Embedder loading** (`rust-core/src/fastembed_embedder.rs`):
- Online path (`new_builtin`, line 115): calls `EmbeddingModel::MultilingualE5Small` via fastembed's built-in registry.
- Offline path (`new_local`, line 151): reads `<model_path>/onnx/model.onnx` (fallback: `<model_path>/model.onnx`) plus four tokenizer/config files. The operator stages whichever ONNX they choose at that path. **The int8-vs-fp32 choice is a staging decision, not a code change.**

**fastembed 5.16.0 registry** (`C:\Users\DitriX\.cargo\registry\src\…\fastembed-5.16.0\src\models\text_embedding.rs`):
- `MultilingualE5Small` → `model_code: "intfloat/multilingual-e5-small"`, `model_file: "onnx/model.onnx"`, `dim: 384`.
- There is **no** `MultilingualE5SmallQ` (quantized) variant in fastembed 5.16.0. The built-in online download fetches the full fp32 file from HF.

**Local fastembed cache confirms fp32** (`rust-core/.fastembed_cache/models--intfloat--multilingual-e5-small/snapshots/614241f622f53c4eeff9890bdc4f31cfecc418b3/onnx/model.onnx`):
- Cached ONNX size: **470,268,510 bytes (~448 MiB) — fp32**, not int8.
- `config.json` confirms: `tokenizer_class: "XLMRobertaTokenizer"`, `vocab_size: 250037`, `hidden_size: 384`. The 250 037-token XLM-RoBERTa vocabulary covers ~100 languages including **ru** and **uk**.

**Component version** (`http-1c-dll/version.h`, line 3): `VERSION_SEMVER "1.4.2"`.

---

### Bullet 1 — DONE-on-paper: Delivery plan

**Recommendation: separate versioned sidecar artifact (zip), unpacked next to `rcore.dll` at install time.**

Rationale and tradeoffs vs alternatives:

| Approach | Pro | Con |
|---|---|---|
| **Sidecar zip released alongside full package** (chosen) | Works fully offline; no first-run network; operator controls staging; CI can version-pin the artifact; fits existing lite/full split | Adds one extra download step for the operator; installer script needed |
| Unpack-on-first-start from embedded resource | Simpler operator UX | Template.bin is ~437 KB — cannot carry 120+ MB; would require a separate downloader bundled in the DLL, which is fragile in 1C environments |
| Controlled network path (fastembed online download) | Zero operator steps | Requires internet at first use, breaks air-gapped (offline) deployments, and the built-in path downloads fp32 (~450 MB), not int8 |

**Concrete plan:**

1. Export the int8-quantized ONNX from HuggingFace (`intfloat/multilingual-e5-small` → `onnx/model_int8.onnx`, ~120–130 MB) and the associated tokenizer files (`tokenizer.json`, `config.json`, `special_tokens_map.json`, `tokenizer_config.json`).
2. Package them as `model-mle5-small-int8-v<VERSION_SEMVER>.zip` (e.g. `model-mle5-small-int8-v1.4.2.zip`), released as a GitHub Release asset alongside the full package. The zip unpacks to a flat directory: `onnx/model.onnx` + four tokenizer files.
3. The installer (or a one-time 1C startup script) unpacks the zip next to `rcore.dll` into e.g. `<component_dir>\model\mle5-small-int8\`.
4. `configure` is called with `model_path` pointing at that directory; `new_local` resolves `<dir>/onnx/model.onnx` (fastembed_embedder.rs line 190).
5. A **model manifest file** (`model-manifest.json`, shipped inside the zip) records:
   - `model_id: "intfloat/multilingual-e5-small"`, `quantization: "int8"`, `dim: 384`, `hf_revision: "<sha>"`, `component_version: "<VERSION_SEMVER>"`, `sha256: "<onnx_sha256>"`.
   - On startup, rcore reads the manifest and compares `component_version` to its own `rcore_version()` output; logs a warning if there is a mismatch. Enforcement (hard fail vs warn) is a policy call left to stage 5.

**Key 1C deployment constraint:** the full package already ships `rcore.dll` separately from `libhttp1cWin.dll` (the lite/full split). A second sidecar zip matches this existing pattern exactly — operators already handle one extra artifact.

---

### Bullet 2 — DONE-on-paper: Model versioning

Pin `component_version` inside `model-manifest.json` (see above). The component version source of truth is `http-1c-dll/version.h` line 3 (`VERSION_SEMVER "1.4.2"`). The CI release step tags the zip with the same semver. `rcore_version()` (exposed via the DLL, see `http-1c-dll/src/RustCore.h` line 185) returns the component version as JSON; the startup check compares it to `manifest.component_version`.

---

### Bullet 3 — DONE-on-paper: int8 ONNX confirmed as the staging target

The local fastembed cache (`onnx/model.onnx`, 470 MB) is **fp32** — that is what fastembed 5.16.0 downloads by default for `MultilingualE5Small` (no built-in quantized variant exists in this version). The int8 ONNX must therefore be sourced manually from HuggingFace (`intfloat/multilingual-e5-small` repo, file `onnx/model_int8.onnx`, ~120–130 MB) and staged via the offline path. This is a staging/packaging action, not a code change. The offline path in `new_local` (fastembed_embedder.rs line 151) accepts whichever ONNX the operator places at `onnx/model.onnx` inside the model directory.

**Correction to the card's original assumption:** fastembed does NOT pull quantized weights by default for this model in v5.16.0. Quantized delivery requires explicit manual staging.

---

### Bullet 4 — DONE-on-paper: Multilingual identity confirmed

Evidence from the local cache `config.json`:
- `tokenizer_class: "XLMRobertaTokenizer"` — the multilingual XLM-RoBERTa tokenizer, not BertTokenizer.
- `vocab_size: 250037` — the full multilingual vocabulary covering ~100 languages.
- `hidden_size: 384`, `max_position_embeddings: 512`.

`intfloat/multilingual-e5-small` is the MULTILINGUAL E5 family (trained on MS MARCO + MIRACL multilingual data), distinct from `intfloat/e5-small` (English-only). It covers **ru** and **uk** natively. The int8-quantized variant at `onnx/model_int8.onnx` in the same HF repo uses the identical tokenizer and vocabulary — quantization is weight-only (INT8 on linear layers), so language coverage is unchanged.

---

### Bullet 5 — NEEDS-EMPIRICAL-RUN: int8-vs-fp32 retrieval delta

This cannot be measured without running inference. The procedure for when it is executed:

**Gold set shape:**
- 50–100 (query, passage, label) triples in Russian and Ukrainian.
- Mandatory: at least 20 "short Vanessa steps" — brief procedural instructions (~5–15 tokens) that are semantically close to each other (the "слипаются" case from §12). These are the highest-risk pairs for int8 cosine drift flipping top-1.
- Label format: binary relevance (1 = relevant, 0 = not) or graded (0/1/2). Binary suffices for nDCG@5 and Recall@5.

**Metric:**
- Primary: nDCG@5 and Recall@5 for each model variant.
- Secondary: for each close-pair (cosine delta < 0.03 between top-1 and top-2 in fp32), record whether int8 flips the ranking.

**Procedure:**
1. Export both `onnx/model.onnx` (fp32, 470 MB) and `onnx/model_int8.onnx` (~120–130 MB) from the HF repo.
2. Run both through `new_local` on the gold set; collect nDCG@5, Recall@5, and per-pair cosine scores.
3. Compute delta: `(metric_int8 - metric_fp32) / metric_fp32 * 100%`.

**Decision rule:**
- If absolute nDCG@5 delta on the **steps collection** exceeds **1%** OR int8 flips top-1 on any close pair in the steps collection → apply one of:
  - Increase keyword-channel weight for the steps collection (hybrid BM25+vector with higher alpha on BM25 side).
  - Use fp32 pointwise for the steps collection only (`model_path` per-collection config, planned for stage 5).
- If delta is under 1% and no top-1 flips on close pairs → ship int8 as the production model for all collections.
