# rcore — Rust search core (Stage 0 FFI skeleton)

`rcore` is the Rust core that will provide semantic + regex search for the 1C
MCP component. It compiles to a **staticlib** and is linked directly into the
C++ DLL (`libhttp1cWin64`). This Stage 0 deliverable is the **FFI skeleton
only**: the C ABI boundary, the memory-ownership convention, the CRT match, and
the process-global singleton. The ML pieces (`fastembed` / `ort` /
`tokenizers`) and the real `configure` / `index_segments` / `search` methods
arrive in **Stage 1**.

## Crate layout

```
rust-core/
├── Cargo.toml            # staticlib, pinned deps (serde, serde_json, once_cell)
├── .cargo/config.toml    # +crt-static for x86_64-pc-windows-msvc (matches /MT)
├── README.md             # this file
└── src/
    ├── lib.rs            # C ABI boundary + dispatch + unit tests
    ├── core.rs           # process-global singleton (Lazy<RwLock<Core>>)
    └── protocol.rs       # JSON success/error envelope types
```

## FFI contract

All data crosses the boundary as **JSON strings**. The surface is four
`extern "C"` functions:

| Function | Signature | Returns |
|----------|-----------|---------|
| `rcore_version` | `char* rcore_version(void)` | JSON `{"name","version","abi"}` |
| `rcore_dispatch` | `char* rcore_dispatch(const char* method, const char* payload_json)` | JSON envelope |
| `rcore_free_string` | `void rcore_free_string(char* s)` | — |
| `rcore_shutdown` | `void rcore_shutdown(void)` | — |

`rcore_dispatch` is the single generic entry point. Response envelope:

- Success: `{"ok": true, "result": <method-specific>}`
- Failure: `{"ok": false, "error": {"code": "...", "message": "..."}}`

Stub methods implemented in Stage 0:

- `ping` — echoes the payload: `{"ok":true,"result":{"pong":true,"echo":<payload>}}`.
- `stats` — reports singleton state: `configured`, `collections`, `callsHandled`.
- `reset` — clears mutable state; idempotent.

Any **unknown method** returns a *structural* error
(`{"ok":false,"error":{"code":"unknown_method",...}}`) — never a panic.
A malformed payload returns `code: "bad_payload"`. Panics are caught at the
boundary (`catch_unwind`) and surface as `code: "internal"`; they never unwind
across the C ABI (which would be UB).

Stage 1 adds `configure` / `index_segments` / `search` as new match arms in
`dispatch()`.

## Memory-ownership rule (critical)

> Every `char*` returned to C is allocated by **Rust** (`CString::into_raw`) and
> must be freed **only** by `rcore_free_string` (`CString::from_raw`).
> **Never** call C's `free`/`delete` on these pointers — that is a cross-CRT
> free and corrupts the heap.

- Input pointers (`method`, `payload_json`) are *borrowed*; Rust never frees them.
- `rcore_free_string(NULL)` is a safe no-op. Double-free is the caller's contract
  to avoid — the C++ `RustString` RAII wrapper (`http-1c-dll/src/RustCore.h`)
  enforces single-free on the C++ side.

## CRT match: `+crt-static` ⇄ `/MT`

The C++ DLL uses the **static** CRT (`/MT` Release, `/MTd` Debug; see
`http-1c-dll/CMakeLists.txt` lines ~62-68). The Rust staticlib **must** use the
static CRT too, or you get duplicate-CRT linker errors and heap-ownership UB.
This is configured in [`.cargo/config.toml`](.cargo/config.toml):

```toml
[target.x86_64-pc-windows-msvc]
rustflags = ["-C", "target-feature=+crt-static"]
```

When linking a Rust MSVC staticlib you must also provide the system import
libraries the Rust std runtime pulls in. CMake adds:
`ntdll userenv bcrypt advapi32 ws2_32 kernel32` (see `CMakeLists.txt`).

## Build

```sh
# from rust-core/
cargo build --release --target x86_64-pc-windows-msvc
# → target/x86_64-pc-windows-msvc/release/rcore.lib

# Debug:
cargo build --target x86_64-pc-windows-msvc

# Run the unit tests (round-trip + free path). Tests use the host target;
# +crt-static is scoped to *-msvc so it does not interfere on other hosts:
cargo test
```

CMake invokes `cargo build` automatically as part of the DLL build (custom
target `rcore_staticlib`) and links the produced `.lib`. The crate is not built
standalone for production — it is a link-time dependency of `http1c`.

## Status

Stage 0 skeleton. **Not yet build/link-verified in this environment** (no Rust /
Cargo / MSVC toolchain present). Verify with the commands above plus a full
CMake build of `http-1c-dll`.
