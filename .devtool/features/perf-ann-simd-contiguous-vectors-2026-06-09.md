---
id: "perf-ann-simd-contiguous-vectors-2026-06-09"
status: "backlog"
priority: "high"
assignee: null
dueDate: null
created: "2026-06-09T12:00:00.000Z"
modified: "2026-06-09T12:00:00.000Z"
completedAt: null
labels: ["perf", "rust-core", "scaling"]
order: "a0"
---

# Perf (масштаб) — contiguous векторное хранилище + SIMD dot, затем ANN-индекс

Фундаментальный фикс масштабирования. Пока dense-поиск brute-force, latency **линейна по корпусу**, и микро-фиксы (top-k heap, кэш токенов) это не меняют — они лишь снижают константу.

- Хранилище — `HashMap<collection> → HashMap<doc> → Vec<Segment{vector: Vec<f32>}>`, скан тройным циклом по всем сегментам — [core.rs:1325](rust-core/src/core.rs#L1325). Нет HNSW/IVF/ANN.
- `dot` — скалярный `zip().map().sum()`, f32-сумма слева-направо, LLVM сам не векторизует — [embed.rs:125](rust-core/src/embed.rs#L125). В `Cargo.toml` нет SIMD/ANN-зависимостей.

Делать **после** top-k heap ([perf-topk-heap-deferred-hit](perf-topk-heap-deferred-hit-2026-06-09.md)), т.к. часть выигрыша от него остаётся актуальной и при ANN.

## Acceptance
- [ ] Векторы коллекции хранятся в contiguous `Vec<f32>` (dim-strided), а не `Vec<Vec<f32>>` — кэш-линейный, авто-векторизуемый dot
- [ ] SIMD dot: ручной chunk на 8 lanes (`chunks_exact(8)` + аккумулятор) или крейт `wide`/`std::simd`; f32 (не f64)
- [ ] ANN-индекс (`hnsw_rs`/`instant-distance`) по `segment_id`, дополняемый в `apply_job`; sublinear-путь для dense
- [ ] Корректность: top-k через ANN совпадает с brute-force на тестовом корпусе (recall@k проверен)
- [ ] Бенч latency vs размер корпуса: линейность ушла
