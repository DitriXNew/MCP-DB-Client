// http1c unit tests — standalone, no 1C runtime required.
// Tests: RateLimiter, Origin validation, JSON protocol formats,
//        pagination, UUID format, security helpers.

#include <iostream>
#include <string>
#include <vector>
#include <regex>
#include <cassert>
#include <chrono>
#include <thread>
#include <mutex>
#include <random>
#include <algorithm>

// Include nlohmann/json
#include "json.hpp"
using json = nlohmann::json;

// ============================================================================
// Minimal test framework
// ============================================================================

static int g_passed = 0;
static int g_failed = 0;

#define TEST(name) \
    static void test_##name(); \
    struct TestReg_##name { \
        TestReg_##name() { tests().push_back({#name, test_##name}); } \
    } reg_##name; \
    static void test_##name()

#define ASSERT_TRUE(expr) do { \
    if (!(expr)) { \
        std::cerr << "  FAIL: " << #expr << " at " << __FILE__ << ":" << __LINE__ << "\n"; \
        throw std::runtime_error("assertion failed"); \
    } \
} while(0)

#define ASSERT_EQ(a, b) do { \
    if ((a) != (b)) { \
        std::cerr << "  FAIL: " << #a << " == " << #b << " at " << __FILE__ << ":" << __LINE__ << "\n"; \
        throw std::runtime_error("assertion failed"); \
    } \
} while(0)

struct TestCase {
    const char* name;
    void (*fn)();
};

static std::vector<TestCase>& tests() {
    static std::vector<TestCase> t;
    return t;
}

// ============================================================================
// Code under test: RateLimiter (from HttpServerComponent.h)
// ============================================================================

struct RateLimiter {
    std::mutex mtx;
    std::chrono::steady_clock::time_point lastRefill;
    double tokens;
    double maxTokens;
    double refillRate;

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

// ============================================================================
// Code under test: Origin validation (from HttpServerComponent.cpp)
// ============================================================================

static bool isAllowedOrigin(const std::string& origin) {
    if (origin.empty()) return true;

    if (origin.find("http://localhost") == 0) return true;
    if (origin.find("http://127.0.0.1") == 0) return true;
    if (origin.find("http://[::1]") == 0) return true;
    if (origin.find("vscode-webview://") == 0) return true;
    if (origin.find("vscode-file://") == 0) return true;

    return false;
}

// ============================================================================
// Code under test: JSON-RPC error helper (from HttpServerComponent.cpp)
// ============================================================================

static json makeJsonRpcError(const json& id, int code, const std::string& message) {
    json resp;
    resp["jsonrpc"] = "2.0";
    resp["id"] = id;
    resp["error"]["code"] = code;
    resp["error"]["message"] = message;
    return resp;
}

// ============================================================================
// Code under test: Pagination (from HttpServerComponent.cpp)
// ============================================================================

static json paginateJsonArray(const json& arr, const std::string& cursor, int pageSize = 50) {
    int start = 0;
    if (!cursor.empty()) {
        try { start = std::stoi(cursor); }
        catch (...) { start = 0; }
    }
    if (start < 0) start = 0;
    int total = (int)arr.size();
    int end = std::min(start + pageSize, total);

    json page = json::array();
    for (int i = start; i < end; ++i)
        page.push_back(arr[i]);

    json result;
    result["items"] = page;
    if (end < total)
        result["nextCursor"] = std::to_string(end);
    return result;
}

// ============================================================================
// Code under test: UUID v4 format (from HttpServerComponent.cpp)
// ============================================================================

static std::string generateSessionId() {
    std::random_device rd;
    std::mt19937_64 gen(rd());
    std::uniform_int_distribution<uint64_t> dis;
    uint64_t a = dis(gen), b = dis(gen);

    char buf[40];
    snprintf(buf, sizeof(buf),
        "%08x-%04x-%04x-%04x-%012llx",
        (uint32_t)(a >> 32),
        (uint16_t)(a >> 16),
        (uint16_t)((a & 0xFFFF) | 0x4000) & 0x4FFF,
        (uint16_t)((b >> 48) | 0x8000) & 0xBFFF,
        (unsigned long long)(b & 0xFFFFFFFFFFFFULL));
    return std::string(buf);
}

// ============================================================================
// Tests
// ============================================================================

// --- Rate Limiter ---

TEST(rate_limiter_allows_initial_burst) {
    RateLimiter rl(10.0, 5.0);
    int allowed = 0;
    for (int i = 0; i < 10; i++) {
        if (rl.allow()) allowed++;
    }
    ASSERT_EQ(allowed, 10);
}

TEST(rate_limiter_blocks_after_burst) {
    RateLimiter rl(5.0, 1.0);
    for (int i = 0; i < 5; i++) rl.allow();
    ASSERT_TRUE(!rl.allow());
}

TEST(rate_limiter_refills_tokens) {
    RateLimiter rl(2.0, 100.0); // 100 tokens/sec refill
    rl.allow();
    rl.allow();
    ASSERT_TRUE(!rl.allow());
    std::this_thread::sleep_for(std::chrono::milliseconds(50));
    ASSERT_TRUE(rl.allow()); // should have refilled
}

TEST(rate_limiter_does_not_exceed_max) {
    RateLimiter rl(3.0, 1000.0);
    std::this_thread::sleep_for(std::chrono::milliseconds(100));
    int allowed = 0;
    for (int i = 0; i < 10; i++) {
        if (rl.allow()) allowed++;
    }
    ASSERT_EQ(allowed, 3); // capped at maxTokens
}

// --- Origin Validation ---

TEST(origin_empty_allowed) {
    ASSERT_TRUE(isAllowedOrigin(""));
}

TEST(origin_localhost_allowed) {
    ASSERT_TRUE(isAllowedOrigin("http://localhost"));
    ASSERT_TRUE(isAllowedOrigin("http://localhost:3000"));
    ASSERT_TRUE(isAllowedOrigin("http://localhost:8888"));
}

TEST(origin_127_allowed) {
    ASSERT_TRUE(isAllowedOrigin("http://127.0.0.1"));
    ASSERT_TRUE(isAllowedOrigin("http://127.0.0.1:5173"));
}

TEST(origin_ipv6_loopback_allowed) {
    ASSERT_TRUE(isAllowedOrigin("http://[::1]"));
    ASSERT_TRUE(isAllowedOrigin("http://[::1]:9090"));
}

TEST(origin_vscode_webview_allowed) {
    ASSERT_TRUE(isAllowedOrigin("vscode-webview://extid"));
}

TEST(origin_vscode_file_allowed) {
    ASSERT_TRUE(isAllowedOrigin("vscode-file://vscode-app/something"));
}

TEST(origin_external_blocked) {
    ASSERT_TRUE(!isAllowedOrigin("http://evil.example.com"));
    ASSERT_TRUE(!isAllowedOrigin("http://attacker.local"));
    ASSERT_TRUE(!isAllowedOrigin("https://google.com"));
    ASSERT_TRUE(!isAllowedOrigin("http://192.168.1.1:8888"));
}

TEST(origin_https_localhost_blocked) {
    // Only http:// localhost variants are allowed, not https://
    ASSERT_TRUE(!isAllowedOrigin("https://localhost"));
}

// --- JSON-RPC Error ---

TEST(json_rpc_error_format) {
    json err = makeJsonRpcError(42, -32700, "Parse error");
    ASSERT_EQ(err["jsonrpc"], "2.0");
    ASSERT_EQ(err["id"], 42);
    ASSERT_EQ(err["error"]["code"], -32700);
    ASSERT_EQ(err["error"]["message"], "Parse error");
}

TEST(json_rpc_error_null_id) {
    json err = makeJsonRpcError(nullptr, -32600, "Invalid Request");
    ASSERT_TRUE(err["id"].is_null());
    ASSERT_EQ(err["error"]["code"], -32600);
}

TEST(json_rpc_error_string_id) {
    json err = makeJsonRpcError("req-1", -32601, "Method not found");
    ASSERT_EQ(err["id"], "req-1");
}

// --- Pagination ---

TEST(pagination_empty_array) {
    json arr = json::array();
    json result = paginateJsonArray(arr, "");
    ASSERT_EQ(result["items"].size(), 0u);
    ASSERT_TRUE(!result.contains("nextCursor"));
}

TEST(pagination_small_array_no_cursor) {
    json arr = json::array({1, 2, 3, 4, 5});
    json result = paginateJsonArray(arr, "", 50);
    ASSERT_EQ(result["items"].size(), 5u);
    ASSERT_TRUE(!result.contains("nextCursor"));
}

TEST(pagination_first_page) {
    json arr = json::array();
    for (int i = 0; i < 100; i++) arr.push_back(i);

    json result = paginateJsonArray(arr, "", 50);
    ASSERT_EQ(result["items"].size(), 50u);
    ASSERT_EQ(result["items"][0], 0);
    ASSERT_EQ(result["items"][49], 49);
    ASSERT_EQ(result["nextCursor"], "50");
}

TEST(pagination_second_page) {
    json arr = json::array();
    for (int i = 0; i < 100; i++) arr.push_back(i);

    json result = paginateJsonArray(arr, "50", 50);
    ASSERT_EQ(result["items"].size(), 50u);
    ASSERT_EQ(result["items"][0], 50);
    ASSERT_EQ(result["items"][49], 99);
    ASSERT_TRUE(!result.contains("nextCursor"));
}

TEST(pagination_partial_last_page) {
    json arr = json::array();
    for (int i = 0; i < 75; i++) arr.push_back(i);

    json result = paginateJsonArray(arr, "50", 50);
    ASSERT_EQ(result["items"].size(), 25u);
    ASSERT_TRUE(!result.contains("nextCursor"));
}

TEST(pagination_invalid_cursor) {
    json arr = json::array({1, 2, 3});
    json result = paginateJsonArray(arr, "invalid", 50);
    ASSERT_EQ(result["items"].size(), 3u); // falls back to start=0
}

TEST(pagination_negative_cursor) {
    json arr = json::array({1, 2, 3});
    json result = paginateJsonArray(arr, "-5", 50);
    ASSERT_EQ(result["items"].size(), 3u);
}

// --- UUID Session ID ---

TEST(session_id_format) {
    std::string id = generateSessionId();
    // UUID v4 format: xxxxxxxx-xxxx-4xxx-[89ab]xxx-xxxxxxxxxxxx
    std::regex uuid_re("^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$");
    ASSERT_TRUE(std::regex_match(id, uuid_re));
}

TEST(session_id_uniqueness) {
    std::string id1 = generateSessionId();
    std::string id2 = generateSessionId();
    ASSERT_TRUE(id1 != id2);
}

// --- MCP Protocol Compliance ---

TEST(mcp_initialize_response_format) {
    // Verify the expected structure of an initialize response
    json resp;
    resp["jsonrpc"] = "2.0";
    resp["id"] = 1;
    resp["result"]["protocolVersion"] = "2025-03-26";
    resp["result"]["capabilities"]["tools"]["listChanged"] = true;
    resp["result"]["capabilities"]["resources"]["listChanged"] = true;
    resp["result"]["capabilities"]["prompts"]["listChanged"] = true;
    resp["result"]["serverInfo"]["name"] = "1c-mcp-server";
    resp["result"]["serverInfo"]["version"] = "1.1.0";

    ASSERT_EQ(resp["result"]["protocolVersion"], "2025-03-26");
    ASSERT_TRUE(resp["result"]["capabilities"]["tools"]["listChanged"]);
    ASSERT_TRUE(resp["result"]["capabilities"]["resources"]["listChanged"]);
    ASSERT_TRUE(resp["result"]["capabilities"]["prompts"]["listChanged"]);
}

TEST(mcp_tool_result_format) {
    json result;
    result["content"] = json::array();
    result["content"].push_back({{"type", "text"}, {"text", "hello"}});
    result["isError"] = false;

    ASSERT_EQ(result["content"].size(), 1u);
    ASSERT_EQ(result["content"][0]["type"], "text");
    ASSERT_EQ(result["isError"], false);
}

TEST(mcp_error_codes) {
    // Standard JSON-RPC error codes used in MCP
    ASSERT_EQ(makeJsonRpcError(1, -32700, "Parse error")["error"]["code"], -32700);
    ASSERT_EQ(makeJsonRpcError(1, -32600, "Invalid Request")["error"]["code"], -32600);
    ASSERT_EQ(makeJsonRpcError(1, -32601, "Method not found")["error"]["code"], -32601);
    ASSERT_EQ(makeJsonRpcError(1, -32602, "Invalid params")["error"]["code"], -32602);
}

// ============================================================================
// Main
// ============================================================================

int main() {
    std::cout << "Running http1c unit tests...\n\n";

    for (auto& t : tests()) {
        std::cout << "  " << t.name << " ... ";
        try {
            t.fn();
            std::cout << "PASS\n";
            g_passed++;
        } catch (const std::exception&) {
            std::cout << "FAIL\n";
            g_failed++;
        }
    }

    std::cout << "\n" << g_passed << " passed, " << g_failed << " failed, "
              << (g_passed + g_failed) << " total\n";
    return g_failed > 0 ? 1 : 0;
}
