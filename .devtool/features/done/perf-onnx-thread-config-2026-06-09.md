---
id: "perf-onnx-thread-config-2026-06-09"
status: "done"
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
- [x] `query`-сессия: `min(2, ncpu)` intra-op потоков, устройство **CPU** (нет host↔device-копий DirectML на одиночном запросе); `bulk`: конфигурируемое устройство (GPU/Auto/CPU) + остаток ядер — `SessionTuning`/`tunings` в [fastembed_embedder.rs](../../rust-core/src/fastembed_embedder.rs); `config.intra_threads` проброшен из `build_embedder`
- [x] Суммарно `bulk + query ≤ ncpu`: query = `min(2,ncpu)`, bulk = явный `intra_threads` (capped `ncpu`) либо `ncpu - query` (≥1). Нет переподписки
- [~] (Опц.) batch_size в конфиг — НЕ делал, остаётся дефолт fastembed (256/`None`); отдельный замер 32/64/128 требует GPU-бокса
- [~] (Опц.) пул `query`-сессий round-robin — НЕ делал: одиночная query-сессия под `Mutex` достаточна, пока нет реальной конкуренции запросов (карта сама помечает «если есть конкуренция»)
- [~] Замер latency до/после — локально не мерял (нужен GPU-бокс; пользователь тестирует «завтра»). Сборка `cargo check --features fastembed` зелёная

## Done 2026-06-09
Две модельные сессии теперь разведены не только по локам, но и по CPU/устройству: `tunings(device, intra_threads)` отдаёт bulk (конфиг-устройство + большинство ядер) и query (CPU + 1–2 потока). `with_intra_threads` есть в fastembed 5.16 (`InitOptions`/`InitOptionsUserDefined`). `cargo check --features fastembed` ок, `cargo test` 86/86. Бенч и опциональные пункты (batch_size, пул query) — задокументированы как отложенные/для GPU-бокса.
