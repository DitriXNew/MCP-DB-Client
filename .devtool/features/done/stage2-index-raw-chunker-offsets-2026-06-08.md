---
id: "stage2-index-raw-chunker-offsets-2026-06-08"
status: "done"
priority: "medium"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T20:47:25.000Z"
completedAt: "2026-06-08T20:47:25.000Z"
labels: ["stage-2", "rust-core"]
order: "aE"
---

# Stage 2 — index_raw + чанкер + таблица офсетов + нумерация строк

## Acceptance
- [ ] `index_raw`: `collection`, `doc_id?`, `name`, `text`, `meta{}`, `chunk_cfg?`
- [ ] Чанкинг **по бюджету токенов** (цель ~300, хард-кап = `max_seq_len`) со **снапом к границам строк**; overlap ~2 строки
- [ ] Edge-case: сверхдлинная строка = один oversized-чанк; для эмбеддинга усекается до `max_seq_len`, в `get_segment` отдаётся целиком
- [ ] Хранить полный текст + таблицу офсетов строк
- [ ] Кодировки: CRLF→LF; нумерация **1-based, включительная**, по `\n` после нормализации
- [ ] Чанкинг строится синхронно на accept (это не эмбеддинг); вектор — в фоне
- [ ] Авто-присвоение `doc_id` (только для fire-and-forget `index_raw`) → **вернуть присвоенные id в ack**, иначе нечем адресовать upsert/delete

Refs: §6.1, §6.5, §5.2, §4.4.
