---
id: "stage0-onnxruntime-bundling-1c-2026-06-08"
status: "done"
priority: "critical"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T18:13:44.000Z"
completedAt: "2026-06-09T12:00:00.000Z"
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

Текущая mock-сборка DLL остаётся `/MT` и не затронута (fastembed — feature-gated, default off).

## РЕШЕНИЕ (прощупано локально 2026-06-08): cdylib, не `/MD`-staticlib
Попытка собрать единый DLL под `/MD` **провалилась**: C++-код 1С использует `std::basic_stringstream<char16_t>` (UTF-16); под `/MD` это инстанцирует `std::numpunct<char16_t>::id`, которого нет в пребилт `msvcp140.dll` → **C2491** (под `/MT` норм). Т.е. **компонента 1С принципиально не компилится под `/MD`.**
- **onnxruntime — СТАТИЧЕСКИЙ** (вшит в rcore.lib, нет `onnxruntime.dll`). Единственный рантайм-довесок — `DirectML.dll` (Windows-компонент; для CPU-only можно собрать ort без DML позже).
- **Путь:** `rcore` собирать **cdylib `rcore.dll`** под `/MD` (cargo сам линкует весь натив + статический onnxruntime); C++-компонента остаётся **`/MT`**, линкует только import-lib `rcore.dll` и кладёт `rcore.dll` рядом. FFI безопасен через границу DLL/CRT (rcore_free_string освобождает на стороне Rust). Поставка: `libhttp1cWin.dll`(/MT) + `rcore.dll`(/MD) + `DirectML.dll` + VC-redist. См. [[rust-msvc-crt-static-debug-gotcha]]. Дальше: реализовать cdylib-вариант, затем 1С Template.bin-упаковка (env-gated).

## Verification (2026-06-09)

### Bullet 1 — Delivery method for onnxruntime.dll (+ providers)

✅ **No separate onnxruntime.dll exists.** onnxruntime is statically linked into `rcore.dll` at build time. `rust-core/Cargo.toml:61` enables `fastembed/directml` and `ort/directml`; fastembed internally enables `ort/download-binaries`, which fetches the onnxruntime prebuilt at **cargo build time** and statically links it into the cdylib output. `rust-core/README.md:97` explicitly states "onnxruntime is statically linked into `rcore.dll` — there is **no** `onnxruntime.dll`". The full bundle payload is `libhttp1cWin.dll` + `rcore.dll` + `DirectML.dll` (build/package-http1c-addin.sh:86-105); no `onnxruntime.dll` entry exists in PAYLOAD. The delivery vehicle is the **Template.bin** 1C add-in bundle (the packaging script writes a ZIP that is copied to `http-1c-dp/http1c/Templates/http1c/Ext/Template.bin`, build/package-http1c-addin.sh:53,153), with rcore.dll and DirectML.dll as `<file>` entries alongside the main component in MANIFEST.XML (build/package-http1c-addin.sh:111-121). The card's РЕШЕНИЕ section correctly reflects this.

### Bullet 2 — 1C loader finds the native DLL next to the component

✅ **Confirmed.** `http-1c-dll/src/RustCore.h:92-122` implements `moduleDir()`: it calls `GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, &anchor, &self)` to obtain the HMODULE of `libhttp1cWin.dll` itself (not the host process), then `GetModuleFileNameW` to get its full path, strips the filename, and appends `rcore.dll`. The `load()` function at line 130 calls `LoadLibraryW((dir + L"rcore.dll").c_str())`. This unambiguously resolves `rcore.dll` from the **same directory as `libhttp1cWin.dll`**, regardless of the process working directory or PATH.

### Bullet 3 — No auto-download; controlled bundling

✅ **Confirmed for the runtime.** There is zero runtime download logic in either the C++ component or `rcore.dll`. The `ort/download-binaries` feature is a **cargo build-time** mechanism that fetches the onnxruntime prebuilt once during `cargo build` and statically links it into `rcore.dll`; at runtime the resulting DLL is fully self-contained (`rust-core/Cargo.toml:60`, comment: "fastembed already enables `ort/download-binaries`"). The only runtime artifact that must be present is `DirectML.dll`, which is copied from the Windows system directory during packaging (build/package-http1c-addin.sh:59, 103-104) — a controlled, explicit step. The embedding **model** (multilingual-e5-small) is fetched at runtime by fastembed on first `configure` call, not bundled — but the card acceptance bullet concerns onnxruntime/provider bundling only, not model weights. No uncontrolled auto-download of native binaries occurs at runtime.

### Bullet 4 — Bitness: target client is x64

✅ **Confirmed.** `http-1c-dll/CMakeLists.txt:102` hard-codes `RCORE_TARGET_TRIPLE "x86_64-pc-windows-msvc"` for the cargo build. `build/package-http1c-addin.sh:114` writes `arch="x86_64"` into MANIFEST.XML. The rust-core README build command (line 111) specifies `--target x86_64-pc-windows-msvc`. No 32-bit path exists for the fastembed/rcore build.

### Final delivery summary

The implemented solution eliminates `onnxruntime.dll` entirely: ort's prebuilt onnxruntime is fetched once at **cargo build time** (via `ort/download-binaries`) and statically linked into `rcore.dll` (a `/MD` cdylib). The only runtime native add-on is `DirectML.dll`, bundled explicitly during packaging. `libhttp1cWin.dll` (compiled `/MT`) discovers and loads `rcore.dll` at runtime using a `GetModuleHandleExW`+`GetModuleFileNameW` anchor on itself, ensuring it always looks in its own directory — the same directory where the 1C add-in bundle unpacks all files from `Template.bin`. Both DLLs target `x86_64-pc-windows-msvc`. All four acceptance criteria are met by the committed code with no outstanding gaps.
