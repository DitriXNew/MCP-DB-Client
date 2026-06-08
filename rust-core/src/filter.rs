//! Combinable, domain-agnostic meta filters shared by `search` and `grep`.
//!
//! The core stores only free-form `meta` (a JSON object) on documents and on
//! segments — there are **no** domain-specific fields (no `sku`, `inn`,
//! `scenario_name`, …) baked into the schema. Filters therefore operate purely
//! over arbitrary key/value pairs.
//!
//! ## Effective meta
//!
//! A hit is filtered against its *effective* meta: the document-level `meta`
//! merged with the segment-level `meta`, where **segment keys override document
//! keys on collision**. Build it with [`effective_meta`]. (Rationale: a segment
//! is a more specific scope than its document, so the narrower annotation wins.)
//!
//! ## Filter shape (parsed from the request payload's `filter` object)
//!
//! ```json
//! "filter": {
//!   "all":      { "kind": "manual", "lang": "en" },   // AND over key==value
//!   "any":      { "status": ["draft", "review"] },     // OR  over key==value
//!   "tags_all": ["security", "db"],                     // every tag present
//!   "tags_any": ["draft", "wip"]                        // at least one tag
//! }
//! ```
//!
//! All four sub-filters are optional and **combinable**; a hit passes only if it
//! satisfies *every* sub-filter that is present (the sub-filters are ANDed with
//! each other, while `any`/`tags_any` are internally ORed). An absent / empty
//! filter matches everything.
//!
//! ### Value matching
//! For a single `key: value` constraint:
//!   * if the constraint `value` is an **array**, the constraint is satisfied
//!     when the effective meta's value for `key` equals *any* element (so you can
//!     express "key is one of …" inline);
//!   * otherwise the constraint is satisfied when the effective meta's value for
//!     `key` equals the constraint value, OR — when the meta value is itself an
//!     array — when the constraint value is a member of it.
//!
//! ### Tags
//! Tags are modelled as a conventional `"tags"` key in meta holding an array of
//! strings (any non-array/absent value is treated as "no tags"). `tags_all`
//! requires every listed tag to be present; `tags_any` requires at least one.

use serde_json::Value;

/// A parsed, ready-to-apply set of meta filters. An all-empty `MetaFilter`
/// matches every hit (the common no-filter case is essentially free).
#[derive(Debug, Default, Clone)]
pub struct MetaFilter {
    /// AND: every `(key, value)` must match the effective meta.
    all: Vec<(String, Value)>,
    /// OR: at least one `(key, value)` must match. Empty ⇒ this clause is unused.
    any: Vec<(String, Value)>,
    /// Every tag in this list must be present in the effective `tags` array.
    tags_all: Vec<String>,
    /// At least one tag in this list must be present. Empty ⇒ clause unused.
    tags_any: Vec<String>,
}

impl MetaFilter {
    /// True when no sub-filter is set, so the filter matches everything. Lets
    /// callers cheaply skip building effective meta on the hot path.
    pub fn is_empty(&self) -> bool {
        self.all.is_empty()
            && self.any.is_empty()
            && self.tags_all.is_empty()
            && self.tags_any.is_empty()
    }

    /// Parse a `filter` object from a request payload. A missing/non-object
    /// `filter` (the common case) yields an empty filter that matches all.
    ///
    /// Unknown keys inside `filter` are ignored (forward-compatible). The `all`
    /// and `any` clauses must be JSON objects; anything else is ignored.
    pub fn parse(payload: &Value) -> MetaFilter {
        let filter = match payload.get("filter") {
            Some(f) if f.is_object() => f,
            _ => return MetaFilter::default(),
        };

        MetaFilter {
            all: parse_kv_object(filter.get("all")),
            any: parse_kv_object(filter.get("any")),
            tags_all: parse_string_array(filter.get("tags_all")),
            tags_any: parse_string_array(filter.get("tags_any")),
        }
    }

    /// Evaluate the filter against an already-computed effective meta object.
    /// Returns `true` if the hit passes every present sub-filter.
    pub fn matches(&self, effective: &Value) -> bool {
        if self.is_empty() {
            return true;
        }

        // all: every constraint must hold (AND).
        for (k, v) in &self.all {
            if !constraint_matches(effective, k, v) {
                return false;
            }
        }

        // any: at least one constraint must hold (OR). Only enforced if present.
        if !self.any.is_empty() && !self.any.iter().any(|(k, v)| constraint_matches(effective, k, v))
        {
            return false;
        }

        // tags: derived from the conventional `tags` string array.
        if !self.tags_all.is_empty() || !self.tags_any.is_empty() {
            let tags = effective_tags(effective);
            if !self.tags_all.iter().all(|t| tags.iter().any(|x| x == t)) {
                return false;
            }
            if !self.tags_any.is_empty()
                && !self.tags_any.iter().any(|t| tags.iter().any(|x| x == t))
            {
                return false;
            }
        }

        true
    }

    /// Convenience: build the effective meta from a `(doc_meta, seg_meta)` pair
    /// and test it. Avoids allocating the merged object when the filter is empty.
    pub fn matches_doc_seg(&self, doc_meta: &Value, seg_meta: &Value) -> bool {
        if self.is_empty() {
            return true;
        }
        let eff = effective_meta(doc_meta, seg_meta);
        self.matches(&eff)
    }
}

/// Merge document-level meta with segment-level meta into the *effective* meta:
/// start from the document's keys, then overlay the segment's keys so a segment
/// value wins on any key collision. Non-object inputs contribute nothing.
pub fn effective_meta(doc_meta: &Value, seg_meta: &Value) -> Value {
    let mut out = serde_json::Map::new();
    if let Some(obj) = doc_meta.as_object() {
        for (k, v) in obj {
            out.insert(k.clone(), v.clone());
        }
    }
    if let Some(obj) = seg_meta.as_object() {
        for (k, v) in obj {
            out.insert(k.clone(), v.clone()); // segment overrides document
        }
    }
    Value::Object(out)
}

/// Pull the effective `tags` as a list of string slices. A missing or non-array
/// `tags` value yields an empty list; non-string array elements are skipped.
fn effective_tags(effective: &Value) -> Vec<&str> {
    match effective.get("tags").and_then(|t| t.as_array()) {
        Some(arr) => arr.iter().filter_map(|x| x.as_str()).collect(),
        None => Vec::new(),
    }
}

/// Does the effective meta satisfy a single `key == value` constraint?
///
/// See the module docs for the array semantics: an array `want` is an inline
/// "one of"; an array meta value is membership-tested against a scalar `want`.
fn constraint_matches(effective: &Value, key: &str, want: &Value) -> bool {
    let have = match effective.get(key) {
        Some(v) => v,
        None => return false, // key absent ⇒ cannot match
    };

    match want {
        // Constraint value is an array ⇒ "have equals any element of want".
        Value::Array(opts) => opts.iter().any(|opt| value_eq_or_member(have, opt)),
        // Scalar constraint ⇒ equality, or membership if `have` is an array.
        _ => value_eq_or_member(have, want),
    }
}

/// Equality between a meta value and a wanted scalar, with array-membership: if
/// `have` is itself an array, the constraint matches when `want` is one of its
/// elements (so a multi-valued meta key like `["a","b"]` matches `want = "a"`).
fn value_eq_or_member(have: &Value, want: &Value) -> bool {
    if have == want {
        return true;
    }
    if let Some(arr) = have.as_array() {
        return arr.iter().any(|el| el == want);
    }
    false
}

/// Parse a JSON object into a `Vec<(key, value)>`, preserving every key. A
/// missing or non-object input yields an empty vec.
fn parse_kv_object(v: Option<&Value>) -> Vec<(String, Value)> {
    match v.and_then(|x| x.as_object()) {
        Some(obj) => obj.iter().map(|(k, val)| (k.clone(), val.clone())).collect(),
        None => Vec::new(),
    }
}

/// Parse a JSON array of strings into `Vec<String>`. Non-string elements are
/// skipped; a missing or non-array input yields an empty vec.
fn parse_string_array(v: Option<&Value>) -> Vec<String> {
    match v.and_then(|x| x.as_array()) {
        Some(arr) => arr.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_filter_matches_everything() {
        let f = MetaFilter::parse(&json!({}));
        assert!(f.is_empty());
        assert!(f.matches(&json!({"anything": "goes"})));
    }

    #[test]
    fn effective_meta_segment_overrides_document() {
        let doc = json!({"kind": "manual", "lang": "en"});
        let seg = json!({"lang": "ru", "section": "intro"});
        let eff = effective_meta(&doc, &seg);
        assert_eq!(eff["kind"], json!("manual")); // from doc
        assert_eq!(eff["lang"], json!("ru")); // segment overrides doc
        assert_eq!(eff["section"], json!("intro")); // from segment
    }

    #[test]
    fn all_is_and() {
        let f = MetaFilter::parse(&json!({"filter": {"all": {"kind": "manual", "lang": "en"}}}));
        assert!(f.matches(&json!({"kind": "manual", "lang": "en"})));
        // Missing one key ⇒ fails the AND.
        assert!(!f.matches(&json!({"kind": "manual"})));
        assert!(!f.matches(&json!({"kind": "manual", "lang": "ru"})));
    }

    #[test]
    fn any_is_or() {
        let f = MetaFilter::parse(&json!({"filter": {"any": {"status": "draft", "kind": "note"}}}));
        assert!(f.matches(&json!({"status": "draft"})));
        assert!(f.matches(&json!({"kind": "note"})));
        assert!(!f.matches(&json!({"status": "published", "kind": "manual"})));
    }

    #[test]
    fn combined_all_and_any() {
        let f = MetaFilter::parse(&json!({"filter": {
            "all": {"lang": "en"},
            "any": {"status": ["draft", "review"]}
        }}));
        // Passes: lang matches AND status is one of the OR options.
        assert!(f.matches(&json!({"lang": "en", "status": "review"})));
        // Fails the AND (wrong lang) even though status matches.
        assert!(!f.matches(&json!({"lang": "ru", "status": "draft"})));
        // Fails the OR (status not in the set) even though lang matches.
        assert!(!f.matches(&json!({"lang": "en", "status": "published"})));
    }

    #[test]
    fn array_constraint_is_one_of() {
        let f = MetaFilter::parse(&json!({"filter": {"all": {"kind": ["manual", "guide"]}}}));
        assert!(f.matches(&json!({"kind": "guide"})));
        assert!(!f.matches(&json!({"kind": "note"})));
    }

    #[test]
    fn array_meta_value_membership() {
        // A multi-valued meta key matches a scalar constraint by membership.
        let f = MetaFilter::parse(&json!({"filter": {"all": {"role": "admin"}}}));
        assert!(f.matches(&json!({"role": ["user", "admin"]})));
        assert!(!f.matches(&json!({"role": ["user", "guest"]})));
    }

    #[test]
    fn tags_all_and_any() {
        let f = MetaFilter::parse(&json!({"filter": {
            "tags_all": ["db", "security"],
            "tags_any": ["draft", "wip"]
        }}));
        assert!(f.matches(&json!({"tags": ["db", "security", "wip", "extra"]})));
        // Missing a required tag.
        assert!(!f.matches(&json!({"tags": ["db", "wip"]})));
        // Has both required, but none of the any tags.
        assert!(!f.matches(&json!({"tags": ["db", "security"]})));
        // No tags at all.
        assert!(!f.matches(&json!({})));
    }

    #[test]
    fn matches_doc_seg_uses_effective() {
        let f = MetaFilter::parse(&json!({"filter": {"all": {"lang": "ru"}}}));
        // doc says en, segment overrides to ru ⇒ effective lang is ru ⇒ matches.
        assert!(f.matches_doc_seg(&json!({"lang": "en"}), &json!({"lang": "ru"})));
        assert!(!f.matches_doc_seg(&json!({"lang": "en"}), &json!({})));
    }
}
