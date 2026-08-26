//! A shared HTTP client with a descriptive `User-Agent` and modest retries.
//!
//! Modrinth's API rules require a `User-Agent` that identifies the application
//! and a way to contact its author; the same string is polite to send to the
//! Mojang and Fabric endpoints too.

use std::path::Path;
use std::time::Duration;

use futures_util::StreamExt as _;
use serde::{de::DeserializeOwned, Serialize};
use tokio::io::AsyncWriteExt as _;

use crate::error::{Error, Result};

/// `User-Agent` sent with every request.
pub const USER_AGENT: &str = concat!(
    "aineasg/minecraft-server-manager/",
    env!("CARGO_PKG_VERSION"),
    " (github.com/aineasg/minecraft-server-manager)"
);

const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
];

/// Cheap to clone; wraps a `reqwest::Client` which is itself a connection-pool handle.
#[derive(Debug, Clone)]
pub struct Http {
    client: reqwest::Client,
}

impl Http {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|source| Error::Http {
                url: "<client builder>".into(),
                source,
            })?;
        Ok(Self { client })
    }

    /// `GET url` and deserialize the JSON body, retrying transient failures.
    pub async fn get_json<T: DeserializeOwned>(
        &self,
        service: &'static str,
        url: &str,
    ) -> Result<T> {
        with_retries(url, || async {
            let resp = self.send(service, self.client.get(url)).await?;
            let text = body_text(service, url, resp).await?;
            serde_json::from_str(&text).map_err(|source| Error::Json {
                what: "API response",
                source,
            })
        })
        .await
    }

    /// `POST url` with a JSON body and deserialize the JSON response.
    pub async fn post_json<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        service: &'static str,
        url: &str,
        body: &B,
    ) -> Result<T> {
        with_retries(url, || async {
            let resp = self
                .send(service, self.client.post(url).json(body))
                .await?;
            let text = body_text(service, url, resp).await?;
            serde_json::from_str(&text).map_err(|source| Error::Json {
                what: "API response",
                source,
            })
        })
        .await
    }

    /// Stream `url` to `dest` (via a `.part` temp file), reporting progress as
    /// `(downloaded_bytes, total_bytes_if_known)`.
    pub async fn download_to_file(
        &self,
        service: &'static str,
        url: &str,
        dest: &Path,
        mut progress: impl FnMut(u64, Option<u64>),
    ) -> Result<()> {
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| Error::io(parent, e))?;
        }

        let resp = with_retries(url, || async {
            self.send(service, self.client.get(url)).await
        })
        .await?;

        let total = resp.content_length();
        let tmp = dest.with_extension("part");
        let mut file = tokio::fs::File::create(&tmp)
            .await
            .map_err(|e| Error::io(&tmp, e))?;

        let mut downloaded: u64 = 0;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|source| Error::Http {
                url: url.to_string(),
                source,
            })?;
            file.write_all(&chunk).await.map_err(|e| Error::io(&tmp, e))?;
            downloaded += chunk.len() as u64;
            progress(downloaded, total);
        }
        file.sync_all().await.map_err(|e| Error::io(&tmp, e))?;
        drop(file);

        tokio::fs::rename(&tmp, dest)
            .await
            .map_err(|e| Error::io(dest, e))
    }

    async fn send(
        &self,
        service: &'static str,
        req: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response> {
        let resp = req.send().await.map_err(|source| Error::Http {
            url: source.url().map(ToString::to_string).unwrap_or_default(),
            source,
        })?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        Err(Error::HttpStatus {
            service,
            status: status.as_u16(),
            url: resp.url().to_string(),
        })
    }
}

async fn body_text(
    service: &'static str,
    url: &str,
    resp: reqwest::Response,
) -> Result<String> {
    resp.text().await.map_err(|source| Error::Http {
        url: format!("{service} {url}"),
        source,
    })
}

/// Retry `op` up to three times, backing off, but only for errors that a retry
/// could plausibly fix (network hiccups, 429, 5xx).
async fn with_retries<F, Fut, T>(url: &str, mut op: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut last_err = None;
    for (attempt, delay) in std::iter::once(&Duration::ZERO)
        .chain(RETRY_DELAYS.iter())
        .enumerate()
    {
        if !delay.is_zero() {
            tokio::time::sleep(*delay).await;
            tracing::debug!(url, attempt, "retrying request");
        }
        match op().await {
            Ok(value) => return Ok(value),
            Err(e) if is_retriable(&e) => last_err = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last_err.unwrap_or_else(|| Error::msg(format!("request to {url} failed"))))
}

fn is_retriable(err: &Error) -> bool {
    match err {
        Error::Http { .. } => true,
        Error::HttpStatus { status, .. } => {
            *status == 429 || (500..=599).contains(status)
        }
        _ => false,
    }
}
