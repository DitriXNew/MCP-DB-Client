---
id: "stage0-offline-fastembed-init-2026-06-08"
status: "todo"
priority: "high"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T18:13:44.000Z"
completedAt: null
labels: ["stage-0", "rust-core", "investigation", "blocker"]
order: "a3"
---

# Stage 0 — Оффлайн-инициализация fastembed (откр. вопрос §11.4)

## Acceptance
- [ ] Точный способ указать локальный путь к модели (ONNX + tokenizer + config)
- [ ] Отключить **все** сетевые загрузки: и моделей, и бинарников `ort`/`onnxruntime`
- [ ] `hf-hub` не использовать в оффлайн-режиме
- [ ] Подтвердить отсутствие неявных обращений к сети при первом `configure`

Refs: §3 (fastembed, hf-hub), §11.4.
