---
id: "stage2-incremental-upsert-delete-2026-06-08"
status: "done"
priority: "medium"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T21:31:30.000Z"
completedAt: "2026-06-08T21:31:30.000Z"
labels: ["stage-2", "rust-core"]
order: "aK"
---

# Stage 2 — Инкрементальные upsert/delete (атомарность)

## Acceptance
- [ ] Upsert по `doc_id`: эмбеддинг новых сегментов — **вне лока**; затем под **одним** коротким write-lock **в той же критической секции** — удалить старые сегменты `doc_id` + вставить новые
- [ ] Никакого промежуточного состояния «половина старых, половина новых»
- [ ] `delete_document` (`doc_id`), `delete_collection` (`collection`)
- [ ] Инкремент одной записи (изменился один товар/тест) — переэмбеддить только её, не весь корпус; завершается почти мгновенно

Refs: §4.4 (Атомарный upsert), §5.2, §6.4.
