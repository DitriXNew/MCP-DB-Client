//! JSON envelopes that cross the FFI boundary.
//!
//! Every call to `rcore_dispatch` returns a JSON object with a top-level `ok`
//! field. On success: `{"ok": true, "result": <method-specific>}`. On failure:
//! `{"ok": false, "error": {"code": "...", "message": "..."}}`.
//!
//! Errors are *structural* — they are normal return values, never panics. This
//! keeps the C ABI boundary panic-free: a malformed payload or unknown method
//! produces a well-formed JSON error object that the C++ side can inspect,
//! rather than unwinding across the FFI boundary (which is undefined behaviour).

use serde::Serialize;
use serde_json::Value;

/// Stable error codes returned in the `error.code` field. Kept as `&'static str`
/// constants so call sites (and future Stage 1 code) share one source of truth.
pub mod codes {
    pub const UNKNOWN_METHOD: &str = "unknown_method";
    pub const BAD_PAYLOAD: &str = "bad_payload";
    pub const INTERNAL: &str = "internal";
}

/// A structural error object: `{"code": ..., "message": ...}`.
#[derive(Serialize)]
pub struct ErrorBody {
    pub code: &'static str,
    pub message: String,
}

/// The success/failure envelope serialized back to C as a JSON string.
#[derive(Serialize)]
#[serde(untagged)]
pub enum Envelope {
    Ok { ok: bool, result: Value },
    Err { ok: bool, error: ErrorBody },
}

impl Envelope {
    /// Build a success envelope: `{"ok": true, "result": <result>}`.
    pub fn ok(result: Value) -> Self {
        Envelope::Ok { ok: true, result }
    }

    /// Build an error envelope: `{"ok": false, "error": {"code", "message"}}`.
    pub fn err(code: &'static str, message: impl Into<String>) -> Self {
        Envelope::Err {
            ok: false,
            error: ErrorBody {
                code,
                message: message.into(),
            },
        }
    }

    /// Serialize to a JSON string. Serialization of these envelopes cannot
    /// realistically fail (only owned, serializable types), but if it ever
    /// does we fall back to a hand-written constant error string so the
    /// boundary never returns malformed JSON.
    pub fn to_json_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            String::from(r#"{"ok":false,"error":{"code":"internal","message":"serialization failed"}}"#)
        })
    }
}
