---
id: "stage2-concurrent-query-embedding-2026-06-08"
status: "backlog"
priority: "medium"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T18:03:21.000Z"
completedAt: null
labels: ["stage-2", "rust-core"]
order: "aJ"
---

# Stage 2 — Конкурентный query-эмбеддинг (одна ось параллелизма)

Снимает head-of-line blocking: во время `building` запрос исполняется параллельно с реиндексом, а не ждёт батч. Держит NFR латентности §9.

## Acceptance
- [ ] bulk: один крупный `Run` с intra-op = ncores−1, **без rayon поверх эмбеддинга** (избежать oversubscription rayon×intra-op)
- [ ] query-`Run` — конкурентно на той же сессии; зарезервированное ядро = headroom через планировщик ОС (на мелкой задаче), не партиционирование пула
- [ ] `rayon` — только на CPU-подготовку (чанкинг, офсеты, хэши), **не** на сам эмбеддинг; роль `rayon` в §3 скорректировать соответственно
- [ ] (Если выбран fallback двух экземпляров из stage-0 — bulk и query на раздельных сессиях, что даёт лучшую изоляцию латентности)

Refs: §4.4 (Правка 1), §3, §9.
