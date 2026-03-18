# http1c — MCP Server Framework for 1C:Enterprise

## Project Overview

This is a framework for building Model Context Protocol (MCP) servers from 1C:Enterprise.
It consists of two parts:

- **http-1c-dll/** — C++17 native DLL (1C Native API add-in) that handles MCP transport
- **http-1c-dp/** — Reference 1C data processor (BSL module) showing how to implement tools, resources, and prompts

The DLL handles protocol-level concerns (HTTP, JSON-RPC, sessions, security).
The BSL module handles business logic (tool handlers, resource data, prompt generation).

## Architecture

```
MCP Client ←→ Native DLL (C++ httplib) ←→ 1C:Enterprise (BSL via ExternalEvent)
```

- POST /mcp — JSON-RPC MCP requests
- GET /mcp — SSE notification stream
- DELETE /mcp — Session termination

## Key Conventions

### C++ (http-1c-dll/src/)

- Build: CMake + MSVC, NMake Makefiles, `/MT` static CRT, `/utf-8`
- JSON: nlohmann/json (`json.hpp`)
- HTTP: cpp-httplib (`httplib.h`)  
- 1C API: Native API headers in `include/` (`AddInDefBase.h`, `IMemoryManager.h`, `types.h`, `ComponentBase.h`)
- String conversion: `MB2WCHAR()` / `WCHAR2MB()` for UTF-8 ↔ UTF-16
- Thread safety: Separate mutexes per resource (`toolsMutex`, `resourcesMutex`, `sessionMutex`, etc.)
- Events to 1C: `ExtEvent(u"HttpServer", u"EventName", data)` 
- Method registration: `AddProcedure()` / `AddFunction()` with EN/RU names
- Version: Defined in `version.h` as `VERSION_SEMVER`

### BSL (http-1c-dp/http1c/Forms/Form/Ext/Form/Module.bsl)

- All client-side handlers are `Async Procedure`
- Server calls for metadata/computation use `&AtServer` functions
- Component interaction via `Await Component.MethodAsync()`
- DO NOT use reserved 1C names as local variables (`Catalogs`, `Documents`, `Metadata`, etc.)
- JSON: `JSONReader`/`JSONWriter` + `ReadJSON()`/`WriteJSON()`
- Error pattern: Try/Except → `SendToolError()` / `SendResourceError()` / `SendPromptError()`
- Tool definitions: `NewTool()`, `AddToolParam()`, `AddToolAnnotations()`, `SetToolOutputSchema()`
- Resource definitions: Structure with `uri`, `name`, `description`, `mimeType`
- Prompt definitions: Structure with `name`, `description`, `arguments` array

### Adding New Tools

1. Create a `ToolXxx()` function that returns a tool definition
2. Add it to the `RegisterMCPTools()` procedure
3. Add a handler `HandleXxx()` in the ToolHandlers region
4. Add a dispatch case in `ProcessToolCall()`

### Adding New Resources

1. Add a resource definition in `RegisterMCPResources()`
2. Add a handler `HandleReadXxx()` in the ResourceHandlers region
3. Add a dispatch case in `ProcessResourceRead()` matching the URI

### Adding New Prompts

1. Add a prompt definition in `RegisterMCPPrompts()`
2. Add a handler `HandlePromptXxx()` in the PromptHandlers region
3. Add a dispatch case in `ProcessPromptGet()` matching the name

## MCP Protocol Version

`2025-03-26` — Streamable HTTP transport, session management via `Mcp-Session-Id`.

## Build Commands

```bash
build/build-http1c-dll-release.sh   # Build DLL  
build/compile-http1c-epf.sh          # Compile EPF from XML
```

Both scripts auto-detect Visual Studio Build Tools and CMake.

## Security

- Origin validation: localhost, 127.0.0.1, [::1], vscode-* only
- Bearer token auth: Optional, set via `SetAuthToken()`
- Rate limiting: Token bucket (60 burst, 20/sec)
- Binding: 127.0.0.1 only (not 0.0.0.0)

## Testing

See `TESTING.md` for a complete test plan with 18 tests covering all tools, resources, prompts, and security features.
