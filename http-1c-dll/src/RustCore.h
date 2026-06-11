#ifndef HTTP1C_RUSTCORE_H
#define HTTP1C_RUSTCORE_H

// ---------------------------------------------------------------------------
// RustCore.h — C++ surface for the Rust search core (rust-core/, crate `rcore`).
//
// The search core ships as a SEPARATE rcore.dll (cdylib, built /MD with a
// self-contained static onnxruntime). The C++ component is /MT and cannot
// compile under /MD (char16_t streams hit MSVC C2491), so it does NOT link the
// core: it loads rcore.dll at RUNTIME via LoadLibrary + GetProcAddress. One
// libhttp1cWin.dll therefore serves both packages:
//   * "lite" — rcore.dll absent → RCore::available() == false → search tools
//     return a structured "install RAG" result.
//   * "full" — rcore.dll present (shipped next to libhttp1cWin.dll) → real
//     fastembed search.
//
// rcore.dll exposes a minimal JSON-in / JSON-out C ABI (see rust-core/src):
//   char* rcore_version(void);
//   char* rcore_dispatch(const char* method, const char* payload_json);
//   void  rcore_free_string(char* s);
//   void  rcore_shutdown(void);
//
// MEMORY OWNERSHIP (must match rust-core/src/lib.rs):
//   Every `char*` returned by an rcore_* function is allocated by Rust and MUST
//   be released by calling rcore_free_string EXACTLY ONCE. Never call free() /
//   delete on it — that would be a cross-CRT (and now cross-DLL) free → heap
//   corruption. The RustString wrapper below frees via the LOADED
//   rcore_free_string pointer, so ownership stays entirely inside rcore.dll.
// ---------------------------------------------------------------------------

#include <string>
#include <utility> // std::exchange

// The Rust core ships as a Windows DLL only; on non-Windows builds (the CMake
// project still supports UNIX) this header degrades to inert stubs so the rest
// of the component compiles as the lite variant — no <windows.h> leakage.
#if defined(_WIN32)
#include <atomic>  // std::atomic<bool> ready (lock-free fast path)
#include <mutex>   // std::mutex serializing load attempts

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>
#endif // _WIN32

// ---------------------------------------------------------------------------
// RCore — lazy, thread-safe runtime loader for rcore.dll.
//
// The DLL is located NEXT TO THIS COMPONENT (libhttp1cWin.dll), not the process
// working directory: we resolve this module's own path via GetModuleHandleExW
// (FROM_ADDRESS of a function defined here) + GetModuleFileNameW, then load
// "<that dir>\\rcore.dll".
//
// A FAILED load is retried on the next call (serialized by a mutex): the user
// can install rcore.dll next to libhttp1cWin.dll while 1C is running (e.g. the
// VA plugin's "install from archive" button) and the very next search call
// picks it up — no process restart. A failed probe is just LoadLibrary on a
// missing file (cheap), and it only happens on lite-mode search calls. Once
// loaded, the DLL stays loaded; the lock-free `ready` fast path makes the
// loaded case contention-free for concurrent httplib worker threads.
//
// If rcore.dll is missing or any of the 4 entry points is absent, available()
// returns false and the loader is an inert no-op (lite component) — never a
// crash.
// ---------------------------------------------------------------------------
class RustString; // fwd

namespace RCore {

#if defined(_WIN32)

// ---- C ABI signatures of rcore.dll's exported entry points. ----
using version_fn_t   = char* (*)(void);
using dispatch_fn_t  = char* (*)(const char*, const char*);
using free_fn_t      = void  (*)(char*);
using shutdown_fn_t  = void  (*)(void);

namespace detail {

// Loader state. Pointer fields are written only under loadMutex(); readers
// reach them through the `ready` acquire/release handshake in loaded(): the
// release store of ready=true happens AFTER the pointers are populated, so any
// thread that observes ready==true also observes the resolved pointers.
struct State {
    HMODULE       module   = nullptr;
    version_fn_t  version  = nullptr;
    dispatch_fn_t dispatch = nullptr;
    free_fn_t     freeStr  = nullptr;
    shutdown_fn_t shutdown = nullptr;
    std::atomic<bool> ready{false}; // module loaded AND all 4 symbols resolved
};

// An ordinary function whose address lives inside THIS module — used as the
// FROM_ADDRESS anchor so GetModuleHandleExW resolves libhttp1cWin.dll (not the
// host process). inline → one definition across all translation units.
inline void anchor() {}

inline State& state() {
    static State s;
    return s;
}

inline std::mutex& loadMutex() {
    static std::mutex m;
    return m;
}

// Directory containing this component DLL, with a trailing backslash, or empty
// on failure. Uses the address of anchor() to identify the owning module.
inline std::wstring moduleDir() {
    HMODULE self = nullptr;
    if (!GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS |
            GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            reinterpret_cast<LPCWSTR>(&anchor),
            &self)) {
        return std::wstring();
    }

    std::wstring path(MAX_PATH, L'\0');
    for (;;) {
        DWORD len = GetModuleFileNameW(self, path.data(),
                                       static_cast<DWORD>(path.size()));
        if (len == 0) {
            return std::wstring();
        }
        if (len < path.size()) {
            path.resize(len);
            break;
        }
        // Buffer was too small (truncated) — grow and retry.
        path.resize(path.size() * 2);
    }

    std::wstring::size_type slash = path.find_last_of(L"\\/");
    if (slash == std::wstring::npos) {
        return std::wstring();
    }
    return path.substr(0, slash + 1); // keep the trailing separator
}

inline void load(State& s) {
    std::wstring dir = moduleDir();
    if (dir.empty()) {
        return;
    }

    s.module = LoadLibraryW((dir + L"rcore.dll").c_str());
    if (!s.module) {
        return; // lite component: rcore.dll simply isn't installed.
    }

    s.version  = reinterpret_cast<version_fn_t>(
        GetProcAddress(s.module, "rcore_version"));
    s.dispatch = reinterpret_cast<dispatch_fn_t>(
        GetProcAddress(s.module, "rcore_dispatch"));
    s.freeStr  = reinterpret_cast<free_fn_t>(
        GetProcAddress(s.module, "rcore_free_string"));
    s.shutdown = reinterpret_cast<shutdown_fn_t>(
        GetProcAddress(s.module, "rcore_shutdown"));

    // Require the whole ABI: a partial/mismatched DLL is treated as "not there"
    // so we degrade to the lite path instead of crashing on a null pointer.
    // (A DLL caught mid-copy by "install from archive" lands here too — it is
    // unloaded and the NEXT call retries the now-complete file.)
    if (s.version && s.dispatch && s.freeStr && s.shutdown) {
        s.ready.store(true, std::memory_order_release);
        return;
    }
    FreeLibrary(s.module);
    s.module   = nullptr;
    s.version  = nullptr;
    s.dispatch = nullptr;
    s.freeStr  = nullptr;
    s.shutdown = nullptr;
}

inline const State& loaded() {
    State& s = state();
    // Fast path: already loaded — lock-free (acquire pairs with the release
    // store in load(), making the resolved pointers visible).
    if (s.ready.load(std::memory_order_acquire)) {
        return s;
    }
    // Not loaded yet (or every previous attempt failed): retry under the lock
    // so a late-installed rcore.dll is picked up without a process restart.
    std::lock_guard<std::mutex> lock(loadMutex());
    if (!s.ready.load(std::memory_order_relaxed)) { // re-check under the lock
        load(s);
    }
    return s;
}

} // namespace detail

// True iff rcore.dll loaded AND all 4 entry points resolved (full component).
// Retries the load on every call until it succeeds (see loaded()), so this can
// flip lite -> full at runtime after the user installs rcore.dll.
inline bool available() {
    return detail::loaded().ready.load(std::memory_order_acquire);
}

// Free a char* returned by an rcore_* function, via the LOADED rcore_free_string
// pointer. Safe no-op if null or the core never loaded.
inline void freeString(char* s) {
    if (!s) {
        return;
    }
    const detail::State& st = detail::loaded();
    if (st.ready.load(std::memory_order_acquire) && st.freeStr) {
        st.freeStr(s);
    }
    // If the core isn't loaded we cannot have a pointer it allocated, so there
    // is nothing safe to free — intentionally leave it (never cross-CRT free()).
}

// {"name","version","abi"} as JSON, or "" if the core isn't available.
RustString version();

// Generic JSON-in / JSON-out call. Returns an empty RustString if the core
// isn't available (callers must check available() first / treat empty as error).
RustString dispatch(const std::string& method, const std::string& payloadJson);

// Best-effort teardown of the core's background workers. No-op if rcore.dll was
// never loaded. Idempotent; hook onto doStopListen()/form-close, NOT a
// component destructor — the Rust singleton outlives any single instance.
inline void shutdown() {
    const detail::State& st = detail::loaded();
    if (st.ready.load(std::memory_order_acquire) && st.shutdown) {
        st.shutdown();
    }
}

#else // !_WIN32 — inert lite stubs (no Rust core on this platform)

inline bool available() { return false; }
inline void freeString(char*) {}
inline void shutdown() {}

RustString version();
RustString dispatch(const std::string& method, const std::string& payloadJson);

#endif // _WIN32

} // namespace RCore

// ---------------------------------------------------------------------------
// RustString — RAII owner for a char* returned by the Rust core.
//
// Guarantees the string is freed via the loaded rcore_free_string exactly once.
// Move-only (copying would risk a double free). Use .c_str() to read, .str() to
// copy into a std::string.
//
//   RustString r = RCore::dispatch("ping", "{}");
//   std::string body = r.str();   // copy out; r frees the buffer on scope exit
// ---------------------------------------------------------------------------
class RustString {
public:
    // Take ownership of a char* returned by the Rust core.
    static RustString adopt(char* raw) { return RustString(raw); }

    RustString() noexcept : ptr_(nullptr) {}
    ~RustString() { reset(); }

    // Move-only: the wrapper is the unique owner of the buffer.
    RustString(RustString&& other) noexcept
        : ptr_(std::exchange(other.ptr_, nullptr)) {}
    RustString& operator=(RustString&& other) noexcept {
        if (this != &other) {
            reset();
            ptr_ = std::exchange(other.ptr_, nullptr);
        }
        return *this;
    }

    RustString(const RustString&) = delete;
    RustString& operator=(const RustString&) = delete;

    // Raw, still-owned pointer (may be nullptr). Read-only use.
    const char* c_str() const noexcept { return ptr_; }

    // Copy the contents into a std::string ("" if null).
    std::string str() const { return ptr_ ? std::string(ptr_) : std::string(); }

    bool valid() const noexcept { return ptr_ != nullptr; }

    // Release ownership without freeing (rarely needed).
    char* release() noexcept { return std::exchange(ptr_, nullptr); }

private:
    explicit RustString(char* raw) noexcept : ptr_(raw) {}

    void reset() noexcept {
        if (ptr_) {
            RCore::freeString(ptr_);
            ptr_ = nullptr;
        }
    }

    char* ptr_;
};

namespace RCore {

#if defined(_WIN32)

inline RustString version() {
    const detail::State& st = detail::loaded();
    if (!st.ready.load(std::memory_order_acquire)) {
        return RustString();
    }
    return RustString::adopt(st.version());
}

inline RustString dispatch(const std::string& method, const std::string& payloadJson) {
    const detail::State& st = detail::loaded();
    if (!st.ready.load(std::memory_order_acquire)) {
        return RustString();
    }
    return RustString::adopt(st.dispatch(method.c_str(), payloadJson.c_str()));
}

#else // !_WIN32

inline RustString version() { return RustString(); }
inline RustString dispatch(const std::string&, const std::string&) { return RustString(); }

#endif // _WIN32

} // namespace RCore

#endif // HTTP1C_RUSTCORE_H
