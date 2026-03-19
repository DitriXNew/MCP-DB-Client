#include "stdafx.h"
#include "HttpServerComponent.h"

#include "../version.h"

#include "httplib.h"
#include "json.hpp"

#include <codecvt>
#include <sstream>
#include <chrono>
#include <fstream>
#include <filesystem>
#include <random>
#include <algorithm>

using json = nlohmann::json;

// Latest MCP protocol version this server supports.
static constexpr const char* MCP_PROTOCOL_VERSION = "2025-03-26";

namespace {

// =========================================================================
// Logging
// =========================================================================

std::string getDefaultLogPath() {
    std::error_code errorCode;
    std::filesystem::path tempDir = std::filesystem::temp_directory_path(errorCode);
    if (errorCode) {
        return "http_debug.log";
    }

    return (tempDir / "http1c.log").string();
}

bool g_loggingEnabled = true;
std::string g_logPath = getDefaultLogPath();
std::mutex g_loggingMutex;

void logToFile(const std::string& message) {
    std::string currentPath;
    {
        std::lock_guard<std::mutex> lock(g_loggingMutex);
        if (!g_loggingEnabled || g_logPath.empty()) {
            return;
        }
        currentPath = g_logPath;
    }

    std::ofstream file(currentPath, std::ios::app);
    if (!file.is_open()) {
        return;
    }

    auto now = std::chrono::system_clock::now();
    auto milliseconds = std::chrono::duration_cast<std::chrono::milliseconds>(
        now.time_since_epoch()).count();
    file << "[" << milliseconds << "] " << message << std::endl;
}

json parseJsonSafe(const std::string& rawJson) {
    return json::parse(rawJson, nullptr, false);
}

json parseJsonOrNull(const std::string& rawJson) {
    json value = parseJsonSafe(rawJson);
    return value.is_discarded() ? json(nullptr) : value;
}

bool clientAcceptsEventStream(const httplib::Request& request) {
    return request.has_header("Accept") &&
        request.get_header_value("Accept").find("text/event-stream") != std::string::npos;
}

std::string makeSseMessage(const json& payload) {
    return "event: message\ndata: " + payload.dump() + "\n\n";
}

json makeTextToolResult(const std::string& text, bool isError = false) {
    json result;
    result["content"] = json::array({ {{"type", "text"}, {"text", text}} });
    if (isError) {
        result["isError"] = true;
    }
    return result;
}

json wrapToolResultResponse(const std::string& rpcIdJson, const std::string& responseBody) {
    json rpcResponse;
    rpcResponse["jsonrpc"] = "2.0";
    rpcResponse["id"] = parseJsonOrNull(rpcIdJson);

    json toolResult = parseJsonSafe(responseBody);
    if (!toolResult.is_discarded() && toolResult.is_object() && toolResult.contains("content")) {
        rpcResponse["result"] = toolResult;
    } else {
        rpcResponse["result"] = makeTextToolResult(responseBody);
    }

    return rpcResponse;
}

json makeProgressNotification(const std::string& progressTokenJson, double progress,
    double total, const std::string& message) {
    json notification;
    notification["jsonrpc"] = "2.0";
    notification["method"] = "notifications/progress";

    json params;
    params["progressToken"] = parseJsonOrNull(progressTokenJson);
    params["progress"] = progress;
    params["total"] = total;
    if (!message.empty()) {
        params["message"] = message;
    }

    notification["params"] = params;
    return notification;
}

json makeJsonRpcToolResponse(const std::string& rpcIdJson, const json& result) {
    json response;
    response["jsonrpc"] = "2.0";
    response["id"] = parseJsonOrNull(rpcIdJson);
    response["result"] = result;
    return response;
}

json makeJsonRpcError(const json& rpcId, int code, const std::string& message) {
    json resp;
    resp["jsonrpc"] = "2.0";
    resp["id"] = rpcId;
    resp["error"] = {{"code", code}, {"message", message}};
    return resp;
}

void enqueueSseMessage(const std::shared_ptr<HttpServerComponent::PendingRequest>& pending,
    const json& payload, bool completeStream = false) {
    std::lock_guard<std::mutex> lock(pending->mtx);
    pending->sseMessages.push_back(makeSseMessage(payload));
    if (completeStream) {
        pending->streamCompleted = true;
        pending->ready = true;
    }
    pending->cv.notify_all();
}

// =========================================================================
// Pagination helper: extract a page from a JSON array using a cursor.
// Cursor is a base-10 string of the start offset (e.g. "10" = skip first 10).
// =========================================================================
static const int DEFAULT_PAGE_SIZE = 50;

json paginateJsonArray(const json& arr, const json& params) {
    std::string cursor = params.value("cursor", "");
    int offset = 0;
    if (!cursor.empty()) {
        try { offset = std::stoi(cursor); } catch (...) { offset = 0; }
    }

    int total = static_cast<int>(arr.size());
    int end = std::min(offset + DEFAULT_PAGE_SIZE, total);

    json page = json::array();
    for (int i = offset; i < end; ++i) {
        page.push_back(arr[i]);
    }

    json result;
    result["items"] = page;
    if (end < total) {
        result["nextCursor"] = std::to_string(end);
    }
    return result;
}

// =========================================================================
// Origin validation for DNS rebinding protection.
// Allowed origins: localhost variants only.
// =========================================================================
bool isAllowedOrigin(const std::string& origin) {
    if (origin.empty()) return true; // No Origin header = same-origin request
    // Allow localhost variants
    if (origin.find("://localhost") != std::string::npos) return true;
    if (origin.find("://127.0.0.1") != std::string::npos) return true;
    if (origin.find("://[::1]") != std::string::npos) return true;
    // Allow VS Code extension origins
    if (origin.find("vscode-webview://") == 0) return true;
    if (origin.find("vscode-file://") == 0) return true;
    return false;
}

}

std::vector<std::u16string> HttpServerComponent::names = {
    AddComponent(u"HttpServer", []() { return new HttpServerComponent; }),
};

// =========================================================================
// Session management helpers
// =========================================================================

std::string HttpServerComponent::generateSessionId() {
    static std::random_device rd;
    static std::mt19937_64 gen(rd());
    static std::uniform_int_distribution<uint64_t> dist;

    uint64_t a = dist(gen);
    uint64_t b = dist(gen);

    char buf[37];
    snprintf(buf, sizeof(buf),
        "%08x-%04x-%04x-%04x-%012llx",
        static_cast<uint32_t>(a >> 32),
        static_cast<uint16_t>(a >> 16),
        static_cast<uint16_t>(0x4000 | ((a >> 4) & 0x0fff)),   // UUID v4
        static_cast<uint16_t>(0x8000 | (b & 0x3fff)),          // variant 1
        static_cast<unsigned long long>(b >> 16));
    return std::string(buf);
}

std::string HttpServerComponent::createSession(const std::string& protocolVersion) {
    auto session = std::make_shared<McpSession>();
    session->sessionId = generateSessionId();
    session->protocolVersion = protocolVersion;
    session->initialized = true;
    session->createdAt = std::chrono::steady_clock::now();
    session->lastActivity = session->createdAt;

    std::lock_guard<std::mutex> lock(sessionMutex);
    sessions[session->sessionId] = session;
    return session->sessionId;
}

std::shared_ptr<McpSession> HttpServerComponent::findSession(const std::string& sessionId) {
    std::lock_guard<std::mutex> lock(sessionMutex);
    auto it = sessions.find(sessionId);
    if (it != sessions.end()) {
        it->second->lastActivity = std::chrono::steady_clock::now();
        return it->second;
    }
    return nullptr;
}

// =========================================================================
// Security middleware
// =========================================================================

bool HttpServerComponent::validateOrigin(const httplib::Request& req, httplib::Response& res) {
    std::string origin = req.get_header_value("Origin");
    if (!isAllowedOrigin(origin)) {
        logToFile("SECURITY: Blocked request with Origin: " + origin);
        res.status = 403;
        res.set_content(R"({"error":"Forbidden: invalid Origin"})", "application/json");
        return false;
    }
    return true;
}

bool HttpServerComponent::validateAuth(const httplib::Request& req, httplib::Response& res) {
    if (authToken.empty()) return true; // No auth configured

    std::string auth = req.get_header_value("Authorization");
    if (auth.size() > 7 && auth.substr(0, 7) == "Bearer ") {
        if (auth.substr(7) == authToken) return true;
    }

    logToFile("SECURITY: Unauthorized request");
    res.status = 401;
    json err = makeJsonRpcError(nullptr, -32000, "Unauthorized: invalid or missing Bearer token");
    res.set_content(err.dump(), "application/json");
    return false;
}

bool HttpServerComponent::checkRateLimit(httplib::Response& res) {
    if (!rateLimiter.allow()) {
        logToFile("SECURITY: Rate limit exceeded");
        res.status = 429;
        res.set_content(R"({"error":"Too Many Requests"})", "application/json");
        return false;
    }
    return true;
}

// =========================================================================
// Constructor — register all methods and properties exposed to 1C.
// =========================================================================

HttpServerComponent::HttpServerComponent()
    : logPath(getDefaultLogPath())
{
    // Start the HTTP listener on the given port.
    AddFunction(u"StartListen", u"НачатьПрослушивание",
        [&](VH port) { this->doStartListen((int)(int64_t)port); this->result = true; },
        {{0, (int64_t)8765}});

    // Stop the listener and unblock all pending tool calls.
    AddProcedure(u"StopListen", u"ОстановитьПрослушивание",
        [&]() { this->doStopListen(); });

    // Final response for a pending request.
    AddProcedure(u"SendResponse", u"ОтправитьОтвет",
        [&](VH jsonStr) { this->doSendResponse((std::u16string)jsonStr); });

    // Progress notification for a pending request.
    AddProcedure(u"SendProgress", u"ОтправитьПрогресс",
        [&](VH requestId, VH progress, VH total, VH message) {
            this->doSendProgress((std::u16string)requestId, (double)progress,
                (double)total, (std::u16string)message);
        });

    // Whether logging is active (read/write property).
    AddProperty(u"LoggingEnabled", u"ЛогированиеВключено",
        [&](VH var) { var = this->loggingEnabled; },
        [&](VH var) {
            this->loggingEnabled = (bool)var;
            {
                std::lock_guard<std::mutex> lock(g_loggingMutex);
                g_loggingEnabled = this->loggingEnabled;
            }
            logToFile("Logging " + std::string(this->loggingEnabled ? "enabled" : "disabled"));
        });

    // Path for the log file (read/write property).
    AddProperty(u"LogPath", u"ПутьЛога",
        [&](VH var) {
            std::lock_guard<std::mutex> lock(this->loggingMutex);
            var = MB2WCHAR(this->logPath);
        },
        [&](VH var) {
            std::u16string val = (std::u16string)var;
            std::string utf8Path = WCHAR2MB(std::basic_string_view<WCHAR_T>(
                reinterpret_cast<const WCHAR_T*>(val.data()), val.size()));
            {
                std::lock_guard<std::mutex> lock(this->loggingMutex);
                this->logPath = utf8Path.empty() ? getDefaultLogPath() : utf8Path;
            }
            {
                std::lock_guard<std::mutex> lock(g_loggingMutex);
                g_logPath = this->logPath;
            }
            logToFile("Log path set to: " + this->logPath);
        });

    // Operational status of the native listener (read-only property).
    AddProperty(u"Status", u"Статус",
        [&](VH var) {
            json status;
            status["running"] = running.load();
            status["port"] = listenPort;
            {
                std::lock_guard<std::mutex> lock(pendingMutex);
                status["pending_requests"] = (int)pendingRequests.size();
            }
            {
                std::lock_guard<std::mutex> lock(toolsMutex);
                json tools = json::parse(cachedToolsJson, nullptr, false);
                status["tools_registered"] = tools.is_array() ? (int)tools.size() : 0;
            }
            {
                std::lock_guard<std::mutex> lock(resourcesMutex);
                json res = json::parse(cachedResourcesJson, nullptr, false);
                status["resources_registered"] = res.is_array() ? (int)res.size() : 0;
            }
            {
                std::lock_guard<std::mutex> lock(promptsMutex);
                json pr = json::parse(cachedPromptsJson, nullptr, false);
                status["prompts_registered"] = pr.is_array() ? (int)pr.size() : 0;
            }
            {
                std::lock_guard<std::mutex> lock(sessionMutex);
                status["active_sessions"] = (int)sessions.size();
            }
            {
                std::lock_guard<std::mutex> lock(loggingMutex);
                status["logging_enabled"] = loggingEnabled;
                status["log_path"] = logPath;
            }
            status["auth_enabled"] = !authToken.empty();
            status["version"] = VERSION_SEMVER;
            var = MB2WCHAR(status.dump());
        });

    // Timeout applied to in-flight forwarded requests (read/write property).
    AddProperty(u"Timeout", u"Таймаут",
        [&](VH var) { var = (int64_t)this->timeout; },
        [&](VH var) { this->timeout = (int)(int64_t)var; });

    // Tool definitions cache (write-only property; expects JSON array).
    AddProperty(u"Tools", u"Инструменты",
        nullptr,
        [&](VH var) { this->doRegisterTools((std::u16string)var); });

    // Resource definitions cache (write-only property; expects JSON array).
    AddProperty(u"Resources", u"Ресурсы",
        nullptr,
        [&](VH var) { this->doRegisterResources((std::u16string)var); });

    // Prompt definitions cache (write-only property; expects JSON array).
    AddProperty(u"Prompts", u"Промпты",
        nullptr,
        [&](VH var) { this->doRegisterPrompts((std::u16string)var); });

    // Bearer token for authentication (write-only property; empty = no auth).
    AddProperty(u"AuthToken", u"ТокенАвторизации",
        nullptr,
        [&](VH var) { this->doSetAuthToken((std::u16string)var); });
}

HttpServerComponent::~HttpServerComponent() noexcept
{
    try {
        doStopListen();
    } catch (...) {
    }
}

void HttpServerComponent::doStartListen(int port)
{
    if (running.load()) {
        return;
    }

    server = new httplib::Server();
    running = true;
    listenPort = port;

    // Legacy handler kept for compatibility with the older plain HTTP contract.
    auto legacyHandler = [this](const httplib::Request& req, httplib::Response& res) {
        if (!validateOrigin(req, res) || !validateAuth(req, res) || !checkRateLimit(res)) return;

        logToFile("LEGACY HANDLER: " + req.method + " " + req.path);

        res.set_header("Access-Control-Allow-Origin", "*");
        res.set_header("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE, PATCH, OPTIONS");
        res.set_header("Access-Control-Allow-Headers", "Content-Type, Authorization");

        std::string reqId;
        {
            std::lock_guard<std::mutex> lock(pendingMutex);
            reqId = "req_" + std::to_string(++requestCounter);
        }

        auto pending = std::make_shared<PendingRequest>();
        {
            std::lock_guard<std::mutex> lock(pendingMutex);
            pendingRequests[reqId] = pending;
        }

        json eventData;
        eventData["id"] = reqId;
        eventData["method"] = req.method;
        eventData["path"] = req.path;
        eventData["body"] = req.body;
        eventData["content_type"] = req.get_header_value("Content-Type");

        json params = json::object();
        for (auto& p : req.params) {
            params[p.first] = p.second;
        }
        eventData["params"] = params;

        std::string jsonStr = eventData.dump();
        ExtEvent(u"HttpServer", u"Request", MB2WCHAR(jsonStr));

        {
            std::unique_lock<std::mutex> lock(pending->mtx);
            bool gotResponse = pending->cv.wait_for(lock,
                std::chrono::seconds(timeout),
                [&] { return pending->ready.load(); });

            if (gotResponse) {
                res.set_content(pending->responseBody, pending->contentType);
                res.status = pending->statusCode;
            } else {
                json err;
                err["error"] = "Timeout waiting for 1C response";
                res.set_content(err.dump(), "application/json");
                res.status = 504;
            }
        }

        {
            std::lock_guard<std::mutex> lock(pendingMutex);
            pendingRequests.erase(reqId);
        }
    };

    // Health is served locally because it reflects transport state only.
    server->Get("/health", [this](const httplib::Request&, httplib::Response& res) {
        res.set_header("Access-Control-Allow-Origin", "*");
        json resp = {{"status", "ok"}, {"running", running.load()}, {"port", listenPort}};
        {
            std::lock_guard<std::mutex> lock(pendingMutex);
            resp["pending_requests"] = (int)pendingRequests.size();
        }
        res.set_content(resp.dump(), "application/json");
    });

    // MCP endpoint — POST for JSON-RPC messages (Streamable HTTP transport).
    server->Post("/mcp", [this](const httplib::Request& req, httplib::Response& res) {
        handleMcpRequest(req, res);
    });

    // MCP endpoint — GET for SSE notification stream (Streamable HTTP transport).
    server->Get("/mcp", [this](const httplib::Request& req, httplib::Response& res) {
        handleMcpGet(req, res);
    });

    // MCP endpoint — DELETE for session termination (Streamable HTTP transport).
    server->Delete("/mcp", [this](const httplib::Request& req, httplib::Response& res) {
        handleMcpDelete(req, res);
    });

    server->Options(".*", [](const httplib::Request&, httplib::Response& res) {
        res.set_header("Access-Control-Allow-Origin", "*");
        res.set_header("Access-Control-Allow-Methods", "GET, POST, DELETE, PUT, PATCH, OPTIONS");
        res.set_header("Access-Control-Allow-Headers",
            "Content-Type, Authorization, Mcp-Session-Id, MCP-Protocol-Version");
        res.status = 204;
    });

    server->Get(".*", legacyHandler);
    server->Post(".*", legacyHandler);
    server->Put(".*", legacyHandler);
    server->Delete(".*", legacyHandler);
    server->Patch(".*", legacyHandler);

    serverThread = std::thread([this, port]() {
        // Bind only to localhost for security (prevents DNS rebinding).
        server->listen("127.0.0.1", port);
        running = false;
    });
}

void HttpServerComponent::doStopListen()
{
    if (server) {
        server->stop();
    }

    {
        std::lock_guard<std::mutex> lock(pendingMutex);
        for (auto& [id, pending] : pendingRequests) {
            if (pending->useEventStream) {
                enqueueSseMessage(pending,
                    makeJsonRpcToolResponse(pending->rpcIdJson,
                        makeTextToolResult("Server is shutting down.", true)),
                    true);
            } else {
                pending->ready = true;
                pending->statusCode = 503;
                pending->responseBody = R"({"error":"Server shutting down"})";
                pending->cv.notify_all();
            }
        }
        pendingRequests.clear();
    }

    {
        std::lock_guard<std::mutex> lock(sessionMutex);
        sessions.clear();
    }

    // Close all SSE notification streams.
    {
        std::lock_guard<std::mutex> lock(sseStreamsMutex);
        for (auto& stream : sseStreams) {
            stream->closed = true;
            stream->cv.notify_all();
        }
        sseStreams.clear();
    }

    if (serverThread.joinable()) {
        serverThread.join();
    }

    delete server;
    server = nullptr;
    running = false;
}

void HttpServerComponent::doSendResponse(const std::u16string& jsonStr)
{
    std::string utf8 = WCHAR2MB(std::basic_string_view<WCHAR_T>(
        reinterpret_cast<const WCHAR_T*>(jsonStr.data()), jsonStr.size()));

    logToFile("SendResponse: " + utf8.substr(0, 300));

    json response = json::parse(utf8, nullptr, false);
    if (response.is_discarded()) {
        logToFile("SendResponse: INVALID JSON");
        AddError(u"Invalid JSON in SendResponse");
        return;
    }

    std::string reqId = response.value("id", "");
    if (reqId.empty()) {
        logToFile("SendResponse: MISSING ID");
        AddError(u"Missing 'id' in SendResponse JSON");
        return;
    }

    logToFile("SendResponse: looking for reqId=" + reqId);

    std::shared_ptr<PendingRequest> pending;
    {
        std::lock_guard<std::mutex> lock(pendingMutex);
        auto it = pendingRequests.find(reqId);
        if (it == pendingRequests.end()) {
            return;
        }
        pending = it->second;
    }

    pending->statusCode = response.value("status", 200);
    pending->contentType = response.value("content_type", "application/json");

    if (response.contains("body") && response["body"].is_string()) {
        pending->responseBody = response["body"].get<std::string>();
    } else if (response.contains("body")) {
        pending->responseBody = response["body"].dump();
    } else {
        json body = response;
        body.erase("id");
        body.erase("status");
        body.erase("content_type");
        pending->responseBody = body.dump();
    }

    if (pending->useEventStream) {
        enqueueSseMessage(pending,
            wrapToolResultResponse(pending->rpcIdJson, pending->responseBody),
            true);
        return;
    }

    pending->ready = true;
    pending->cv.notify_all();
}

void HttpServerComponent::doSendProgress(const std::u16string& requestId, double progress,
    double total, const std::u16string& message)
{
    std::string reqId = WCHAR2MB(std::basic_string_view<WCHAR_T>(
        reinterpret_cast<const WCHAR_T*>(requestId.data()), requestId.size()));
    std::string progressMessage = WCHAR2MB(std::basic_string_view<WCHAR_T>(
        reinterpret_cast<const WCHAR_T*>(message.data()), message.size()));

    std::shared_ptr<PendingRequest> pending;
    {
        std::lock_guard<std::mutex> lock(pendingMutex);
        auto it = pendingRequests.find(reqId);
        if (it == pendingRequests.end()) {
            return;
        }
        pending = it->second;
    }

    if (!pending->useEventStream || pending->progressTokenJson == "null") {
        return;
    }

    enqueueSseMessage(pending,
        makeProgressNotification(pending->progressTokenJson, progress, total, progressMessage));
}

void HttpServerComponent::handleMcpRequest(const httplib::Request& req, httplib::Response& res)
{
    // ---- Security middleware ----
    if (!validateOrigin(req, res)) return;
    if (!validateAuth(req, res)) return;
    if (!checkRateLimit(res)) return;

    res.set_header("Access-Control-Allow-Origin", "*");
    res.set_header("Access-Control-Allow-Methods", "POST, GET, DELETE, OPTIONS");
    res.set_header("Access-Control-Allow-Headers",
        "Content-Type, Authorization, Mcp-Session-Id, MCP-Protocol-Version");

    logToFile("MCP REQUEST: " + req.body.substr(0, 500));

    // Parse JSON-RPC
    json rpc = json::parse(req.body, nullptr, false);
    if (rpc.is_discarded() || !rpc.is_object()) {
        res.set_content(makeJsonRpcError(nullptr, -32700, "Parse error").dump(), "application/json");
        return;
    }

    std::string method = rpc.value("method", "");
    bool isNotification = !rpc.contains("id");
    json rpcId = rpc.contains("id") ? rpc["id"] : json(nullptr);
    json params = rpc.value("params", json::object());

    logToFile("MCP method=" + method + " id=" + rpcId.dump());

    // =========================================================================
    // initialize — create session, negotiate capabilities
    // =========================================================================
    if (method == "initialize") {
        std::string clientProtocolVersion = params.value("protocolVersion", "");

        json result;
        result["protocolVersion"] = MCP_PROTOCOL_VERSION;
        result["capabilities"] = {
            {"tools",     {{"listChanged", true}}},
            {"resources", {{"listChanged", true}}},
            {"prompts",   {{"listChanged", true}}}
        };
        result["serverInfo"] = {
            {"name", "1c-mcp-server"},
            {"version", VERSION_SEMVER}
        };

        // Create a new session and return the ID via Mcp-Session-Id header.
        std::string sessionId = createSession(
            clientProtocolVersion.empty() ? MCP_PROTOCOL_VERSION : clientProtocolVersion);

        json resp;
        resp["jsonrpc"] = "2.0";
        resp["id"] = rpcId;
        resp["result"] = result;

        res.set_header("Mcp-Session-Id", sessionId);
        res.set_content(resp.dump(), "application/json");

        logToFile("MCP -> initialize OK, session=" + sessionId);
        return;
    }

    // ---- Session validation for all non-initialize requests ----
    std::string sessionId = req.get_header_value("Mcp-Session-Id");
    if (!sessionId.empty()) {
        auto session = findSession(sessionId);
        if (!session) {
            logToFile("MCP: invalid session " + sessionId);
            res.status = 404;
            res.set_content(R"({"error":"Session not found"})", "application/json");
            return;
        }
    }

    // =========================================================================
    // notifications/initialized — client confirms initialization
    // =========================================================================
    if (method == "notifications/initialized") {
        res.status = 200;
        return;
    }

    // =========================================================================
    // ping
    // =========================================================================
    if (method == "ping") {
        json resp;
        resp["jsonrpc"] = "2.0";
        resp["id"] = rpcId;
        resp["result"] = json::object();
        res.set_content(resp.dump(), "application/json");
        return;
    }

    // =========================================================================
    // tools/list — paginated tool listing
    // =========================================================================
    if (method == "tools/list") {
        json tools;
        {
            std::lock_guard<std::mutex> lock(toolsMutex);
            tools = json::parse(cachedToolsJson, nullptr, false);
            if (tools.is_discarded()) tools = json::array();
        }

        json page = paginateJsonArray(tools, params);

        json resp;
        resp["jsonrpc"] = "2.0";
        resp["id"] = rpcId;
        resp["result"] = {{"tools", page["items"]}};
        if (page.contains("nextCursor")) {
            resp["result"]["nextCursor"] = page["nextCursor"];
        }
        res.set_content(resp.dump(), "application/json");
        logToFile("MCP -> tools/list: " + std::to_string(page["items"].size()) + " tools");
        return;
    }

    // =========================================================================
    // tools/call — delegated to 1C
    // =========================================================================
    if (method == "tools/call") {
        std::string toolName = params.value("name", "");
        json toolArgs = params.value("arguments", json::object());
        json meta = params.value("_meta", json::object());
        json progressToken = meta.value("progressToken", json(nullptr));

        if (toolName.empty()) {
            res.set_content(
                makeJsonRpcError(rpcId, -32602, "Missing tool name in params").dump(),
                "application/json");
            return;
        }

        std::string reqId;
        auto pending = std::make_shared<PendingRequest>();
        {
            std::lock_guard<std::mutex> lock(pendingMutex);
            reqId = "req_" + std::to_string(++requestCounter);
            pendingRequests[reqId] = pending;
        }

        pending->rpcIdJson = rpcId.dump();
        pending->useEventStream = clientAcceptsEventStream(req) && !progressToken.is_null();
        pending->progressTokenJson = progressToken.dump();

        json eventData;
        eventData["id"] = reqId;
        eventData["type"] = "tool_call";
        eventData["tool"] = toolName;
        eventData["arguments"] = toolArgs;
        if (!progressToken.is_null()) {
            eventData["progressToken"] = progressToken;
        }

        logToFile("MCP -> tools/call: " + toolName + " reqId=" + reqId);
        ExtEvent(u"HttpServer", u"ToolCall", MB2WCHAR(eventData.dump()));

        if (pending->useEventStream) {
            res.set_chunked_content_provider("text/event-stream",
                [this, pending](size_t, httplib::DataSink& sink) {
                    std::unique_lock<std::mutex> lock(pending->mtx);
                    bool hasData = pending->cv.wait_for(lock,
                        std::chrono::seconds(timeout),
                        [&] {
                            return !pending->sseMessages.empty() || pending->streamCompleted;
                        });

                    if (!hasData) {
                        pending->sseMessages.push_back(makeSseMessage(
                            makeJsonRpcToolResponse(pending->rpcIdJson,
                                makeTextToolResult(
                                    "Timeout: 1C did not respond within " +
                                    std::to_string(timeout) + " seconds.",
                                    true))));
                        pending->streamCompleted = true;
                    }

                    while (!pending->sseMessages.empty()) {
                        std::string payload = pending->sseMessages.front();
                        pending->sseMessages.pop_front();

                        lock.unlock();
                        if (!sink.write(payload.c_str(), payload.size())) {
                            return false;
                        }
                        lock.lock();
                    }

                    if (pending->streamCompleted) {
                        sink.done();
                    }

                    return true;
                },
                [this, reqId](bool) {
                    std::lock_guard<std::mutex> lock(pendingMutex);
                    pendingRequests.erase(reqId);
                });
            return;
        }

        json rpcResp;
        rpcResp["jsonrpc"] = "2.0";
        rpcResp["id"] = rpcId;
        {
            std::unique_lock<std::mutex> lock(pending->mtx);
            bool gotResponse = pending->cv.wait_for(lock,
                std::chrono::seconds(timeout),
                [&] { return pending->ready.load(); });

            if (gotResponse) {
                logToFile("MCP tools/call RESPONSE: " + pending->responseBody.substr(0, 300));

                json toolResult = json::parse(pending->responseBody, nullptr, false);

                if (!toolResult.is_discarded() && toolResult.contains("content")) {
                    rpcResp["result"] = toolResult;
                } else {
                    rpcResp["result"] = {
                        {"content", {{{"type", "text"}, {"text", pending->responseBody}}}}
                    };
                }
            } else {
                logToFile("MCP tools/call TIMEOUT for " + reqId);
                rpcResp["result"] = {
                    {"isError", true},
                    {"content", {{{"type", "text"}, {"text",
                        "Timeout: 1C did not respond within " + std::to_string(timeout) + " seconds"}}}}
                };
            }
        }

        res.set_content(rpcResp.dump(), "application/json");

        {
            std::lock_guard<std::mutex> lock(pendingMutex);
            pendingRequests.erase(reqId);
        }
        return;
    }

    // =========================================================================
    // resources/list — paginated resource listing
    // =========================================================================
    if (method == "resources/list") {
        json resources;
        {
            std::lock_guard<std::mutex> lock(resourcesMutex);
            resources = json::parse(cachedResourcesJson, nullptr, false);
            if (resources.is_discarded()) resources = json::array();
        }

        json page = paginateJsonArray(resources, params);

        json resp;
        resp["jsonrpc"] = "2.0";
        resp["id"] = rpcId;
        resp["result"] = {{"resources", page["items"]}};
        if (page.contains("nextCursor")) {
            resp["result"]["nextCursor"] = page["nextCursor"];
        }
        res.set_content(resp.dump(), "application/json");
        logToFile("MCP -> resources/list: " + std::to_string(page["items"].size()) + " resources");
        return;
    }

    // =========================================================================
    // resources/read — delegated to 1C via ExternalEvent
    // =========================================================================
    if (method == "resources/read") {
        std::string uri = params.value("uri", "");
        if (uri.empty()) {
            res.set_content(
                makeJsonRpcError(rpcId, -32602, "Missing 'uri' in params").dump(),
                "application/json");
            return;
        }

        std::string reqId;
        auto pending = std::make_shared<PendingRequest>();
        {
            std::lock_guard<std::mutex> lock(pendingMutex);
            reqId = "req_" + std::to_string(++requestCounter);
            pendingRequests[reqId] = pending;
        }
        pending->rpcIdJson = rpcId.dump();

        json eventData;
        eventData["id"] = reqId;
        eventData["type"] = "resource_read";
        eventData["uri"] = uri;

        logToFile("MCP -> resources/read: " + uri + " reqId=" + reqId);
        ExtEvent(u"HttpServer", u"ResourceRead", MB2WCHAR(eventData.dump()));

        {
            std::unique_lock<std::mutex> lock(pending->mtx);
            bool gotResponse = pending->cv.wait_for(lock,
                std::chrono::seconds(timeout),
                [&] { return pending->ready.load(); });

            json rpcResp;
            rpcResp["jsonrpc"] = "2.0";
            rpcResp["id"] = rpcId;

            if (gotResponse) {
                json body = json::parse(pending->responseBody, nullptr, false);
                if (!body.is_discarded() && body.is_object() && body.contains("contents")) {
                    rpcResp["result"] = body;
                } else {
                    rpcResp["result"] = {
                        {"contents", json::array({{
                            {"uri", uri},
                            {"mimeType", "text/plain"},
                            {"text", pending->responseBody}
                        }})}
                    };
                }
            } else {
                rpcResp["error"] = {{"code", -32000},
                    {"message", "Timeout: 1C did not respond within " + std::to_string(timeout) + " seconds"}};
            }

            res.set_content(rpcResp.dump(), "application/json");
        }

        {
            std::lock_guard<std::mutex> lock(pendingMutex);
            pendingRequests.erase(reqId);
        }
        return;
    }

    // =========================================================================
    // prompts/list — paginated prompt listing
    // =========================================================================
    if (method == "prompts/list") {
        json prompts;
        {
            std::lock_guard<std::mutex> lock(promptsMutex);
            prompts = json::parse(cachedPromptsJson, nullptr, false);
            if (prompts.is_discarded()) prompts = json::array();
        }

        json page = paginateJsonArray(prompts, params);

        json resp;
        resp["jsonrpc"] = "2.0";
        resp["id"] = rpcId;
        resp["result"] = {{"prompts", page["items"]}};
        if (page.contains("nextCursor")) {
            resp["result"]["nextCursor"] = page["nextCursor"];
        }
        res.set_content(resp.dump(), "application/json");
        logToFile("MCP -> prompts/list: " + std::to_string(page["items"].size()) + " prompts");
        return;
    }

    // =========================================================================
    // prompts/get — delegated to 1C via ExternalEvent
    // =========================================================================
    if (method == "prompts/get") {
        std::string promptName = params.value("name", "");
        if (promptName.empty()) {
            res.set_content(
                makeJsonRpcError(rpcId, -32602, "Missing 'name' in params").dump(),
                "application/json");
            return;
        }

        std::string reqId;
        auto pending = std::make_shared<PendingRequest>();
        {
            std::lock_guard<std::mutex> lock(pendingMutex);
            reqId = "req_" + std::to_string(++requestCounter);
            pendingRequests[reqId] = pending;
        }
        pending->rpcIdJson = rpcId.dump();

        json eventData;
        eventData["id"] = reqId;
        eventData["type"] = "prompt_get";
        eventData["name"] = promptName;
        eventData["arguments"] = params.value("arguments", json::object());

        logToFile("MCP -> prompts/get: " + promptName + " reqId=" + reqId);
        ExtEvent(u"HttpServer", u"PromptGet", MB2WCHAR(eventData.dump()));

        {
            std::unique_lock<std::mutex> lock(pending->mtx);
            bool gotResponse = pending->cv.wait_for(lock,
                std::chrono::seconds(timeout),
                [&] { return pending->ready.load(); });

            json rpcResp;
            rpcResp["jsonrpc"] = "2.0";
            rpcResp["id"] = rpcId;

            if (gotResponse) {
                json body = json::parse(pending->responseBody, nullptr, false);
                if (!body.is_discarded() && body.is_object() && body.contains("messages")) {
                    rpcResp["result"] = body;
                } else {
                    rpcResp["result"] = {
                        {"messages", json::array({{
                            {"role", "user"},
                            {"content", {{"type", "text"}, {"text", pending->responseBody}}}
                        }})}
                    };
                }
            } else {
                rpcResp["error"] = {{"code", -32000},
                    {"message", "Timeout: 1C did not respond within " + std::to_string(timeout) + " seconds"}};
            }

            res.set_content(rpcResp.dump(), "application/json");
        }

        {
            std::lock_guard<std::mutex> lock(pendingMutex);
            pendingRequests.erase(reqId);
        }
        return;
    }

    // =========================================================================
    // Notifications — accept silently
    // =========================================================================
    if (isNotification) {
        res.status = 200;
        return;
    }

    // =========================================================================
    // Unknown method
    // =========================================================================
    res.set_content(
        makeJsonRpcError(rpcId, -32601, "Method not found: " + method).dump(),
        "application/json");
}

// =========================================================================
// Session termination via HTTP DELETE /mcp (Streamable HTTP transport)
// =========================================================================
void HttpServerComponent::handleMcpDelete(const httplib::Request& req, httplib::Response& res)
{
    std::string sessionId = req.get_header_value("Mcp-Session-Id");
    if (sessionId.empty()) {
        res.status = 400;
        res.set_content(R"({"error":"Missing Mcp-Session-Id header"})", "application/json");
        return;
    }

    {
        std::lock_guard<std::mutex> lock(sessionMutex);
        auto it = sessions.find(sessionId);
        if (it == sessions.end()) {
            res.status = 404;
            res.set_content(R"({"error":"Session not found"})", "application/json");
            return;
        }
        sessions.erase(it);
    }

    logToFile("MCP session terminated: " + sessionId);
    res.status = 200;
}

// =========================================================================
// GET /mcp — SSE notification stream (server-to-client messages)
// =========================================================================
void HttpServerComponent::handleMcpGet(const httplib::Request& req, httplib::Response& res)
{
    if (!validateOrigin(req, res) || !validateAuth(req, res)) return;

    // Check Accept header
    std::string accept = req.get_header_value("Accept");
    if (accept.find("text/event-stream") == std::string::npos) {
        res.status = 406;
        res.set_content("Accept header must include text/event-stream", "text/plain");
        return;
    }

    auto stream = std::make_shared<SseStream>();
    {
        std::lock_guard<std::mutex> lock(sseStreamsMutex);
        sseStreams.push_back(stream);
    }

    logToFile("MCP SSE stream opened");

    res.set_header("Access-Control-Allow-Origin", "*");
    res.set_chunked_content_provider("text/event-stream",
        [this, stream](size_t, httplib::DataSink& sink) {
            std::unique_lock<std::mutex> lock(stream->mtx);
            // Wait for messages or close signal
            stream->cv.wait_for(lock, std::chrono::seconds(30),
                [&] { return !stream->messages.empty() || stream->closed.load(); });

            if (stream->closed.load()) {
                sink.done();
                return false;
            }

            // Send pending messages
            while (!stream->messages.empty()) {
                std::string msg = stream->messages.front();
                stream->messages.pop_front();
                lock.unlock();
                if (!sink.write(msg.c_str(), msg.size())) {
                    return false;
                }
                lock.lock();
            }

            // Send keepalive comment to prevent timeout
            if (stream->messages.empty()) {
                std::string keepalive = ": keepalive\n\n";
                lock.unlock();
                sink.write(keepalive.c_str(), keepalive.size());
            }

            return true;
        },
        [this, stream](bool) {
            stream->closed = true;
            std::lock_guard<std::mutex> lock(sseStreamsMutex);
            sseStreams.erase(
                std::remove(sseStreams.begin(), sseStreams.end(), stream),
                sseStreams.end());
            logToFile("MCP SSE stream closed");
        });
}

// =========================================================================
// Broadcast a notification to all active SSE streams.
// Used for notifications/tools/list_changed, resources/list_changed, etc.
// =========================================================================
void HttpServerComponent::broadcastNotification(const std::string& method)
{
    json notification;
    notification["jsonrpc"] = "2.0";
    notification["method"] = method;

    std::string sseMsg = makeSseMessage(notification);

    std::lock_guard<std::mutex> lock(sseStreamsMutex);
    for (auto& stream : sseStreams) {
        if (!stream->closed.load()) {
            std::lock_guard<std::mutex> sLock(stream->mtx);
            stream->messages.push_back(sseMsg);
            stream->cv.notify_all();
        }
    }
    logToFile("Broadcast notification: " + method + " to " + std::to_string(sseStreams.size()) + " streams");
}

void HttpServerComponent::doRegisterTools(const std::u16string& jsonStr)
{
    std::string utf8 = WCHAR2MB(std::basic_string_view<WCHAR_T>(
        reinterpret_cast<const WCHAR_T*>(jsonStr.data()), jsonStr.size()));

    logToFile("RegisterTools: " + utf8.substr(0, 500));

    json tools = json::parse(utf8, nullptr, false);
    if (tools.is_discarded() || !tools.is_array()) {
        AddError(u"RegisterTools: expected JSON array of tool definitions");
        return;
    }

    {
        std::lock_guard<std::mutex> lock(toolsMutex);
        cachedToolsJson = utf8;
    }

    logToFile("RegisterTools: cached " + std::to_string(tools.size()) + " tools");

    // Notify connected MCP clients that the tool list has changed.
    broadcastNotification("notifications/tools/list_changed");
}

void HttpServerComponent::doRegisterResources(const std::u16string& jsonStr)
{
    std::string utf8 = WCHAR2MB(std::basic_string_view<WCHAR_T>(
        reinterpret_cast<const WCHAR_T*>(jsonStr.data()), jsonStr.size()));

    logToFile("RegisterResources: " + utf8.substr(0, 500));

    json resources = json::parse(utf8, nullptr, false);
    if (resources.is_discarded() || !resources.is_array()) {
        AddError(u"RegisterResources: expected JSON array of resource definitions");
        return;
    }

    {
        std::lock_guard<std::mutex> lock(resourcesMutex);
        cachedResourcesJson = utf8;
    }

    logToFile("RegisterResources: cached " + std::to_string(resources.size()) + " resources");

    // Notify connected MCP clients that the resource list has changed.
    broadcastNotification("notifications/resources/list_changed");
}

void HttpServerComponent::doRegisterPrompts(const std::u16string& jsonStr)
{
    std::string utf8 = WCHAR2MB(std::basic_string_view<WCHAR_T>(
        reinterpret_cast<const WCHAR_T*>(jsonStr.data()), jsonStr.size()));

    logToFile("RegisterPrompts: " + utf8.substr(0, 500));

    json prompts = json::parse(utf8, nullptr, false);
    if (prompts.is_discarded() || !prompts.is_array()) {
        AddError(u"RegisterPrompts: expected JSON array of prompt definitions");
        return;
    }

    {
        std::lock_guard<std::mutex> lock(promptsMutex);
        cachedPromptsJson = utf8;
    }

    logToFile("RegisterPrompts: cached " + std::to_string(prompts.size()) + " prompts");

    // Notify connected MCP clients that the prompt list has changed.
    broadcastNotification("notifications/prompts/list_changed");
}

void HttpServerComponent::doSetAuthToken(const std::u16string& token)
{
    std::string utf8 = WCHAR2MB(std::basic_string_view<WCHAR_T>(
        reinterpret_cast<const WCHAR_T*>(token.data()), token.size()));

    authToken = utf8;
    logToFile("Auth token " + std::string(utf8.empty() ? "disabled" : "set"));
}
