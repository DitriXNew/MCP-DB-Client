# C++ Transport Thread-Safety Audit — `HttpServerComponent`

**Card:** `stage0-cpp-thread-safety-review-2026-06-08`
**Scope:** Audit every piece of shared mutable state in `http-1c-dll/src/HttpServerComponent.{cpp,h}`
for correct synchronization now that a **synchronous native search path** (Rust core via
`rcore_dispatch`) runs on httplib worker threads alongside the existing 1C-forwarded
(`ExtEvent` + `pendingRequests`) path and the 1C-thread native AddFunction/AddProperty methods.

## Where the audited code lives (read this first)

The native search path (`isNativeTool` / `nativeToolDefinitions` / `dispatchNativeTool`, the
`tools/call` native branch, the `tools/list` merge, `RustCore.h`, the Rust `rust-core/` crate)
exists on branch **`feat/search-core`**, not on the `master` base this worktree was cut from. The
audit was performed against the `feat/search-core` versions of:

- `http-1c-dll/src/HttpServerComponent.cpp` (1599 lines on that branch)
- `http-1c-dll/src/HttpServerComponent.h` (unchanged from `master`)
- `http-1c-dll/src/RustCore.h`
- `rust-core/src/lib.rs`, `rust-core/src/core.rs`

All `HttpServerComponent.cpp:NNN` line citations below are **line numbers in the
`feat/search-core` version** of that file. Logic shared with `master` (sessions, caches,
pending map, SSE, logging, auth, rate limiter) is identical on both branches; only the native
additions differ.

## Threads involved

1. **httplib worker threads** — `cpp-httplib` serves `/mcp`, `/health`, legacy routes on a
   thread pool, so multiple MCP requests (and the native search path) run **concurrently**.
2. **1C main thread** — calls the native AddFunction/AddProcedure/AddProperty entry points:
   `SendResponse`, `SendProgress`, `RegisterTools/Resources/Prompts`, `SetAuthToken`,
   `LoggingEnabled`, `LogPath`, `StartListen`, `StopListen`, `Status`, `Timeout`.
3. **The httplib listener thread** (`serverThread`) — runs `server->listen(...)`.

The new native search path executes **entirely on a httplib worker thread** (inside
`handleMcpRequest`). It does not spawn threads and does not touch the 1C thread.

---

## Verdict per shared-state item

| Shared state | Mutex / guard | Threads | Verdict |
|---|---|---|---|
| `sessions` | `sessionMutex` | workers + 1C (`Status`) | **Safe** |
| `cachedToolsJson` | `toolsMutex` | workers + 1C (`RegisterTools`, `Status`) | **Safe** |
| `cachedResourcesJson` | `resourcesMutex` | workers + 1C | **Safe** |
| `cachedPromptsJson` | `promptsMutex` | workers + 1C | **Safe** |
| `pendingRequests` + `requestCounter` | `pendingMutex` | workers + 1C (`SendResponse`/`SendProgress`) | **Safe** (native path does not touch it) |
| `PendingRequest` internals | `PendingRequest::mtx` + `atomic ready` | producer (1C) + consumer (worker) | **Safe** |
| `sseStreams` | `sseStreamsMutex` (+ per-stream `mtx`) | workers + 1C (broadcast) | **Safe** |
| Logging (`g_logPath`, `g_loggingEnabled`) | `g_loggingMutex` | all | **Safe** |
| Instance logging (`logPath`, `loggingEnabled`) | `loggingMutex` (+ see note) | workers + 1C | **Safe** with one benign data race on `loggingEnabled` (pre-existing) |
| `authToken` | **none** | workers (read) + 1C (write) | **Benign data race** (pre-existing, not introduced by native path) |
| `rateLimiter` | internal `RateLimiter::mtx` | workers | **Safe** |
| `running` / `listenPort` / `timeout` | `atomic` / plain int | workers + 1C | `running` safe; `listenPort`/`timeout` benign (pre-existing) |
| Native helpers (`isNativeTool`, `nativeToolDefinitions`, `dispatchNativeTool`, `metaFiltersSchema`) | — (stateless) | workers | **Safe** (reentrant, no shared state) |
| Rust core singleton (`CORE`) | `RwLock<Core>` **inside Rust** | workers (search) + 1C (ingest/configure, once wired) | **Safe** (locking owned by Rust) |

**Bottom line: the native search path introduces no new C++ data races.** It is a stateless,
synchronous pass-through to a Rust core that does its own locking. The only races in the file are
two **pre-existing, benign** unsynchronized reads of config strings/ints (`authToken`,
`loggingEnabled`, `timeout`, `listenPort`) that predate this work and are not material. No C++
code changes were required; details and reasoning follow.

---

## Detailed analysis

### 1. The native `tools/call` branch is correctly isolated

`HttpServerComponent.cpp:1071-1094`. When `isNativeTool(toolName)` is true the handler:

- builds the result via `dispatchNativeTool(toolName, toolArgs)` wrapped in `try/catch (...)`,
- assembles a local `rpcResp`, calls `res.set_content(...)`, and **`return`s at line 1093**.

The `pendingRequests` / `requestCounter` / `pendingMutex` machinery only begins at
`HttpServerComponent.cpp:1096` (`std::string reqId; auto pending = std::make_shared<...>();` then
`std::lock_guard<std::mutex> lock(pendingMutex); reqId = "req_" + std::to_string(++requestCounter);`).
Because the native branch returns before reaching that code, **the native path never inserts into
`pendingRequests`, never increments `requestCounter`, and never acquires `pendingMutex`.**
Verified: there is no path where a native tool call and `SendResponse`/`SendProgress` from 1C can
collide over the same `reqId` (no `reqId` is ever created for a native call). **Verdict: safe.**

### 2. Native helpers are stateless / reentrant

All four helpers live in the file-local anonymous namespace (opens
`HttpServerComponent.cpp:24`, closes line 420), so they have internal linkage and are plain free
functions:

- `isNativeTool` (`:189-193`) — three string compares on its parameter; pure.
- `metaFiltersSchema` (`:197-217`) — constructs and returns a fresh `json` from literals; pure.
- `nativeToolDefinitions` (`:221-351`) — builds three fresh `json` objects from literals each call;
  no statics, no globals; pure.
- `dispatchNativeTool` (`:359-402`) — operates only on its parameters and stack locals
  (`argsJson`, `raw`, `envelopeStr`, `envelope`, `text`/`errText`); no shared state touched.

None reads or writes any member of `HttpServerComponent` or any global. Concurrent invocation from
multiple worker threads is therefore data-race-free on the C++ side. **Verdict: safe / reentrant.**

### 3. `cachedToolsJson` read under `toolsMutex`; merge copies, holds no references

`HttpServerComponent.cpp:1021-1035`:

```cpp
json tools;
{
    std::lock_guard<std::mutex> lock(toolsMutex);
    tools = json::parse(cachedToolsJson, nullptr, false);   // deep copy out of shared string
    if (tools.is_discarded()) tools = json::array();
}
if (!tools.is_array()) tools = json::array();
for (auto& def : nativeToolDefinitions()) {                  // lock NOT held here
    tools.push_back(std::move(def));
}
```

- `cachedToolsJson` is read only while holding `toolsMutex`. `json::parse` produces an
  **independent value** (`tools`); the lock is released before the merge.
- The merge appends into the local `tools` copy. It does **not** retain a pointer/reference into
  `cachedToolsJson` or any shared object — `nativeToolDefinitions()` returns a temporary built from
  literals, and `std::move(def)` moves out of that temporary's elements (the temporary outlives the
  range-`for`, so this is well-defined and merely an optimization).
- The writer side, `doRegisterTools` (`HttpServerComponent.cpp` `:cachedToolsJson = utf8;` under
  `toolsMutex`), is fully serialized against this read. **Verdict: safe.**

`cachedResourcesJson` / `cachedPromptsJson` follow the identical, correct pattern under
`resourcesMutex` / `promptsMutex`. The `Status` property also reads all three under their
respective mutexes. **Verdict: safe.**

### 4. `rcore_dispatch` is called with NO C++ lock held; `RustString` lifetime is correct

`HttpServerComponent.cpp:367`:

```cpp
RustString raw = RustString::adopt(rcore_dispatch(toolName.c_str(), argsJson.c_str()));
std::string envelopeStr = raw.str();   // copy out
```

- `dispatchNativeTool` acquires **no** `HttpServerComponent` mutex anywhere; `toolsMutex` is not
  taken in the `tools/call` branch at all. So `rcore_dispatch` is called outside every C++ lock.
  This is the desired property: a slow search cannot stall `tools/list`, `Status`, or any other
  worker waiting on `toolsMutex`/`pendingMutex`.
- **Lifetime / ownership** (contract in `RustCore.h` and `rust-core/src/lib.rs`): `rcore_dispatch`
  returns a Rust-allocated `char*` that must be freed exactly once via `rcore_free_string`.
  `RustString::adopt` takes sole ownership; `RustString` is move-only (copy ctor/assign `= delete`),
  so no accidental double-adopt. `.str()` copies the bytes into `envelopeStr`; the original buffer
  is freed by `~RustString` (calling `rcore_free_string`) when `raw` leaves scope at the end of
  `dispatchNativeTool`. Freed **exactly once**, on the **Rust allocator** (no cross-CRT free).
  `rcore_dispatch` is documented to never return null and never panic across the boundary
  (`rcore_free_string(nullptr)` is a safe no-op regardless). **Verdict: safe.**

### 5. The Rust core is the synchronization point for ingest-vs-search

`rust-core/src/core.rs:268`: `pub static CORE: Lazy<RwLock<Core>> = ...`. Every FFI method
(`search`, `grep`, `get_segment`, `stats`, `configure`, `index_segments`, `reset`) takes
`CORE.read()` or `CORE.write()` inside `rcore_dispatch`. Concurrent searches take read locks;
configure/ingest take write locks; the `RwLock` serializes them. Each `extern "C"` entry is wrapped
in `catch_unwind` (`guard(...)`), so a Rust panic becomes a structured error envelope rather than UB
across the ABI. Therefore the C++ side genuinely is a **thin pass-through**: the
search-vs-ingest concurrency question is answered entirely inside Rust, and the C++ glue neither
needs nor adds a lock for it. **Verdict: safe** (correctness of the Rust locking itself is out of
scope for this C++ audit, but the boundary contract — single-writer/multi-reader, panic-guarded,
single-free — is upheld on the C++ side).

### 6. `pendingRequests` + `requestCounter` (existing forwarded path)

`pendingMutex` guards both the map and the counter at every site: legacy handler
(`reqId`/insert/erase), `tools/call` non-native branch (`:1099`), `resources/read`, `prompts/get`,
`doSendResponse`, `doSendProgress`, `doStopListen`, `Status`, `/health`, and the chunked-stream
release callbacks. The shared `PendingRequest` object retrieved under `pendingMutex` is then handed
off; its own fields are guarded by `PendingRequest::mtx` + the `std::atomic<bool> ready`, with the
producer (1C thread via `SendResponse`/`SendProgress`) and the consumer (worker in `wait_for`)
synchronizing through the condition variable. This is correct and unchanged by the native work.
**Verdict: safe.**

### 7. Sessions, SSE streams, logging, rate limiter

- **`sessions` / `sessionMutex`** — `createSession`, `findSession`, `handleMcpDelete`,
  `doStopListen`, `Status` all lock `sessionMutex` around every access. `findSession` also mutates
  `lastActivity` under the lock. **Safe.**
- **`sseStreams` / `sseStreamsMutex`** — registration (`handleMcpGet`), removal (stream-release
  callback and `doStopListen`), and `broadcastNotification` all hold `sseStreamsMutex`; per-stream
  `messages` are additionally guarded by `SseStream::mtx`, with `closed` atomic. **Safe.**
- **Global logging** — `logToFile`, the `LoggingEnabled`/`LogPath` setters, and `Status` lock
  `g_loggingMutex` for `g_logPath`/`g_loggingEnabled`. `logToFile` copies the path under the lock,
  then writes the file outside it. **Safe.**
- **`rateLimiter`** — `RateLimiter::allow()` locks its own `mtx` for the whole token computation
  (`HttpServerComponent.h:42-50`). Concurrent `checkRateLimit` calls are serialized. **Safe.**

---

## Pre-existing, non-material races (not introduced by this card; not fixed)

These predate the native path. They are unsynchronized reads/writes of small scalar/string config
values. They are flagged for honesty but are **not** material enough to warrant a fix in this
audit, and fixing them is out of this card's "minimal C++ change" mandate since the native path
neither created nor worsened them.

1. **`authToken`** (`HttpServerComponent.h:120`). Read on worker threads in `validateAuth`
   (`authToken.empty()`, `auth.substr(7) == authToken`) and in `Status` (`!authToken.empty()`);
   written on the 1C thread in `doSetAuthToken` (`authToken = utf8;`) with **no mutex**. A
   concurrent `std::string` assignment vs. read is a formal data race (UB), though in practice the
   token is set once at startup before traffic. *Latent risk:* if a future feature rotates the token
   at runtime while serving, this becomes a real bug. **Minimal future fix:** a dedicated
   `authMutex` (or `std::shared_mutex`), or store the token snapshot in an
   `std::shared_ptr<const std::string>` swapped atomically.

2. **`loggingEnabled`** (instance field, `HttpServerComponent.h:130`). The `LoggingEnabled` getter
   reads it and the setter writes it without `loggingMutex` (only the mirrored *global*
   `g_loggingEnabled` is locked); `Status` reads it under `loggingMutex` but the property accessor
   does not. Benign `bool` race.

3. **`timeout`** (`HttpServerComponent.h:65`, plain `int`) and **`listenPort`** — written by 1C
   properties / `doStartListen`, read on workers (`wait_for(seconds(timeout))`, `Status`,
   `/health`) without synchronization. Benign torn-read territory for an `int`; values are
   effectively configured once.

(`running` is already `std::atomic<bool>`, so it is fine.)

---

## Latent risks for future stages

1. **Native tools must stay off `pendingRequests` / `SendProgress`.** The current safety argument
   for the native branch rests on it returning synchronously at `HttpServerComponent.cpp:1093`
   *before* any `pendingMutex`/`requestCounter` use. If a future stage makes native search
   long-running and wants streaming progress, do **not** reuse the 1C `PendingRequest` +
   `SendProgress` plumbing (that channel is driven by the 1C thread via `reqId`, and native calls
   have no `reqId`). Instead either (a) stream directly from the worker thread inside the native
   branch using `set_chunked_content_provider` with a Rust-side progress callback, or (b) if the
   `PendingRequest` machinery is reused, allocate a `reqId` under `pendingMutex` and ensure the
   completion/erase paths cannot race a 1C `SendResponse` for a *different* request. Re-audit at
   that point.

2. **1C-thread ingest/configure vs. concurrent worker search.** Once `configure` /
   `index_segments` are wired to 1C AddFunction methods, they will call `rcore_dispatch` from the
   **1C thread** while worker threads call `search`/`grep` concurrently. Correctness then depends
   **entirely on the Rust `RwLock<Core>`** (`rust-core/src/core.rs:268`) — the C++ side adds no
   lock and must not, or it would serialize unrelated MCP requests. The risk to watch is on the
   Rust side: a `configure` that swaps `dim` and resets the index while a `search` read is in
   flight must be handled by the write lock (it is, today). No C++ action needed; noted so a future
   reviewer does not "helpfully" add a C++ mutex around `rcore_dispatch` and reintroduce
   head-of-line blocking.

3. **`rcore_shutdown` wiring.** `RustCore.h` documents that `rcore_shutdown()` should be hooked onto
   `doStopListen()` / form-close, **not** `~HttpServerComponent` (the Rust singleton outlives any
   one component instance). As of `feat/search-core`, `doStopListen` does not yet call
   `rcore_shutdown`. This is a lifecycle/teardown concern (background-worker join on DLL unload),
   not a data race, but it is the natural place to wire it in a later stage. Calling it from the
   destructor instead would be a latent bug.

4. **Config-string races (above) under runtime reconfiguration.** If any of `authToken` /
   `timeout` / logging fields become runtime-mutable under live traffic (e.g. token rotation, hot
   re-config), promote them from "benign" to "must-fix" and add the corresponding mutex/atomic.

---

## Build

No C++ source was modified by this audit (only this Markdown document was added), so no rebuild was
required (per the card: "If the audit finds nothing to change, no build is needed").
