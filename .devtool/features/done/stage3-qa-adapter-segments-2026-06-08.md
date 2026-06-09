---
id: "stage3-qa-adapter-segments-2026-06-08"
status: "done"
priority: "medium"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-09T09:42:00.000Z"
completedAt: "2026-06-09T09:42:00.000Z"
labels: ["stage-3", "1c-bsl", "adapter"]
order: "aN"
---

# Stage 3 — QA-адаптер: сегменты сценариев/шагов (BSL)

Адаптер на стороне 1С/BSL — в ядро не входит. Зависит от блокеров §11.1 и §11.5.

## Acceptance
- [x] Оркестрация из 1С: данные сценариев → BSL мапит в `index_segments` (источник — стаб-массив; Gherkin1C-парс — задокументированная точка подмены, см. [[stage3-gherkin1c-json-output-check]])
- [x] **Тесты** (единица = сценарий): `text` = дословный сценарий; `embed_text` = имя+теги+шаги; `meta` = `{type:scenario, feature, tags, name}`; `line_start/line_end` присутствуют
- [x] **Каталог шагов**: `text` = каноническая фраза; `embed_text` = фраза+описание+параметры; `meta` = `{type:step, params}`
- [x] Gherkin1C / реестр шагов — внешние источники, НЕ в ядре (стаб подменяется реальным фидом, см. [[stage3-vanessa-step-registry]])

Refs: §7.1.

---

## Done (2026-06-09, commit 81957aa) — implemented + asserted in the http1c contour

Two BSL adapters build `index_segments` payloads with the exact metadata the
card specifies, verified by the headless self-test (`onec-rag-selftest`):
- **QA scenarios** (`ScenariosPayload`): `text` = verbatim `Сценарий: <name>` + steps;
  `embed_text` = name + tags + steps; `meta` = `{type:scenario, feature, tags, name}`;
  `line_start`/`line_end` per segment. Contour case 2 PASS — query «фильтр по
  компании» returns the scenario with `line_start:12 line_end:17` and the real
  param «Феррон».
- **QA step catalog** (`StepCatalogPayload`): `text` = canonical phrase; `embed_text`
  = phrase | description | param-types; `meta` = `{type:step, params}`. Contour case 1
  PASS — «удаление пользователя» → «Я удаляю пользователя».
Data source is a pluggable in-BSL stub (`StubScenarios` / `StubStepCatalog`);
swap it for the Gherkin1C parser output / `ТаблицаИзвестныхStepDefinition` (both
investigated and de-risked in the linked cards) without touching the adapter shape.
