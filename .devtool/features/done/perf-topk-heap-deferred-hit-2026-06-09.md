---
id: "perf-topk-heap-deferred-hit-2026-06-09"
status: "done"
priority: "high"
assignee: null
dueDate: null
created: "2026-06-09T12:00:00.000Z"
modified: "2026-06-09T12:00:00.000Z"
completedAt: null
labels: ["perf", "rust-core"]
order: "a0"
---

# Perf — top-k heap вместо full-sort + материализация Hit на каждый сегмент

Самый выгодный по соотношению выигрыш/риск фикс dense/keyword-поиска. Сегодня на **каждый** просканированный сегмент строится полный `Hit` (deep-clone `doc.meta` + клон `seg.text`), затем весь набор сортируется целиком, и `limit_hits` оставляет только `k`.

Проблема — три источника лишней работы на запрос:
- `make_hit` клонирует `doc.meta` (serde_json дерево) и `seg.text` на каждый сегмент, прошедший фильтр — [core.rs:1287](rust-core/src/core.rs#L1287), вызовы из dense [core.rs:1352](rust-core/src/core.rs#L1352) и keyword [core.rs:1394](rust-core/src/core.rs#L1394).
- `include_text:false` ([core.rs:1192](rust-core/src/core.rs#L1192)) **игнорируется** — текст клонируется всегда.
- Полная сортировка `O(N log N)` вместо top-k селекции — [core.rs:1356](rust-core/src/core.rs#L1356), [core.rs:1398](rust-core/src/core.rs#L1398), [core.rs:1445](rust-core/src/core.rs#L1445).

## Acceptance
- [x] Скоринг идёт в лёгкий список без построения `Hit` — новый `Scored<'a>` (refs, ноль аллокаций/сегмент)
- [x] Top-k через `select_nth_unstable_by` (O(N+k·log k)), без полной сортировки в общем пути (`max_per_doc=None`) — `top_k_scored`
- [x] `Hit` строится только для `k` выживших — `finalize` → `make_hit`
- [x] `include_text == false` → клонируется только `PREVIEW_CHARS`-префикс, не весь текст
- [x] Учтён `max_per_doc` — ранжируем лёгкие refs и применяем per-doc cap
- [~] Микробенч — отдельный bench не добавлял; поведение покрыто тестами (`top_k_selection_is_descending_and_bounded`, `include_text_false_caps_hit_text_to_preview`); 85/85 `cargo test` зелёные

## Done 2026-06-09
Весь ранкинг-путь `search` переписан на лёгкий `Scored<'a>`: каналы возвращают refs без сорта, `rrf_fuse` сортирует только refs, единственная материализация `Hit` — в `finalize` для ≤k выживших. Убраны `make_hit`-на-каждый-сегмент, полный `sort_desc` набора и безусловный clone текста. Заодно (бакет из perf-ann) `dot` переписан на 8-lane chunked-аккумулятор (авто-векторизация LLVM). `apply_job` **перемещает** вектор из job'а (`mem::take`) вместо clone под write-локом.
