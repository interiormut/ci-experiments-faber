//! The crate against real pages.
//!
//! Fixtures are actual responses, saved as they arrived — including the
//! non-UTF-8 one, which is stored as EUC-KR bytes rather than as a string, so
//! the decoding path is exercised rather than assumed. The unit tests check
//! each stage; these check that a page an agent might genuinely be handed
//! comes out readable.

use readable::{Error, Mode, Options, read};
use url::Url;

fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("reading {}: {error}", path.display()))
}

fn url(text: &str) -> Url {
    Url::parse(text).unwrap()
}

/// A news post: the case article extraction is for.
#[test]
fn an_article_keeps_its_prose_structure_and_loses_the_furniture() {
    let document = read(
        &fixture("rust-blog-release.html"),
        &url("https://blog.rust-lang.org/2024/09/05/Rust-1.81.0/"),
        Some("text/html; charset=utf-8"),
        &Options::default(),
    )
    .unwrap();

    assert_eq!(document.extraction.mode, readable::ModeUsed::Article);
    assert_eq!(
        document.title.as_deref(),
        Some("Announcing Rust 1.81.0 | Rust Blog")
    );
    assert!(document.byline.is_some());

    // Structure that carries meaning survives.
    assert!(document.content.contains("## What's in 1.81.0 stable"));
    assert!(
        document.content.contains("```"),
        "code fences should survive"
    );
    assert!(document.content.contains("rustup update stable"));

    // Links are followable without knowing where the page came from.
    assert!(
        document
            .content
            .contains("(https://www.rust-lang.org/install.html)")
    );
    assert!(
        !document.content.contains("](/"),
        "no relative link should survive"
    );

    // And none of the page's machinery came along.
    for junk in ["<script", "<style", "function(", "@media"] {
        assert!(!document.content.contains(junk), "{junk} survived");
    }
}

/// A reference page whose sidebar is the API index — the case extraction gets
/// wrong, and the reason both modes exist.
///
/// Measured, not assumed: on this page article extraction keeps a handful of
/// method links and drops the rest of the index.
#[test]
fn the_full_page_mode_keeps_navigation_that_extraction_discards() {
    let bytes = fixture("rustdoc-barrier.html");
    let page = url("https://doc.rust-lang.org/std/sync/struct.Barrier.html");

    let article = read(&bytes, &page, Some("text/html"), &Options::article()).unwrap();
    let full = read(&bytes, &page, Some("text/html"), &Options::full_page()).unwrap();

    let method_links = |text: &str| text.matches("#method.").count();
    assert!(
        method_links(&full.content) > method_links(&article.content) * 3,
        "the sidebar index should survive full-page conversion: {} vs {}",
        method_links(&full.content),
        method_links(&article.content)
    );

    // Both still find the same page, and both still read as documentation.
    assert_eq!(article.title, full.title);
    assert!(article.content.contains("Barrier"));
    assert!(full.content.contains("Barrier"));
}

/// The trap this crate takes bytes for: a legacy-encoded page under a
/// `Content-Type` that does not say so.
#[test]
fn a_euc_kr_page_survives_when_only_the_meta_tag_declares_it() {
    let bytes = fixture("legacy-euc-kr.html");
    // Precondition: these really are not UTF-8 bytes.
    assert!(String::from_utf8(bytes.clone()).is_err());

    let document = read(
        &bytes,
        &url("http://example.kr/page.html"),
        Some("text/html"),
        &Options::default(),
    )
    .unwrap();

    assert_eq!(document.extraction.charset, "EUC-KR");
    assert!(!document.extraction.lossy_decode);
    assert!(document.content.contains("한국어 문서 제목"));
    assert!(document.content.contains("인코딩이 틀리면 요약도 틀립니다"));
    assert!(!document.content.contains('\u{fffd}'));
}

/// A page that parses perfectly and says nothing.
#[test]
fn a_client_rendered_page_is_refused_rather_than_returned_empty() {
    let error = read(
        &fixture("client-rendered.html"),
        &url("https://app.example.com/dashboard"),
        Some("text/html"),
        &Options::default(),
    )
    .unwrap_err();

    match error {
        Error::NoContent {
            likely_needs_javascript,
            ..
        } => assert!(
            likely_needs_javascript,
            "a shell around a mount point should be recognised as one"
        ),
        other => panic!("expected NoContent, got {other}"),
    }
}

/// The other half of that flag, and the reason it is not simply "does the page
/// have scripts": every page in this corpus has scripts, and most have a
/// `<noscript>`. A page with no prose that a browser would not help with must
/// come back with the flag *off*, or the advice is noise.
#[test]
fn a_page_that_is_merely_contentless_is_not_blamed_on_javascript() {
    let bytes = fixture("image-gallery.html");
    // Precondition: this fixture has exactly the scripts that would fool a
    // naive check.
    let source = String::from_utf8(bytes.clone()).unwrap();
    assert!(source.contains("<script"));
    assert!(source.contains("<noscript"));

    let error = read(
        &bytes,
        &url("https://photos.example/gallery"),
        Some("text/html"),
        &Options::default(),
    )
    .unwrap_err();

    match error {
        Error::NoContent {
            likely_needs_javascript,
            ..
        } => assert!(
            !likely_needs_javascript,
            "a gallery is not waiting to be rendered; it just has no prose"
        ),
        other => panic!("expected NoContent, got {other}"),
    }
}

/// Metadata must be read before the tree is cleaned.
///
/// JSON-LD lives inside a `<script>`, so a pipeline that strips scripts first
/// loses the byline, the site name and the date — and loses them silently,
/// which is why this is pinned rather than left to the fixtures.
#[test]
fn json_ld_metadata_survives_the_cleaning_pass() {
    let prose = "This paragraph exists so that the page has enough text for \
                 extraction to consider it an article rather than a fragment. "
        .repeat(6);
    let html = format!(
        r#"<!doctype html><html lang="fr"><head><title>Fallback Title</title>
        <script type="application/ld+json">{{
          "@context": "https://schema.org",
          "@type": "NewsArticle",
          "headline": "The Declared Headline",
          "author": {{"@type": "Person", "name": "Declared Author"}},
          "publisher": {{"@type": "Organization", "name": "Declared Publisher"}},
          "datePublished": "2026-03-04T10:00:00Z"
        }}</script></head>
        <body><article><h1>The Declared Headline</h1><p>{prose}</p></article>
        <script>window.analytics = 1;</script></body></html>"#
    );

    let document = read(
        html.as_bytes(),
        &url("https://news.example/story"),
        Some("text/html; charset=utf-8"),
        &Options::default(),
    )
    .unwrap();

    assert_eq!(document.byline.as_deref(), Some("Declared Author"));
    assert_eq!(document.site_name.as_deref(), Some("Declared Publisher"));
    assert_eq!(document.published.as_deref(), Some("2026-03-04T10:00:00Z"));
    assert_eq!(document.title.as_deref(), Some("The Declared Headline"));
    // The scripts themselves are still gone from the content.
    assert!(!document.content.contains("window.analytics"));
    assert!(!document.content.contains("schema.org"));
}

#[test]
fn a_non_html_response_is_refused_before_it_is_parsed() {
    let error = read(
        b"%PDF-1.7\n1 0 obj",
        &url("https://example.org/paper.pdf"),
        Some("application/pdf"),
        &Options::default(),
    )
    .unwrap_err();
    assert!(matches!(error, Error::NotHtml { .. }), "got {error}");
}

#[test]
fn output_over_the_budget_is_cut_and_says_so() {
    let bytes = fixture("rustdoc-barrier.html");
    let page = url("https://doc.rust-lang.org/std/sync/struct.Barrier.html");
    let options = Options::full_page().with_max_output_bytes(4096);

    let document = read(&bytes, &page, Some("text/html"), &options).unwrap();
    let truncation = document
        .extraction
        .truncation
        .expect("this page is larger than the budget");

    assert!(document.content.len() <= 4096);
    assert!(truncation.total_bytes > truncation.kept_bytes);
    assert!(
        document.content.contains("truncated"),
        "a reader who only sees the Markdown must still know it was cut"
    );

    // The same page, uncapped, is whole.
    let whole = read(
        &bytes,
        &page,
        Some("text/html"),
        &Options::full_page().with_max_output_bytes(0),
    )
    .unwrap();
    assert!(whole.extraction.truncation.is_none());
    assert!(whole.content.len() > 4096);
}

#[test]
fn the_json_rendering_carries_the_same_page_as_the_markdown() {
    let document = read(
        &fixture("rust-blog-release.html"),
        &url("https://blog.rust-lang.org/2024/09/05/Rust-1.81.0/"),
        Some("text/html; charset=utf-8"),
        &Options::default(),
    )
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&document.to_json()).unwrap();
    assert_eq!(json["title"], document.title.clone().unwrap().as_str());
    assert_eq!(json["extraction"]["mode"], "article");
    assert_eq!(json["content"], document.content.as_str());

    // The Markdown rendering adds the header and nothing else.
    let markdown = document.to_markdown();
    assert!(markdown.contains("Source: <https://blog.rust-lang.org/2024/09/05/Rust-1.81.0/>"));
    assert!(markdown.ends_with(&document.content));
}

/// `Mode::Article` is a promise the caller can rely on: prose or an error,
/// never a silent fallback to the whole page.
#[test]
fn article_mode_refuses_rather_than_falling_back() {
    let bytes = fixture("client-rendered.html");
    let page = url("https://app.example.com/dashboard");

    assert!(read(&bytes, &page, Some("text/html"), &Options::article()).is_err());
    assert!(read(&bytes, &page, Some("text/html"), &Options::full_page()).is_err());
    assert!(
        read(
            &bytes,
            &page,
            Some("text/html"),
            &Options {
                mode: Mode::Auto,
                ..Options::default()
            }
        )
        .is_err()
    );
}

#[test]
fn an_oversized_response_is_refused_before_it_is_parsed() {
    let huge = vec![b'x'; 4096];
    let error = read(
        &huge,
        &url("https://example.org/"),
        Some("text/html"),
        &Options {
            max_input_bytes: 1024,
            ..Options::default()
        },
    )
    .unwrap_err();
    assert!(
        matches!(error, Error::TooLarge { limit: 1024, .. }),
        "got {error}"
    );
}
