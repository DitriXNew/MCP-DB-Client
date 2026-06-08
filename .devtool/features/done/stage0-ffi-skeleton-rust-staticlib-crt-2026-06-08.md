---
id: "stage0-ffi-skeleton-rust-staticlib-crt-2026-06-08"
status: "done"
priority: "critical"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T18:21:53.000Z"
completedAt: "2026-06-08T18:21:53.000Z"
labels: ["stage-0", "rust-core", "cpp-glue", "blocker"]
order: "a0"
---

# Stage 0 — Каркас FFI: Rust staticlib + C ABI + CRT + синглтон

Самый рисковый инфраструктурный кусок — делать первым (§8, этап 0).

## Acceptance
- [ ] Скелет Rust-ядра, компилируется в **staticlib**
- [ ] Граница FFI: `extern "C"`, JSON in / JSON out (`char*` туда/обратно)
- [ ] Конвенция владения памятью: возвращаемые строки выделяет Rust, освобождает **только** `rust_free_string` (никакого кросс-CRT `free`)
- [ ] Rust собирается с **`+crt-static`** — совпадение с `/MT` (Release) / `/MTd` (Debug) из `CMakeLists.txt`
- [ ] Линковка Rust staticlib в MSVC-DLL `http1c`
- [ ] Процессный синглтон стора (`OnceCell`/lazy), общий для всех экземпляров `HttpServerComponent` (фабрика `AddComponent`), переживает переоткрытие формы, умирает с процессом
- [ ] Экспорт `rust_shutdown` (best-effort): stop-accept → cancel → join воркеров → разрешить выгрузку DLL. Повесить на `doStopListen`/закрытие формы, **НЕ** на `~HttpServerComponent` (синглтон жив пока жив процесс; реальный хазард — краш при выгрузке `onnxruntime.dll` во время `Run`)

Refs: §2, §4.4, §8 (этап 0), §12. Код: [HttpServerComponent.cpp](http-1c-dll/src/HttpServerComponent.cpp), [CMakeLists.txt:62-68](http-1c-dll/CMakeLists.txt#L62-L68).

## Implementation notes

Created (Stage 0 skeleton):
- `rust-core/Cargo.toml` — `crate-type = ["staticlib"]`, pinned deps `serde =1.0.219`, `serde_json =1.0.140`, `once_cell =1.21.3` (no fastembed/ort/tokenizers — deferred to Stage 1).
- `rust-core/.cargo/config.toml` — `target-feature=+crt-static` for `x86_64-pc-windows-msvc` (matches `/MT`/`/MTd`).
- `rust-core/src/lib.rs` — C ABI: `rcore_version`, `rcore_dispatch`, `rcore_free_string`, `rcore_shutdown`. Ownership convention documented at top; all entry points `catch_unwind`-guarded (panic-free boundary). 8 `#[test]`s incl. ping round-trip + free path.
- `rust-core/src/core.rs` — process-global singleton `static CORE: Lazy<RwLock<Core>>` (near-empty `Core`: `configured`, `collections`, `calls_handled`).
- `rust-core/src/protocol.rs` — `{"ok":true,"result":...}` / `{"ok":false,"error":{"code","message"}}` envelopes; structural errors (`unknown_method`, `bad_payload`, `internal`).
- `rust-core/README.md`, `rust-core/.gitignore`.
- `http-1c-dll/src/RustCore.h` — `extern "C"` decls + move-only `RustString` RAII wrapper (frees via `rcore_free_string`). NOT wired into routing (Stage 1 card).
- `http-1c-dll/CMakeLists.txt` — `rcore_staticlib` custom target invokes `cargo build`, links `rcore.lib` + system libs `ntdll userenv bcrypt advapi32 ws2_32 kernel32`. C++ source build config untouched.

Unverified (no Rust/Cargo/CMake/MSVC toolchain in this env): does not compile/link here. To verify: `cd rust-core && cargo test` (unit tests), `cargo build --release --target x86_64-pc-windows-msvc` (staticlib), then a full CMake/MSVC build of `http-1c-dll` to confirm link + system-lib list. Trim the system-lib list if the actual std footprint needs fewer.

## Review — accepted (2026-06-08)

Code прочитан построчно, все acceptance-критерии скелета выполнены: staticlib + минимальные пинованные deps; FFI `rcore_version/dispatch/free_string/shutdown` с `catch_unwind` (panic-free граница) и структурными ошибками `unknown_method`/`bad_payload`; конвенция владения (CString into_raw/from_raw, null-safe free) + move-only `RustString` RAII; `+crt-static`; синглтон `Lazy<RwLock<Core>>`; CMake-врезка под `if (NOT UNIX)` не ломает C++-сборку; 8 unit-тестов. Идиоматично, well-commented.

**Carried forward (НЕ блокеры скелета — всплывут на первой реальной сборке, это и есть смысл де-риска этапа 0):**
1. **Debug-CRT mismatch (существенно).** Rust `+crt-static` на MSVC всегда линкует **release** static CRT (`libcmt`) — у Rust нет `/MTd`-эквивалента. Значит C++ Debug-сборка (`/MTd` → `libcmtd`) + Rust staticlib (`libcmt`) даст конфликт CRT (LNK4098) / смешанные кучи. Критерий «`+crt-static` совпадает с `/MT` Release **и** `/MTd` Debug» неточен. Решение на первой сборке: форсить `/MT` для C++ и в Debug (или `/NODEFAULTLIB:libcmtd` + линк `libcmt`). Зафиксировано в памяти проекта.
2. **CMake не переинвокирует cargo при правке `.rs` (минор).** У `add_custom_command(OUTPUT ...)` нет `DEPENDS` на исходники Rust → при изменении `.rs` `.lib` считается актуальным и cargo не запускается. Харднуть в Stage 1: always-run cargo-таргет (cargo сам инкрементален) либо перечислить исходники в `DEPENDS`.

Build-верификация остаётся отложенной (нет тулчейна в этой среде).
