---
id: "stage4-products-adapter-2026-06-08"
status: "done"
priority: "medium"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-09T09:42:00.000Z"
completedAt: "2026-06-09T09:42:00.000Z"
labels: ["stage-4", "1c-bsl", "adapter"]
order: "aP"
---

# Stage 4 — Адаптер «Товары»

## Acceptance
- [x] 1С → `index_segments` (источник — стаб-массив номенклатуры; подменяется реальным чтением справочника)
- [x] `text` = карточка (имя + арт./SKU); `embed_text` = имя+бренд+категория; `meta` = `{sku, article, category, brand, tags}`
- [x] `search mode=hybrid`: артикул/SKU ловятся keyword-каналом, интент — dense
- [x] Настройка hybrid под точные идентификаторы (артикул/SKU кладём в `text`, т.к. keyword-канал читает только текст)

Refs: §7.2, §6.3.

---

## Done (2026-06-09, commit 81957aa) — implemented + asserted

`ProductsPayload` maps a stub catalog (`StubProducts`: name/brand/category/sku/
article/tags) to `index_segments`: `text` = «<имя> (арт. <article>, <sku>)»,
`embed_text` = name | brand | category, `meta` = `{type, sku, article, category,
brand, tags}`. Key design point confirmed against the core: the **keyword channel
reads only `text`** (rust-core/src/lib.rs:1377), so exact identifiers go in `text`
while semantic intent goes in `embed_text`. Two contour cases PASS: «ноутбук»
(hybrid/dense) → «Ноутбук Lenovo…», and exact-id «ART-1003» (keyword) →
«Кофемашина DeLonghi…». Swap the stub for a real номенклатура read to ship.
