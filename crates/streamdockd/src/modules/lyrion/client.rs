//! Minimal LMS JSON-RPC client: `POST <url>/jsonrpc.js` with `slim.request`.

use std::time::Duration;

use anyhow::Context as _;
use serde_json::{Value, json};

/// LMS answers in well under a second normally, but takes several seconds during a library rescan.
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    url: String,
    endpoint: reqwest::Url,
}

impl Client {
    pub fn new(url: &str) -> anyhow::Result<Self> {
        let base = url.trim_end_matches('/');
        let endpoint = format!("{base}/jsonrpc.js")
            .parse()
            .with_context(|| format!("[modules.lyrion] url {url:?} is not a valid URL"))?;
        let http = reqwest::Client::builder().timeout(TIMEOUT).build()?;
        Ok(Self {
            http,
            url: base.to_owned(),
            endpoint,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Fetches a file from the server (a cover image), at most `max` bytes.
    pub async fn get_bytes(&self, url: &str, max: usize) -> anyhow::Result<Vec<u8>> {
        let response = self
            .http
            .get(url)
            .timeout(Duration::from_secs(5))
            .send()
            .await?
            .error_for_status()?;
        let bytes = response.bytes().await?;
        anyhow::ensure!(bytes.len() <= max, "{url}: {} bytes is too big", bytes.len());
        Ok(bytes.to_vec())
    }

    /// Runs one command for `player` (`""` for server-wide queries) and returns its `result`.
    pub async fn request(&self, player: &str, command: Value) -> anyhow::Result<Value> {
        let body = json!({ "id": 1, "method": "slim.request", "params": [player, command] });
        let response: Value = self
            .http
            .post(self.endpoint.clone())
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if let Some(error) = response.get("error").filter(|e| !e.is_null()) {
            anyhow::bail!("LMS error: {error}");
        }
        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }
}
