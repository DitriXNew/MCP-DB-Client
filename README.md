# http1c — MCP Server Framework for 1C:Enterprise

**http1c** is a framework for building [Model Context Protocol (MCP)](https://modelcontextprotocol.io/) servers from 1C:Enterprise. It provides a native component (DLL) that handles the MCP transport layer and a reference 1C data processor that demonstrates how to implement tools, resources, and prompts.

Use it as a template to expose any 1C business logic — catalogs, documents, reports, calculations — to AI applications like VS Code Copilot, Claude Desktop, and other MCP-compatible clients.

## Core Concept

The project is intentionally split into two layers with different responsibilities.

### Native component responsibilities

The DLL is the MCP engine. It handles protocol and transport details that should not be reimplemented in 1C business code:

- HTTP/SSE transport
- JSON-RPC request/response lifecycle
- MCP session management
- authentication, origin validation, and rate limiting
- pagination, notifications, and progress streaming
- converting 1C responses into valid MCP replies

### 1C responsibilities

The 1C side owns business logic. A 1C developer should work at the level of tools, resources, and prompts, not at the level of MCP internals:

- describe a tool/resource/prompt in BSL
- register it in the component
- handle the incoming call in 1C
- run any required client-side or server-side 1C logic
- return the result or send progress updates

### Why the project is designed this way

The goal is to let a 1C developer publish almost any 1C functionality through MCP without having to understand HTTP, JSON-RPC, SSE, session handling, or MCP message formatting.

In other words:

- the component knows how to be an MCP server
- 1C knows what the tool actually does

This keeps the integration point simple. To add new functionality, a 1C developer does not need to change the native transport layer. They only add or update 1C definitions and handlers.

### Mental model for a 1C developer

From the 1C side, the workflow is intentionally simple:

1. Define the MCP object in BSL.
2. Register it in the component.
3. Receive the request through `ExternalEvent`.
4. Execute arbitrary 1C logic.
5. Return the final result, and optionally send progress while the operation is running.

This means the project is not a fixed set of built-in utilities. It is an MCP transport and protocol layer for 1C, with the actual application behavior defined in 1C code.

## Key Features

- **Full MCP protocol support** — tools, resources, prompts with `listChanged` notifications
- **Streamable HTTP transport** — `POST /mcp` for requests, `GET /mcp` for SSE notification stream
- **Session management** — `Mcp-Session-Id` header, UUID v4 sessions per spec
- **Security** — Origin validation (DNS rebinding protection), Bearer token auth, rate limiting
- **Progress streaming** — SSE-based progress notifications for long-running operations
- **Pagination** — cursor-based pagination for `tools/list`, `resources/list`, `prompts/list`
- **Tool annotations** — `readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`
- **Output schemas** — typed response contracts for tool results
- **Dynamic registration** — register/update tools, resources, prompts at runtime from 1C
- **Built-in semantic search (RAG)** — optional `search` / `grep` / `get_segment` tools backed by a Rust search core (`rcore`) with dense/keyword/hybrid retrieval (see [Search Subsystem](#search-subsystem-rag))

## Architecture

```
┌─────────────────┐     HTTP/SSE      ┌──────────────────┐    ExternalEvent   ┌──────────────────┐
│   MCP Client    │◄─────────────────►│   Native DLL     │◄──────────────────►│  1C:Enterprise   │
│  (VS Code, etc) │   POST/GET /mcp   │  (HttpServer)    │   ToolCall, etc.   │  (BSL Module)    │
└─────────────────┘                   └──────────────────┘                    └──────────────────┘
```

1. A 1C form loads the native add-in and starts the HTTP server.
2. An MCP client connects to `http://localhost:PORT/mcp`.
3. The native component handles protocol-level messages (initialize, tools/list, etc.).
4. Business logic requests (tools/call, resources/read, prompts/get) are forwarded to 1C via `ExternalEvent`.
5. The 1C module processes the request and sends results back through `SendResponse`.
6. The DLL wraps the result in a JSON-RPC response and returns it to the client.

## Quick Start

### 1. Build the DLL

```bash
build/build-http1c-dll-release.sh
```

Requires Visual Studio Build Tools 2019+ with C++ support.

### 2. Package the add-in

Packaging is done automatically at the end of the build script. To run it standalone:

```bash
build/package-http1c-addin.sh
```

### 3. Compile the EPF (requires OneScript)

```bash
build/compile-http1c-epf.sh
```

### 4. Open in 1C

1. Open the `http1c.epf` data processor in your 1C infobase.
2. Click **Connect** — the MCP server starts on the configured port.
3. Configure your MCP client to connect to `http://localhost:PORT/mcp`.

### 5. VS Code configuration

Add to your `.vscode/mcp.json`:

**Without authentication:**

```json
{
  "servers": {
    "1c-mcp-server": {
      "type": "sse",
      "url": "http://localhost:8888/mcp"
    }
  }
}
```

**With Bearer token authentication:**

```json
{
  "servers": {
    "1c-mcp-server": {
      "type": "sse",
      "url": "http://localhost:8888/mcp",
      "headers": {
        "Authorization": "Bearer ${input:mcpToken}"
      }
    }
  },
  "inputs": [
    {
      "id": "mcpToken",
      "type": "promptString",
      "description": "Bearer token for the 1C MCP server",
      "password": true
    }
  ]
}
```

When using the `${input:...}` syntax, VS Code will prompt for the token each time the MCP server connection starts. The entered value is masked as a password.

> **Important:** The token in VS Code must match the value set on the 1C side. If the server has no token configured (empty string), authentication is disabled and no `headers` are needed. If a token is set on the server but VS Code sends no `Authorization` header, the server responds with HTTP 401 and VS Code may try to start an OAuth flow — this is not supported; use the `headers` approach above instead.

## How to Build Your Own MCP Server from 1C

The reference data processor (`http-1c-dp`) is a working example. Use it as a starting point:

### Registering Tools

Tools are executable functions that AI clients can invoke. Define them as JSON structures and register with the component:

```bsl
// Create a tool definition
Tool = NewTool("myTool", "Description of what this tool does");
AddToolParam(Tool, "paramName", "string", "Parameter description");
AddToolAnnotations(Tool, True);  // readOnly, safe

// Define output schema (optional, helps clients validate responses)
Schema = NewOutputSchema();
AddOutputProperty(Schema, "result", "string", "Result description");
SetToolOutputSchema(Tool, Schema);

// Register all tools
Tools = New Array;
Tools.Add(Tool);
Await Component.RegisterToolsAsync(SerializeToJson(Tools));
```

Handle the tool call in the `ExternalEvent` handler:

```bsl
&AtClient
Async Procedure ExternalEvent(Source, Event, Data)
    If Source <> "HttpServer" Then Return; EndIf;
    If Event = "ToolCall" Then ProcessToolCall(Data); EndIf;
EndProcedure
```

### Registering Resources

Resources provide contextual data to AI clients (metadata, file contents, etc.):

```bsl
Resource = New Structure;
Resource.Insert("uri", "1c://metadata/catalogs");
Resource.Insert("name", "1C Catalogs");
Resource.Insert("description", "List of all catalog metadata objects");
Resource.Insert("mimeType", "application/json");

Resources = New Array;
Resources.Add(Resource);
Await Component.RegisterResourcesAsync(SerializeToJson(Resources));
```

Handle resource reads via `"ResourceRead"` events.

### Registering Prompts

Prompts are reusable interaction templates:

```bsl
Prompt = New Structure;
Prompt.Insert("name", "analyzeData");
Prompt.Insert("description", "Prompt for analyzing 1C data");

PromptArgs = New Array;
Arg = New Structure("name,description,required", "topic", "Analysis topic", False);
PromptArgs.Add(Arg);
Prompt.Insert("arguments", PromptArgs);

Prompts = New Array;
Prompts.Add(Prompt);
Await Component.RegisterPromptsAsync(SerializeToJson(Prompts));
```

Handle prompt gets via `"PromptGet"` events.

### Dynamic Updates

Call `RegisterToolsAsync()` / `RegisterResourcesAsync()` / `RegisterPromptsAsync()` again at any time with an updated list. The component will automatically send `notifications/tools/list_changed` (or equivalent) to all connected MCP clients.

### Security Configuration

The component supports optional Bearer token authentication. When a token is set, every HTTP request must include the `Authorization: Bearer <token>` header or it will be rejected with HTTP 401.

```bsl
// Enable authentication — all requests must include Authorization: Bearer my-secret-token
Component.AuthToken = "my-secret-token";

// Disable authentication — any request is accepted
Component.AuthToken = "";
```

**How it works:**

| Server token | Client header | Result |
|---|---|---|
| Empty (default) | None needed | All requests accepted |
| `"my-secret"` | `Authorization: Bearer my-secret` | Request accepted |
| `"my-secret"` | Missing or wrong token | HTTP 401 Unauthorized |

**Changing the token at runtime:** You can set or clear `AuthToken` while the server is running. The change takes effect immediately for all new requests — no restart needed.

**VS Code note:** If the server returns 401, VS Code may attempt an OAuth 2.0 PKCE authorization flow (redirecting to `/authorize`). This is **not supported** by the component. Always configure the token in `.vscode/mcp.json` via the `headers` field (see [VS Code configuration](#5-vs-code-configuration) above).

The component also enforces:
- **Origin validation** — only requests from `localhost` / `127.0.0.1` / VS Code origins are accepted
- **Rate limiting** — token-bucket algorithm (60 burst, 20/sec)
- **Session management** — `Mcp-Session-Id` assigned on initialize, validated on subsequent requests

## Native Component API

Methods exposed to 1C (English / Russian names):

| Method | Description |
|--------|-------------|
| `StartListen(port)` / `НачатьПрослушивание` | Start the HTTP server on the given port |
| `StopListen()` / `ОстановитьПрослушивание` | Stop the server and unblock all pending requests |
| `SendResponse(json)` / `ОтправитьОтвет` | Send the final response for a pending request |
| `SendProgress(id, progress, total, message)` / `ОтправитьПрогресс` | Send a progress notification for a pending request |

Properties exposed to 1C:

| Property | Type | Description |
|----------|------|-------------|
| `Status` / `Статус` | Read-only | Returns JSON with server status |
| `Timeout` / `Таймаут` | Read/Write | Response timeout in seconds (default: 30) |
| `AuthToken` / `ТокенАвторизации` | Write-only | Bearer token for authentication (empty = no auth) |
| `LoggingEnabled` / `ЛогированиеВключено` | Read/Write | Whether runtime logging is active |
| `LogPath` / `ПутьЛога` | Read/Write | Path to the log file |
| `Tools` / `Инструменты` | Write-only | Register/update the tool list (JSON array) |
| `Resources` / `Ресурсы` | Write-only | Register/update the resource list (JSON array) |
| `Prompts` / `Промпты` | Write-only | Register/update the prompt list (JSON array) |
| `Version` / `Версия` | Read-only | Component version string |

## ExternalEvent Types

Events sent from the native component to 1C:

| Event | Description | Data |
|-------|-------------|------|
| `ToolCall` | MCP `tools/call` request | `{id, type, tool, arguments, progressToken}` |
| `ResourceRead` | MCP `resources/read` request | `{id, type, uri}` |
| `PromptGet` | MCP `prompts/get` request | `{id, type, name, arguments}` |
| `Request` | Legacy HTTP request (non-MCP) | `{id, method, path, body, params}` |

## MCP Protocol Support

### Implemented Methods

| Method | Handler |
|--------|---------|
| `initialize` | Native — returns capabilities, creates session |
| `notifications/initialized` | Native — accepted silently |
| `ping` | Native — returns empty result |
| `tools/list` | Native — paginated, from cache |
| `tools/call` | Delegated to 1C via ExternalEvent |
| `resources/list` | Native — paginated, from cache |
| `resources/read` | Delegated to 1C via ExternalEvent |
| `prompts/list` | Native — paginated, from cache |
| `prompts/get` | Delegated to 1C via ExternalEvent |

### Capabilities Advertised

```json
{
  "tools": { "listChanged": true },
  "resources": { "listChanged": true },
  "prompts": { "listChanged": true }
}
```

### HTTP Endpoints

| Endpoint | Description |
|----------|-------------|
| `POST /mcp` | MCP JSON-RPC messages |
| `GET /mcp` | SSE notification stream (list_changed events) |
| `DELETE /mcp` | Session termination |
| `GET /health` | Health check |
| `OPTIONS *` | CORS preflight |

## Reference Tools (in the demo processor)

| Tool | Purpose | Annotations |
|------|---------|-------------|
| `getStatus` | Component + runtime status | readOnly |
| `openForm` | Open a 1C form by path | idempotent |
| `execute` | Execute arbitrary 1C code | destructive |
| `evaluate` | Evaluate a 1C expression | readOnly, idempotent |
| `runLongTask` | Test progress notification | readOnly |

## Reference Resources

| URI | Description |
|-----|-------------|
| `1c://metadata/catalogs` | JSON list of catalog metadata objects |
| `1c://metadata/documents` | JSON list of document metadata objects |

## Reference Prompts

| Prompt | Arguments | Description |
|--------|-----------|-------------|
| `analyze1CData` | `topic` (optional) | System prompt for data analysis with metadata context |
| `generate1CCode` | `task` (required) | System prompt for BSL code generation with conventions |

## Search Subsystem (RAG)

The component ships with an optional semantic-search subsystem so a 1C MCP server can index its own content (documentation, code, catalog descriptions, logs) and let AI clients search it. The retrieval engine is a small Rust core, crate `rcore`, that runs entirely in-process.

### MCP tools

Three search tools are served **natively by the component** — they are handled inside the DLL and are *not* forwarded to 1C via `ExternalEvent`:

| Tool | Purpose |
|------|---------|
| `search` | Semantic / keyword / hybrid ranked search over indexed segments. Returns hits with `doc_id`, `name`, `collection`, `score`, segment line ranges, and (optionally) text. |
| `grep` | RE2 regex search (linear time, no backreferences/lookaround) over the stored document text. Works the instant a document is ingested — no vectors needed. |
| `get_segment` | O(1) line-range slice of a raw document by `doc_id` via an offset table. Out-of-range requests are clamped and the actual range is returned. |

`search` supports three retrieval modes via the `mode` argument:

- **dense** (default) — cosine/dot-product similarity over normalized embedding vectors.
- **keyword** — lexical scoring, no vectors required.
- **hybrid** — Reciprocal Rank Fusion (RRF) of the dense and keyword result lists.

Both `search` and `grep` accept metadata filters (`all`/`any` clauses) and a `collection` to scope the query.

### Ingest

Documents are pushed into the core through `rcore`'s JSON ABI. Ingest is **asynchronous**: the call is accepted under a short lock and returns immediately, while a background worker embeds the segments off-thread. Text-based tools (`grep`, `get_segment`) work right after acceptance; dense `search` becomes available once embedding finishes (`stats` exposes per-collection progress).

- **`index_segments`** — ingest a document as a list of pre-chunked segments (each with optional `embed_text`, line range, and per-segment `meta`). `doc_id` is required so segments can be upserted/deleted.
- **`index_raw`** — ingest a raw multi-line document; the core normalizes line endings, builds a line-offset table, stores the full text, and chunks it (line-snapped, by token budget, with overlap). `doc_id` is optional (auto-assigned and returned in the ack).

Supporting methods include `configure`, `stats`, `reset`, `delete_document`, and `delete_collection`.

### Lite vs full

The same `libhttp1cWin.dll` ships in **two distributions**. The component is pure C++ built `/MT` and does **not** link the Rust core; instead it loads an optional **`rcore.dll`** at runtime (`LoadLibrary` + `GetProcAddress`, resolved next to the component DLL — see `http-1c-dll/src/RustCore.h`):

| Distribution | Contents | Search behavior |
|--------------|----------|-----------------|
| **lite** | `libhttp1cWin.dll` only | `search` / `grep` / `get_segment` are advertised in `tools/list` but return a structured error (`code: rag_not_installed`) telling the caller to install the RAG package. |
| **full** | `libhttp1cWin.dll` + `rcore.dll` + `DirectML.dll` | Real fastembed/onnxruntime search core; the three tools run for real. |

Detection is automatic at runtime: if `rcore.dll` is present next to the component **and** all four ABI entry points (`rcore_version` / `rcore_dispatch` / `rcore_free_string` / `rcore_shutdown`) resolve, the component runs the full search path. A missing, partial, or version-mismatched `rcore.dll` degrades cleanly to the lite path (an "install RAG" error) — never a crash. The tool schemas are identical in both distributions, so a client sees the same `tools/list` either way.

### GPU / CPU (DirectML)

The full search core uses onnxruntime's **DirectML** execution provider for GPU acceleration, with **automatic CPU fallback** when no compatible GPU is available. The device is selected via the `configure` method's `device` field:

| `device` | Behavior |
|----------|----------|
| `auto` (default) | DirectML with onnxruntime's automatic CPU fallback. |
| `dml` | Force the DirectML (GPU) execution provider. |
| `cpu` | Force CPU execution. |

> **`DirectML.dll` is a hard dependency of the full package.** ort's prebuilt onnxruntime bundles the DirectML provider, so `rcore.dll` hard-imports `DirectML.dll`. The **full** bundle must ship `DirectML.dll` alongside `rcore.dll`, otherwise `rcore.dll` fails to load and the component silently falls back to the lite "install RAG" behavior. On Windows it lives at `C:\Windows\System32\DirectML.dll`. (Version-coupling caveat: this `DirectML.dll` is paired with the bundled onnxruntime; a future CPU-only onnxruntime build would remove the import and this DLL could be dropped.)

The embedding model itself is **fetched at runtime**, not at build time — nothing model-related is downloaded during the build or in CI.

## Repository Layout

```text
├── build/
│   ├── build-http1c-dll-debug.sh       # Build DLL in debug mode
│   ├── build-http1c-dll-release.sh     # Build DLL in release mode
│   ├── compile-http1c-epf.sh           # Compile EPF from XML
│   ├── package-http1c-addin.sh         # Package DLL as 1C add-in ZIP
│   └── onescript/
│       └── compile-external-processor.os
├── http-1c-dll/
│   ├── CMakeLists.txt                  # CMake build configuration
│   ├── version.h                       # Version management
│   ├── include/                        # 1C API headers + vendored libraries
│   │   ├── httplib.h                   # cpp-httplib (HTTP server)
│   │   ├── json.hpp                    # nlohmann/json (JSON parser)
│   │   └── AddInDefBase.h, ...         # 1C Native API headers
│   └── src/
│       ├── AddInNative.cpp/h           # Generic 1C add-in framework
│       ├── HttpServerComponent.cpp/h   # MCP server implementation
│       ├── RustCore.h                  # Runtime loader for rcore.dll (search)
│       └── AddInNative.def             # DLL export definitions
├── rust-core/                          # `rcore` — Rust search core (rcore.dll)
│   ├── Cargo.toml                      # cdylib crate, `fastembed` feature
│   └── src/                            # dispatch, embed, grep, filter, core
├── http-1c-dp/
│   ├── http1c.xml                      # 1C data processor XML source
│   └── http1c/
│       └── Forms/Form/Ext/Form/
│           └── Module.bsl              # Reference MCP server implementation
└── http1c.epf                          # Compiled 1C external data processor
```

## Technology Stack

- **C++17** — native component
- **CMake** + **MSVC** — build system
- **[cpp-httplib](https://github.com/yhirose/cpp-httplib)** — embedded HTTP server
- **[nlohmann/json](https://github.com/nlohmann/json)** — JSON parser
- **1C Native API** — integration with 1C:Enterprise
- **OneScript** — EPF compilation from XML
- **Rust** (`rcore` crate) + **[fastembed](https://github.com/Anush008/fastembed-rs)** / **onnxruntime (ort)** — optional search core in `rcore.dll` (full distribution only)

Based on [lintest/AddinTemplate](https://github.com/lintest/AddinTemplate) for the native add-in layer.

## License

See [LICENSE](LICENSE).

## Build Requirements

The checked-in build scripts target Windows.

Required tools:

- Microsoft Visual Studio Build Tools 2019+ with MSVC (C++ workload) — must be installed manually (system-level, requires admin)
- CMake and Ninja — downloaded automatically to `build/tools/` on first run if not found in PATH

### Setting Up a New Developer Machine

Full Visual Studio is **not** required — the free standalone Build Tools are sufficient, and they are the **only thing you need to install manually**.

Install via `winget` (run once, requires admin):
```bat
winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

Or via Chocolatey:
```bat
choco install visualstudio2022buildtools --package-parameters "--add Microsoft.VisualStudio.Workload.VCTools --includeRecommended" -y
```

Or download the installer manually from https://visualstudio.microsoft.com/downloads/#build-tools-for-visual-studio-2022 and select the **"Desktop development with C++"** workload.

After that, just run a build script — CMake and Ninja will be downloaded automatically to `build/tools/` on first use.

## Building the DLL

Release build:

```bash
build/build-http1c-dll-release.sh
```

Debug build:

```bash
build/build-http1c-dll-debug.sh
```

The DLL output is written to:

```text
http-1c-dll/bin/libhttp1cWin.dll
```

Debug symbols are also written into `http-1c-dll/bin/`.

After a successful DLL build, the top-level build scripts also package the native add-in bundle with `MANIFEST.XML` metadata and update the embedded 1C template here:

```text
http-1c-dp/http1c/Templates/http1c/Ext/Template.bin
```

### Building the search core (full distribution)

By default the build produces the **lite** component only — pure C++, no Rust/cargo involved. To also build the **full** search core (`rcore.dll`), configure CMake with `-DRCORE_FASTEMBED=ON`:

```bash
cd http-1c-dll && mkdir -p build && cd build
# lite — libhttp1cWin.dll alone (no cargo):
cmake .. -G Ninja -DCMAKE_BUILD_TYPE=Release
cmake --build .

# full — also builds rcore.dll via cargo:
cmake .. -G Ninja -DCMAKE_BUILD_TYPE=Release -DRCORE_FASTEMBED=ON
cmake --build .
```

The full build invokes `cargo build --release --features fastembed` (handled inside CMake) and drops `rcore.dll` next to the component in `http-1c-dll/bin/`. The embedding model is fetched at runtime, so no model is downloaded at build time.

**Why two binaries (`/MT` vs `/MD`).** The C++ component is built with the **static** CRT (`/MT`) — it cannot compile under `/MD` because MSVC's `char16_t` stream code hits error C2491. onnxruntime (via `ort`), however, requires the **dynamic** CRT (`/MD`). Linking the two together is impossible, so the search core is a *separate* `rcore.dll` (a cdylib) that the `/MT` component loads at runtime instead of linking. CMake builds the cdylib with the dynamic CRT by removing the `+crt-static` default (`RUSTFLAGS=-C target-feature=-crt-static`); this is independent of the component's `/MT` and they never link together.

### Packaging the add-in (lite / full)

`build/package-http1c-addin.sh` assembles a 1C add-in ZIP (and updates `Template.bin`). The variant is selected via the `VARIANT` env var:

```bash
# lite (default) — libhttp1cWin.dll only:
VARIANT=lite bash build/package-http1c-addin.sh

# full — also bundles rcore.dll + DirectML.dll:
VARIANT=full bash build/package-http1c-addin.sh
```

The **full** packaging step copies `DirectML.dll` from `C:\Windows\System32\DirectML.dll` (override with `DIRECTML_SRC`) into the bundle — it is a hard dependency of `rcore.dll` (see [GPU / CPU](#gpu--cpu-directml)) and the script fails clearly if it is missing. Set `SKIP_TEMPLATE=1` to produce only the ZIP without overwriting the committed `Template.bin` (which tracks the lite bundle).

The release workflow (`.github/workflows/release.yml`) builds both variants and attaches both ZIPs to the GitHub release:

```text
http1c-addin-lite-v<version>.zip   # libhttp1cWin.dll
http1c-addin-full-v<version>.zip   # libhttp1cWin.dll + rcore.dll + DirectML.dll
```

## Running the 1C Side

The repository includes an external data processor:

- `http-1c-dp/http1c.epf`

Its form module is responsible for:

- attaching the native add-in,
- starting the HTTP listener,
- registering MCP tools,
- handling requests forwarded from the DLL,
- sending responses and progress messages back to the DLL.

The add-in is attached from the embedded 1C template instead of a machine-specific DLL path.

### Logging

The default log file is no longer a hardcoded absolute path.

```text
http_debug.log
```

On the native side, when no explicit log path is provided from 1C, the component falls back to the system temporary directory.

```text
%TEMP%\http1c.log
```

You can still override the log path through the form field before connecting.

## VS Code MCP Configuration

The workspace already contains a sample MCP client configuration:

```json
{
  "servers": {
    "ConnectionTo1C": {
      "type": "sse",
      "url": "http://localhost:8888/mcp"
    }
  }
}
```

This matches the default listener port used by the 1C form when no custom port is set.

## Typical Local Workflow

1. Build `http-1c-dll`.
2. Open `http-1c-dp/http1c.epf` in 1C:Enterprise.
3. Optionally adjust the port and log path in the form.
4. Attach the add-in from the embedded template.
5. Start the local listener.
6. Use the MCP server from VS Code or another MCP-capable client.

EPF compilation from XML sources is now launched from the top-level build folder:

```bash
build/compile-http1c-epf.sh
```

The compiled processor artifact is written to the repository root:

```text
http1c.epf
```

## Limitations and Caveats

- Windows-focused setup. The build scripts target MSVC and NMake via bash.
- The build still depends on Windows toolchain availability through `PATH` or an initialized Visual Studio build environment.
- The HTTP server is intentionally local-only and binds to `127.0.0.1`.
- `execute` and `evaluate` expose powerful server-side capabilities and should only be used in trusted local environments.
- The current tool set mixes practical helpers with demo functionality such as `runLongTask`.
- There is no packaging or deployment flow yet for distributing the add-in as a polished product.

## Testing & Deployment Notes

When updating the native add-in DLL during development:

1. **Clear the component cache.** 1C caches native add-ins in a temporary directory. Delete the cache folder before loading a new version:
   ```text
   %APPDATA%\1C\1cv8\ExtCompT
   ```
2. **Restart the 1C session.** Close the 1C application completely and reopen it — a running session keeps the old DLL locked.
3. **Disable dangerous action protection.** In the 1C user settings, uncheck *"Protection from dangerous actions"* (`Защита от опасных действий`). Otherwise the platform will block add-in attachment.

Without these steps the platform may silently load an outdated DLL or refuse to attach the add-in.

## Version

The native component version defined in the source is:

```text
1.3.0
```

## License

This project is licensed under the MIT License. See `LICENSE`.