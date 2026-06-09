---
id: "stage3-find-step-usages-2026-06-08"
status: "done"
priority: "medium"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-09T09:42:00.000Z"
completedAt: "2026-06-09T09:42:00.000Z"
labels: ["stage-3", "1c-bsl", "adapter"]
order: "aO"
---

# Stage 3 — find_step_usages (анти-галлюцинация)

ИИ копирует рабочий вызов с реальными параметрами, а не выдумывает.

## Acceptance
- [x] `find_step_usages(step)` — обратный индекс шаг→сценарии: реальные вызовы с конкретными параметрами
- [x] Реализация: на стороне адаптера (keyword-канал ядра по тексту сценария; генерик-ссылки §5.5 не нужны)
- [ ] Опционально `validate_steps(scenario)` — отложено (опциональное)

Refs: §7.1, §5.5.

---

## Done (2026-06-09, commit 81957aa) — implemented + asserted

`find_step_usages` is an adapter-side reverse lookup: a `keyword`-mode `search`
over the `qa_scenarios` collection (whose segment `text` is the verbatim scenario
incl. its steps). It returns the scenarios that use a given step **with the real
parameter values**, which is the anti-hallucination guarantee. No core change
needed — the keyword channel reads segment text and works immediately (no
embedding wait). Contour case 3 PASS: query «я удаляю пользователя» returns the
«Удаление пользователя» scenario carrying the concrete param **`VanessaUser1`**.
`validate_steps(scenario)` is left as an optional future helper.
