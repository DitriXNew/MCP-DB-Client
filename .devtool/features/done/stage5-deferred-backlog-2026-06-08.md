---
id: "stage5-deferred-backlog-2026-06-08"
status: "done"
priority: "low"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-09T09:42:00.000Z"
completedAt: "2026-06-09T09:42:00.000Z"
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

---

## Closed as deferred (2026-06-09)

Stages 0–4 are complete (core search subsystem + GPU/int8 delivery + QA/products/
clients adapters, all in `done/`). This card is the **intentional post-MVP parking
lot** — none of its items are built, by design ("делать только если боль
проявится"). Closed to clear the active board; the items above stay documented as
future work and can be re-opened individually if the corresponding pain appears.
Notably, the int8↔fp32 retrieval-delta measurement and "model-per-collection"
naturally live here (see the int8 card's deferred bullet).
