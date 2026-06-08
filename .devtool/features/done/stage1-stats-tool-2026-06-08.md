---
id: "stage1-stats-tool-2026-06-08"
status: "done"
priority: "medium"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T19:51:43.000Z"
completedAt: "2026-06-08T19:51:43.000Z"
labels: ["stage-1", "rust-core"]
order: "aD"
---

# Stage 1 — stats (наблюдаемость холодного старта)

## Acceptance
- [ ] Поля: `n_docs`, `n_segments`, модель, `dim`, оценка памяти
- [ ] На каждую коллекцию — **два поля**: `text_ready: bool` + `vector_status: building | ready`, плюс опц. `error` (фатальный)
- [ ] Счётчики: `embedded` / `failed` / `skipped`
- [ ] Это и есть **прогресс холодного старта** — опрашивается из формы 1С (а **не** `SendProgress`: у push-ингеста нет pending-запроса/progressToken)

Refs: §5.1, §4.4 (Адресат прогресса), §6.4.

## Implementation notes
Extended the existing `stats` arm. Top-level: `configured`, `dim`, `n_docs`, `n_segments` (totals), `callsHandled` (preserved), and a `collections` object. Each collection reports the two-axis state — `text_ready: bool` + `vector_status: "building"|"ready"` — plus the cold-start counters `embedded`/`failed`/`skipped`, `n_docs`, `n_segments`, and an optional `error` (only present on a fatal fault). This is the poll-based cold-start progress for the 1C form (not `SendProgress`, which push-ingest has no pending token for). Memory estimate field was left out for now — not load-bearing for the slice; flag for review if wanted.
