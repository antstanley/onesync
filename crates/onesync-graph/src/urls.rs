//! URL-segment and URL-path percent-encoding for Microsoft Graph endpoints.
//!
//! RP2-F4: filenames flow into `{drive}/items/{parent}:/{name}:/content`-shaped
//! Graph URLs. Names containing `#`, `?`, `%`, ` `, or `:` will either truncate
//! the URL (`#`) or inject a query string (`?`) when interpolated raw. This
//! module centralises the encoding so every call site (small upload, large
//! upload, mkdir 409 fallback, item-by-path) shares the same rules and the
//! same UTF-8-byte-level encoding for non-ASCII characters.

/// Percent-encode a single path segment for Microsoft Graph URLs.
///
/// Encodes every byte except the RFC 3986 *unreserved* set
/// (`A-Z a-z 0-9 - _ . ~`). `/` is encoded too — segments must not contain
/// path separators. Non-ASCII characters are encoded as their UTF-8 byte
/// sequence (`é` → `%C3%A9`), matching how Graph decodes URL components.
#[must_use]
pub fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        if is_unreserved(byte) {
            out.push(byte as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Percent-encode a forward-slash-separated path for Microsoft Graph URLs.
///
/// Each non-empty segment between `/` is encoded via [`encode_segment`]; the
/// `/` delimiters are preserved. An empty input yields an empty string;
/// leading/trailing slashes are preserved literally.
#[must_use]
pub fn encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut first = true;
    for segment in s.split('/') {
        if !first {
            out.push('/');
        }
        first = false;
        out.push_str(&encode_segment(segment));
    }
    out
}

const fn is_unreserved(byte: u8) -> bool {
    matches!(byte,
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_ascii_segment_is_unchanged() {
        assert_eq!(encode_segment("hello.txt"), "hello.txt");
        assert_eq!(encode_segment("Report_v2-final.pdf"), "Report_v2-final.pdf");
    }

    #[test]
    fn segment_with_space_is_encoded() {
        assert_eq!(encode_segment("hello world.txt"), "hello%20world.txt");
    }

    #[test]
    fn segment_with_hash_is_encoded() {
        assert_eq!(encode_segment("v1.0#draft.txt"), "v1.0%23draft.txt");
    }

    #[test]
    fn segment_with_question_is_encoded() {
        assert_eq!(encode_segment("draft?.txt"), "draft%3F.txt");
    }

    #[test]
    fn segment_with_percent_is_encoded() {
        assert_eq!(encode_segment("100%.txt"), "100%25.txt");
    }

    #[test]
    fn segment_with_colon_is_encoded() {
        // Critical: `:` is the Graph root/item delimiter in
        // `/items/{id}:/{name}:/content`. Names containing `:` must be
        // encoded or the URL parser gets confused.
        assert_eq!(encode_segment("time:1234.log"), "time%3A1234.log");
    }

    #[test]
    fn segment_with_slash_is_encoded() {
        assert_eq!(encode_segment("foo/bar"), "foo%2Fbar");
    }

    #[test]
    fn segment_with_non_ascii_is_utf8_byte_encoded() {
        // `é` is U+00E9 = UTF-8 bytes [0xC3, 0xA9].
        assert_eq!(encode_segment("naïve.txt"), "na%C3%AFve.txt");
        // `é` U+00E9 = [0xC3, 0xA9]; `É` U+00C9 = [0xC3, 0x89].
        assert_eq!(encode_segment("résumé.pdf"), "r%C3%A9sum%C3%A9.pdf");
    }

    #[test]
    fn path_preserves_slashes_but_encodes_segments() {
        assert_eq!(encode_path("docs/report.pdf"), "docs/report.pdf");
        assert_eq!(
            encode_path("docs/hello world.txt"),
            "docs/hello%20world.txt"
        );
        assert_eq!(
            encode_path("My Docs/2026#draft/notes.md"),
            "My%20Docs/2026%23draft/notes.md"
        );
    }

    #[test]
    fn path_handles_leading_slash() {
        assert_eq!(encode_path("/docs/report.pdf"), "/docs/report.pdf");
    }

    #[test]
    fn path_handles_empty_string() {
        assert_eq!(encode_path(""), "");
    }
}
