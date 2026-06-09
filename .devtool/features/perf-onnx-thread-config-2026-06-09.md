---
id: "perf-onnx-thread-config-2026-06-09"
status: "todo"
priority: "medium"
assignee: null
dueDate: null
created: "2026-06-09T12:00:00.000Z"
modified: "2026-06-09T12:00:00.000Z"
completedAt: null
labels: ["perf", "rust-core"]
order: "a4"
---

# Perf — настроить число ONNX-потоков и устройство по сессиям (query vs bulk)

Два инстанса модели (`bulk`/`query`) разводят reindex и запросы по **локам**, но не по **CPU**: число intra-op потоков ONNX не задаётся → каждая сессия поднимает пул на все ядра, и во время reindex+query они дерутся за ядра. Гарантия «запросы не блокируются reindex» верна только для лока.

- Сессии строятся без конфигурации потоков — [fastembed_embedder.rs:169](rust-core/src/fastembed_embedder.rs#L169), [fastembed_embedder.rs:181](rust-core/src/fastembed_embedder.rs#L181).
- `query`-путь сериализуется одним `Mutex` — два параллельных запроса не эмбедятся одновременно ([fastembed_embedder.rs:238](rust-core/src/fastembed_embedder.rs#L238)).
- DirectML на одиночном коротком запросе часто проигрывает CPU из-за host↔device-копий — [fastembed_embedder.rs:80](rust-core/src/fastembed_embedder.rs#L80).

## Acceptance
- [ ] `query`-сессия: малое intra-op (1–2), устройство CPU; `bulk`: GPU/больше потоков
- [ ] Суммарно `bulk` + `query` потоки ≤ числу ядер (нет переподписки)
- [ ] (Опц.) batch_size для `embed_passages` вынесен в конфиг и измерен (32/64/128) вместо дефолта 256
- [ ] (Опц., если есть конкуренция запросов) пул `query`-сессий с round-robin
- [ ] Замер query-latency во время reindex до/после
