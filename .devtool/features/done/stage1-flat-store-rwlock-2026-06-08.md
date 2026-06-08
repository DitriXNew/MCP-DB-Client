---
id: "stage1-flat-store-rwlock-2026-06-08"
status: "done"
priority: "high"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T19:51:43.000Z"
completedAt: "2026-06-08T19:51:43.000Z"
labels: ["stage-1", "rust-core"]
order: "aA"
---

# Stage 1 — Flat in-memory стор под RwLock

## Acceptance
- [ ] Flat brute-force структура: сегменты, нормализованные векторы, `meta`, таблица офсетов строк, индекс по `doc_id`/`collection`
- [ ] `RwLock`: читатели — `search`/`get_segment`/`grep`; писатель — применение ингеста/upsert/delete
- [ ] Эмбеддинг **не касается** лока индекса (считается на сессии/воркере вне лока)
- [ ] Применение готового вектора — короткий write-lock (по возможности батчить, чтобы не молотить read-локи)
- [ ] Память: десятки МБ на 5–10к записей

Refs: §4.3, §2, §9.

## Implementation notes
Flat in-memory store in `src/core.rs`: `Collection { docs: HashMap<doc_id, Document>, text_ready, vector_status, error, pending_jobs, embedded/failed/skipped }`, `Document { doc_id, name, meta, segments }`, `Segment { segment_id (stable), text, embed_text?, line_start/end?, meta, vector: Option<Vec<f32>> }`. The whole store lives in the existing `Lazy<RwLock<Core>>` singleton. Readers = `search`/`stats`; writers = ingest accept/apply + `reset`/`configure`. Embedding never touches the index lock — the worker embeds outside it and only takes a short write lock for the apply/swap. Brute-force dense scoring is dot product over normalized vectors. Search is the natural place keyword/meta-filters slot in later.
