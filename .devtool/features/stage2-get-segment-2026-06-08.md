---
id: "stage2-get-segment-2026-06-08"
status: "backlog"
priority: "medium"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T18:03:21.000Z"
completedAt: null
labels: ["stage-2", "rust-core"]
order: "aF"
---

# Stage 2 — get_segment

## Acceptance
- [ ] Поля: `doc_id`, `line_start`, `line_end`, `max_lines?`
- [ ] O(1)-срез по таблице офсетов
- [ ] Выход за границы → клампинг с возвратом **фактического** диапазона
- [ ] Обслуживается **сразу после accept** (по `text`+офсетам), не ждёт векторов
- [ ] **ПРЕДУСЛОВИЕ (carried-forward из Stage 1):** `accept` должен класть `text`+офсеты в стор **синхронно**; сейчас Stage 1 (`store::accept_index`) откладывает установку всего документа в воркер → до эмбеддинга текста в сторе нет. Поправить здесь: на accept ставить text-сегменты (vector=None), воркер дозаполняет только вектор
- [ ] Для атомарных записей (товар/клиент/шаг без line-range) не используется

Refs: §5.3, §4.4 (текстовые операции не ждут векторов).
