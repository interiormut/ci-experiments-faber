//! Reads a saved HTTP response and prints it as Markdown or JSON.
//!
//! ```text
//! cargo run -p readable --example read -- page.html https://example.org/page [json] [full]
//! ```
//!
//! Useful for eyeballing what a real page turns into — which is how the
//! defaults in this crate were chosen.

use readable::{Mode, Options, read};
use url::Url;

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(path), Some(page)) = (args.next(), args.next()) else {
        eprintln!("usage: read <file.html> <url> [json] [full]");
        std::process::exit(2);
    };
    let flags: Vec<String> = args.collect();
    let flag = |name: &str| flags.iter().any(|f| f == name);

    let bytes = std::fs::read(&path).expect("reading the file");
    let url = Url::parse(&page).expect("parsing the URL");
    let options = Options {
        mode: if flag("full") {
            Mode::FullPage
        } else {
            Mode::Auto
        },
        ..Options::default()
    };

    match read(&bytes, &url, Some("text/html"), &options) {
        Ok(document) if flag("json") => println!("{}", document.to_json()),
        Ok(document) => {
            // To stderr, so the Markdown on stdout stays pipeable.
            eprintln!(
                "[{:?}, {} chars of text, charset {}{}]",
                document.extraction.mode,
                document.extraction.text_chars,
                document.extraction.charset,
                if document.is_truncated() {
                    ", truncated"
                } else {
                    ""
                }
            );
            println!("{}", document.to_markdown());
        }
        Err(error) => {
            eprintln!("could not read {url}: {error}");
            std::process::exit(1);
        }
    }
}
