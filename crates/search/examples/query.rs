//! Runs one real query, against one instance or against the public network.
//!
//! ```text
//! cargo run -p search --example query -- "rust ownership"
//! cargo run -p search --example query -- "rust ownership" https://searx.example.org
//! ```
//!
//! Prints the census and every failure the pool hit on the way, because the
//! interesting part of the public network is rarely the hits.

use search::{Query, SearchEngine, public, searxng};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "search=debug".parse().expect("a static filter")),
        )
        .with_writer(std::io::stderr)
        .init();

    let mut args = std::env::args().skip(1);
    let Some(text) = args.next() else {
        eprintln!("usage: query <text> [instance-url]");
        std::process::exit(2);
    };

    let engine: Box<dyn SearchEngine> = match args.next() {
        Some(url) => Box::new(
            searxng::SearxNg::new(searxng::Config::new(&url).expect("a usable instance URL"))
                .expect("a client"),
        ),
        None => {
            let engine = public::PublicSearxNg::discover(public::Config::default())
                .await
                .expect("discovering the public network");
            eprintln!("census after discovery: {:?}", engine.census());
            Box::new(engine)
        }
    };

    match engine.search(&Query::new(text)).await {
        Ok(results) => {
            eprintln!(
                "served by {} — {} hits",
                results.source.as_deref().unwrap_or("an unnamed instance"),
                results.hits.len()
            );
            if !results.unresponsive_engines.is_empty() {
                eprintln!("unresponsive: {}", results.unresponsive_engines.join(", "));
            }
            for hit in &results.hits {
                println!("{}\n  {}\n", hit.title, hit.url);
            }
        }
        Err(error) => {
            eprintln!("search failed: {error}");
            std::process::exit(1);
        }
    }
}
