---
id: "perf-grep-keyword-cache-tokens-2026-06-09"
status: "done"
priority: "medium"
assignee: null
dueDate: null
created: "2026-06-09T12:00:00.000Z"
modified: "2026-06-09T12:00:00.000Z"
completedAt: null
labels: ["perf", "rust-core"]
order: "a3"
---

# Perf — не пересчитывать нормализацию/токены на каждый запрос (grep + keyword)

Структура текста сегмента не меняется после индексации, но grep и keyword-канал переделывают её на **каждый** запрос по **каждому** сегменту.

- grep: `normalize_newlines` делает два прохода с двумя аллокациями (`.replace("\r\n").replace('\r')`), потом split в `Vec<&str>` — [grep.rs:131](rust-core/src/grep.rs#L131), [grep.rs:198](rust-core/src/grep.rs#L198). Для `index_raw`-доков нормализованный `full_text` и таблица офсетов **уже** лежат в `Document` ([core.rs:919](rust-core/src/core.rs#L919), [core.rs:922](rust-core/src/core.rs#L922)) — grep их игнорирует.
- grep на каждый матч клонирует line + `2*context_lines` строк + `doc_id/name/collection` (одинаковые в доке), затем клонирует это ещё раз в `Value` — [grep.rs:141](rust-core/src/grep.rs#L141), [grep.rs:206](rust-core/src/grep.rs#L206).
- keyword: `keyword_tokens` ре-токенизирует текст каждого сегмента с аллокацией `Vec<String>` на запрос — [core.rs:1259](rust-core/src/core.rs#L1259), [core.rs:1226](rust-core/src/core.rs#L1226).

## Acceptance
- [x] При LF-only тексте `normalize_newlines` **заимствует** вход (`Cow::Borrowed`) — один проход-проверка на `\r` перед двойным `replace`; CRLF-ветка только при наличии `\r`. Каждый `index_raw`-сегмент уже LF-only → ноль аллокаций. [grep.rs](../../rust-core/src/grep.rs)
- [x] Keyword: токен-мультимножество (`kw_counts: HashMap<String,u32>`) кэшируется на `Segment` при индексации (`token_multiset`, тот же токенайзер, что и у запроса); `keyword_score` = `O(query_terms)` hash-lookup'ов, без `Vec<String>`/ре-токенизации на запрос
- [~] «таблица строк один раз при accept» — отдельную пер-сегментную нормализованную таблицу строк НЕ кэшировал: `Cow`-fast-path уже даёт ноль аллокаций для LF-сегментов (общий случай), а кэш ещё одной копии строк раздул бы память без выигрыша сверх этого
- [~] «grep сериализует hit'ы заимствуя `&str`; per-doc поля из цикла» — **отложено**: `GrepHit` владеет `String`'ами и клонирует `doc_id/name/collection` **на матч** (не на скан-строку, ограничено `max_matches`); заимствование потребовало бы лайфтайма на публичный тип результата — несоразмерный риск ради клонов, ограниченных кепом

## Done 2026-06-09
Главная пер-запросная трата keyword-канала (ре-токенизация каждого сегмента + аллокация `Vec<String>`) убрана кэшем мультимножества на `Segment`, заполняемым в обоих ingest-путях (`accept_index`, `index_raw`). grep больше не переаллоцирует LF-only текст. 86/86 `cargo test` (добавлен `token_multiset_counts_occurrences`, тест нормализации проверяет Borrowed/Owned ветки). Оставшийся zero-copy-writer для grep — отдельная, более рискованная задача; задокументирован как осознанно отложенный.
