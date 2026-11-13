//! What the calls to the network share: the agent, the reading of an answer with a size limit and
//! the check of a download against its SHA-256. Every call blocks.
use anyhow::{Context as _, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::Duration;
use ureq::{RequestBuilder, typestate::WithoutBody};

type Response = ureq::http::Response<ureq::Body>;

pub(crate) fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .user_agent(concat!("shcp/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// A server shcp talks to, as its errors name it.
pub(crate) struct Service {
    pub name: &'static str,
    /// The status codes with which it says it is limiting requests.
    pub rate_limited: &'static [u16],
    /// Sent as the `Accept` header.
    pub accept: Option<&'static str>,
}

impl Service {
    pub(crate) fn call(&self, request: RequestBuilder<WithoutBody>) -> Result<Response> {
        let request = match self.accept {
            Some(accept) => request.header("Accept", accept),
            None => request,
        };
        request.call().map_err(|error| match error {
            ureq::Error::StatusCode(code) if self.rate_limited.contains(&code) => {
                anyhow::anyhow!("{} is limiting requests. Try again later", self.name)
            }
            error => {
                anyhow::Error::from(error).context(format!("{} could not be reached", self.name))
            }
        })
    }

    /// The whole answer, refused when larger than `limit`.
    fn fetch(&self, request: RequestBuilder<WithoutBody>, limit: u64) -> Result<Vec<u8>> {
        self.call(request)?
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
            .with_context(|| format!("{} sent an unreadable or too large response", self.name))
    }

    pub(crate) fn fetch_json(
        &self,
        request: RequestBuilder<WithoutBody>,
        limit: u64,
    ) -> Result<Value> {
        let bytes = self.fetch(request, limit)?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("{} sent an unreadable response", self.name))
    }

    pub(crate) fn get(&self, agent: &ureq::Agent, url: &str, limit: u64) -> Result<Vec<u8>> {
        self.fetch(agent.get(url), limit)
    }

    pub(crate) fn get_json(&self, agent: &ureq::Agent, url: &str, limit: u64) -> Result<Value> {
        self.fetch_json(agent.get(url), limit)
    }
}

/// Whether `data` has the SHA-256 that starts `checksum`, as in a `.sha256` file.
pub(crate) fn sha256_matches(data: &[u8], checksum: &[u8]) -> bool {
    let checksum = String::from_utf8_lossy(checksum);
    let expected = checksum.split_whitespace().next().unwrap_or("");
    let actual: String = Sha256::digest(data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    expected.eq_ignore_ascii_case(&actual)
}
