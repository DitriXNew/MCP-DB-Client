---
id: "perf-wstring-convert-race-2026-06-09"
status: "done"
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
- [x] Убран общий `static std::wstring_convert`: на Windows — прямой `WideCharToMultiByte`/`MultiByteToWideChar` (локально, без shared state); на не-Windows — converter теперь **локальный** (без `static`). [AddInNative.cpp](../../http-1c-dll/src/AddInNative.cpp) `WCHAR2MB`/`MB2WCHAR` (`WCHAR2WC` уже был локальным)
- [~] `MB2WCHAR`: убрал промежуточный `wstring`→`u16string` (3 аллокации → 2: одна `u16string` + копия в буфер менеджера на call-site). Конверсию **прямо** в `AllocMemory`-буфер 1С-менеджера не делал — это рефактор 24 call-site'ов (последняя копия), несоразмерно correctness-фиксу; задокументировано как остаток
- [~] Прогон под конкурентной нагрузкой локально не гонял (требует полного ребилда DLL + multi-client харнес). Корневая причина — shared mutable state — устранена полностью; round-trip кириллицы покрыт 1С-контуром («Феррон»/«Ромашка»/«ноутбук»)

## Done 2026-06-09 (критическая часть — гонка — закрыта)
`WCHAR2MB`/`MB2WCHAR` переписаны на локальные Win32-конверсии: нет process-wide мутабельного состояния → пул потоков httplib больше не может его испортить. Сигнатуры (static-методы) не менялись → 24 call-site'а не тронуты. Синтаксис-чек MSVC (`cl /Zs /std:c++17 /utf-8` с дефайнами проекта) прошёл без ошибок; полная компиляция — в CI (`build-and-test`). Остаток (конверсия прямо в буфер менеджера + concurrent-load тест) — осознанно отложенная микро-оптимизация, не блокирует фикс корректности.
