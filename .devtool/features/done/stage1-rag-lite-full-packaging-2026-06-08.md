---
id: "stage1-rag-lite-full-packaging-2026-06-08"
status: "done"
priority: "high"
assignee: null
dueDate: null
created: "2026-06-08T22:55:43.000Z"
modified: "2026-06-08T23:33:19.000Z"
completedAt: "2026-06-08T23:33:19.000Z"
labels: ["stage-1", "cpp-glue", "infra"]
order: "aU"
---

# Stage 1 — Две версии компоненты (lite / full с RAG) через рантайм-загрузку rcore.dll

Решение по §11.2: C++ компонента не может быть `/MD` (C2491 на char16_t-стримах), поэтому `rcore` для RAG — отдельный **`rcore.dll`** (cdylib, `/MD`, статический onnxruntime). Вместо линк-тайма — **рантайм `LoadLibrary`** → один и тот же `libhttp1cWin.dll`, два пакета.

## Acceptance
- [ ] `RustCore.h`: extern "C" линк-тайм → тонкий лоадер на `LoadLibrary("rcore.dll")` + `GetProcAddress` (`rcore_version/dispatch/free_string/shutdown`)
- [ ] Компонента (`libhttp1cWin.dll`) остаётся `/MT`, **больше не линкует** staticlib `rcore`
- [ ] Натив-роутинг в `tools/call`: rcore.dll загружен → реальный RAG; не загружен/несовместимая версия → структурная ошибка «это lite-версия компоненты, установите RAG-пакет»
- [ ] Версия-чек через `rcore_version` (ABI) → ловить и «нет dll», и «старая dll»
- [ ] MockEmbedder становится **только тестовым** (`cargo test`); в проде rcore.dll = реальный fastembed
- [ ] CMake: собирать компоненту (`/MT`, без rcore-линка) + отдельно `rcore.dll` (cdylib, `--features fastembed`, `/MD`); опция `RCORE_FASTEMBED`/таргет под rcore.dll
- [ ] release.yml/build-скрипты: пакет **lite** (`libhttp1cWin.dll`) и **full** (`+ rcore.dll + DirectML.dll`)
- [ ] Обновить README (две версии, search-подсистема, GPU/CPU, сборка/пакеты)
- [ ] Смоук: lite → «install RAG»; full → грузит rcore.dll + реально ищет

Refs: §4.4, §11.2, [[rust-msvc-crt-static-debug-gotcha]], [[mcp-search-subsystem]].
