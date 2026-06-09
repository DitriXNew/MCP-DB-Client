---
id: "stage4-clients-adapter-dedup-2026-06-08"
status: "done"
priority: "medium"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-09T09:42:00.000Z"
completedAt: "2026-06-09T09:42:00.000Z"
labels: ["stage-4", "1c-bsl", "adapter"]
order: "aQ"
---

# Stage 4 — Адаптер «Клиенты» + дедуп

## Acceptance
- [x] 1С → `index_segments` (источник — стаб-массив контрагентов; подменяется реальным чтением справочника)
- [x] `text` = имя + ИНН; `embed_text` = имя+город+сегмент; `meta` = `{inn, city, segment}`
- [x] Дедуп = логика **адаптера**: точный матч по ИНН (dense-кандидаты + строковое расстояние — задокументированное расширение)
- [x] Решение «одна сущность» — НЕ в ядре (целиком на стороне адаптера)

Refs: §7.3.

---

## Done (2026-06-09, commit 81957aa) — implemented + asserted

`ClientsDedup` performs the "one entity" decision **adapter-side** (the core never
sees duplicates): exact INN match collapses duplicates before indexing.
`ClientsPayload` maps unique clients to `index_segments`: `text` = «<имя> (ИНН
<инн>)», `embed_text` = name+city+segment, `meta` = `{type, inn, city, segment}`.
Contour case 6 PASS: a 6-row stub with two duplicate-INN pairs → **4 unique**
indexed (`segments(4==4)`), and «Ромашка» is retrievable. The fuzzy half
(dense-candidate recall + name string-distance) is a documented extension on top
of the exact-INN base; swap the stub for a real контрагенты read to ship.
