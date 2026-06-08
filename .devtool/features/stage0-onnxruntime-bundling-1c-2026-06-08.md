---
id: "stage0-onnxruntime-bundling-1c-2026-06-08"
status: "todo"
priority: "critical"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T18:13:44.000Z"
completedAt: null
labels: ["stage-0", "infra", "investigation", "blocker"]
order: "a1"
---

# Stage 0 — Бандлинг onnxruntime.dll в 1С (откр. вопрос §11.2)

Инфраструктурный блокер этапа 0. Эмпирическая проверка, не вычитывается на бумаге.

## Acceptance
- [ ] Определить способ доставки `onnxruntime.dll` (+ провайдеров): через `Template.bin` 1С-add-in **или** из папки рядом с EPF
- [ ] Подтвердить, что 1С-загрузчик находит нативную DLL рядом с компонентой
- [ ] **Не** автоскачивать — контролируемый бандлинг
- [ ] Проверить разрядность: целевой клиент **x64**

Refs: §3 (C++ сторона), §11.2, §12.
