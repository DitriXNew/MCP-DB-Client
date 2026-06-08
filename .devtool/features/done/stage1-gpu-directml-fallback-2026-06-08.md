---
id: "stage1-gpu-directml-fallback-2026-06-08"
status: "done"
priority: "medium"
assignee: null
dueDate: null
created: "2026-06-08T22:55:43.000Z"
modified: "2026-06-08T23:09:25.000Z"
completedAt: "2026-06-08T23:09:25.000Z"
labels: ["stage-1", "rust-core"]
order: "aT"
---

# Stage 1 — GPU (DirectML) с фоллбэком на CPU

`FastEmbedder` сейчас всегда CPU (fastembed `execution_providers` по умолчанию пуст). Добавить опциональное GPU-ускорение через DirectML с авто-фоллбэком на CPU.

## Acceptance
- [ ] rust-core `fastembed` фича включает `fastembed/directml` + `dep:ort` (`=2.0.0-rc.12`) для типа `ort::ep::DirectML`
- [ ] `configure` принимает `device: cpu | dml | auto` (по умолчанию `auto` = DML с фоллбэком на CPU; либо `cpu`/`dml` явно)
- [ ] При `dml`/`auto`: `execution_providers = vec![DirectML::default().build()]` (best-effort → ort сам падает на CPU, если GPU/драйвера нет). fastembed при `has_directml` сам ставит `memory_pattern(false)` + `parallel_execution(false)`
- [ ] Не падать, если GPU недоступен — тихий фоллбэк на CPU
- [ ] Проверить: `cargo test --features fastembed` (DML-вариант) собирается и реально эмбеддит (на этой машине, скорее всего, WARP/CPU-фоллбэк — главное, что путь не крашит)
- [ ] DirectML.dll остаётся рантайм-зависимостью full-пакета (Windows-компонент)

Refs: §3 (GPU как опц. рычаг), §10 (GPU не обязателен), [[mcp-search-subsystem]].
