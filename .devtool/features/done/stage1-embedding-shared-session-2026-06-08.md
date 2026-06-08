---
id: "stage1-embedding-shared-session-2026-06-08"
status: "done"
priority: "high"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T22:11:45.000Z"
completedAt: "2026-06-08T22:11:45.000Z"
labels: ["stage-1", "rust-core"]
order: "a9"
---

# Stage 1 — Эмбеддинг (fastembed, общая ONNX-сессия)

## Acceptance
- [ ] Встроенная мультиязычная модель fastembed → корректные префиксы `query:`/`passage:`, pooling, normalize из коробки
- [ ] **Общая ONNX-сессия, конкурентный `Run`** (путь подтверждён в stage-0 confirm-embed-signature)
- [ ] Векторы хранить **нормализованными** → поиск = скалярное произведение
- [ ] Эмбеддинг идёт **вне лока индекса** (через сессию/воркер)
- [ ] Учитывать: ru/uk токенизируются в больше токенов → бюджет чанка покрывает меньше текста
- [ ] При кастомной ONNX-модели — точно воспроизвести препроцессинг (префикс, pooling, normalize); ошибка молча убивает релевантность

Refs: §6.2, §4.4.
