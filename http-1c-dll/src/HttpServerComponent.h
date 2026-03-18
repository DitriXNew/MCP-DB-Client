#ifndef __HTTPSERVERCOMPONENT_H__
#define __HTTPSERVERCOMPONENT_H__

#include "AddInNative.h"
#include <deque>
#include <thread>
#include <mutex>
#include <atomic>
#include <condition_variable>
#include <map>
#include <chrono>
#include <random>

namespace httplib { class Server; struct Request; struct Response; }

// ---------------------------------------------------------------------------
// MCP Session — tracks a single client initialization handshake.
// Streamable HTTP transport requires Mcp-Session-Id management.
// ---------------------------------------------------------------------------
struct McpSession {
    std::string sessionId;
    std::string protocolVersion;
    bool initialized = false;
    std::chrono::steady_clock::time_point createdAt;
    std::chrono::steady_clock::time_point lastActivity;
};

// ---------------------------------------------------------------------------
// Simple token-bucket rate limiter (per-component instance).
// ---------------------------------------------------------------------------
struct RateLimiter {
    std::mutex mtx;
    std::chrono::steady_clock::time_point lastRefill;
    double tokens;
    double maxTokens;
    double refillRate; // tokens per second

    RateLimiter(double maxTok = 60.0, double rate = 20.0)
        : lastRefill(std::chrono::steady_clock::now())
        , tokens(maxTok), maxTokens(maxTok), refillRate(rate) {}

    bool allow() {
        std::lock_guard<std::mutex> lock(mtx);
        auto now = std::chrono::steady_clock::now();
        double elapsed = std::chrono::duration<double>(now - lastRefill).count();
        tokens = std::min(maxTokens, tokens + elapsed * refillRate);
        lastRefill = now;
        if (tokens >= 1.0) { tokens -= 1.0; return true; }
        return false;
    }
};

class HttpServerComponent : public AddInNative
{
private:
    static std::vector<std::u16string> names;
    HttpServerComponent();
    ~HttpServerComponent() noexcept;

    // HTTP listener state. The component exposes a local MCP endpoint and keeps
    // pending tool calls alive until 1C sends either progress or the final result.
    httplib::Server* server = nullptr;
    std::thread serverThread;
    std::atomic<bool> running{false};
    int timeout = 30;
    int listenPort = 0;

public:
    // State for one in-flight request forwarded to 1C.
    //
    // For application/json responses, the request waits for a single final body.
    // For text/event-stream responses, progress notifications and the final
    // JSON-RPC response are queued in sseMessages and streamed in order.
    struct PendingRequest {
        std::string responseBody;
        int statusCode = 200;
        std::string contentType = "application/json";
        std::string progressTokenJson = "null";
        std::string rpcIdJson = "null";
        std::deque<std::string> sseMessages;
        std::condition_variable cv;
        std::mutex mtx;
        std::atomic<bool> ready{false};
        bool useEventStream = false;
        bool streamCompleted = false;
    };

private:
    std::mutex pendingMutex;
    std::map<std::string, std::shared_ptr<PendingRequest>> pendingRequests;
    int requestCounter = 0;

    // -----------------------------------------------------------------------
    // MCP registries. 1C is the source of truth; the component caches data
    // for fast list responses and notifies clients on changes.
    // -----------------------------------------------------------------------
    std::string cachedToolsJson = "[]";
    std::mutex toolsMutex;

    std::string cachedResourcesJson = "[]";
    std::mutex resourcesMutex;

    std::string cachedPromptsJson = "[]";
    std::mutex promptsMutex;

    // -----------------------------------------------------------------------
    // Session management (MCP Streamable HTTP transport).
    // -----------------------------------------------------------------------
    std::mutex sessionMutex;
    std::map<std::string, std::shared_ptr<McpSession>> sessions;

    std::string createSession(const std::string& protocolVersion);
    std::shared_ptr<McpSession> findSession(const std::string& sessionId);

    static std::string generateSessionId();

    // -----------------------------------------------------------------------
    // Security configuration.
    // -----------------------------------------------------------------------
    std::string authToken;       // Bearer token; empty = no auth required.
    RateLimiter rateLimiter;

    bool validateOrigin(const httplib::Request& req, httplib::Response& res);
    bool validateAuth(const httplib::Request& req, httplib::Response& res);
    bool checkRateLimit(httplib::Response& res);

    // -----------------------------------------------------------------------
    // Runtime logging configuration controlled from 1C.
    // -----------------------------------------------------------------------
    bool loggingEnabled = true;
    std::string logPath;
    std::mutex loggingMutex;

    // -----------------------------------------------------------------------
    // SSE notification streams (GET /mcp connections for server-to-client
    // messages like notifications/tools/list_changed).
    // -----------------------------------------------------------------------
    struct SseStream {
        std::deque<std::string> messages;
        std::condition_variable cv;
        std::mutex mtx;
        std::atomic<bool> closed{false};
    };

    std::mutex sseStreamsMutex;
    std::vector<std::shared_ptr<SseStream>> sseStreams;

    void broadcastNotification(const std::string& method);

    // -----------------------------------------------------------------------
    // Internal lifecycle helpers.
    // -----------------------------------------------------------------------
    void doStartListen(int port);
    void doStopListen();
    void doSendResponse(const std::u16string& jsonStr);
    void doSendProgress(const std::u16string& requestId, double progress, double total,
        const std::u16string& message);
    void doConfigureLogging(bool enabled, const std::u16string& path);
    void doRegisterTools(const std::u16string& jsonStr);
    void doRegisterResources(const std::u16string& jsonStr);
    void doRegisterPrompts(const std::u16string& jsonStr);
    void doSetAuthToken(const std::u16string& token);

    // JSON-RPC endpoint handler for the MCP transport.
    void handleMcpRequest(const httplib::Request& req, httplib::Response& res);
    void handleMcpGet(const httplib::Request& req, httplib::Response& res);
    void handleMcpDelete(const httplib::Request& req, httplib::Response& res);
};

#endif //__HTTPSERVERCOMPONENT_H__
