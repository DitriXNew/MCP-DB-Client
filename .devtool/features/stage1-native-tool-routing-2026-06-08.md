---
id: "stage1-native-tool-routing-2026-06-08"
status: "backlog"
priority: "high"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T18:03:21.000Z"
completedAt: null
labels: ["stage-1", "cpp-glue"]
order: "aC"
---

# Stage 1 — Роутинг native-тулзов в tools/call + схемы в tools/list

**Это НЕ текущее поведение.** Сейчас любой `tools/call` безусловно форвардится в 1С через `ExtEvent`. `tools/list` нативен потому, что это отдельный *метод*, — иная ситуация.

## Acceptance
- [ ] В ветке `tools/call`: если `toolName ∈ {search, get_segment, grep}` → звать Rust-ядро и вернуть результат **прямо в обработчике**; иначе — существующий путь `ExtEvent("HttpServer","ToolCall",…)` в 1С
- [ ] `tools/list` отдаёт **union**: `cachedToolsJson` (от 1С) + схемы native-тулзов (владеет и домешивает C++-компонент)
- [ ] Поисковая подсистема самодостаточна — не зависит от того, объявит ли native-тулзы 1С
- [ ] Ошибки тулзов (битый regex, несуществующий `doc_id`) — **структурный результат** MCP-тула, не сбой транспорта/сессии

Refs: §4.1, §4.4, §5.3, §9. Точка врезки: [HttpServerComponent.cpp:822](http-1c-dll/src/HttpServerComponent.cpp#L822).
