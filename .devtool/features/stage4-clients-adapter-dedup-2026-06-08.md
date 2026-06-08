---
id: "stage4-clients-adapter-dedup-2026-06-08"
status: "backlog"
priority: "medium"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T18:03:21.000Z"
completedAt: null
labels: ["stage-4", "1c-bsl", "adapter"]
order: "aQ"
---

# Stage 4 — Адаптер «Клиенты» + дедуп

## Acceptance
- [ ] 1С читает контрагентов → `index_segments`
- [ ] `text` = имя+реквизиты; `embed_text` = нормализованное имя+атрибуты; `meta` = `{inn, city, segment}`
- [ ] Дедуп = логика **адаптера**: dense-кандидаты + строковое расстояние по имени + точный матч по ИНН
- [ ] Решение «одна сущность» — НЕ в ядре

Refs: §7.3.
