//! Home Assistant's assist pipeline over its WebSocket API (H15-43): `assist_pipeline/run` from
//! `stt` to `tts`. The audio goes as binary messages (the run's handler id byte, then 16 kHz mono
//! s16le); a message with only the id byte ends it. The pipeline answers with events:
//! `run-start` (handler id), `stt-end` (the text), `intent-end` (the answer), `tts-end` (the
//! answer's audio URL), `run-end`, or `error`.

use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use bytes::Bytes;
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use super::capture::{Capture, RATE};

/// How long a request may record at most, by default.
pub const MAX_LISTEN: Duration = Duration::from_secs(15);
/// Connecting and the pipeline's answer after the audio ended.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);

/// What the run went through, in order, for the key and the activity feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// Connected; the audio is streaming.
    Listening,
    /// The audio ended; Home Assistant recognises and answers.
    Thinking,
    /// The recognised text.
    Heard(String),
    /// Home Assistant's answer as text.
    Answer(String),
}

/// What a finished run produced.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Outcome {
    pub heard: String,
    pub answer: String,
    /// The answer's audio, an absolute URL.
    pub tts_url: Option<String>,
}

/// Where and how to run the pipeline.
#[derive(Debug, Clone)]
pub struct Target {
    /// Home Assistant's base URL (`https://…`).
    pub url: String,
    pub token: String,
    /// Pipeline id; `None` = Home Assistant's preferred pipeline.
    pub pipeline: Option<String>,
    /// Longest recording.
    pub max_listen: Duration,
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures_util::stream::SplitSink<Ws, Message>;
type WsResult = Result<Message, tokio_tungstenite::tungstenite::Error>;

fn websocket_url(base: &str) -> String {
    let rest = base.strip_prefix("https://").map_or_else(
        || format!("ws://{}", base.trim_start_matches("http://")),
        |host| format!("wss://{host}"),
    );
    format!("{rest}/api/websocket")
}

/// Joins a URL Home Assistant gave (often `/api/tts_proxy/…`) to its base URL.
fn absolute(base: &str, url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") {
        url.to_owned()
    } else {
        format!("{}/{}", base.trim_end_matches('/'), url.trim_start_matches('/'))
    }
}

async fn recv_json(ws: &mut (impl futures_util::Stream<Item = WsResult> + Unpin)) -> anyhow::Result<Value> {
    loop {
        match ws.next().await.ok_or_else(|| anyhow!("connection closed"))?? {
            Message::Text(text) => return Ok(serde_json::from_str(&text)?),
            Message::Close(_) => bail!("connection closed by Home Assistant"),
            _ => {}
        }
    }
}

async fn connect(target: &Target) -> anyhow::Result<Ws> {
    let url = websocket_url(&target.url);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .with_context(|| format!("connecting to {url}"))?;
    let hello = recv_json(&mut ws).await?;
    anyhow::ensure!(
        hello["type"] == "auth_required",
        "unexpected greeting {}",
        hello["type"]
    );
    ws.send(Message::text(
        json!({ "type": "auth", "access_token": target.token }).to_string(),
    ))
    .await?;
    match recv_json(&mut ws).await?["type"].as_str() {
        Some("auth_ok") => Ok(ws),
        Some("auth_invalid") => bail!("auth rejected: the token is invalid or expired"),
        other => bail!("unexpected auth reply {other:?}"),
    }
}

/// An assist pipeline as Home Assistant lists it (H15-68: the Voice settings' choice).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineInfo {
    pub id: String,
    pub name: String,
    pub language: String,
    /// The speech-to-text language, e.g. "en".
    pub stt_language: String,
}

/// Home Assistant's assist pipelines and the id of its preferred one.
pub async fn pipelines(target: &Target) -> anyhow::Result<(Vec<PipelineInfo>, String)> {
    let list = async {
        let mut ws = connect(target).await?;
        ws.send(Message::text(
            json!({ "id": 1, "type": "assist_pipeline/pipeline/list" }).to_string(),
        ))
        .await?;
        loop {
            let msg = recv_json(&mut ws).await?;
            if msg["id"] == 1 && msg["type"] == "result" {
                anyhow::ensure!(
                    msg["success"] == true,
                    "pipeline list failed: {}",
                    msg["error"]["message"]
                );
                return Ok::<_, anyhow::Error>(msg["result"].clone());
            }
        }
    };
    let result = tokio::time::timeout(ANSWER_TIMEOUT, list)
        .await
        .map_err(|_| anyhow!("Home Assistant did not answer"))??;
    let text = |v: &Value, key: &str| v[key].as_str().unwrap_or_default().to_owned();
    let pipelines = result["pipelines"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|p| PipelineInfo {
                    id: text(p, "id"),
                    name: text(p, "name"),
                    language: text(p, "language"),
                    stt_language: text(p, "stt_language"),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok((pipelines, text(&result, "preferred_pipeline")))
}

/// An event of our run: its type and data. Errors end the run.
fn event(msg: &Value, id: u64) -> anyhow::Result<Option<(String, Value)>> {
    if msg["id"] != id {
        return Ok(None);
    }
    match msg["type"].as_str() {
        Some("result") if msg["success"] == false => {
            bail!(
                "assist_pipeline/run failed: {}",
                msg["error"]["message"].as_str().unwrap_or("?")
            )
        }
        Some("event") => {
            let kind = msg["event"]["type"].as_str().unwrap_or_default().to_owned();
            let data = msg["event"]["data"].clone();
            if kind == "error" {
                bail!(
                    "{} ({})",
                    data["message"].as_str().unwrap_or("assist pipeline error"),
                    data["code"].as_str().unwrap_or("?")
                );
            }
            Ok(Some((kind, data)))
        }
        _ => Ok(None),
    }
}

/// The text of a pipeline event that carries one.
fn text_of(kind: &str, data: &Value) -> Option<String> {
    let text = match kind {
        "stt-end" => &data["stt_output"]["text"],
        "intent-end" => &data["intent_output"]["response"]["speech"]["plain"]["speech"],
        _ => return None,
    };
    text.as_str().map(|t| t.trim().to_owned())
}

/// Runs one request: streams `capture` until it ends (or `release` fires, for sources that end on
/// release, or `max_listen`), then collects the answer. Progress goes to `progress`.
pub async fn run(
    target: &Target,
    mut capture: Box<dyn Capture>,
    mut release: oneshot::Receiver<()>,
    progress: mpsc::UnboundedSender<Progress>,
) -> anyhow::Result<Outcome> {
    let mut ws = tokio::time::timeout(ANSWER_TIMEOUT, connect(target))
        .await
        .context("Home Assistant did not answer in time")??;
    let id = 1;
    let mut request = json!({
        "id": id,
        "type": "assist_pipeline/run",
        "start_stage": "stt",
        "end_stage": "tts",
        "input": { "sample_rate": RATE },
    });
    if let Some(pipeline) = &target.pipeline {
        request["pipeline"] = pipeline.clone().into();
    }
    ws.send(Message::text(request.to_string())).await?;

    let mut outcome = Outcome::default();
    let handler = tokio::time::timeout(ANSWER_TIMEOUT, async {
        loop {
            if let Some((kind, data)) = event(&recv_json(&mut ws).await?, id)?
                && kind == "run-start"
            {
                return data["runner_data"]["stt_binary_handler_id"]
                    .as_u64()
                    .and_then(|h| u8::try_from(h).ok())
                    .context("run-start without stt_binary_handler_id");
            }
        }
    })
    .await
    .context("the assist pipeline did not start")??;
    let _ = progress.send(Progress::Listening);

    // Stream until the source ends, the key is released or Home Assistant ends the speech itself.
    let (mut sink, mut events) = ws.split();
    let stt_done = stream(
        target.max_listen,
        (&mut sink, &mut events),
        (id, handler),
        capture.as_mut(),
        &mut release,
        &progress,
        &mut outcome,
    )
    .await?;
    drop(capture);
    if !stt_done {
        sink.send(Message::binary(Bytes::from(vec![handler]))).await?;
    }
    let _ = progress.send(Progress::Thinking);

    tokio::time::timeout(ANSWER_TIMEOUT, async {
        loop {
            let Some((kind, data)) = event(&recv_json(&mut events).await?, id)? else {
                continue;
            };
            handle(&kind, &data, &target.url, &progress, &mut outcome);
            if kind == "run-end" {
                return anyhow::Ok(());
            }
        }
    })
    .await
    .context("Home Assistant did not answer in time")??;
    let _ = sink.close().await;
    Ok(outcome)
}

/// Records what an event says.
fn handle(
    kind: &str,
    data: &Value,
    base: &str,
    progress: &mpsc::UnboundedSender<Progress>,
    outcome: &mut Outcome,
) {
    match (kind, text_of(kind, data)) {
        ("stt-end", Some(text)) => {
            outcome.heard.clone_from(&text);
            let _ = progress.send(Progress::Heard(text));
        }
        ("intent-end", Some(text)) => {
            outcome.answer.clone_from(&text);
            let _ = progress.send(Progress::Answer(text));
        }
        ("tts-end", _) => {
            outcome.tts_url = data["tts_output"]["url"].as_str().map(|u| absolute(base, u));
        }
        _ => {}
    }
}

/// Sends audio blocks; `true` when Home Assistant already ended the speech (`stt-vad-end` or
/// `stt-end`), so no end marker is needed.
async fn stream(
    max_listen: Duration,
    (sink, events): (
        &mut WsSink,
        &mut (impl futures_util::Stream<Item = WsResult> + Unpin),
    ),
    (id, handler): (u64, u8),
    capture: &mut dyn Capture,
    release: &mut oneshot::Receiver<()>,
    progress: &mpsc::UnboundedSender<Progress>,
    outcome: &mut Outcome,
) -> anyhow::Result<bool> {
    let ends_on_release = capture.ends_on_release();
    let deadline = tokio::time::sleep(max_listen);
    tokio::pin!(deadline);
    let mut frame = Vec::with_capacity(1 + 640);
    loop {
        tokio::select! {
            block = capture.next() => {
                let Some(block) = block.context("recording failed")? else { return Ok(false) };
                frame.clear();
                frame.push(handler);
                frame.extend_from_slice(&block);
                sink.send(Message::binary(Bytes::copy_from_slice(&frame))).await?;
            }
            // Released (or the key's task went away): the request ends. Never polled again.
            _ = &mut *release, if ends_on_release => return Ok(false),
            () = &mut deadline => return Ok(false),
            msg = recv_json(events) => {
                if let Some((kind, data)) = event(&msg?, id)? {
                    handle(&kind, &data, "", progress, outcome);
                    if kind == "stt-vad-end" || kind == "stt-end" {
                        return Ok(true);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
pub mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::net::TcpListener;

    use super::*;

    /// What the fake Home Assistant saw.
    #[derive(Debug, Default)]
    pub struct Seen {
        pub request: Value,
        pub audio_bytes: usize,
        pub ended: bool,
    }

    /// How the fake answers once the audio ended.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Behavior {
        /// "Es ist 12:00", with a TTS URL.
        Answer,
        /// An STT error.
        Fail,
        /// Nothing at all (the run stays in "thinking").
        Stall,
    }

    /// A fake Home Assistant that runs one assist pipeline: it answers "Es ist 12:00" to
    /// whatever it hears, with a TTS URL. `fail` makes it report an STT error instead.
    pub async fn fake_ha(fail: bool) -> (String, Arc<Mutex<Seen>>) {
        fake_ha_with(if fail { Behavior::Fail } else { Behavior::Answer }).await
    }

    pub async fn fake_ha_with(behavior: Behavior) -> (String, Arc<Mutex<Seen>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Seen::default()));
        let seen2 = Arc::clone(&seen);
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let seen = Arc::clone(&seen2);
                tokio::spawn(async move {
                    let _ = serve(tcp, behavior, &seen).await;
                });
            }
        });
        (url, seen)
    }

    async fn send(
        ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        v: Value,
    ) -> anyhow::Result<()> {
        ws.send(Message::text(v.to_string())).await?;
        Ok(())
    }

    async fn serve(tcp: tokio::net::TcpStream, behavior: Behavior, seen: &Mutex<Seen>) -> anyhow::Result<()> {
        let mut ws = tokio_tungstenite::accept_async(tcp).await?;
        send(&mut ws, json!({ "type": "auth_required" })).await?;
        let auth = next_text(&mut ws).await?;
        if auth["access_token"] != "secret" {
            send(&mut ws, json!({ "type": "auth_invalid" })).await?;
            return Ok(());
        }
        send(&mut ws, json!({ "type": "auth_ok" })).await?;
        let request = next_text(&mut ws).await?;
        let id = request["id"].clone();
        seen.lock().unwrap().request = request;
        let ev = |kind: &str, data: Value| json!({ "id": id, "type": "event", "event": { "type": kind, "data": data } });
        send(
            &mut ws,
            json!({ "id": id, "type": "result", "success": true, "result": null }),
        )
        .await?;
        send(
            &mut ws,
            ev(
                "run-start",
                json!({ "runner_data": { "stt_binary_handler_id": 3 } }),
            ),
        )
        .await?;
        // Audio until the end marker.
        while let Some(msg) = ws.next().await {
            if let Message::Binary(b) = msg? {
                assert_eq!(b[0], 3, "handler id prefix");
                let mut s = seen.lock().unwrap();
                if b.len() == 1 {
                    s.ended = true;
                    break;
                }
                s.audio_bytes += b.len() - 1;
            }
        }
        if behavior == Behavior::Stall {
            std::future::pending::<()>().await;
        }
        if behavior == Behavior::Fail {
            send(
                &mut ws,
                ev(
                    "error",
                    json!({ "code": "stt-no-text-recognized", "message": "No text recognized" }),
                ),
            )
            .await?;
            return Ok(());
        }
        send(
            &mut ws,
            ev("stt-end", json!({ "stt_output": { "text": "Wie spät ist es?" } })),
        )
        .await?;
        send(
            &mut ws,
            ev("intent-end", json!({ "intent_output": { "response": { "speech": { "plain": { "speech": "Es ist 12:00" } } } } })),
        )
        .await?;
        send(
            &mut ws,
            ev(
                "tts-end",
                json!({ "tts_output": { "url": "/api/tts_proxy/abc.mp3", "mime_type": "audio/mpeg" } }),
            ),
        )
        .await?;
        send(&mut ws, ev("run-end", Value::Null)).await?;
        Ok(())
    }

    async fn next_text(
        ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    ) -> anyhow::Result<Value> {
        loop {
            if let Message::Text(t) = ws.next().await.context("closed")?? {
                return Ok(serde_json::from_str(&t)?);
            }
        }
    }

    /// A microphone that yields `blocks` blocks, then waits (for the release).
    pub struct FakeMic(pub usize);

    impl Capture for FakeMic {
        fn next(&mut self) -> futures_util::future::BoxFuture<'_, std::io::Result<Option<Bytes>>> {
            Box::pin(async move {
                if self.0 == 0 {
                    std::future::pending::<()>().await;
                }
                self.0 -= 1;
                Ok(Some(Bytes::from_static(&[0; 640])))
            })
        }

        fn ends_on_release(&self) -> bool {
            true
        }
    }

    // A file-like source that ends by itself.
    struct Short(bool);
    impl Capture for Short {
        fn next(&mut self) -> futures_util::future::BoxFuture<'_, std::io::Result<Option<Bytes>>> {
            Box::pin(async move { Ok(std::mem::take(&mut self.0).then(|| Bytes::from_static(&[0; 64]))) })
        }
        fn ends_on_release(&self) -> bool {
            false
        }
    }

    fn target(url: &str, token: &str) -> Target {
        Target {
            url: url.to_owned(),
            token: token.to_owned(),
            pipeline: Some("pipe-1".into()),
            max_listen: MAX_LISTEN,
        }
    }

    #[tokio::test]
    async fn runs_a_request() {
        let (url, seen) = fake_ha(false).await;
        let (release_tx, release) = oneshot::channel();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let run = tokio::spawn({
            let url = url.clone();
            async move { run(&target(&url, "secret"), Box::new(FakeMic(5)), release, tx).await }
        });
        assert_eq!(rx.recv().await, Some(Progress::Listening));
        tokio::time::sleep(Duration::from_millis(100)).await;
        release_tx.send(()).unwrap();
        let outcome = run.await.unwrap().unwrap();
        assert_eq!(
            outcome,
            Outcome {
                heard: "Wie spät ist es?".into(),
                answer: "Es ist 12:00".into(),
                tts_url: Some(format!("{url}/api/tts_proxy/abc.mp3")),
            }
        );
        let progress: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(
            progress,
            [
                Progress::Thinking,
                Progress::Heard("Wie spät ist es?".into()),
                Progress::Answer("Es ist 12:00".into())
            ]
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen.audio_bytes, 5 * 640);
        assert!(seen.ended);
        assert_eq!(seen.request["start_stage"], "stt");
        assert_eq!(seen.request["end_stage"], "tts");
        assert_eq!(seen.request["input"]["sample_rate"], 16_000);
        assert_eq!(seen.request["pipeline"], "pipe-1");
    }

    #[tokio::test]
    async fn errors_are_reported() {
        let (url, _) = fake_ha(true).await;
        let (_release_tx, release) = oneshot::channel::<()>();
        let (tx, _rx) = mpsc::unbounded_channel();
        let err = run(
            &target(&url, "secret"),
            Box::new(Short(true)),
            release,
            tx.clone(),
        )
        .await
        .unwrap_err();
        assert!(format!("{err:#}").contains("No text recognized"), "{err:#}");

        let (_r, release) = oneshot::channel::<()>();
        let err = run(&target(&url, "wrong"), Box::new(Short(true)), release, tx)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("auth rejected"), "{err:#}");
    }

    #[test]
    fn urls() {
        assert_eq!(websocket_url("https://ha.x"), "wss://ha.x/api/websocket");
        assert_eq!(websocket_url("http://ha.x:8123"), "ws://ha.x:8123/api/websocket");
        assert_eq!(
            absolute("https://ha.x/", "/api/tts_proxy/a.mp3"),
            "https://ha.x/api/tts_proxy/a.mp3"
        );
        assert_eq!(absolute("https://ha.x", "http://other/a"), "http://other/a");
    }
}
