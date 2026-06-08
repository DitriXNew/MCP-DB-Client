---
id: "stage3-find-step-usages-2026-06-08"
status: "backlog"
priority: "medium"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T18:03:21.000Z"
completedAt: null
labels: ["stage-3", "1c-bsl", "adapter"]
order: "aO"
---

# Stage 3 — find_step_usages (анти-галлюцинация)

ИИ копирует рабочий вызов с реальными параметрами, а не выдумывает.

## Acceptance
- [ ] `find_step_usages(step)` — обратный индекс шаг→сценарии: реальные вызовы с конкретными параметрами
- [ ] Реализация: генерик-ссылки (§5.5, отложено в ядре) **или** на стороне адаптера
- [ ] Опционально `validate_steps(scenario)`

Refs: §7.1, §5.5.
