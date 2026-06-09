---
id: "perf-ffi-typed-serde-no-value-dom-2026-06-09"
status: "todo"
priority: "high"
assignee: null
dueDate: null
created: "2026-06-09T12:00:00.000Z"
modified: "2026-06-09T12:00:00.000Z"
completedAt: null
labels: ["perf", "rust-core"]
order: "a2"
---

# Perf — убрать serde_json::Value-DOM round-trip на FFI-границе

Один search-вызов прогоняет payload через ~6 копий + 2 JSON-парса + 2 сериализации + 2 транскода UTF-8↔UTF-16. Самая дешёвая и эффектная часть — Rust-сторона: сейчас вход парсится в `Value`-DOM, ответ строится как `Value`-DOM, затем `to_string`, затем `CString::new` (ещё O(len) копия + скан на NUL).

- Вход: `serde_json::from_str` в `Value` + ручной `opt_str`/`get` — [lib.rs:120](rust-core/src/lib.rs#L120), [lib.rs:418](rust-core/src/lib.rs#L418).
- Выход: построение `Value` → `to_string` — [lib.rs:409](rust-core/src/lib.rs#L409), [protocol.rs:66](rust-core/src/protocol.rs#L66), [lib.rs:312](rust-core/src/lib.rs#L312).
- `CString::new` копирует + скан — [lib.rs:69](rust-core/src/lib.rs#L69).

## Acceptance
- [ ] Запросы парсятся в типизированные структуры: `serde_json::from_str::<TypedRequest>` вместо `Value` + ручной разбор
- [ ] Ответы сериализуются типизированными структурами через `serde_json::to_writer` (в `String`/`Vec<u8>`), без промежуточного `Value`-дерева
- [ ] Для no-arg методов (`stats`/`reset`/`ping`) не строить `Value`-DOM
- [ ] (Опц.) FFI-вариант возврата `(ptr,len)` из готового `Vec<u8>` через `into_raw`, чтобы убрать копию+скан `CString::new`
- [ ] Бенч до/после на крупном ответе (`k=50`, `include_text:true`)
