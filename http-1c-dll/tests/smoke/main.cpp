// ===========================================================================
// http1c_smoke — automated black-box smoke harness for the 1C Native API
// component (libhttp1cWin.dll).
//
// Loads the BUILT DLL exactly like 1C:Enterprise does:
//   LoadLibrary -> GetClassObject("HttpServer") -> setMemManager -> Init
// with minimal stub implementations of the 1C-side interfaces
// (IMemoryManager, IAddInDefBase/IAddInDefBaseEx), drives the component
// over real HTTP (cpp-httplib client against the component's MCP endpoint)
// and asserts the behaviors:
//
//   T1  load + init + GetProcessId plumbing
//   T2  ApplyConfig(tools_json, timeout) + StartListen + MCP initialize
//   T3  tools/list = 1C tools UNION native search tools, no duplicates
//   T4  RegisterTools reserved-name rejection (AddError, cache unchanged)
//   T5  unknown Mcp-Session-Id is transparently resurrected (200, not 404)
//   T6  Bearer auth 401/200 + concurrent requests vs ApplyConfig token flips
//   T7  tools/call timeout: ExternalEvent fired, "1 second" error, no hang
//   T8  UTF-8 round-trip of Russian tool name/description
//   T9  native list_collections callable (ok-collections OR rag_not_installed)
//   T10 late rcore.dll install: lite copy answers rag_not_installed (message
//       names all 4 native tools), then rcore.dll is copied in and the SAME
//       loaded module flips to full — no process restart (loader retry)
//
// Usage: http1c_smoke.exe [path\to\libhttp1cWin.dll]
//   Default DLL path: "..\bin\libhttp1cWin.dll" relative to the exe
//   (falls back to "libhttp1cWin.dll" next to the exe).
//
// Exit code: 0 = all checks passed, otherwise the number of failed checks.
// ===========================================================================

#ifndef _WINDOWS
#define _WINDOWS // SDK headers key off this: WCHAR_T = wchar_t, ADDIN_API = __stdcall
#endif

#include "ComponentBase.h"
#include "AddInDefBase.h"
#include "IMemoryManager.h"

#include "httplib.h" // also pulls in winsock2/ws2tcpip and runs WSAStartup
#include "json.hpp"

#include <atomic>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <mutex>
#include <set>
#include <string>
#include <thread>
#include <vector>

using json = nlohmann::json;

// ===========================================================================
// Tiny assertion framework
// ===========================================================================

static int g_checks = 0;
static int g_failures = 0;
static bool g_fatal = false; // set when continuing makes no sense (DLL/listen down)

static bool checkImpl(bool ok, const std::string& msg, int line) {
    ++g_checks;
    if (!ok) {
        ++g_failures;
        std::printf("    CHECK FAILED (main.cpp:%d): %s\n", line, msg.c_str());
        std::fflush(stdout);
    }
    return ok;
}

#define CHECK(cond, msg) checkImpl(!!(cond), (msg), __LINE__)
// Failing a REQUIRE marks the whole run fatal (subsequent tests are skipped).
#define REQUIRE(cond, msg) do { if (!checkImpl(!!(cond), (msg), __LINE__)) g_fatal = true; } while (0)

template <typename Fn>
static void runTest(const char* name, Fn&& fn) {
    std::printf("=== %s ===\n", name);
    std::fflush(stdout);
    if (g_fatal) {
        std::printf("SKIP: %s (previous fatal failure)\n", name);
        std::fflush(stdout);
        return;
    }
    const int before = g_failures;
    try {
        fn();
    } catch (const std::exception& e) {
        CHECK(false, std::string("unhandled exception: ") + e.what());
    } catch (...) {
        CHECK(false, "unhandled non-standard exception");
    }
    std::printf("%s: %s\n", (g_failures == before) ? "PASS" : "FAIL", name);
    std::fflush(stdout);
}

// ===========================================================================
// UTF helpers — on Windows WCHAR_T is 16-bit wchar_t; convert via Win32.
// ===========================================================================

static std::wstring utf8ToWide(const std::string& s) {
    if (s.empty()) return std::wstring();
    const int n = MultiByteToWideChar(CP_UTF8, 0, s.data(), (int)s.size(), nullptr, 0);
    if (n <= 0) return std::wstring();
    std::wstring w((size_t)n, L'\0');
    MultiByteToWideChar(CP_UTF8, 0, s.data(), (int)s.size(), &w[0], n);
    return w;
}

static std::string wideToUtf8(const wchar_t* p) {
    if (!p || !*p) return std::string();
    const int wlen = (int)wcslen(p);
    const int n = WideCharToMultiByte(CP_UTF8, 0, p, wlen, nullptr, 0, nullptr, nullptr);
    if (n <= 0) return std::string();
    std::string s((size_t)n, '\0');
    WideCharToMultiByte(CP_UTF8, 0, p, wlen, &s[0], n, nullptr, nullptr);
    return s;
}

// ===========================================================================
// 1C interface stubs
// ===========================================================================

// IMemoryManager: malloc/free. The component allocates return-value strings
// through this; the harness (playing 1C) frees them after reading.
class MemoryManagerStub : public IMemoryManager {
public:
    bool ADDIN_API AllocMemory(void** pMemory, unsigned long ulCountByte) override {
        if (!pMemory) return false;
        *pMemory = std::malloc(ulCountByte ? ulCountByte : 1);
        return *pMemory != nullptr;
    }
    void ADDIN_API FreeMemory(void** pMemory) override {
        if (pMemory && *pMemory) {
            std::free(*pMemory);
            *pMemory = nullptr;
        }
    }
};

// IAddInDefBaseEx: captures AddError calls and ExternalEvent notifications.
// ExternalEvent arrives from httplib worker threads -> mutex-guarded.
class ConnectionStub : public IAddInDefBaseEx {
public:
    struct ErrorRecord { std::string source, descr; };
    struct EventRecord { std::string source, event, data; };

    bool ADDIN_API AddError(unsigned short /*wcode*/, const WCHAR_T* source,
                            const WCHAR_T* descr, long /*scode*/) override {
        std::lock_guard<std::mutex> lock(mtx_);
        errors_.push_back({ wideToUtf8(source), wideToUtf8(descr) });
        return true;
    }
    bool ADDIN_API Read(WCHAR_T*, tVariant*, long*, WCHAR_T**) override { return false; }
    bool ADDIN_API Write(WCHAR_T*, tVariant*) override { return false; }
    bool ADDIN_API RegisterProfileAs(WCHAR_T*) override { return true; }
    bool ADDIN_API SetEventBufferDepth(long depth) override { depth_ = depth; return true; }
    long ADDIN_API GetEventBufferDepth() override { return depth_; }
    bool ADDIN_API ExternalEvent(WCHAR_T* source, WCHAR_T* message, WCHAR_T* data) override {
        std::lock_guard<std::mutex> lock(mtx_);
        events_.push_back({ wideToUtf8(source), wideToUtf8(message), wideToUtf8(data) });
        return true;
    }
    void ADDIN_API CleanEventBuffer() override {}
    bool ADDIN_API SetStatusLine(WCHAR_T*) override { return true; }
    void ADDIN_API ResetStatusLine() override {}
    IInterface* ADDIN_API GetInterface(Interfaces) override { return nullptr; }

    void clearErrors() { std::lock_guard<std::mutex> l(mtx_); errors_.clear(); }
    void clearEvents() { std::lock_guard<std::mutex> l(mtx_); events_.clear(); }
    std::vector<ErrorRecord> errors() { std::lock_guard<std::mutex> l(mtx_); return errors_; }
    std::vector<EventRecord> events() { std::lock_guard<std::mutex> l(mtx_); return events_; }

private:
    std::mutex mtx_;
    std::vector<ErrorRecord> errors_;
    std::vector<EventRecord> events_;
    long depth_ = 0;
};

// ===========================================================================
// tVariant helpers
// ===========================================================================

static void setVariantString(tVariant& v, const std::wstring& s) {
    tVarInit(&v);
    v.vt = VTYPE_PWSTR;
    v.pwstrVal = const_cast<WCHAR_T*>(s.c_str()); // null-terminated, harness-owned
    v.wstrLen = (uint32_t)s.size();
}

static void setVariantInt(tVariant& v, int32_t value) {
    tVarInit(&v);
    v.vt = VTYPE_I4;
    v.lVal = value;
}

// Free a return value allocated by the component via MemoryManagerStub.
static void freeVariant(tVariant& v) {
    if ((v.vt == VTYPE_PWSTR || v.vt == VTYPE_BLOB) && v.pwstrVal) {
        std::free(v.pwstrVal);
    }
    tVarInit(&v);
}

static std::string variantToUtf8(const tVariant& v) {
    if (v.vt == VTYPE_PWSTR && v.pwstrVal) return wideToUtf8(v.pwstrVal);
    return std::string();
}

static long long variantToInt(const tVariant& v) {
    switch (v.vt) {
    case VTYPE_I2: return v.shortVal;
    case VTYPE_I4: return v.lVal;
    case VTYPE_R8: return (long long)v.dblVal;
    case VTYPE_I8: return v.llVal;
    default: return 0;
    }
}

// ===========================================================================
// Component harness — loads the DLL and dispatches methods like 1C does.
// ===========================================================================

struct Harness {
    HMODULE dll = nullptr;
    GetClassObjectPtr getClassObject = nullptr;
    DestroyObjectPtr destroyObject = nullptr;
    GetClassNamesPtr getClassNames = nullptr;
    IComponentBase* comp = nullptr;
    MemoryManagerStub mem;
    ConnectionStub conn;

    bool load(const std::wstring& dllPath) {
        dll = ::LoadLibraryW(dllPath.c_str());
        if (!dll) return false;
        getClassObject = (GetClassObjectPtr)::GetProcAddress(dll, "GetClassObject");
        destroyObject = (DestroyObjectPtr)::GetProcAddress(dll, "DestroyObject");
        getClassNames = (GetClassNamesPtr)::GetProcAddress(dll, "GetClassNames");
        return getClassObject && destroyObject && getClassNames;
    }

    long findMethod(const wchar_t* nameEn) {
        return comp ? comp->FindMethod((const WCHAR_T*)nameEn) : -1;
    }

    // Calls a method as a procedure (no return value).
    bool callProc(const wchar_t* nameEn, tVariant* params, long count) {
        const long num = findMethod(nameEn);
        if (num < 0) return false;
        return comp->CallAsProc(num, params, count);
    }

    // Calls a method as a function; caller owns/frees *ret via freeVariant.
    bool callFunc(const wchar_t* nameEn, tVariant* ret, tVariant* params, long count) {
        const long num = findMethod(nameEn);
        if (num < 0) return false;
        tVarInit(ret);
        return comp->CallAsFunc(num, ret, params, count);
    }

    // ApplyConfig with a JSON object built by the caller. Async methods in this
    // component execute synchronously inside CallAsProc (1C wraps the async
    // part), so a plain call is the real production code path.
    bool applyConfig(const json& cfg) {
        const std::wstring w = utf8ToWide(cfg.dump());
        tVariant p;
        setVariantString(p, w);
        return callProc(L"ApplyConfig", &p, 1);
    }

    void destroy() {
        if (comp && destroyObject) {
            destroyObject(&comp);
            comp = nullptr;
        }
        // Intentionally no FreeLibrary: rcore.dll (if loaded) keeps background
        // state; the process is about to exit anyway and the OS reclaims it.
    }
};

// ===========================================================================
// HTTP helpers
// ===========================================================================

static std::unique_ptr<httplib::Client> makeClient(int port) {
    auto cli = std::make_unique<httplib::Client>("127.0.0.1", port);
    cli->set_connection_timeout(5, 0);
    cli->set_read_timeout(15, 0);
    cli->set_write_timeout(5, 0);
    return cli;
}

struct RpcOptions {
    std::string sessionId;   // adds Mcp-Session-Id when non-empty
    std::string bearerToken; // adds Authorization: Bearer ... when non-empty
    std::string accept = "application/json";
};

static httplib::Result mcpPost(httplib::Client& cli, const json& rpc, const RpcOptions& opt = {}) {
    httplib::Headers headers;
    headers.emplace("Accept", opt.accept);
    if (!opt.sessionId.empty()) headers.emplace("Mcp-Session-Id", opt.sessionId);
    if (!opt.bearerToken.empty()) headers.emplace("Authorization", "Bearer " + opt.bearerToken);
    return cli.Post("/mcp", headers, rpc.dump(), "application/json");
}

static json rpcRequest(const std::string& method, int id, const json& params = json::object()) {
    json rpc;
    rpc["jsonrpc"] = "2.0";
    rpc["id"] = id;
    rpc["method"] = method;
    if (!params.empty()) rpc["params"] = params;
    return rpc;
}

// Bind-probe 127.0.0.1:<port>; returns the first free port from the range.
static int pickFreePort() {
    for (int port = 18876; port < 18896; ++port) {
        SOCKET s = ::socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
        if (s == INVALID_SOCKET) continue;
        sockaddr_in addr;
        std::memset(&addr, 0, sizeof(addr));
        addr.sin_family = AF_INET;
        addr.sin_port = htons((u_short)port);
        ::inet_pton(AF_INET, "127.0.0.1", &addr.sin_addr);
        const int rc = ::bind(s, (sockaddr*)&addr, sizeof(addr));
        ::closesocket(s);
        if (rc == 0) return port;
    }
    return 18876; // last resort; StartListen will fail loudly if busy
}

// Extract tool names from a tools/list response body.
static std::vector<std::string> toolNames(const json& body) {
    std::vector<std::string> names;
    if (body.contains("result") && body["result"].contains("tools")) {
        for (const auto& t : body["result"]["tools"]) {
            names.push_back(t.value("name", std::string()));
        }
    }
    return names;
}

// Directory of the running exe, with trailing backslash.
static std::wstring exeDir() {
    wchar_t buf[MAX_PATH * 4] = L"";
    ::GetModuleFileNameW(nullptr, buf, (DWORD)(sizeof(buf) / sizeof(buf[0])));
    std::wstring path(buf);
    const size_t slash = path.find_last_of(L"\\/");
    return (slash == std::wstring::npos) ? std::wstring() : path.substr(0, slash + 1);
}

static bool fileExists(const std::wstring& p) {
    const DWORD attrs = ::GetFileAttributesW(p.c_str());
    return attrs != INVALID_FILE_ATTRIBUTES && !(attrs & FILE_ATTRIBUTE_DIRECTORY);
}

// ===========================================================================
// main — tests run sequentially against one component instance.
// ===========================================================================

int wmain(int argc, wchar_t** argv) {
    Harness h;
    int port = 0;
    std::string sessionId;

    std::wstring dllPath;
    if (argc > 1) {
        dllPath = argv[1];
    } else {
        dllPath = exeDir() + L"..\\bin\\libhttp1cWin.dll";
        if (!fileExists(dllPath)) dllPath = exeDir() + L"libhttp1cWin.dll";
    }
    std::printf("DLL under test: %s\n", wideToUtf8(dllPath.c_str()).c_str());

    // -----------------------------------------------------------------------
    // T1: load + init + call plumbing sanity (GetProcessId).
    // -----------------------------------------------------------------------
    runTest("T1 load+init", [&] {
        REQUIRE(h.load(dllPath), "LoadLibrary + GetProcAddress(GetClassObject/DestroyObject/GetClassNames)");
        if (g_fatal) return;

        const std::string classNames = wideToUtf8(h.getClassNames());
        CHECK(classNames.find("HttpServer") != std::string::npos,
              "GetClassNames contains HttpServer, got: " + classNames);

        h.comp = nullptr;
        h.getClassObject(L"HttpServer", &h.comp);
        REQUIRE(h.comp != nullptr, "GetClassObject(\"HttpServer\") returned an instance");
        if (g_fatal) return;

        REQUIRE(h.comp->setMemManager(&h.mem), "setMemManager accepted the stub");
        REQUIRE(h.comp->Init(static_cast<IAddInDefBase*>(&h.conn)), "Init(IAddInDefBase*) returned true");
        CHECK(h.comp->GetInfo() == 2000, "GetInfo() == 2000");

        tVariant ret;
        REQUIRE(h.callFunc(L"GetProcessId", &ret, nullptr, 0), "CallAsFunc(GetProcessId)");
        CHECK((unsigned long long)variantToInt(ret) == (unsigned long long)::GetCurrentProcessId(),
              "GetProcessId returned the current pid");
        freeVariant(ret);
    });

    // -----------------------------------------------------------------------
    // T2: ApplyConfig (tools_json + timeout, logging off) -> StartListen ->
    //     HTTP MCP initialize returns serverInfo + Mcp-Session-Id header.
    // -----------------------------------------------------------------------
    runTest("T2 config+listen+initialize", [&] {
        json tool = {
            {"name", "my1cTool"},
            {"description", "d"},
            {"inputSchema", {{"type", "object"}, {"properties", json::object()}}}
        };
        json cfg;
        cfg["logging_enabled"] = false; // keep the harness from writing log files
        cfg["tools_json"] = json::array({ tool }).dump();
        cfg["timeout"] = 2;
        REQUIRE(h.applyConfig(cfg), "ApplyConfig(tools_json, timeout)");

        port = pickFreePort();
        std::printf("    listening port: %d\n", port);
        tVariant pPort, ret;
        setVariantInt(pPort, port);
        REQUIRE(h.callFunc(L"StartListen", &ret, &pPort, 1), "CallAsFunc(StartListen)");
        freeVariant(ret);
        if (g_fatal) return;

        // The listener binds on a background thread — poll /health until live.
        auto cli = makeClient(port);
        bool up = false;
        for (int i = 0; i < 100 && !up; ++i) {
            auto res = cli->Get("/health");
            if (res && res->status == 200) up = true;
            else std::this_thread::sleep_for(std::chrono::milliseconds(50));
        }
        REQUIRE(up, "GET /health responds 200 after StartListen");
        if (g_fatal) return;

        json params = {
            {"protocolVersion", "2025-03-26"},
            {"capabilities", json::object()},
            {"clientInfo", {{"name", "http1c_smoke"}, {"version", "1.0"}}}
        };
        RpcOptions opt;
        opt.accept = "application/json, text/event-stream";
        auto res = mcpPost(*cli, rpcRequest("initialize", 1, params), opt);
        REQUIRE(res && res->status == 200, "POST /mcp initialize -> 200");
        if (g_fatal) return;

        const json body = json::parse(res->body, nullptr, false);
        CHECK(!body.is_discarded() && body.contains("result"), "initialize: JSON-RPC result present");
        CHECK(body["result"].contains("serverInfo") &&
              body["result"]["serverInfo"].value("name", std::string()) == "1c-mcp-server",
              "initialize: serverInfo.name == 1c-mcp-server");
        sessionId = res->get_header_value("Mcp-Session-Id");
        REQUIRE(!sessionId.empty(), "initialize: Mcp-Session-Id header present");
    });

    // -----------------------------------------------------------------------
    // T3: tools/list = union of the 1C tool and the 4 native search tools,
    //     all names unique, count exactly 5.
    // -----------------------------------------------------------------------
    runTest("T3 tools/list union, no duplicates", [&] {
        auto cli = makeClient(port);
        RpcOptions opt; opt.sessionId = sessionId;
        auto res = mcpPost(*cli, rpcRequest("tools/list", 2), opt);
        REQUIRE(res && res->status == 200, "tools/list -> 200");
        if (g_fatal) return;

        const json body = json::parse(res->body, nullptr, false);
        const auto names = toolNames(body);
        const std::set<std::string> unique(names.begin(), names.end());

        CHECK(names.size() == 5, "tools/list returns exactly 5 tools, got " + std::to_string(names.size()));
        CHECK(unique.size() == names.size(), "all tool names are unique");
        for (const char* expected : {"my1cTool", "search", "grep", "get_segment", "list_collections"}) {
            CHECK(unique.count(expected) == 1, std::string("tools/list contains ") + expected);
        }
    });

    // -----------------------------------------------------------------------
    // T4: registering a tool named like a native one is rejected with AddError
    //     ("reserved...") and the previous cache stays intact.
    // -----------------------------------------------------------------------
    runTest("T4 reserved-name rejection", [&] {
        h.conn.clearErrors();
        json tool = {
            {"name", "search"},
            {"description", "imposter"},
            {"inputSchema", {{"type", "object"}, {"properties", json::object()}}}
        };
        json cfg;
        cfg["tools_json"] = json::array({ tool }).dump();
        CHECK(h.applyConfig(cfg), "ApplyConfig itself succeeds (rejection is reported via AddError)");

        bool sawReserved = false;
        for (const auto& e : h.conn.errors()) {
            if (e.descr.find("reserved") != std::string::npos) sawReserved = true;
        }
        CHECK(sawReserved, "AddError captured with message containing \"reserved\"");

        // Cache unchanged: still the previous 5 tools incl. my1cTool.
        auto cli = makeClient(port);
        RpcOptions opt; opt.sessionId = sessionId;
        auto res = mcpPost(*cli, rpcRequest("tools/list", 3), opt);
        REQUIRE(res && res->status == 200, "tools/list after rejection -> 200");
        if (g_fatal) return;
        const auto names = toolNames(json::parse(res->body, nullptr, false));
        const std::set<std::string> unique(names.begin(), names.end());
        CHECK(names.size() == 5, "tool cache unchanged: still 5 tools");
        CHECK(unique.count("my1cTool") == 1, "tool cache unchanged: my1cTool still present");
    });

    // -----------------------------------------------------------------------
    // T5: unknown session id is transparently resurrected (no 404).
    // -----------------------------------------------------------------------
    runTest("T5 session resurrection", [&] {
        auto cli = makeClient(port);
        RpcOptions opt; opt.sessionId = "bogus-id-123";
        auto res = mcpPost(*cli, rpcRequest("tools/list", 4), opt);
        REQUIRE((bool)res, "tools/list with bogus session got a response");
        if (g_fatal) return;
        CHECK(res->status == 200, "bogus Mcp-Session-Id -> HTTP 200 (resurrected), got " + std::to_string(res->status));
        const json body = json::parse(res->body, nullptr, false);
        CHECK(!body.is_discarded() && body.contains("result") &&
              body["result"].contains("tools") && body["result"]["tools"].is_array() &&
              !body["result"]["tools"].empty(),
              "resurrected session still serves a valid tools result");
    });

    // -----------------------------------------------------------------------
    // T5b: the session map is capped — a flood of unknown ids cannot grow it
    //      without bound (LRU eviction at MAX_SESSIONS=256). Paced to respect
    //      the component's token-bucket rate limiter, so this test takes ~12s.
    // -----------------------------------------------------------------------
    runTest("T5b session cap under id flood", [&] {
        auto cli = makeClient(port);
        int served = 0;
        const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(45);
        for (int i = 0; i < 300 && std::chrono::steady_clock::now() < deadline; ++i) {
            RpcOptions opt; opt.sessionId = "flood-" + std::to_string(i);
            for (;;) {
                auto res = mcpPost(*cli, rpcRequest("tools/list", 1000 + i), opt);
                if (res && res->status == 429) { // rate limiter — wait for refill
                    std::this_thread::sleep_for(std::chrono::milliseconds(100));
                    if (std::chrono::steady_clock::now() >= deadline) break;
                    continue;
                }
                if (res && res->status == 200) ++served;
                break;
            }
        }
        CHECK(served >= 280, "id flood mostly served, got " + std::to_string(served) + "/300");

        tVariant ret;
        REQUIRE(h.callFunc(L"GetStatus", &ret, nullptr, 0), "CallAsFunc(GetStatus)");
        if (g_fatal) return;
        const json st = json::parse(variantToUtf8(ret), nullptr, false);
        freeVariant(ret);
        REQUIRE(!st.is_discarded() && st.contains("active_sessions"),
                "GetStatus returns active_sessions");
        if (g_fatal) return;
        const int active = st["active_sessions"].get<int>();
        CHECK(active <= 256, "sessions capped at 256 (active=" + std::to_string(active) + ")");
        CHECK(active >= 200, "cap actually engaged near the limit (active=" + std::to_string(active) + ")");
    });

    // -----------------------------------------------------------------------
    // T6: Bearer auth + concurrency smoke against ApplyConfig token flips.
    // -----------------------------------------------------------------------
    runTest("T6 auth + concurrent token flips", [&] {
        json cfg; cfg["auth_token"] = "sekret";
        REQUIRE(h.applyConfig(cfg), "ApplyConfig(auth_token=sekret)");

        auto cli = makeClient(port);
        RpcOptions noAuth; noAuth.sessionId = sessionId;
        auto res = mcpPost(*cli, rpcRequest("tools/list", 5), noAuth);
        REQUIRE((bool)res, "request without Authorization got a response");
        if (!g_fatal) CHECK(res->status == 401, "no Authorization -> 401, got " + std::to_string(res->status));

        RpcOptions withAuth; withAuth.sessionId = sessionId; withAuth.bearerToken = "sekret";
        res = mcpPost(*cli, rpcRequest("tools/list", 6), withAuth);
        REQUIRE((bool)res, "request with Bearer sekret got a response");
        if (!g_fatal) CHECK(res->status == 200, "Bearer sekret -> 200, got " + std::to_string(res->status));
        if (g_fatal) return;

        // Concurrency smoke: 4 threads x 50 requests alternating valid token /
        // no token, while the main thread flips auth_token between "sekret" and
        // "" twenty times. Every response must be 200/401/429 — 429 comes from
        // the component's own token-bucket rate limiter (60 burst, 20 rps) that
        // 200 rapid-fire requests legitimately trip; anything else (5xx,
        // connection error, hang) is a failure.
        std::atomic<int> bad{0}, ok200{0}, unauth401{0}, limited429{0}, connErr{0};
        std::vector<std::thread> threads;
        for (int t = 0; t < 4; ++t) {
            threads.emplace_back([&, t] {
                auto threadCli = makeClient(port);
                for (int i = 0; i < 50; ++i) {
                    RpcOptions opt;
                    opt.sessionId = sessionId;
                    if (i % 2 == 0) opt.bearerToken = "sekret";
                    auto r = mcpPost(*threadCli, rpcRequest("tools/list", 100 + t * 50 + i), opt);
                    if (!r) { ++connErr; ++bad; continue; }
                    switch (r->status) {
                    case 200: ++ok200; break;
                    case 401: ++unauth401; break;
                    case 429: ++limited429; break;
                    default: ++bad; break;
                    }
                }
            });
        }
        for (int i = 0; i < 20; ++i) {
            json flip; flip["auth_token"] = (i % 2 == 0) ? "" : "sekret";
            h.applyConfig(flip);
            std::this_thread::sleep_for(std::chrono::milliseconds(25));
        }
        // End with auth ON so trailing no-token requests still see 401s.
        { json flip; flip["auth_token"] = "sekret"; h.applyConfig(flip); }
        for (auto& th : threads) th.join();

        std::printf("    concurrency: 200=%d 401=%d 429=%d connErr=%d other=%d\n",
            ok200.load(), unauth401.load(), limited429.load(), connErr.load(),
            bad.load() - connErr.load());
        CHECK(bad.load() == 0, "no 5xx / connection errors / unexpected statuses under concurrent token flips");
        CHECK(ok200.load() > 0, "at least one 200 observed");
        CHECK(unauth401.load() > 0, "at least one 401 observed");

        // Disable auth for the remaining tests.
        json off; off["auth_token"] = "";
        REQUIRE(h.applyConfig(off), "ApplyConfig(auth_token=\"\") resets auth");
    });

    // -----------------------------------------------------------------------
    // T7: tools/call of a 1C tool with timeout=1 — ExternalEvent fired, and
    //     with nobody answering the call returns the timeout error promptly.
    // -----------------------------------------------------------------------
    runTest("T7 timeout + ExternalEvent", [&] {
        json cfg; cfg["timeout"] = 1;
        REQUIRE(h.applyConfig(cfg), "ApplyConfig(timeout=1)");
        h.conn.clearEvents();

        json params = {
            {"name", "my1cTool"},
            {"arguments", json::object()}
        };
        auto cli = makeClient(port);
        RpcOptions opt; opt.sessionId = sessionId; // plain JSON (no SSE): no progressToken
        const auto t0 = std::chrono::steady_clock::now();
        auto res = mcpPost(*cli, rpcRequest("tools/call", 7, params), opt);
        const auto elapsedMs = std::chrono::duration_cast<std::chrono::milliseconds>(
            std::chrono::steady_clock::now() - t0).count();

        REQUIRE(res && res->status == 200, "tools/call my1cTool -> 200 (timeout is a tool-level error)");
        if (g_fatal) return;
        CHECK(elapsedMs < 3000, "timeout response arrived within ~3s (took " + std::to_string(elapsedMs) + "ms)");

        const json body = json::parse(res->body, nullptr, false);
        bool isError = false;
        std::string text;
        if (!body.is_discarded() && body.contains("result")) {
            isError = body["result"].value("isError", false);
            if (body["result"].contains("content") && !body["result"]["content"].empty()) {
                text = body["result"]["content"][0].value("text", std::string());
            }
        }
        CHECK(isError, "timeout reported as result.isError = true");
        CHECK(text.find("did not respond within 1 second") != std::string::npos,
              "timeout message mentions \"1 second\", got: " + text);

        bool sawToolCall = false;
        for (const auto& e : h.conn.events()) {
            if (e.source == "HttpServer" && e.event == "ToolCall") {
                const json data = json::parse(e.data, nullptr, false);
                if (!data.is_discarded() && data.value("tool", std::string()) == "my1cTool") {
                    sawToolCall = true;
                }
            }
        }
        CHECK(sawToolCall, "ExternalEvent(HttpServer, ToolCall, {tool: my1cTool}) captured by the stub");
    });

    // -----------------------------------------------------------------------
    // T8: UTF-8 round-trip — Russian tool name/description survive
    //     MB2WCHAR/WCHAR2MB byte-identically.
    // -----------------------------------------------------------------------
    runTest("T8 UTF-8 round-trip", [&] {
        const std::string ruName = (const char*)u8"инструмент_тест";
        const std::string ruDescr = (const char*)u8"Описание по-русски ёЁ";
        json tool = {
            {"name", ruName},
            {"description", ruDescr},
            {"inputSchema", {{"type", "object"}, {"properties", json::object()}}}
        };
        json cfg;
        cfg["tools_json"] = json::array({ tool }).dump();
        REQUIRE(h.applyConfig(cfg), "ApplyConfig(tools_json with Russian strings)");

        auto cli = makeClient(port);
        RpcOptions opt; opt.sessionId = sessionId;
        auto res = mcpPost(*cli, rpcRequest("tools/list", 8), opt);
        REQUIRE(res && res->status == 200, "tools/list -> 200");
        if (g_fatal) return;

        const json body = json::parse(res->body, nullptr, false);
        bool found = false;
        std::string gotDescr;
        if (!body.is_discarded() && body.contains("result") && body["result"].contains("tools")) {
            for (const auto& t : body["result"]["tools"]) {
                if (t.value("name", std::string()) == ruName) {
                    found = true;
                    gotDescr = t.value("description", std::string());
                }
            }
        }
        CHECK(found, "tools/list returns the byte-identical UTF-8 tool name");
        CHECK(gotDescr == ruDescr, "tools/list returns the byte-identical UTF-8 description");
    });

    // -----------------------------------------------------------------------
    // T9: native list_collections is callable end-to-end. Either real
    //     collections (rcore.dll present beside the DLL) or the structured
    //     rag_not_installed error (lite) — both are correct.
    // -----------------------------------------------------------------------
    runTest("T9 native list_collections", [&] {
        json params = {
            {"name", "list_collections"},
            {"arguments", json::object()}
        };
        auto cli = makeClient(port);
        RpcOptions opt; opt.sessionId = sessionId;
        auto res = mcpPost(*cli, rpcRequest("tools/call", 9, params), opt);
        REQUIRE(res && res->status == 200, "tools/call list_collections -> 200");
        if (g_fatal) return;

        const json body = json::parse(res->body, nullptr, false);
        REQUIRE(!body.is_discarded() && body.contains("result"), "list_collections: JSON-RPC result present");
        if (g_fatal) return;

        const bool isError = body["result"].value("isError", false);
        std::string text;
        if (body["result"].contains("content") && !body["result"]["content"].empty()) {
            text = body["result"]["content"][0].value("text", std::string());
        }
        const json payload = json::parse(text, nullptr, false);

        const bool okCollections = !isError && !payload.is_discarded() &&
            payload.is_object() && payload.contains("collections");
        const bool ragNotInstalled = isError && !payload.is_discarded() &&
            payload.is_object() && payload.value("code", std::string()) == "rag_not_installed";

        std::printf("    list_collections variant: %s\n",
            okCollections ? "ok-collections (rcore.dll present)" :
            ragNotInstalled ? "rag_not_installed (lite)" : "UNEXPECTED");
        CHECK(okCollections || ragNotInstalled,
              "list_collections returned ok-collections or rag_not_installed, got: " + text);
    });

    // -----------------------------------------------------------------------
    // T10: late rcore.dll install — lite flips to full WITHOUT a restart.
    //      A second copy of the component DLL is loaded from a temp dir with
    //      no rcore.dll beside it: native search must answer rag_not_installed
    //      and the message must name all four native tools. Then rcore.dll is
    //      copied in and the SAME loaded module is called again — the RCore
    //      loader retries the failed load and the answer must no longer be
    //      rag_not_installed. The flip stage needs the real rcore.dll from the
    //      full bundle, so on the lite CI build it is skipped (stage A — the
    //      rag_not_installed contract — still runs everywhere).
    // -----------------------------------------------------------------------
    runTest("T10 late rcore.dll install (lite -> full, no restart)", [&] {
        const size_t srcSlash = dllPath.find_last_of(L"\\/");
        const std::wstring srcDir = (srcSlash == std::wstring::npos)
            ? std::wstring() : dllPath.substr(0, srcSlash + 1);

        wchar_t tmpBuf[MAX_PATH] = L"";
        REQUIRE(::GetTempPathW(MAX_PATH, tmpBuf) != 0, "GetTempPath succeeded");
        if (g_fatal) return;
        const std::wstring dir = std::wstring(tmpBuf) + L"http1c_smoke_t10_" +
            std::to_wstring(::GetCurrentProcessId()) + L"\\";
        ::CreateDirectoryW(dir.c_str(), nullptr);

        // Force the lite layout: component DLL only. A stale rcore.dll left by
        // a previous run (PID reuse) would defeat stage A — remove it first.
        ::DeleteFileW((dir + L"rcore.dll").c_str());
        REQUIRE(::CopyFileW(dllPath.c_str(), (dir + L"libhttp1cWin.dll").c_str(), FALSE),
                "copy the component DLL into an rcore-free temp dir");
        if (g_fatal) return;

        Harness h2; // separate module path => separate statics => fresh RCore state
        REQUIRE(h2.load(dir + L"libhttp1cWin.dll"), "load the temp copy of the component");
        if (g_fatal) return;
        h2.comp = nullptr;
        h2.getClassObject(L"HttpServer", &h2.comp);
        REQUIRE(h2.comp && h2.comp->setMemManager(&h2.mem) &&
                h2.comp->Init(static_cast<IAddInDefBase*>(&h2.conn)),
                "init the temp component instance");
        if (g_fatal) return;

        json cfg;
        cfg["logging_enabled"] = false;
        cfg["tools_json"] = "[]";
        cfg["timeout"] = 2;
        CHECK(h2.applyConfig(cfg), "ApplyConfig on the temp instance");

        const int port2 = pickFreePort();
        std::printf("    temp instance port: %d\n", port2);
        tVariant pPort, ret;
        setVariantInt(pPort, port2);
        const bool listening = h2.callFunc(L"StartListen", &ret, &pPort, 1);
        freeVariant(ret);
        REQUIRE(listening, "StartListen on the temp instance");
        if (g_fatal) return;

        auto cli = makeClient(port2);
        bool up = false;
        for (int i = 0; i < 100 && !up; ++i) {
            auto res = cli->Get("/health");
            if (res && res->status == 200) up = true;
            else std::this_thread::sleep_for(std::chrono::milliseconds(50));
        }
        REQUIRE(up, "temp instance /health responds 200");
        if (g_fatal) return;

        json initParams = {
            {"protocolVersion", "2025-03-26"},
            {"capabilities", json::object()},
            {"clientInfo", {{"name", "http1c_smoke_t10"}, {"version", "1.0"}}}
        };
        RpcOptions initOpt;
        initOpt.accept = "application/json, text/event-stream";
        auto initRes = mcpPost(*cli, rpcRequest("initialize", 1, initParams), initOpt);
        REQUIRE(initRes && initRes->status == 200, "temp instance initialize -> 200");
        if (g_fatal) return;
        RpcOptions sess;
        sess.sessionId = initRes->get_header_value("Mcp-Session-Id");

        // tools/call search -> parsed inner payload (the text of content[0]).
        auto callSearch = [&](int id, bool& isError) -> json {
            json params = {
                {"name", "search"},
                {"arguments", {{"query", "ping"}}}
            };
            auto res = mcpPost(*cli, rpcRequest("tools/call", id, params), sess);
            if (!res || res->status != 200) { isError = true; return json(); }
            const json body = json::parse(res->body, nullptr, false);
            if (body.is_discarded() || !body.contains("result")) { isError = true; return json(); }
            isError = body["result"].value("isError", false);
            std::string text;
            if (body["result"].contains("content") && !body["result"]["content"].empty()) {
                text = body["result"]["content"][0].value("text", std::string());
            }
            return json::parse(text, nullptr, false);
        };

        // Stage A — without rcore.dll: structured rag_not_installed naming
        // every native tool (so a caller knows the full surface it is missing).
        bool isError = false;
        json payload = callSearch(2, isError);
        CHECK(isError && payload.is_object() &&
              payload.value("code", std::string()) == "rag_not_installed",
              "without rcore.dll native search returns rag_not_installed");
        const std::string msg = payload.is_object()
            ? payload.value("message", std::string()) : std::string();
        for (const char* native : {"search", "grep", "get_segment", "list_collections"}) {
            CHECK(msg.find(native) != std::string::npos,
                  std::string("rag_not_installed message names ") + native);
        }

        // Stage B — drop rcore.dll next to the RUNNING component and call again:
        // the loader must retry and leave lite mode within the same process.
        if (fileExists(srcDir + L"rcore.dll")) {
            REQUIRE(::CopyFileW((srcDir + L"rcore.dll").c_str(),
                                (dir + L"rcore.dll").c_str(), FALSE),
                    "copy rcore.dll next to the running temp component");
            if (fileExists(srcDir + L"DirectML.dll")) { // lazy-loaded by ort; ship alongside
                ::CopyFileW((srcDir + L"DirectML.dll").c_str(),
                            (dir + L"DirectML.dll").c_str(), FALSE);
            }
            if (!g_fatal) {
                payload = callSearch(3, isError);
                const std::string code = payload.is_object()
                    ? payload.value("code", std::string()) : std::string("<non-json>");
                std::printf("    after install: search answered code=\"%s\" isError=%d\n",
                            code.c_str(), (int)isError);
                CHECK(payload.is_object() && code != "rag_not_installed",
                      "after copying rcore.dll the loader picks it up without a restart");
            }
        } else {
            std::printf("    SKIP flip stage: no rcore.dll beside the primary DLL (lite build)\n");
        }

        tVariant none;
        tVarInit(&none);
        CHECK(h2.callProc(L"StopListen", &none, 0), "temp instance StopListen");
        h2.destroy();
        // Best-effort cleanup: both DLLs stay mapped until process exit (no
        // FreeLibrary by design), so deletes may fail — that is fine, the next
        // run gets a fresh PID-suffixed dir and scrubs stale files itself.
        ::DeleteFileW((dir + L"libhttp1cWin.dll").c_str());
        ::DeleteFileW((dir + L"DirectML.dll").c_str());
        ::DeleteFileW((dir + L"rcore.dll").c_str());
        ::RemoveDirectoryW(dir.c_str());
    });

    // -----------------------------------------------------------------------
    // Cleanup: stop the listener (joins the server thread + shuts down rcore)
    // and destroy the component. The exe must exit on its own.
    // -----------------------------------------------------------------------
    std::printf("=== cleanup ===\n");
    if (h.comp) {
        tVariant none;
        tVarInit(&none);
        const bool stopped = h.callProc(L"StopListen", &none, 0);
        CHECK(stopped, "StopListen succeeded");
        h.destroy();
    }

    std::printf("\n%d checks, %d failed\n", g_checks, g_failures);
    std::printf(g_failures == 0 ? "ALL TESTS PASSED\n" : "TESTS FAILED\n");
    return g_failures > 255 ? 255 : g_failures;
}
