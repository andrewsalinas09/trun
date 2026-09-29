//! HTTP client for the hub, with auto-start and a small SSE reader.

use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::time::Duration;
use trun_hub::Paths;
use trun_proto::{ApiError, DEFAULT_PORT, HealthInfo};

pub struct Client {
    http: reqwest::Client,
    pub base: String,
    token: String,
}

impl Client {
    /// Connect to the running hub, starting one in the background if needed.
    pub async fn connect(paths: &Paths, autostart: bool) -> Result<Self> {
        let token = paths.load_or_create_token()?;
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .build()?;
        let port = paths
            .read_hub_info()
            .map(|i| i.port)
            .unwrap_or(DEFAULT_PORT);
        let client = Client {
            http,
            base: format!("http://127.0.0.1:{port}"),
            token,
        };
        if let Ok(h) = client.health().await {
            // Is it *our* hub? A hub started under another TRUN_HOME can hold the port.
            let probe = client
                .req(reqwest::Method::GET, "/runs?limit=1")
                .send()
                .await?;
            if probe.status() == reqwest::StatusCode::UNAUTHORIZED {
                bail!(
                    "the hub on {} belongs to a different trun home ({}); stop it or use another port",
                    client.base,
                    h.data_dir
                );
            }
            let mine = trun_proto::build_id();
            let outdated = h.build.is_some() && mine.is_some() && h.build != mine;
            if !outdated {
                return Ok(client);
            }
            // The hub is from an older (or different) build of trun.
            let active: Vec<trun_proto::RunSummary> =
                client.get("/runs?active=true").await.unwrap_or_default();
            if !autostart || !active.is_empty() {
                eprintln!(
                    "trun: note: the hub is running a different build of trun{}; restart it with `trun hub stop` when convenient",
                    if active.is_empty() {
                        String::new()
                    } else {
                        format!(" ({} active run(s))", active.len())
                    }
                );
                return Ok(client);
            }
            eprintln!("trun: restarting the hub (trun was updated)");
            client.post_empty("/shutdown").await?;
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while client.health().await.is_ok() {
                if std::time::Instant::now() > deadline {
                    bail!("the old hub did not stop");
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        } else if !autostart {
            bail!("no hub is running (start one with `trun hub start`)");
        }
        crate::daemon::spawn_hub(paths, port)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            if client.health().await.is_ok() {
                return Ok(client);
            }
            if std::time::Instant::now() > deadline {
                bail!(
                    "started a hub but it did not become ready; see {}",
                    paths.log_file().display()
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub async fn health(&self) -> Result<HealthInfo> {
        let res = self
            .http
            .get(format!("{}/api/health", self.base))
            .timeout(Duration::from_secs(2))
            .send()
            .await?;
        Ok(res.error_for_status()?.json().await?)
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}/api{}", self.base, path))
            .bearer_auth(&self.token)
    }

    async fn decode<T: DeserializeOwned>(res: reqwest::Response) -> Result<T> {
        let status = res.status();
        if status.is_success() {
            return Ok(res.json().await?);
        }
        let body = res.text().await.unwrap_or_default();
        let msg = serde_json::from_str::<ApiError>(&body)
            .map(|e| e.error)
            .unwrap_or(body);
        Err(anyhow!("{msg}")).context(format!("hub returned {status}"))
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        Self::decode(self.req(reqwest::Method::GET, path).send().await?).await
    }

    pub async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        Self::decode(
            self.req(reqwest::Method::POST, path)
                .json(body)
                .send()
                .await?,
        )
        .await
    }

    pub async fn post_empty(&self, path: &str) -> Result<()> {
        let res = self.req(reqwest::Method::POST, path).send().await?;
        res.error_for_status()?;
        Ok(())
    }

    /// Open an SSE stream. Returns a reader yielding `(event, data)` pairs.
    pub async fn sse(&self, path: &str) -> Result<SseReader> {
        let res = self
            .req(reqwest::Method::GET, path)
            .header("accept", "text/event-stream")
            .send()
            .await?;
        if !res.status().is_success() {
            return Err(Self::decode::<serde_json::Value>(res).await.unwrap_err());
        }
        Ok(SseReader {
            stream: Box::pin(res.bytes_stream()),
            buf: String::new(),
        })
    }
}

type ByteStream =
    std::pin::Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>;

pub struct SseReader {
    stream: ByteStream,
    buf: String,
}

#[derive(Debug)]
pub struct SseMessage {
    pub event: String,
    pub data: String,
}

impl SseReader {
    /// Next message, or `None` when the server closed the stream.
    pub async fn next(&mut self) -> Result<Option<SseMessage>> {
        loop {
            if let Some(msg) = self.take_message() {
                return Ok(Some(msg));
            }
            match self.stream.next().await {
                Some(chunk) => self
                    .buf
                    .push_str(&String::from_utf8_lossy(&chunk?).replace("\r\n", "\n")),
                None => return Ok(None),
            }
        }
    }

    fn take_message(&mut self) -> Option<SseMessage> {
        loop {
            let end = self.buf.find("\n\n")?;
            let block: String = self.buf.drain(..end + 2).collect();
            let mut event = String::from("message");
            let mut data = Vec::new();
            for line in block.lines() {
                if let Some(v) = line.strip_prefix("event:") {
                    event = v.trim().to_string();
                } else if let Some(v) = line.strip_prefix("data:") {
                    data.push(v.strip_prefix(' ').unwrap_or(v).to_string());
                }
            }
            // Comment-only blocks (keep-alives) carry no data.
            if !data.is_empty() {
                return Some(SseMessage {
                    event,
                    data: data.join("\n"),
                });
            }
        }
    }
}
