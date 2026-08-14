//! The one place an HTTP client is built.
//!
//! Shared by both providers so the no-ambient-configuration rule is stated
//! once rather than remembered twice. Faber runs many users' work in a single
//! process, and `reqwest`'s default of reading `HTTP_PROXY`/`HTTPS_PROXY` from
//! the process environment would route every user's searches through whatever
//! the host operator happened to export. A proxy is a parameter here or it
//! does not exist.

use std::time::Duration;

use reqwest::Client;

use crate::error::{Error, Result};

pub(crate) fn client(user_agent: &str, timeout: Duration, proxy: Option<&str>) -> Result<Client> {
    let mut builder = Client::builder()
        .user_agent(user_agent.to_owned())
        .timeout(timeout)
        .no_proxy();
    if let Some(proxy) = proxy {
        builder = builder.proxy(
            reqwest::Proxy::all(proxy).map_err(|error| Error::Config(format!("proxy: {error}")))?,
        );
    }
    builder
        .build()
        .map_err(|error| Error::Config(format!("http client: {error}")))
}
