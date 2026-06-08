---
id: "stage4-products-adapter-2026-06-08"
status: "backlog"
priority: "medium"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T18:03:21.000Z"
completedAt: null
labels: ["stage-4", "1c-bsl", "adapter"]
order: "aP"
---

# Stage 4 — Адаптер «Товары»

## Acceptance
- [ ] 1С читает номенклатуру → `index_segments`
- [ ] `text` = карточка; `embed_text` = имя+бренд+категория+ключевые свойства; `meta` = `{sku, article, category, brand}`
- [ ] `search mode=hybrid`: артикул/SKU ловятся keyword-каналом, интент — dense
- [ ] Настройка hybrid под точные идентификаторы

Refs: §7.2, §6.3.
