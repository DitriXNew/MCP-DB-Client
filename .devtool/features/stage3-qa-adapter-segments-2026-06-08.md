---
id: "stage3-qa-adapter-segments-2026-06-08"
status: "backlog"
priority: "medium"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T18:03:21.000Z"
completedAt: null
labels: ["stage-3", "1c-bsl", "adapter"]
order: "aN"
---

# Stage 3 — QA-адаптер: сегменты сценариев/шагов (BSL)

Адаптер на стороне 1С/BSL — в ядро не входит. Зависит от блокеров §11.1 и §11.5.

## Acceptance
- [ ] Оркестрация из 1С: Gherkin1C парсит → JSON → BSL мапит в `index_segments`
- [ ] **Тесты** (единица = сценарий): `text` = дословный сценарий; `embed_text` = имя+теги+шаги; `meta` = `{type:scenario, tags, callable, feature}`; `line_start/end` из парсера; предысторию привязывать к фиче
- [ ] **Каталог шагов** (из реестра Ванессы): `text` = каноническая фраза; `embed_text` = фраза+описание+параметры; `meta` = `{type:step}`
- [ ] Gherkin1C — отдельная компонента (`lintest/Gherkin1C`), НЕ встраивать в ядро и НЕ переписывать на Rust

Refs: §7.1.
