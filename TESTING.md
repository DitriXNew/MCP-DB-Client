# Testing Guide — http1c MCP Server

This guide is designed for an AI agent (such as GitHub Copilot in VS Code) that connects to the http1c MCP server. All tests are performed by invoking MCP tools, reading resources, and using prompts through the standard MCP client interface — not via raw HTTP/curl.

## Automated C++ smoke harness

`http-1c-dll/tests/smoke/main.cpp` builds into a console exe (`http1c_smoke.exe`)
that needs **no 1C at all**: it loads the built `bin/libhttp1cWin.dll` exactly
like 1C:Enterprise does (`LoadLibrary` → `GetClassObject("HttpServer")` →
`setMemManager` → `Init` with stubbed `IMemoryManager` /
`IAddInDefBase(+Ex)` interfaces), then drives the component over real HTTP and
asserts behavior. CI runs it on every push (see `.github/workflows/ci.yml`);
`build/run-tests.sh` also builds and runs it after the unit tests.

**Build & run locally** (from a VS Developer/vcvars x64 prompt):

```bash
cd http-1c-dll
mkdir build && cd build
cmake .. -G Ninja -DCMAKE_BUILD_TYPE=Release -DHTTP1C_BUILD_TESTS=ON
cmake --build .            # or: cmake --build . --target http1c_smoke
../bin/http1c_smoke.exe    # optional arg: path to libhttp1cWin.dll
```

The exe exits 0 when everything passes (nonzero = number of failed checks) and
prints PASS/FAIL per test:

| Test | Covers |
|------|--------|
| T1 | DLL load, `GetClassObject`, `Init`, call plumbing (`GetProcessId` = current pid) |
| T2 | `ApplyConfig` (`tools_json`, `timeout`, logging off) → `StartListen` → MCP `initialize` returns `serverInfo` + `Mcp-Session-Id` |
| T3 | `tools/list` is the union of 1C tools + the 4 native search tools, names unique, count = 5 |
| T4 | Reserved-name rejection: registering a tool named `search` raises `AddError` ("reserved…") and leaves the tool cache unchanged |
| T5 | Unknown `Mcp-Session-Id` is transparently resurrected (HTTP 200, not 404) |
| T5b | Session-map cap: a paced flood of 300 unknown session ids is all served, yet `GetStatus().active_sessions` stays ≤ 256 (LRU eviction, no memory DoS) |
| T6 | Bearer auth (401 without / 200 with token) + concurrency smoke: 4×50 requests racing `ApplyConfig` token flips — only 200/401/429 allowed (429 = the component's own rate limiter), no 5xx/hang |
| T7 | `tools/call` of a 1C tool with `timeout: 1`: `ExternalEvent(ToolCall)` reaches the stub and the "did not respond within 1 second" error returns within ~3 s |
| T8 | UTF-8 round-trip: Russian tool name/description come back byte-identical through `MB2WCHAR`/`WCHAR2MB` |
| T9 | Native `list_collections` end-to-end: ok-collections JSON (with `rcore.dll` beside the DLL) or structured `rag_not_installed` (lite) — both valid |
| T10 | Late `rcore.dll` install: a second component copy in a temp dir without `rcore.dll` answers `rag_not_installed` (message names all 4 native tools; `GetStatus` shows `rag_available=false`, `rag_load_error=0`); after copying `rcore.dll` beside it the SAME loaded module flips to full (`rag_available=true`) — no process restart (flip stage skips on the lite CI build) |

The harness never enables component logging and always finishes with
`StopListen`, so it leaves no log files and no dangling threads.

## Prerequisites

1. The `http1c.epf` data processor is open in 1C:Enterprise.
2. The server is running (the "Connect" button has been pressed).
3. VS Code has the MCP server configured in `.vscode/mcp.json`:

```json
{
  "servers": {
    "1c-mcp-server": {
      "type": "http",
      "url": "http://localhost:8888/mcp"
    }
  }
}
```

4. Copilot Chat is open and can see the `1c-mcp-server` tools.

## Verification Approach

Tests are split into two groups:

- **Tests 1–9, 16–18**: Use the MCP tools directly via Copilot Chat (`getStatus`, `evaluate`, `execute`, `query`, `openForm`, `runLongTask`).
- **Tests 10–15, 19+**: Use `curl` (or equivalent HTTP client) to test MCP primitives that are not tools — `resources/list`, `resources/read`, `prompts/list`, `prompts/get`, and HTTP-level behavior (session management, security, error codes).

After each test, state what you observed and whether it matches the expected result. If a test fails, report the actual response and move to the next test.

---

## Test 1: List Available Tools

**Action:** Ask Copilot to list the tools available from `1c-mcp-server`.

**Expected:** Five tools are listed:
- `getStatus` — returns component and runtime status
- `openForm` — opens a 1C form by path
- `execute` — executes arbitrary BSL code (on the server or, with `location: "client"`, on the thin client)
- `evaluate` — evaluates a BSL expression
- `query` — runs a 1C query with typed parameters and returns a Markdown table
- `runLongTask` — runs a long operation with progress

**Verify:**
- All 5 tools are visible.
- Each tool has a description.
- Each tool has `inputSchema` with defined parameters.

---

## Test 2: getStatus

**Action:** Call the `getStatus` tool with no arguments.

**Expected result:** A JSON object with two sections:
- `runtimeStatus` — contains `IsBusy` (should be `false`), `LastResult`, `LastError`
- `componentStatus` — contains `running: true`, `toolCount: 5`, `resourceCount: 2`, `promptCount: 2`, `sessionCount` ≥ 1

**Verify:**
- `componentStatus.running` is `true`
- `componentStatus.toolCount` is `5`
- `componentStatus.resourceCount` is `2`
- `componentStatus.promptCount` is `2`
- `runtimeStatus.IsBusy` is `false`

---

## Test 3: evaluate — Simple Expression

**Action:** Call the `evaluate` tool with `expression`: `"1 + 2 + 3"`

**Expected result:** A JSON with `result` equal to `"6"` (string representation of the computed value).

**Verify:** The result text contains `6`.

---

## Test 4: evaluate — String Expression

**Action:** Call the `evaluate` tool with `expression`: `"Upper(""hello world"")"`

**Expected result:** `result` contains `"HELLO WORLD"`.

**Verify:** The returned string is the uppercased version of the input.

---

## Test 5: execute — Get Current Date

**Action:** Call the `execute` tool with `code`: `"Result = String(CurrentDate());"`

**Expected result:** A JSON with `result` containing a date/time string in the current locale format.

**Verify:** The result is a non-empty string that looks like a date.

---

## Test 6: execute — Build a Structure

**Action:** Call the `execute` tool with:
```
code: "S = New Structure; S.Insert(""name"", ""test""); S.Insert(""value"", 42); Result = JSONString(S, New JSONWriterSettings(, Chars.LF));"
```

> Note: The exact BSL syntax for JSON serialization may vary. If `JSONString` is not available, try: `Writer = New JSONWriter; Writer.SetString(); WriteJSON(Writer, S); Result = Writer.Close();`

**Expected result:** A JSON string `{"name":"test","value":42}` (or equivalent).

**Verify:** The output is valid JSON with the expected fields.

---

## Test 7: execute — Error Handling

**Action:** Call the `execute` tool with `code`: `"Result = 1 / 0;"`

**Expected result:** The tool returns with `isError: true` and an error message describing a division by zero.

**Verify:** The response indicates an error occurred. The error text references division by zero.

---

## Test 7a: execute — Run on the Client

**Action:** Call the `execute` tool with `code`: `"Result = ""hi from "" + ?(IsBlankString(""""), """", String(CurrentDate()));"` and `location`: `"client"`.

(A simpler smoke variant: `code`: `"Result = String(CurrentDate());"`, `location`: `"client"`.)

**Expected result:** The tool returns `isError: false`. The progress message mentions executing **client** code, and the result is the string produced on the thin client.

**Verify:**
- `isError` is `false`.
- With `location` omitted or `"server"`, the same code still runs on the server (backward compatible).

---

## Test 7b: query — Typed Parameters → Markdown

**Action:** Call the `query` tool with:

```json
{
  "query": "SELECT TOP 5 Description AS Name, Code FROM Catalog.Users WHERE DeletionMark = &Mark",
  "parameters": [ { "name": "Mark", "type": "Boolean", "value": false } ],
  "limit": 100
}
```

(Adjust the catalog to one that exists in the test infobase.)

**Expected result:** The tool returns `isError: false` and a Markdown table: a header row (`Name`, `Code`), a `| --- | --- |` separator, up to `limit` data rows, and a trailing `_N row(s)._` / `_Shown N of M rows…_` note. A query with no rows returns `_Query returned 0 rows._`.

**Verify:**
- The result is valid GitHub-flavored Markdown.
- `Date` / `Ref:<Kind.Name>` parameters bind without a type-conversion error.
- A malformed query text returns `isError: true` with the 1C error description (no crash).

---

## Test 8: openForm — Invalid Form

**Action:** Call the `openForm` tool with `formPath`: `"CommonForm.NonExistentFormXYZ123"`

**Expected result:** The tool returns `isError: true` with an error message (form not found).

**Verify:** The response indicates an error. The 1C application did not crash.

---

## Test 8a: openForm — Existing Form

**Action:** Call the `openForm` tool with `formPath`: `"ExternalDataProcessor.http1c.Form.Form"`

**Expected result:** The tool returns `isError: false` with a confirmation message like `"Form opened successfully"`.

**Verify:**
- `isError` is `false`
- The confirmation message is non-empty
- The form actually opened in the 1C window (visual check)

---

## Test 9: runLongTask — With Progress

**Action:** Call the `runLongTask` tool with:
- `steps`: `3`
- `iterationsPerStep`: `100000`

**Expected result:** The tool completes after a few seconds and returns:
- `completedSteps`: `3`
- `summary`: a text description of the completed work

**Verify:**
- `completedSteps` equals the requested number of steps (3)
- The summary text is non-empty
- If the MCP client supports progress display, progress updates should have been visible during execution

---

## Test 10: List Resources

> **Note:** Resources are an MCP primitive, not a tool. Test via `curl` or HTTP client.
>
> First, initialize a session:
> ```bash
> curl -s -D - -X POST http://localhost:8888/mcp \
>   -H "Content-Type: application/json" \
>   -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}'
> ```
> Save the `Mcp-Session-Id` header from the response. Use it in all subsequent requests as `$SESSION_ID`.

**Action:** Send `resources/list`:
```bash
curl -s -X POST http://localhost:8888/mcp \
  -H "Content-Type: application/json" \
  -H "Mcp-Session-Id: $SESSION_ID" \
  -d '{"jsonrpc":"2.0","id":2,"method":"resources/list"}'
```

**Expected:** Two resources:
- `1c://metadata/catalogs` — catalog metadata
- `1c://metadata/documents` — document metadata

**Verify:** Both resources are listed with names, descriptions, and `application/json` MIME type.

---

## Test 11: Read Resource — Catalogs

**Action:** Send `resources/read`:
```bash
curl -s -X POST http://localhost:8888/mcp \
  -H "Content-Type: application/json" \
  -H "Mcp-Session-Id: $SESSION_ID" \
  -d '{"jsonrpc":"2.0","id":3,"method":"resources/read","params":{"uri":"1c://metadata/catalogs"}}'
```

**Expected result:** A JSON array of objects, each with:
- `name` — internal catalog name (e.g., `"Currencies"`, `"Counterparties"`)
- `synonym` — user-facing name
- `attributeCount` — number of attributes
- `tabularSectionCount` — number of tabular sections

**Verify:**
- The result is a non-empty JSON array
- Each element has the expected fields
- The catalog names correspond to the actual catalogs in the connected 1C infobase

---

## Test 12: Read Resource — Documents

**Action:** Send `resources/read`:
```bash
curl -s -X POST http://localhost:8888/mcp \
  -H "Content-Type: application/json" \
  -H "Mcp-Session-Id: $SESSION_ID" \
  -d '{"jsonrpc":"2.0","id":4,"method":"resources/read","params":{"uri":"1c://metadata/documents"}}'
```

**Expected result:** Same structure as catalogs but for documents.

**Verify:**
- A non-empty JSON array
- Each element has `name`, `synonym`, `attributeCount`, `tabularSectionCount`
- The document names match the actual documents in the infobase

---

## Test 13: List Prompts

**Action:** Send `prompts/list`:
```bash
curl -s -X POST http://localhost:8888/mcp \
  -H "Content-Type: application/json" \
  -H "Mcp-Session-Id: $SESSION_ID" \
  -d '{"jsonrpc":"2.0","id":5,"method":"prompts/list"}'
```

**Expected:** Two prompts:
- `analyze1CData` — with optional argument `topic`
- `generate1CCode` — with required argument `task`

**Verify:** Both prompts are listed with names, descriptions, and correct arguments.

---

## Test 14: Use Prompt — analyze1CData

**Action:** Send `prompts/get`:
```bash
curl -s -X POST http://localhost:8888/mcp \
  -H "Content-Type: application/json" \
  -H "Mcp-Session-Id: $SESSION_ID" \
  -d '{"jsonrpc":"2.0","id":6,"method":"prompts/get","params":{"name":"analyze1CData","arguments":{"topic":"sales performance"}}}'
```

**Expected result:** A system message containing:
- A description of the 1C infobase metadata (catalogs, documents, etc.)
- Instructions for analyzing data on the given topic
- Context about the infobase structure

**Verify:**
- The prompt returns at least one message
- The message text mentions catalogs and/or documents from the infobase
- The topic "sales performance" is referenced in the generated text

---

## Test 15: Use Prompt — generate1CCode

**Action:** Send `prompts/get`:
```bash
curl -s -X POST http://localhost:8888/mcp \
  -H "Content-Type: application/json" \
  -H "Mcp-Session-Id: $SESSION_ID" \
  -d '{"jsonrpc":"2.0","id":7,"method":"prompts/get","params":{"name":"generate1CCode","arguments":{"task":"Create a query to get all counterparties"}}}'
```

**Expected result:** A system message containing:
- BSL coding conventions
- Metadata context from the infobase
- Instructions for the code generation task

**Verify:**
- The prompt returns at least one message
- The message text contains 1C/BSL coding guidelines
- The task description is included

---

## Test 16: Status After All Tests

**Action:** Call `getStatus` again after running all previous tests.

**Expected:**
- `runtimeStatus.IsBusy` is `false`
- `runtimeStatus.LastError` may or may not be empty (depending on test 7)
- `componentStatus.running` is still `true`
- `componentStatus.sessionCount` ≥ 1

**Verify:** The server is still running and responsive after all tests.

---

## Test 17: evaluate — 1C Metadata Access

**Action:** Call `evaluate` with `expression`: `"Metadata.Catalogs.Count()"`

**Expected result:** A number representing the count of catalogs in the infobase. Should match the number of items returned by the catalogs resource (Test 11).

**Verify:** The result is a positive integer. Cross-reference with the catalog list from Test 11.

---

## Test 18: execute — Complex Operation

**Action:** Call `execute` with:
```
code: "Query = New Query; Query.Text = ""SELECT COUNT(*) AS Cnt FROM Catalog.Users""; Res = Query.Execute().Unload(); Result = String(Res[0].Cnt);"
```

> Note: Replace `Catalog.Users` with an actual catalog name from Test 11.

**Expected result:** A number representing the count of items in the specified catalog.

**Verify:** The result is a non-negative integer.

---

## Test 19: Security — Origin Validation

**Action:** Send a request with a malicious Origin header:
```bash
curl -s -w "\nHTTP:%{http_code}" -X POST http://localhost:8888/mcp \
  -H "Content-Type: application/json" \
  -H "Origin: http://evil.example.com" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"evil","version":"1.0"}}}'
```

**Expected:** HTTP 403, body: `{"error":"Forbidden: invalid Origin"}`

**Verify:** The server rejects requests from non-local origins.

---

## Test 20: Unknown Session — Transparent Resurrection

**Action:** Send a request with a non-existent session ID:
```bash
curl -s -w "\nHTTP:%{http_code}" -X POST http://localhost:8888/mcp \
  -H "Content-Type: application/json" \
  -H "Mcp-Session-Id: 00000000-0000-0000-0000-000000000000" \
  -d '{"jsonrpc":"2.0","id":1,"method":"ping"}'
```

**Expected:** HTTP 200 with a normal `ping` result — an unknown session ID is **not** rejected with 404; the session is transparently **resurrected** (recreated under the presented ID). This keeps IDE clients alive across server restarts (they typically never re-initialize after a 404). With logging enabled, the log contains `MCP: unknown session ... resurrected (server restart?)`. See also Test 38.

---

## Test 21: Error Handling — Invalid JSON

```bash
curl -s -X POST http://localhost:8888/mcp \
  -H "Content-Type: application/json" \
  -H "Mcp-Session-Id: $SESSION_ID" \
  -d 'NOT A JSON'
```

**Expected:** JSON-RPC error code `-32700`, message `"Parse error"`.

---

## Test 22: Error Handling — Unknown Method

```bash
curl -s -X POST http://localhost:8888/mcp \
  -H "Content-Type: application/json" \
  -H "Mcp-Session-Id: $SESSION_ID" \
  -d '{"jsonrpc":"2.0","id":40,"method":"unknown/method"}'
```

**Expected:** JSON-RPC error code `-32601`, message `"Method not found: unknown/method"`.

---

## Test 23: DELETE Session

```bash
curl -s -w "\nHTTP:%{http_code}" -X DELETE http://localhost:8888/mcp \
  -H "Mcp-Session-Id: $SESSION_ID"
```

**Expected:** HTTP 200 — the server-side session state is freed. Note: a *subsequent* request that still presents this session ID is served again (the ID is transparently resurrected, see Test 20); `DELETE` releases resources, it does not blacklist the ID.

---

## Test 24: takeScreenshot — Current Process (Base64)

**Action:** Call the `takeScreenshot` tool with no arguments (or `pid`: `0`).

**Expected result:** A multi-part MCP content response:
- A text item: `"Captured N window(s) for PID ..."` where N ≥ 1
- For each window:
  - A text item with indented title, dimensions, and flags.
    Top-level windows: `"MainWindow (1200x800) [MAIN] [z=0]"`
    Owned windows are indented by 2 spaces per level and tagged with `[MODAL]`/`[MIN]`/`[MAX]`/`[DISABLED]` as applicable.
    All windows include `[z=N]` (Z-order) and `[owner=#N]` (owner array index) when applicable.
  - An image item with `type: "image"`, `data` (base64-encoded JPEG), `mimeType: "image/jpeg"`

**Verify:**
- At least one window is captured (the 1C main window)
- The main window text contains `[MAIN]` and `[z=0]`
- Image data is present and non-empty
- The base64 data decodes to a valid JPEG (`/9j/` prefix)

---

## Test 25: takeScreenshot — With Modal Dialog

**Action:**
1. First, open a modal dialog in 1C using the `execute` tool:
   `code: "ShowMessageBox(, ""Test modal dialog for screenshot"");"`
   (Note: this will block — use a timeout-aware approach or call `openForm` to trigger a modal)
2. From another client or tool, call `takeScreenshot` with the 1C process PID.

**Expected result:** Multiple windows captured with hierarchy info:
- The main 1C window: text line `"MainTitle (WxH) [MAIN] [z=0]"`
- The modal dialog: text line `"  DialogTitle (WxH) [MODAL] [z=1] [owner=#0]"` (indented 2 spaces = level 1)
- Each has its own separate screenshot image

**Verify:**
- `windowCount` ≥ 2
- At least one window text contains `[MODAL]` and is indented
- At least one window text contains `[MAIN]`
- The modal window text contains `[owner=#0]` referencing the main window at index 0
- Both windows have valid image data

---

## Test 26: takeScreenshot — PNG Format

**Action:** Call `takeScreenshot` with `format`: `"png"`.

**Expected result:** The response includes lossless PNG images instead of JPEG:
- Each window has `mimeType: "image/png"`
- Image data is valid base64 PNG (starts with `iVBOR` when decoded)

**Verify:**
- The response contains base64-encoded PNG images
- mimeType is `image/png` for each window

---

## Test 27: takeScreenshot — Invalid PID

**Action:** Call `takeScreenshot` with `pid`: `999999999` (a non-existent process).

**Expected result:** The tool returns with `isError: true` and a message like `"No visible windows found for PID 999999999"`.

**Verify:** The error is reported gracefully without crashing the server.

---

## Test 28: List Tools — Includes takeScreenshot

**Action:** Ask Copilot to list the tools available from `1c-mcp-server`.

**Expected:** Seven tools are listed (five original + `takeScreenshot` + `testScreenshot`):
- `getStatus`, `openForm`, `execute`, `evaluate`, `runLongTask`, `takeScreenshot`, `testScreenshot`

**Verify:**
- `takeScreenshot` is present with description mentioning screenshots and modal dialogs
- It has optional parameters `pid` (number), `format` (string), `quality` (number), and `grayscale` (boolean)
- Each window entry in the response includes `level`, `ownerIndex`, `zOrder`, `isEnabled`, `isMinimized`, `isMaximized`

---

## Test 29: takeScreenshot — Grayscale Mode

**Action:** Call `takeScreenshot` with `grayscale`: `true` (and optionally `format`: `"jpeg"`).

**Expected result:** The response contains the same windows as a color capture but the images are grayscale:
- Each window image has `mimeType: "image/jpeg"` (or `"image/png"` if format was `"png"`)
- The top-level JSON contains `"grayscale": true`
- Image file size should be noticeably smaller than the equivalent color JPEG

**Verify:**
- `windowCount` ≥ 1
- `grayscale` field in the result is `true`
- Image data is present and base64-encoded
- The decoded image visually contains no color (can verify by inspecting pixel channels are equal)

---

## Search Subsystem Tests (RAG)

These tests cover the `search` / `grep` / `get_segment` / `list_collections`
tools served natively by the component (see the
[Search Subsystem](README.md#search-subsystem-rag) docs).
They are **always advertised in `tools/list`** — so the tool counts in Test 1
and Test 28 are actually **+4** (the demo tools plus `search`, `grep`,
`get_segment`, `list_collections`).

**Distribution matters:**
- **lite** (`libhttp1cWin.dll` only) — the four tools are listed but every call
  returns a structured `rag_not_installed` result. Run Tests 30–31 only.
- **full** (`+ rcore.dll + DirectML.dll`) — the tools run for real. Run all of
  Tests 30–40.

**Prerequisite for the "full" tests:** the operator must have **indexed some
content first**. Indexing, `configure`, and `stats` are **1C-side admin
operations** (driven from the form via the component's `RagDispatch` method) —
they are *not* MCP tools and are not callable by the client. Use the form's
**Sync** button (or the headless self-test) to populate at least one collection
before searching. `list_collections` exists on both sides: as a `RagDispatch`
method for the operator and as the fourth native MCP tool (Test 37) — either
way it is how you confirm a collection reached `vector_status: ready`.

---

## Test 30: List Tools — Includes search / grep / get_segment / list_collections

**Action:** Ask Copilot to list the tools available from `1c-mcp-server`.

**Expected:** In addition to the demo + screenshot tools, four search tools are present:
- `search` — semantic / keyword / hybrid ranked search
- `grep` — RE2 regex / substring scan over stored text
- `get_segment` — O(1) line-range slice of a document by `doc_id`
- `list_collections` — the collection registry (no arguments)

**Verify:**
- All four tools appear with descriptions and an `inputSchema`.
- `search` exposes `query`, `collection`, `mode` (dense/keyword/hybrid), `k`, and meta-filter parameters.
- The schemas are identical whether the lite or full distribution is loaded.

---

## Test 31: search — Lite distribution / backend not installed

**Action:** On a **lite** install (or before `rcore.dll` is present), call `search` with `query`: `"anything"`.

**Expected result:** A structured, non-crashing result with `code: rag_not_installed` and a message telling the caller to install the RAG (full) package.

**Verify:**
- The server stays up; the error is a normal tool result, not a transport failure.
- The same applies to `grep`, `get_segment`, and `list_collections`.

> On a **full** install with a ready collection this call returns real hits instead — skip to Test 32.

---

## Test 32: grep — Works without vectors

**Action:** On a **full** install with at least one indexed document, call `grep` with `pattern`: a literal string you know is in the corpus (e.g. a step phrase or article code), `ignore_case`: `true`.

**Expected result:** Matching lines with `doc_id`, `name`, `line_number`, `line_text`, plus `total_found` and a `truncated` flag.

**Verify:**
- Matches are returned the instant a document is ingested — `grep` does not wait for embedding.
- An invalid regex returns a structured tool error (not a crashed session).

---

## Test 33: search — dense (semantic)

**Action:** Call `search` with `mode`: `"dense"`, a natural-language `query` describing intent (not an exact code), and optionally a `collection` from `list_collections`.

**Expected result:** Ranked hits with `doc_id`, `name`, `collection`, `score`, segment `line_start`/`line_end` (when available), and text/preview.

**Verify:**
- The semantically closest segment ranks at or near the top, even when no exact words match.
- Omitting `collection` searches all collections; passing a comma-separated list scopes to that subset.
- If the collection is still `building`, results come back flagged as partial/incomplete.

---

## Test 34: search — hybrid (exact identifier + intent)

**Action:** Call `search` with `mode`: `"hybrid"` and a `query` containing an exact identifier (SKU / article / INN / a precise step token).

**Expected result:** The record carrying that exact identifier ranks at the top — the keyword channel rescues what pure dense similarity would miss.

**Verify:**
- The exact-identifier hit outranks merely semantically-similar records.
- `hybrid` is the recommended mode for products/clients/QA-step lookups.

---

## Test 35: get_segment — Verbatim line range

**Action:** Take a `doc_id` + `line_start`/`line_end` from a Test 33 hit and call `get_segment` with them.

**Expected result:** The exact stored text for that line range, returned verbatim (1-based, inclusive line numbering).

**Verify:**
- The returned text matches the source lines exactly.
- An out-of-range request is **clamped** and the response reports the actual range returned (no error).

---

## Test 36: search — Metadata filters

**Action:** Call `search` with a meta filter — e.g. `all`: `{ "type": "scenario" }` (AND) or `any`: `{ "category": ["laptops", "phones"] }` (OR).

**Expected result:** Only hits whose `meta` satisfies the filter are returned.

**Verify:**
- `all` requires every clause to match; `any` requires at least one.
- Filters compose with `collection` scoping and with all three `mode`s.

---

## Test 37: list_collections — Collection registry via MCP

**Action:** On a **full** install with at least one indexed collection, call the `list_collections` tool (no arguments).

**Expected result:** A `collections` array, one entry per collection, each with `name`, `description`, `n_docs`, `n_segments`, `vector_status` (`empty`/`building`/`ready`/`error`), and `text_ready`.

**Verify:**
- Every collection indexed from the 1C side is present, sorted by name.
- `description` carries the human/AI-facing text supplied at ingest (empty if none was given).
- On a **lite** install the call returns `rag_not_installed`, like the other three tools.

---

## Test 38: Session Resurrection — Server restart does not strand clients

**Action:**
1. Initialize a session and save `$SESSION_ID` (as in Test 10).
2. In 1C, press **Disconnect**, then **Connect** again (server restart — all in-memory sessions are gone).
3. Re-send a request with the *old* session ID:
```bash
curl -s -w "\nHTTP:%{http_code}" -X POST http://localhost:8888/mcp \
  -H "Content-Type: application/json" \
  -H "Mcp-Session-Id: $SESSION_ID" \
  -d '{"jsonrpc":"2.0","id":1,"method":"ping"}'
```

**Expected:** HTTP 200 with a normal `ping` result — the old session is transparently recreated under the same ID; no 404, no re-initialize required.

**Verify:**
- The request succeeds immediately after the restart.
- With logging enabled, the log contains `MCP: unknown session <id> resurrected (server restart?)`.
- An MCP client (VS Code) that stayed configured across the restart keeps working without reconnecting.

---

## Test 39: index_raw — collection_description visible in the registry

**Action:** From the 1C side, ingest a raw document with a collection description via `RagDispatch`: `index_raw` with `{"collection":"desc_test","collection_description":"Throwaway docs for testing","name":"doc.txt","text":"line one\nline two"}` (the Vanessa plugin's file-collections table does exactly this per file). Then call the `list_collections` MCP tool.

**Expected result:** The `desc_test` entry carries `"description": "Throwaway docs for testing"`.

**Verify:**
- The same description is visible in the 1C-side `stats` payload.
- Same last-non-empty-wins semantics as `index_segments`: re-ingesting with an empty description keeps the old text; a new non-empty one replaces it.

---

## Test 40: search — Hits echo the effective segment meta

**Action:** Index (1C side) a document whose doc-level `meta` is `{"src":"doc","lang":"en"}` and one of whose segments carries `meta: {"lang":"ru"}`. Then `search` for that segment.

**Expected result:** The hit's `meta` is the **effective** view — doc meta overlaid by segment meta, segment wins on collision: `{"src":"doc","lang":"ru"}`.

**Verify:**
- Doc-only keys (`src`) are preserved; colliding keys take the segment value (`lang: ru`).
- This is the same view the `filter` clauses match against — `all: {"lang":"ru"}` matches the hit, and the returned `meta` shows why.

---

## Vanessa Plugin Tests (MCPRagSearch)

These tests target the **MCPRagSearch** plugin (see
[`vanessa-plugin/README.md`](vanessa-plugin/README.md)) rather than the demo
processor. Prerequisites: Vanessa Automation is open, the plugin form is
started in **MCP+RAG** mode (default port **8765**), and the indexing status
shows `Готово`. Adjust the URL/port in the curl examples accordingly.

---

## Test 41: Prompts — search-guide and the get_search_guide tool

**Action:**
1. Send `prompts/list` (curl, as in Test 13, against port 8765).
2. Send `prompts/get` with `{"name":"search-guide"}`.
3. Call the `get_search_guide` tool via the MCP client.

**Expected:**
- `prompts/list` returns one prompt: `search-guide` (title `Vanessa: как искать шаги (RAG)`), no arguments.
- `prompts/get` returns one `user` message whose text is the search skill (collections, search modes, workflow) from the `СкилПоиска` template.
- The `get_search_guide` tool returns the **same** skill text — it duplicates the prompt for clients that hide MCP prompts.

**Verify:**
- The prompt text and the tool text are identical.
- `prompts/get` also accepts the legacy name `search_guide`.

---

## Test 42: File Collections — Indexing from the form table

**Action:** Add a row to the «Коллекции файлов» table (e.g. collection `project_features`, a description, a folder with `.feature` files), press «Переиндексировать», wait for `Готово`. Then, via MCP: `list_collections`, `search` scoped to the collection, `grep` for a known literal, and `get_segment` with a `doc_id` (file path) + line range taken from a hit.

**Expected:**
- `list_collections` shows the collection with the table row's description and non-zero counts.
- `search`/`grep` hits carry `doc_id` = full file path and `meta.path`/`meta.ext`.
- `get_segment` returns the exact lines of the source file.

**Verify:** Files larger than 5 MB are skipped with a message; table rows without a collection name or folder are skipped.

---

## Test 43: vanessa_kb — Knowledge base collection

**Action:** After indexing, call `list_collections`, then `search` with `{"query":"как провести документ","collection":"vanessa_kb","mode":"dense"}`.

**Expected:**
- `vanessa_kb` is present in the registry (one document; one segment per Q&A entry from the VA `БазаЗнанийДляMCP` template).
- Hits are question+answer segments; `meta.type` is `qa` and `meta.num` is the question number.

**Verify:** The top hit's text starts with `Вопрос:` and contains a relevant answer.

---

## Test 44: Watcher — Automatic reindex on language / step library change

**Action:** With the plugin running in MCP+RAG mode, change the Gherkin generator language in VA settings (or load additional step libraries so the step count changes). Wait up to 30 seconds.

**Expected:** The plugin notices the change (the watcher polls every 30 s), reports it («Язык генератора Gherkin изменился…» / «Библиотека шагов изменилась…») and reindexes `vanessa_steps` automatically.

**Verify:**
- After reindexing completes, `vanessa_steps` hits carry the new `meta.lang`.
- The watcher only runs while the transport is started in MCP+RAG mode.

---

## Summary Checklist

After completing all tests, verify:

- [ ] All 7 tools are visible and callable
- [ ] `getStatus` returns valid status with correct counts
- [ ] `evaluate` correctly computes expressions
- [ ] `execute` runs BSL code and returns results
- [ ] `execute` properly reports errors (division by zero)
- [ ] `openForm` handles invalid forms gracefully
- [ ] `openForm` successfully opens an existing form
- [ ] `runLongTask` completes with progress and returns the expected step count
- [ ] `takeScreenshot` captures the current process main window with base64 JPEG
- [ ] `takeScreenshot` captures modal dialogs separately with `[MODAL]` tag and indented hierarchy
- [ ] `takeScreenshot` window text includes `[z=N]`, `[owner=#N]`, `[DISABLED]`/`[MIN]`/`[MAX]` when applicable
- [ ] `takeScreenshot` supports PNG format when `format` is `"png"`
- [ ] `takeScreenshot` supports grayscale mode when `grayscale` is `true`
- [ ] `takeScreenshot` handles invalid PID gracefully
- [ ] Both resources are listed
- [ ] Catalog metadata resource returns valid data
- [ ] Document metadata resource returns valid data
- [ ] Both prompts are listed with correct arguments
- [ ] `analyze1CData` prompt generates contextual messages
- [ ] `generate1CCode` prompt generates coding instructions
- [ ] Server remains stable after all tests
- [ ] Cross-validation: catalog count from `evaluate` matches resource data
- [ ] Origin validation blocks external domains (HTTP 403)
- [ ] Unknown session IDs are transparently resurrected (HTTP 200, no 404)
- [ ] Invalid JSON returns parse error (-32700)
- [ ] Unknown methods return method not found (-32601)
- [ ] DELETE terminates sessions
- [ ] `search` / `grep` / `get_segment` / `list_collections` appear in `tools/list` (both distributions)
- [ ] Lite distribution returns `rag_not_installed` (not a crash) for all four
- [ ] `grep` matches work before embedding finishes; bad regex is a structured error
- [ ] `search mode=dense` ranks the semantically closest segment near the top
- [ ] `search mode=hybrid` surfaces the exact-identifier record (SKU/INN/article)
- [ ] `get_segment` returns the exact line range and clamps out-of-range requests
- [ ] `search` meta filters (`all`/`any`) and `collection` scoping work
- [ ] `list_collections` returns the registry with names, descriptions, counts and statuses
- [ ] An old session ID still works after a server restart (resurrection)
- [ ] `index_raw` `collection_description` shows up in `list_collections` / `stats`
- [ ] Search hits echo the effective meta (doc overlaid by segment, segment wins)
- [ ] Plugin: `search-guide` prompt and `get_search_guide` tool return the same skill text
- [ ] Plugin: file collections are indexed and searchable (`search`/`grep`/`get_segment`)
- [ ] Plugin: `vanessa_kb` is present with Q&A segments (`meta.type=qa`, `meta.num`)
- [ ] Plugin: watcher auto-reindexes on Gherkin language / step library change

## Troubleshooting

| Symptom | Likely Cause | Fix |
|---------|-------------|-----|
| No tools visible | Server not running | Press "Connect" in 1C |
| Connection refused | Wrong port | Check port in 1C form and mcp.json |
| Tool call timeout | 1C is processing a long operation | Increase timeout or wait |
| `isError: true` on execute | BSL syntax error | Check the 1C/BSL code syntax |
| Empty resource data | Empty infobase | Use an infobase with actual catalogs/documents |
| Prompt returns no metadata | Server-side error | Call `getStatus` to check the component state |
| `search`/`grep` return `rag_not_installed` | Lite distribution (no `rcore.dll`) | Install the full RAG package (`rcore.dll` + `DirectML.dll` beside the component) |
| `search` returns empty on full install | No content indexed yet, or collection still `building` | Index via the form's Sync button; poll `list_collections` until `vector_status: ready` |
| `search mode=dense` misses exact codes | Embeddings are weak on identifiers | Use `mode: hybrid` for SKU/INN/article/step lookups |
