//! src/epub/paths.rs — archive-relative href resolution + dependency-free
//! percent-decode. ALL paths are '/'-separated; we never touch `std::path` here
//! because EPUB hrefs are URL-like, not OS paths (and OS separators differ).

/// Decode `%XX` escapes in an href. Lenient: a malformed escape is passed
/// through verbatim. Decodes byte-by-byte then re-interprets as UTF-8 so that
/// multibyte sequences (e.g. percent-encoded Japanese filenames) round-trip.
pub fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = hex_val(bytes[i + 1]);
            let lo = hex_val(bytes[i + 2]);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    // Lossy is fine: invalid byte runs become U+FFFD rather than panicking.
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Directory portion of a '/'-separated path. `"a/b/c.xhtml"` -> `"a/b"`,
/// `"c.xhtml"` -> `""`. A trailing slash yields the path without it.
pub fn dir_of(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(idx) => trimmed[..idx].to_string(),
        None => String::new(),
    }
}

/// Split a `#fragment` off an href. Returns `(href_without_fragment, fragment)`.
/// The fragment (if any) is returned without the leading `#`.
pub fn split_fragment(href: &str) -> (&str, Option<String>) {
    match href.find('#') {
        Some(idx) => (&href[..idx], Some(href[idx + 1..].to_string())),
        None => (href, None),
    }
}

/// Resolve `href` (which may be `./`, `../`, fragment-bearing, or percent-encoded)
/// against `base_dir`, yielding a normalized archive-relative '/'-path with the
/// fragment stripped and percent escapes decoded. Absolute hrefs (leading '/')
/// are treated as archive-root-relative.
pub fn resolve_href(base_dir: &str, href: &str) -> String {
    let (no_frag, _) = split_fragment(href);
    let decoded = percent_decode(no_frag);

    // Absolute (root-relative) href: ignore base_dir entirely.
    let combined = if decoded.starts_with('/') {
        decoded.trim_start_matches('/').to_string()
    } else if base_dir.is_empty() {
        decoded
    } else {
        format!("{}/{}", base_dir.trim_end_matches('/'), decoded)
    };

    normalize_segments(&combined)
}

/// Collapse `.` and `..` segments in a '/'-separated path. `..` that would
/// escape the root is dropped (clamped), matching how a sane EPUB reader behaves.
fn normalize_segments(path: &str) -> String {
    let mut stack: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    stack.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_basic() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("100%"), "100%"); // trailing bare %
        assert_eq!(percent_decode("%2F"), "/");
    }

    #[test]
    fn percent_decode_multibyte() {
        // "あ" = U+3042 = E3 81 82
        assert_eq!(percent_decode("%E3%81%82"), "あ");
    }

    #[test]
    fn dir_of_cases() {
        assert_eq!(dir_of("a/b/c.xhtml"), "a/b");
        assert_eq!(dir_of("c.xhtml"), "");
        assert_eq!(dir_of("a/b/"), "a");
    }

    #[test]
    fn split_fragment_cases() {
        assert_eq!(
            split_fragment("p.xhtml#sec1"),
            ("p.xhtml", Some("sec1".into()))
        );
        assert_eq!(split_fragment("p.xhtml"), ("p.xhtml", None));
    }

    #[test]
    fn resolve_href_relative() {
        assert_eq!(
            resolve_href("OEBPS/Text", "ch1.xhtml"),
            "OEBPS/Text/ch1.xhtml"
        );
        assert_eq!(
            resolve_href("OEBPS/Text", "../Images/a.png"),
            "OEBPS/Images/a.png"
        );
        assert_eq!(
            resolve_href("OEBPS/Text", "./ch1.xhtml#frag"),
            "OEBPS/Text/ch1.xhtml"
        );
    }

    #[test]
    fn resolve_href_root() {
        assert_eq!(resolve_href("OEBPS/Text", "/cover.xhtml"), "cover.xhtml");
    }

    #[test]
    fn resolve_href_empty_base() {
        assert_eq!(resolve_href("", "content.opf"), "content.opf");
    }

    #[test]
    fn resolve_href_clamps_escaping_dotdot() {
        assert_eq!(resolve_href("OEBPS", "../../a.png"), "a.png");
    }
}
