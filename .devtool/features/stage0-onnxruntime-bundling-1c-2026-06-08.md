---
id: "stage0-onnxruntime-bundling-1c-2026-06-08"
status: "todo"
priority: "critical"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T18:13:44.000Z"
completedAt: null
labels: ["stage-0", "infra", "investigation", "blocker"]
order: "a1"
---

# Stage 0 — Бандлинг onnxruntime.dll в 1С (откр. вопрос §11.2)

Инфраструктурный блокер этапа 0. Эмпирическая проверка, не вычитывается на бумаге.

## Acceptance
- [ ] Определить способ доставки `onnxruntime.dll` (+ провайдеров): через `Template.bin` 1С-add-in **или** из папки рядом с EPF
- [ ] Подтвердить, что 1С-загрузчик находит нативную DLL рядом с компонентой
- [ ] **Не** автоскачивать — контролируемый бандлинг
- [ ] Проверить разрядность: целевой клиент **x64**

Refs: §3 (C++ сторона), §11.2, §12.

## Findings (от реального эмбеддера, 2026-06-08)
Реальный fastembed/ort **собрался и заработал локально** (dim 384, ru/uk ок). Но вскрылись два конкретных блокера для **fastembed-сборки DLL**:
1. **CRT: ort требует `/MD` (динамический CRT).** Префиб onnxruntime у ort собран под `/MD`; наш `+crt-static` (под C++ `/MT`) даёт ~66 unresolved `__imp_*` (strtod, log1pf, …). Тест-бинарь линкуется только с `RUSTFLAGS=-C target-feature=-crt-static`. **Для production-DLL с fastembed → переводить ОБЕ стороны на `/MD`** (C++ CMake `/MD[d]` + Rust без `+crt-static`), либо собирать onnxruntime из исходников под `/MT` (тяжело). См. [[rust-msvc-crt-static-debug-gotcha]].
2. **onnxruntime.dll в рантайме.** ort по умолчанию грузит `onnxruntime.dll` (download-binaries кладёт её в `target`); production-пакет должен **класть `onnxruntime.dll` рядом с компонентой** или настроить ort load-strategy. Это и есть исходный вопрос карточки (Template.bin vs папка рядом с EPF).

Текущая mock-сборка DLL остаётся `/MT` и не затронута (fastembed — feature-gated, default off). Карточка остаётся открытой: сделать **build-вариант** «fastembed-DLL на `/MD` + бандлинг onnxruntime.dll», затем 1С-упаковка.
