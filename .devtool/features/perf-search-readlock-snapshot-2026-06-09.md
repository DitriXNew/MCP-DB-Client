---
id: "perf-search-readlock-snapshot-2026-06-09"
status: "todo"
priority: "medium"
assignee: null
dueDate: null
created: "2026-06-09T12:00:00.000Z"
modified: "2026-06-09T12:00:00.000Z"
completedAt: null
labels: ["perf", "rust-core"]
order: "a5"
---

# Perf — сократить удержание read-lock на время скана

`search`/`grep` держат `CORE.read()` на **весь** O(корпус) скан. Долгий запрос блокирует `apply_job` воркера, который берёт `CORE.write()` чтобы установить готовые векторы → коллекция дольше висит в `Building`, другие запросы дольше отдают `partial:true`. На Windows SRWLOCK (writer-preferring) один медленный grep может выстроить конвой новых читателей.

- Глобальный `RwLock<Core>` — [core.rs:286](rust-core/src/core.rs#L286).
- Диспетчер держит `.read()` всю ветку — [lib.rs:330](rust-core/src/lib.rs#L330), [lib.rs:304](rust-core/src/lib.rs#L304).
- `apply_job` берёт `.write()` и держит его весь цикл fill + клон векторов — [core.rs:632](rust-core/src/core.rs#L632).

## Acceptance
- [ ] Read-секция снапшотит нужное (текст/векторы за `Arc`) и **отпускает guard до** дорогого скана/скоринга — ИЛИ per-collection локи (скан лочит только коллекции в scope)
- [ ] `apply_job`: векторы **перемещаются** из job'а (`mem::take`/`into_iter`), а не клонируются под write-локом — [core.rs:679](rust-core/src/core.rs#L679)
- [ ] Время удержания write-лока в `apply_job` минимизировано (HashMap-построение вне горячего общего случая)
- [ ] Проверка: долгий grep больше не задерживает переход коллекции из `Building`
