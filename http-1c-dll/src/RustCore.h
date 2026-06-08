#ifndef __RUSTCORE_H__
#define __RUSTCORE_H__

// ---------------------------------------------------------------------------
// RustCore.h — C++ surface for the Rust search core (rust-core/, crate `rcore`).
//
// The Rust staticlib is linked into this DLL (see CMakeLists.txt). It exposes a
// minimal JSON-in / JSON-out C ABI. This header declares those entry points and
// provides a tiny RAII helper so C++ call sites cannot leak the returned
// strings.
//
// MEMORY OWNERSHIP (must match rust-core/src/lib.rs):
//   Every `char*` returned by an rcore_* function is allocated by Rust and MUST
//   be released by calling rcore_free_string EXACTLY ONCE. Never call free() /
//   delete on it (that would be a cross-CRT free → heap corruption). Prefer the
//   RustString wrapper below, which frees in its destructor and forbids copies.
//
// NOTE: This header only makes the boundary callable. Wiring rcore_dispatch into
// tools/call routing / HttpServerComponent is a separate Stage 1 card
// (stage1-native-tool-routing) and is intentionally NOT done here.
// ---------------------------------------------------------------------------

#include <string>
#include <utility> // std::exchange

extern "C" {

// Returns a JSON object string: {"name","version","abi"}.
// Caller owns the result; free with rcore_free_string.
char* rcore_version(void);

// Generic JSON-in / JSON-out entry point.
//   method       — operation name, e.g. "ping" / "stats" / "reset".
//   payload_json — method arguments as JSON; may be nullptr or "" (= no args).
// Returns a JSON envelope: {"ok":true,"result":...} or
// {"ok":false,"error":{"code":...,"message":...}}.
// Never returns nullptr. Caller owns the result; free with rcore_free_string.
char* rcore_dispatch(const char* method, const char* payload_json);

// Frees a string previously returned by an rcore_* function.
// Passing nullptr is a safe no-op. Never call on the same pointer twice.
void rcore_free_string(char* s);

// Best-effort teardown (cancel + join background workers). Stage 0 no-op;
// idempotent and safe to call from a shutdown / form-close path.
// Hook onto doStopListen()/form-close, NOT ~HttpServerComponent — the Rust
// singleton outlives any single component instance.
void rcore_shutdown(void);

} // extern "C"

// ---------------------------------------------------------------------------
// RustString — RAII owner for a char* returned by the Rust core.
//
// Guarantees the string is freed via rcore_free_string exactly once. Move-only
// (copying would risk a double free). Use .c_str() to read, .str() to copy into
// a std::string.
//
//   RustString r = RustString::adopt(rcore_dispatch("ping", "{}"));
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
            rcore_free_string(ptr_);
            ptr_ = nullptr;
        }
    }

    char* ptr_;
};

#endif // __RUSTCORE_H__
