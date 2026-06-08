---
id: "stage1-async-ingest-worker-state-machine-2026-06-08"
status: "done"
priority: "high"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T19:51:43.000Z"
completedAt: "2026-06-08T19:51:43.000Z"
labels: ["stage-1", "rust-core"]
order: "a8"
---

# Stage 1 — Асинхронный index_segments + фоновый воркер + стейт-машина

Ядро решения §4.4: эмбеддинг 15–60 с не должен морозить UI клиента 1С.

## Acceptance
- [ ] `index_segments`: `collection`, `doc_id` (обязателен — см. ниже), `name`, `meta{}`, `segments[]` = `{text, embed_text?, line_start?, line_end?, meta{}}`
- [ ] Вызов **возвращается немедленно**: `{accepted, collection, doc_count}`, работа в очередь
- [ ] Эмбеддинг — на **фоновом потоке Rust**; вызов из BSL не блокирует поток 1С
- [ ] **Двухосевая** стейт-машина коллекции: `text_ready: bool` (сразу на accept) + `vector_status: building → ready`; `error` — только фатальный сбой
- [ ] На accept **синхронно и вне write-lock**: парсинг JSON + построение `text`/таблицы офсетов; под коротким write-lock — только вставка в стор
- [ ] Ограничить размер одного пуша (батчи по неск. сотен), чтобы accept не давал хитч UI
- [ ] **skip-and-continue**: битый сегмент (некорректный UTF-8 и т.п.) изолируется → `failed++`, коллекция строится дальше
- [ ] `doc_id` обязателен для всего, что потом обновляется/удаляется (в 1С есть ссылка/GUID)
- [ ] Воркер останавливается через `rust_shutdown` (см. stage-0 FFI), не на разрушение экземпляра компонента

Refs: §4.4, §5.2, §6.4.

## Implementation notes
`index_segments` → `store::accept_index`: under a short write lock it allocates segment ids, marks the collection `text_ready=true`/`vector_status=Building`, bumps `pending_jobs`, counts blank segments as `skipped` immediately, then enqueues one `EmbedJob` on an `mpsc` queue and returns `{accepted, collection, segment_count}` — never blocking on embedding. A lazily-spawned `std::thread` worker (`worker_loop`) pulls jobs, embeds each segment's `embed_text`-or-`text` OUTSIDE the lock, then under one short write lock fills vectors and **atomically upserts by doc_id** (`docs.insert` replaces all old segments — no half-old/half-new). When a collection's `pending_jobs` hits 0 it flips to `Ready` and `embedded` is bumped. skip-and-continue: blank text → `skipped`; non-finite/zero vector → `failed`; the collection keeps building and never goes to `error` (reserved for fatal faults). Jobs whose target collection was cleared mid-flight (reset/dim-change) are dropped harmlessly. Shutdown: `rcore_shutdown` → `store::shutdown` takes the worker out of the singleton, sends `Stop`, and joins it OUTSIDE the lock; idempotent, and a later ingest respawns a fresh worker. A `Condvar`-based `wait_until_ready(collection, timeout)` gives deterministic (sleep-free) waits for tests/callers.
