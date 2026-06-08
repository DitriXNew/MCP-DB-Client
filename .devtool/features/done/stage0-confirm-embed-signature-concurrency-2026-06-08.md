---
id: "stage0-confirm-embed-signature-concurrency-2026-06-08"
status: "done"
priority: "high"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T22:11:45.000Z"
completedAt: "2026-06-08T22:11:45.000Z"
labels: ["stage-0", "rust-core", "investigation"]
order: "a4"
---

# Stage 0 — Сигнатура embed() и модель конкурентности (Правка 1)

Определяет основной путь эмбеддинга. ONNX Runtime потокобезопасен по `Run`.

## Acceptance
- [ ] Подтвердить сигнатуру `fastembed::TextEmbedding::embed`
- [ ] Если `&self + Send + Sync` → **основной путь**: общая ONNX-сессия, конкурентный `Run` (bulk + query на одной сессии)
- [ ] Если `&mut self` → fallback: **два экземпляра модели** (bulk + query), НЕ приоритет-очередь (конкурентный `Run` из двух потоков на одном `&mut self`-экземпляре не компилируется; `Mutex` = сериализация = снова head-of-line). int8 делает двойную загрузку дешёвой
- [ ] Зафиксировать ось параллелизма bulk: один крупный `Run` с intra-op=ncores−1, **без rayon поверх эмбеддинга** (иначе oversubscription rayon×intra-op)
- [ ] Примечание: при общей сессии intra-op пул один на сессию — «зарезервированное ядро» даёт headroom query через планировщик ОС на мелкой задаче, а не через партиционирование пула

Refs: §4.4 (Правка 1), §6.2, §9.
