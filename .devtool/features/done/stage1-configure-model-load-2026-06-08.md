---
id: "stage1-configure-model-load-2026-06-08"
status: "done"
priority: "high"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T19:51:43.000Z"
completedAt: "2026-06-08T19:51:43.000Z"
labels: ["stage-1", "rust-core"]
order: "a7"
---

# Stage 1 — configure (загрузка модели, фиксация dim)

## Acceptance
- [ ] Поля: `model_path` (локальная папка: ONNX + tokenizer + config), `normalize`, `max_seq_len`, `device` (cpu по умолчанию), `intra_threads`
- [ ] Грузит модель **один раз**; `dim` фиксируется моделью и привязывается к индексу
- [ ] Идемпотентен
- [ ] Повторный `configure` с **другой моделью/dim** после индексации → `reset` (старые векторы невалидны в новом пространстве); отменить in-flight ингест перед свопом модели
- [ ] `reset` — полная очистка индекса

Refs: §5.1, §4.4.

## Implementation notes
Implemented behind an `Embedder` trait (`src/embed.rs`); this slice instantiates a deterministic `MockEmbedder` (dim 64) — the real ONNX/tokenizer model load is the separate later card. `configure` (`store::configure`) is idempotent, stores the embedder (`Arc<dyn Embedder>`) + a `Config` echo in the singleton, fixes `dim` from the embedder, sets `configured=true`, and on a different-dim reconfigure with data present performs a full index reset. `reset` clears the whole index. `model_path` is accepted/echoed but loads nothing in mock mode. Wired as the `configure` dispatch arm; returns dim + config echo + a `reset` flag.
