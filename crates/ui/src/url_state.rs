//! URL-backed state helpers (UI-27) — the Leptos counterpart of the
//! reference's `useSearchParams` + `setSearchParams(next, { replace: true })`
//! pattern.
//!
//! The reference keeps every list-page filter in the URL so scope is
//! shareable and back-button safe:
//! - dashboard: `?days=30&mailboxes=1,2&channel=chat`
//! - inbox: `?view=active&tag=x&channel=chat&page=2&q=…`
//! - knowledge: `?doc=N` (opens the document reader)
//! - docs: `?article=N` (opens the article reader)
//! - issues: `?tab=known` (opens that tab)
//!
//! Leptos Router 0.6 exposes `use_query_map()` (a `Memo<ParamsMap>`) for
//! reads and `use_navigate()` with `NavigateOptions { replace: true }` for
//! writes; this module adds the small pieces both need so every page uses
//! ONE implementation (A12: one source of truth per pattern).

use leptos_router::*;

/// Percent-encode one query value (RFC 3986 unreserved set kept as-is).
#[must_use]
pub fn encode_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// One query parameter (name + optional value; `None` or `""` drops the
/// param, like the reference's `next.delete(key)`).
pub type Param = (&'static str, Option<String>);

/// Build the query string (`"?a=b&c=d"` or `""`) from pairs, preserving
/// order and skipping empty values.
#[must_use]
pub fn query_string(pairs: &[Param]) -> String {
    let mut out = String::new();
    for (name, value) in pairs {
        if let Some(v) = value {
            if v.is_empty() {
                continue;
            }
            if out.is_empty() {
                out.push('?');
            } else {
                out.push('&');
            }
            out.push_str(name);
            out.push('=');
            out.push_str(&encode_value(v));
        }
    }
    out
}

/// Read one param from the query map as a non-empty `String`.
#[must_use]
pub fn query_str(map: &ParamsMap, key: &str) -> Option<String> {
    map.get(key)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Read one positive-integer param (the reference's
/// `Number.isFinite(Number(x))` guard: garbage means absent).
#[must_use]
pub fn query_pos_int(map: &ParamsMap, key: &str) -> Option<i64> {
    query_str(map, key).and_then(|v| v.parse::<i64>().ok().filter(|n| *n > 0))
}

/// Replace the current URL's query with `pairs`, keeping the pathname —
/// the reference's `setSearchParams(next, { replace: true })`.
///
/// `navigate` is the router's navigate fn; `pathname` the current path
/// (from `use_location().pathname`) so the page does not need to re-read it
/// inside the helper.
pub fn replace_query<N>(navigate: &N, pathname: &str, pairs: &[Param])
where
    N: Fn(&str, NavigateOptions) + 'static,
{
    navigate(
        &format!("{}{}", pathname, query_string(pairs)),
        NavigateOptions {
            replace: true,
            ..Default::default()
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_only_reserved_characters() {
        assert_eq!(encode_value("a b"), "a%20b");
        assert_eq!(encode_value("a&b=c"), "a%26b%3Dc");
        assert_eq!(encode_value("plain-1_2.3~"), "plain-1_2.3~");
        assert_eq!(encode_value("héllo"), "h%C3%A9llo");
    }

    #[test]
    fn query_string_skips_empty_and_none() {
        let qs = query_string(&[
            ("days", Some("30".into())),
            ("mailboxes", None),
            ("channel", Some(String::new())),
            ("tag", Some("billing issue".into())),
        ]);
        assert_eq!(qs, "?days=30&tag=billing%20issue");
    }

    #[test]
    fn query_string_empty_when_no_values() {
        assert_eq!(query_string(&[]), "");
        assert_eq!(
            query_string(&[("a", None), ("b", Some(String::new()))]),
            "",
            "all-empty params produce no ?"
        );
    }

    #[test]
    fn query_str_trims_and_drops_empty() {
        let mut map = ParamsMap::new();
        map.insert("view".into(), "active".into());
        map.insert("tag".into(), "  ".into());
        assert_eq!(query_str(&map, "view"), Some("active".to_string()));
        assert_eq!(query_str(&map, "tag"), None, "blank means absent");
        assert_eq!(query_str(&map, "missing"), None);
    }

    #[test]
    fn query_pos_int_guards_garbage() {
        let mut map = ParamsMap::new();
        map.insert("doc".into(), "42".into());
        map.insert("bad".into(), "abc".into());
        map.insert("neg".into(), "-3".into());
        assert_eq!(query_pos_int(&map, "doc"), Some(42));
        assert_eq!(query_pos_int(&map, "bad"), None);
        assert_eq!(query_pos_int(&map, "neg"), None, "non-positive is absent");
        assert_eq!(query_pos_int(&map, "missing"), None);
    }
}
