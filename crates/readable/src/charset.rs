//! Bytes to text.
//!
//! This module exists because the obvious shortcut is wrong. `reqwest`'s
//! `text()` honours the charset in the `Content-Type` *header* and otherwise
//! assumes UTF-8; it never looks at `<meta charset>`. Plenty of pages ship
//! EUC-KR, Shift_JIS, GB18030 or windows-1252 under a bare `text/html`, and
//! decoding those as UTF-8 turns every non-ASCII character into `�` — silently,
//! and in a way that looks like the page was simply written in mojibake.
//!
//! So the entry point to this crate takes `&[u8]`, not `&str`, and resolves
//! the encoding the way the HTML standard says to:
//!
//! 1. a byte-order mark, which outranks every declaration;
//! 2. the `charset` parameter of the caller-supplied content type;
//! 3. a `<meta>` declaration in the head of the document;
//! 4. UTF-8.
//!
//! Step 2 before step 3 is the standard's order and not an arbitrary one: the
//! transport layer knows what it actually sent, and a `<meta>` tag is a claim
//! about a file that may have been transcoded on the way out.

use encoding_rs::{Encoding, UTF_8};

/// Text, plus what it took to get there.
#[derive(Debug)]
pub struct Decoded {
    pub text: String,
    /// The encoding actually used, by its canonical name.
    pub charset: &'static str,
    /// Whether any byte could not be decoded and became `U+FFFD`.
    ///
    /// Worth surfacing rather than hiding: a page that decodes with errors is
    /// usually a page whose declared encoding is a lie, and the resulting text
    /// may be subtly wrong in ways an agent cannot detect.
    pub had_errors: bool,
    /// Where the encoding came from.
    pub source: CharsetSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CharsetSource {
    Bom,
    ContentType,
    MetaTag,
    /// Nothing said, so UTF-8 was assumed.
    Default,
}

/// How far into the document a `<meta charset>` is looked for.
///
/// The standard says 1024 bytes. Real pages put a pile of conditional comments
/// and IE shims first and declare the charset well past that, so this is
/// generous; it is still bounded so a pathological document cannot make the
/// scan quadratic.
const META_SCAN_LIMIT: usize = 16 * 1024;

pub fn decode(bytes: &[u8], content_type: Option<&str>) -> Decoded {
    let (encoding, source, offset) = resolve(bytes, content_type);
    let (text, actual, had_errors) = encoding.decode(&bytes[offset..]);
    Decoded {
        text: text.into_owned(),
        charset: actual.name(),
        had_errors,
        source,
    }
}

/// Picks an encoding and reports how many leading BOM bytes to skip.
fn resolve(bytes: &[u8], content_type: Option<&str>) -> (&'static Encoding, CharsetSource, usize) {
    if let Some((encoding, length)) = Encoding::for_bom(bytes) {
        return (encoding, CharsetSource::Bom, length);
    }
    if let Some(encoding) = content_type
        .and_then(charset_parameter)
        .and_then(|label| Encoding::for_label(label.as_bytes()))
    {
        return (encoding, CharsetSource::ContentType, 0);
    }
    if let Some(encoding) = meta_charset(bytes) {
        return (encoding, CharsetSource::MetaTag, 0);
    }
    (UTF_8, CharsetSource::Default, 0)
}

/// Pulls `charset=…` out of a content type, quoted or not.
fn charset_parameter(content_type: &str) -> Option<&str> {
    let lower = content_type.to_ascii_lowercase();
    let start = lower.find("charset")?;
    let rest = content_type[start + "charset".len()..].trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let value = match rest.strip_prefix(['"', '\'']) {
        Some(quoted) => quoted.split(['"', '\'']).next()?,
        // `"` and `>` terminate it too: in the `<meta http-equiv>` spelling the
        // declaration sits *inside* an already-quoted attribute, so an
        // unquoted value there still ends at the attribute's closing quote.
        None => rest.split([';', ' ', '\t', '"', '\'', '>', '/']).next()?,
    };
    (!value.is_empty()).then_some(value)
}

/// Scans the head of the document for a `<meta>` charset declaration.
///
/// Byte-oriented on purpose: this runs *before* an encoding is known, so the
/// input cannot be treated as text. Both spellings are handled — the HTML5
/// `<meta charset>` and the older `<meta http-equiv="Content-Type">` — by
/// looking only inside `<meta` tags, so the word "charset" in a script or a
/// comment cannot masquerade as a declaration.
fn meta_charset(bytes: &[u8]) -> Option<&'static Encoding> {
    let head = &bytes[..bytes.len().min(META_SCAN_LIMIT)];
    let mut cursor = 0;

    while let Some(offset) = find(&head[cursor..], b"<meta") {
        let start = cursor + offset;
        let end = find(&head[start..], b">").map_or(head.len(), |length| start + length);
        let tag = &head[start..end];
        // A tag is short; a lossy conversion of one is safe and lets the
        // parameter parser be shared with the content-type path.
        let tag = String::from_utf8_lossy(tag);
        if let Some(label) = charset_parameter(&tag)
            && let Some(encoding) = Encoding::for_label(label.as_bytes())
        {
            return Some(encoding);
        }
        cursor = end.max(start + 1);
        if cursor >= head.len() {
            break;
        }
    }
    None
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
}

/// Whether a content type describes something this crate can parse.
///
/// Permissive: an absent or unparseable type is treated as HTML, because a
/// caller who has bytes and no metadata is exactly who this crate is for. Only
/// a type that positively says otherwise is refused.
pub fn is_html(content_type: &str) -> bool {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match essence.as_str() {
        "" => true,
        "text/html" | "application/xhtml+xml" | "application/xml" | "text/xml" | "text/plain" => {
            true
        }
        other => other.ends_with("+xml"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bom_outranks_every_declaration() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("<meta charset=euc-kr><p>안녕</p>".as_bytes());
        let decoded = decode(&bytes, Some("text/html; charset=iso-8859-1"));
        assert_eq!(decoded.source, CharsetSource::Bom);
        assert!(decoded.text.contains("안녕"));
        // The BOM itself must not survive into the text.
        assert!(!decoded.text.starts_with('\u{feff}'));
    }

    #[test]
    fn the_header_outranks_the_meta_tag() {
        let (bytes, _, _) = encoding_rs::EUC_KR.encode("<meta charset=utf-8><p>한글</p>");
        let decoded = decode(&bytes, Some("text/html; charset=euc-kr"));
        assert_eq!(decoded.source, CharsetSource::ContentType);
        assert!(decoded.text.contains("한글"));
    }

    /// The case the crate exists for: a legacy page under a bare `text/html`.
    /// Decoding this as UTF-8 — which is what an HTTP client's `text()` does —
    /// replaces every Korean character with `U+FFFD`.
    #[test]
    fn the_meta_tag_is_used_when_the_header_is_silent() {
        let html = "<html><head><meta http-equiv=\"Content-Type\" \
                    content=\"text/html; charset=euc-kr\"></head><body><p>한글 인코딩</p></body></html>";
        let (bytes, _, _) = encoding_rs::EUC_KR.encode(html);

        let decoded = decode(&bytes, Some("text/html"));
        assert_eq!(decoded.source, CharsetSource::MetaTag);
        assert_eq!(decoded.charset, "EUC-KR");
        assert!(decoded.text.contains("한글 인코딩"));
        assert!(!decoded.had_errors);

        // What the shortcut would have produced.
        let naive = String::from_utf8_lossy(&bytes);
        assert!(naive.contains('\u{fffd}'));
    }

    #[test]
    fn shift_jis_and_windows_1252_work_the_same_way() {
        let (bytes, _, _) =
            encoding_rs::SHIFT_JIS.encode("<meta charset=\"shift_jis\"><p>日本語</p>");
        assert!(decode(&bytes, None).text.contains("日本語"));

        let (bytes, _, _) =
            encoding_rs::WINDOWS_1252.encode("<meta charset=windows-1252><p>café</p>");
        assert!(decode(&bytes, None).text.contains("café"));
    }

    #[test]
    fn the_word_charset_outside_a_meta_tag_is_not_a_declaration() {
        let html =
            b"<html><head><script>var charset='euc-kr';</script></head><body>hi</body></html>";
        assert_eq!(decode(html, None).source, CharsetSource::Default);
    }

    #[test]
    fn an_unknown_label_falls_through_rather_than_failing() {
        let decoded = decode(b"<meta charset=\"x-made-up\"><p>hi</p>", None);
        assert_eq!(decoded.source, CharsetSource::Default);
        assert_eq!(decoded.charset, "UTF-8");
    }

    #[test]
    fn undecodable_bytes_are_reported_not_hidden() {
        let decoded = decode(
            &[0xff, 0xfe_u8.wrapping_add(1), 0x00, b'h', b'i'],
            Some("text/html; charset=utf-8"),
        );
        assert!(decoded.had_errors);
    }

    #[test]
    fn charset_parameter_handles_the_spellings_in_the_wild() {
        assert_eq!(charset_parameter("text/html;charset=UTF-8"), Some("UTF-8"));
        assert_eq!(
            charset_parameter("text/html; charset = utf-8"),
            Some("utf-8")
        );
        assert_eq!(
            charset_parameter("text/html; charset=\"euc-kr\""),
            Some("euc-kr")
        );
        assert_eq!(charset_parameter("text/html"), None);
    }

    #[test]
    fn content_types_that_are_not_html_are_recognised() {
        assert!(is_html("text/html; charset=utf-8"));
        assert!(is_html("application/xhtml+xml"));
        assert!(is_html(""));
        assert!(!is_html("application/pdf"));
        assert!(!is_html("application/json"));
        assert!(!is_html("image/png"));
    }
}
