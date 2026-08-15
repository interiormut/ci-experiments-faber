//! `faber-agent` — the daemon half of agent transport
//! (`internal-docs/agent-transport.md`, X40). Two commands, hand-rolled
//! rather than pulled in via `clap`: two subcommands and three flags total
//! doesn't earn a dependency the rest of the workspace doesn't otherwise
//! need.

mod config;
mod dial;
mod enroll;
mod exec;
mod handler;
mod sftp;
mod ws_stream;

use config::Config;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "faber_agent=info".into()),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("install") => {
            let (token, api) = match parse_install_args(&args[1..]) {
                Ok(parsed) => parsed,
                Err(message) => {
                    eprintln!("{message}");
                    std::process::exit(2);
                }
            };
            if let Err(error) = enroll::install(&api, &token).await {
                eprintln!("install failed: {error}");
                std::process::exit(1);
            }
        }
        Some("run") => {
            let config = match Config::load() {
                Ok(config) => config,
                Err(error) => {
                    eprintln!(
                        "could not read the daemon's config ({error}); run `faber-agent install` first"
                    );
                    std::process::exit(1);
                }
            };
            dial::run_forever(&config).await;
        }
        _ => {
            eprintln!(
                "usage:\n  \
                 faber-agent install --token <bootstrap-token> --api <https://faber.example.com>\n  \
                 faber-agent run"
            );
            std::process::exit(2);
        }
    }
}

fn parse_install_args(args: &[String]) -> Result<(String, String), String> {
    let mut token = None;
    let mut api = None;
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--token" => token = iter.next().cloned(),
            "--api" => api = iter.next().cloned(),
            other => return Err(format!("unrecognized argument '{other}'")),
        }
    }
    let token = token.ok_or("--token is required")?;
    let api = api.ok_or("--api is required")?;
    Ok((token, api))
}
