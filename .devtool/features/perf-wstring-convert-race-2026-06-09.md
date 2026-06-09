---
id: "perf-wstring-convert-race-2026-06-09"
status: "todo"
priority: "critical"
assignee: null
dueDate: null
created: "2026-06-09T12:00:00.000Z"
modified: "2026-06-09T12:00:00.000Z"
completedAt: null
labels: ["perf", "cpp-glue", "correctness"]
order: "a1"
---

# Perf/correctness — гонка данных в WCHAR2MB/MB2WCHAR (process-wide static wstring_convert)

Корректность, не только скорость. `WCHAR2MB`/`MB2WCHAR` используют **process-wide `static std::wstring_convert`** ([AddInNative.cpp:452](http-1c-dll/src/AddInNative.cpp#L452), [:471](http-1c-dll/src/AddInNative.cpp#L471)) — у объекта мутабельное внутреннее состояние, он **не thread-safe**. httplib крутит пул `max(8, ncpu-1)` потоков, а новый MCP-путь конвертит UTF-16↔UTF-8 на каждом запросе/ответе/ExtEvent.

Код не из этого PR, но `HttpServerComponent.cpp` впервые подставляет его под реальную конкуренцию → гонка → порча UTF-8 / редкие падения под нагрузкой.

Заодно `MB2WCHAR` делает 3 аллокации на ответ (`wstring` → `u16string` → буфер 1С-менеджера) — [AddInNative.cpp:471-480](http-1c-dll/src/AddInNative.cpp#L471).

## Acceptance
- [ ] Убрать общий `static std::wstring_convert` (локальный на вызов **или** прямой `WideCharToMultiByte`/`MultiByteToWideChar`)
- [ ] UTF-8→UTF-16 конвертировать прямо в буфер 1С-менеджера: длина через `MultiByteToWideChar(...,NULL,0)`, один `AllocMemory`, конверсия на месте (убирает 2 из 3 аллокаций)
- [ ] Прогон под конкурентной нагрузкой (несколько MCP-клиентов параллельно) без порчи payload
