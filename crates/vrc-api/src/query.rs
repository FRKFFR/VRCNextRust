//! Shared helpers for building documented query strings.
//!
//! Search text is percent-encoded as a query *value* only — never
//! interpolated into a path (RULES.md §3).

/// Documented maximum page size (`n`) for list endpoints.
pub(crate) const MAX_PAGE_SIZE: u32 = 100;

/// Upper bound for `offset` so a hostile/huge page request cannot produce
/// an absurd query.
pub(crate) const MAX_OFFSET: u64 = 1_000_000;

/// Percent-encode every byte outside the RFC 3986 unreserved set.
pub(crate) fn query_escape(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(value.len());
    for &b in value.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0x0f) as usize] as char);
            }
        }
    }
    out
}

/// Append `key=value` to `path`, choosing `?`/`&` and skipping empty
/// values. The value is always query-escaped.
pub(crate) fn append_query(path: &mut String, key: &str, value: &str) {
    if value.is_empty() {
        return;
    }
    path.push(if path.contains('?') { '&' } else { '?' });
    path.push_str(key);
    path.push('=');
    path.push_str(&query_escape(value));
}

/// Append an optional query parameter, skipping `None`/empty values.
pub(crate) fn append_query_opt(path: &mut String, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        append_query(path, key, value);
    }
}

/// Append a boolean query parameter (`true`/`false`).
pub(crate) fn append_bool(path: &mut String, key: &str, value: Option<bool>) {
    if let Some(value) = value {
        append_query(path, key, if value { "true" } else { "false" });
    }
}

/// Clamp `n`/`offset` to the documented bounds.
pub(crate) fn page_params(n: Option<u32>, offset: Option<u64>) -> (u32, u64) {
    (
        n.unwrap_or(24).clamp(1, MAX_PAGE_SIZE),
        offset.unwrap_or(0).min(MAX_OFFSET),
    )
}

/// Append clamped `n` and `offset` parameters.
pub(crate) fn append_page(path: &mut String, n: Option<u32>, offset: Option<u64>) {
    let (n, offset) = page_params(n, offset);
    append_query(path, "n", &n.to_string());
    append_query(path, "offset", &offset.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_escape_encodes_delimiters_and_space() {
        assert_eq!(query_escape("hello world"), "hello%20world");
        assert_eq!(query_escape("a&b=c?d#e"), "a%26b%3Dc%3Fd%23e");
        assert_eq!(query_escape("wrld_abc-123.~"), "wrld_abc-123.~");
        assert_eq!(query_escape("caf\u{e9}"), "caf%C3%A9");
    }

    #[test]
    fn append_query_uses_separator_and_skips_empty() {
        let mut path = String::from("/worlds");
        append_query(&mut path, "search", "a b");
        append_query(&mut path, "tag", "");
        append_page(&mut path, Some(0), Some(u64::MAX));
        assert_eq!(path, "/worlds?search=a%20b&n=1&offset=1000000");
    }
}
