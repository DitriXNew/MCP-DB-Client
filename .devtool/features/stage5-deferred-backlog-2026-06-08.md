---
id: "stage5-deferred-backlog-2026-06-08"
status: "backlog"
priority: "low"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T18:03:21.000Z"
completedAt: null
labels: ["stage-5", "deferred"]
order: "aR"
---

# Stage 5 — Отложенное (по необходимости)

Не в MVP. Делать только если соответствующая боль проявится.

## Acceptance
- [ ] File watcher (notify + периодический реконсайл, дебаунс, хэш-детект)
- [ ] Content-hash кэш эмбеддингов (если холодный старт станет раздражать)
- [ ] Модель-на-коллекцию (напр. fp32 точечно для коллекции шагов, если int8 просядет)
- [ ] Генерик-ссылки (§5.5) в GA — связь «A ссылается на B» + «кто ссылается на X»
- [ ] `broadcastNotification`-прогресс для самого MCP-клиента (SSE), если понадобится

Refs: §5.5, §8 (этап 5), §10.
