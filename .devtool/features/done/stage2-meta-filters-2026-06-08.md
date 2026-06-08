---
id: "stage2-meta-filters-2026-06-08"
status: "done"
priority: "medium"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T20:21:55.000Z"
completedAt: "2026-06-08T20:21:55.000Z"
labels: ["stage-2", "rust-core"]
order: "aH"
---

# Stage 2 — Meta-фильтры

## Acceptance
- [ ] `meta` — свободные key-value; **никаких** доменных полей (`sku`, `inn`, `scenario_name`) в схеме ядра
- [ ] Фильтры: `any` (OR) и `all` (AND), комбинируемые
- [ ] Применяются в `search` и `grep` (и dense, и keyword-канале)

Refs: §5.4.
