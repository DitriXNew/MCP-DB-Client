---
id: "stage0-cpp-thread-safety-review-2026-06-08"
status: "done"
priority: "high"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T20:47:25.000Z"
completedAt: "2026-06-08T20:47:25.000Z"
labels: ["stage-0", "cpp-glue"]
order: "a5"
---

# Stage 0/1 — Ревизия потокобезопасности существующего C++ (§12)

Новый нативный путь поиска исполняется на потоках httplib-сервера параллельно с push из 1С.

## Acceptance
- [ ] Ревизия общего состояния под параллельным нативным поиском + push: `sessions` (session map), `cachedToolsJson` (tool cache), `pendingRequests` (pending map)
- [ ] Подтвердить, что нативный роутинг `search`/`grep`/`get_segment` не вводит гонок с существующим `ExtEvent`-путём
- [ ] Проверить мьютексы вокруг кэшей при чтении из обработчика `tools/call`

Refs: §4.3, §12. Код: [HttpServerComponent.cpp](http-1c-dll/src/HttpServerComponent.cpp).
