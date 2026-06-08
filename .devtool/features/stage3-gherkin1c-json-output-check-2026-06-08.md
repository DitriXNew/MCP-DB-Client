---
id: "stage3-gherkin1c-json-output-check-2026-06-08"
status: "backlog"
priority: "high"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T18:03:21.000Z"
completedAt: null
labels: ["stage-3", "investigation", "blocker"]
order: "aL"
---

# Stage 3 — Проверка JSON-выхлопа Gherkin1C (откр. вопрос §11.1)

Блокер для line-addressing QA-адаптера.

## Acceptance
- [ ] Подтвердить, что выхлоп `lintest/Gherkin1C` несёт **позиции строк**
- [ ] Различение элементов: scenario / структура сценария / предыстория (background) / теги / примеры (examples)
- [ ] Если позиций нет — определить fallback для `line_start/line_end`

Refs: §7.1, §11.1.
