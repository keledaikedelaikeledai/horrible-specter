use std::{env, time::Duration};

use anyhow::{Context, Result, bail};
use reqwest::{Client, Response, StatusCode, header};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::provider::{
    InterruptResult, MessageRole, OperationStatus, Provider, ProviderDescriptor, ProviderError,
    ProviderEvent, ProviderEventKind, ProviderFuture, ProviderRequest, ProviderRun,
    ProviderSession,
};

#[derive(Clone)]
pub struct OpenCodeClient {
    http: Client,
    base_url: String,
}

impl OpenCodeClient {
    pub fn local() -> Result<Self> {
        let base_url =
            env::var("OPENCODE_URL").unwrap_or_else(|_| "http://127.0.0.1:4096".to_string());
        let mut builder = Client::builder().connect_timeout(Duration::from_secs(2));
        if let Ok(password) = env::var("OPENCODE_SERVER_PASSWORD") {
            let username =
                env::var("OPENCODE_SERVER_USERNAME").unwrap_or_else(|_| "opencode".to_string());
            let mut value = header::HeaderValue::from_str(&format!("{username}:{password}"))?;
            value.set_sensitive(true);
            let mut headers = header::HeaderMap::new();
            headers.insert(
                header::AUTHORIZATION,
                header::HeaderValue::from_str(&format!("Basic {}", base64_basic(&value)?))?,
            );
            builder = builder.default_headers(headers);
        }
        Ok(Self {
            http: builder.build()?,
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    pub fn with_base_url(base_url: impl Into<String>) -> Result<Self> {
        let mut client = Self::local()?;
        client.base_url = base_url.into().trim_end_matches('/').to_string();
        Ok(client)
    }

    pub async fn health(&self) -> Result<String> {
        let value = self.get("/global/health").await?;
        if value.get("healthy").and_then(Value::as_bool) == Some(false) {
            bail!("OpenCode server reported unhealthy");
        }
        value
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .context("OpenCode health response did not include version")
    }

    pub async fn providers(&self) -> Result<Vec<ProviderDescriptor>> {
        let value = self.get("/provider").await?;
        let all = value
            .get("all")
            .and_then(Value::as_array)
            .context("OpenCode provider response did not include all")?;
        let connected = value
            .get("connected")
            .and_then(Value::as_array)
            .map(|providers| {
                providers
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
            });
        let mut descriptors = all
            .iter()
            .filter(|provider| {
                let id = provider
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                connected
                    .as_ref()
                    .is_none_or(|providers| providers.contains(&id))
            })
            .flat_map(|provider| {
                let id = provider
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let display_name = provider
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_string();
                provider
                    .get("models")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flat_map(move |models| {
                        models.keys().map({
                            let id = id.clone();
                            let display_name = display_name.clone();
                            move |model| ProviderDescriptor {
                                id: id.clone(),
                                display_name: display_name.clone(),
                                model: Some(model.clone()),
                                capabilities: crate::provider::ProviderCapabilities {
                                    streaming_messages: true,
                                    tool_events: true,
                                    interruption: true,
                                    native_session_resume: true,
                                    usage_limits: true,
                                },
                            }
                        })
                    })
            })
            .collect::<Vec<_>>();
        descriptors.sort_by(|left, right| {
            left.id
                .cmp(&right.id)
                .then_with(|| left.model.cmp(&right.model))
        });
        Ok(descriptors)
    }

    async fn create_session(&self, title: &str) -> Result<String> {
        let value = self
            .request(
                self.http
                    .post(self.url("/session"))
                    .json(&json!({"title": title})),
            )
            .await?;
        value
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .context("OpenCode session response did not include id")
    }

    async fn prompt_async(&self, session_id: &str, request: &ProviderRequest) -> Result<()> {
        let model = request
            .model
            .as_deref()
            .and_then(|model| model.split_once('/'))
            .map(|(provider_id, model_id)| json!({"providerID": provider_id, "modelID": model_id}));
        let mut body = json!({"parts": [{"type": "text", "text": request.prompt}]});
        if let Some(model) = model {
            body["model"] = model;
        }
        let response = self
            .http
            .post(self.url(&format!("/session/{session_id}/prompt_async")))
            .json(&body)
            .send()
            .await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(self.status_error(response).await)
        }
    }

    pub async fn abort_session(&self, session_id: &str) -> Result<bool> {
        let value = self
            .request(
                self.http
                    .post(self.url(&format!("/session/{session_id}/abort"))),
            )
            .await?;
        value
            .as_bool()
            .context("OpenCode abort response did not include a boolean result")
    }

    async fn event_stream(&self) -> Result<Response> {
        let response = self.http.get(self.url("/event")).send().await?;
        if response.status().is_success() {
            Ok(response)
        } else {
            Err(self.status_error(response).await)
        }
    }

    async fn get(&self, path: &str) -> Result<Value> {
        self.request(self.http.get(self.url(path))).await
    }

    async fn request(&self, request: reqwest::RequestBuilder) -> Result<Value> {
        let response = request.send().await?;
        if !response.status().is_success() {
            return Err(self.status_error(response).await);
        }
        Ok(response.json().await?)
    }

    async fn status_error(&self, response: Response) -> anyhow::Error {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let message = if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            "OpenCode authentication failed".to_string()
        } else {
            format!("OpenCode request failed ({status}): {body}")
        };
        anyhow::anyhow!(message)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }
}

impl OpenCodeClient {
    pub async fn start_run(&self, request: ProviderRequest) -> Result<ProviderRun> {
        let session_id = match &request.session {
            crate::provider::SessionRequest::New => {
                self.create_session("Serangkai conversation").await?
            }
            crate::provider::SessionRequest::Resume { native_session_id } => {
                native_session_id.clone()
            }
        };
        let response = self.event_stream().await?;
        let (sender, events) = mpsc::channel(128);
        let client = self.clone();
        let run_id = request.run_id.clone();
        let task_run_id = run_id.clone();
        let session_for_task = session_id.clone();
        tokio::spawn(async move {
            let _ = sender
                .send(event(
                    &task_run_id,
                    &session_for_task,
                    "session.started",
                    ProviderEventKind::SessionStarted,
                ))
                .await;
            let mut response = response;
            let mut prompt = Box::pin(client.prompt_async(&session_for_task, &request));
            let mut buffer = Vec::new();
            let mut prompt_done = false;
            let mut completed = false;
            while !completed {
                tokio::select! {
                    result = &mut prompt, if !prompt_done => {
                        if let Err(error) = result { let _ = sender.send(failed_event(&task_run_id, &session_for_task, error.to_string())).await; return; }
                        prompt_done = true;
                    }
                    chunk = response.chunk() => match chunk {
                        Ok(Some(chunk)) => {
                            buffer.extend_from_slice(&chunk);
                            while let Some(frame_length) = sse_frame_length(&buffer) {
                                let frame = buffer.drain(..frame_length).collect::<Vec<_>>();
                                if let Some(event) = parse_sse(&frame, &task_run_id, &session_for_task) {
                                    completed = matches!(event.kind, ProviderEventKind::Completed | ProviderEventKind::Failed { .. } | ProviderEventKind::Interrupted);
                                    let _ = sender.send(event).await;
                                }
                            }
                        }
                        Ok(None) => {
                            if !completed {
                                let _ = sender.send(failed_event(&task_run_id, &session_for_task, "OpenCode event stream ended before the session became idle".to_string())).await;
                            }
                            return;
                        }
                        Err(error) => { let _ = sender.send(failed_event(&task_run_id, &session_for_task, error.to_string())).await; return; }
                    }
                }
            }
        });
        Ok(ProviderRun {
            run_id,
            session: ProviderSession {
                provider_session_id: session_id,
                resumable: true,
            },
            events,
        })
    }
}

pub struct OpenCodeProvider {
    client: OpenCodeClient,
    descriptor: ProviderDescriptor,
}

pub fn classify_error(error: anyhow::Error) -> ProviderError {
    let message = error.to_string();
    let lower = message.to_ascii_lowercase();
    if lower.contains("authentication") || lower.contains("unauthorized") || lower.contains("401") {
        ProviderError::Authentication { message }
    } else {
        ProviderError::Transport { message }
    }
}

impl OpenCodeProvider {
    pub fn new(client: OpenCodeClient, descriptor: ProviderDescriptor) -> Self {
        Self { client, descriptor }
    }
}

impl Provider for OpenCodeProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        self.descriptor.clone()
    }
    fn start<'a>(&'a self, request: ProviderRequest) -> ProviderFuture<'a, ProviderRun> {
        Box::pin(async move { self.client.start_run(request).await.map_err(classify_error) })
    }
    fn interrupt<'a>(
        &'a self,
        session: &'a ProviderSession,
    ) -> ProviderFuture<'a, InterruptResult> {
        Box::pin(async move {
            let acknowledged = self
                .client
                .abort_session(&session.provider_session_id)
                .await
                .map_err(classify_error)?;
            Ok(InterruptResult {
                provider_acknowledged: acknowledged,
                final_status: if acknowledged {
                    OperationStatus::Interrupted
                } else {
                    OperationStatus::Uncertain
                },
            })
        })
    }
}

fn event(
    run_id: &str,
    session_id: &str,
    kind_name: &str,
    kind: ProviderEventKind,
) -> ProviderEvent {
    ProviderEvent {
        event_id: kind_name.to_string(),
        provider_session_id: session_id.to_string(),
        run_id: run_id.to_string(),
        message_id: None,
        tool_call_id: None,
        kind,
    }
}

fn failed_event(run_id: &str, session_id: &str, message: String) -> ProviderEvent {
    event(
        run_id,
        session_id,
        "session.error",
        ProviderEventKind::Failed {
            error: ProviderError::Transport { message },
        },
    )
}

pub fn parse_sse(frame: &[u8], run_id: &str, session_id: &str) -> Option<ProviderEvent> {
    let data = frame
        .split(|byte| *byte == b'\n')
        .filter_map(|line| line.strip_prefix(b"data:"))
        .map(|line| String::from_utf8_lossy(line).trim().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(&data).ok()?;
    map_event(&value, run_id, session_id)
}

fn sse_frame_length(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|position| position + 2)
        .or_else(|| {
            buffer
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|position| position + 4)
        })
}

fn map_event(value: &Value, run_id: &str, session_id: &str) -> Option<ProviderEvent> {
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let properties = value.get("properties").unwrap_or(&Value::Null);
    if event_type.starts_with("server.")
        || properties
            .get("sessionID")
            .and_then(Value::as_str)
            .is_none_or(|event_session| event_session != session_id)
    {
        return None;
    }
    let event_id = value
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            format!(
                "{}:{}",
                event_type,
                serde_json::to_string(properties).unwrap_or_default()
            )
        });
    let part = properties.get("part");
    let message_id = properties
        .get("messageID")
        .or_else(|| part.and_then(|part| part.get("messageID")))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let tool_call_id = properties
        .get("part")
        .and_then(|part| part.get("callID"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let kind = if event_type == "session.idle" {
        ProviderEventKind::Completed
    } else if event_type == "session.error" {
        let message = properties
            .get("error")
            .or_else(|| properties.get("message"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| properties.to_string());
        let lower = message.to_ascii_lowercase();
        let error = if lower.contains("auth") || lower.contains("unauthorized") {
            ProviderError::Authentication { message }
        } else {
            ProviderError::Failed { message }
        };
        ProviderEventKind::Failed { error }
    } else if event_type == "message.part.delta" {
        if properties.get("field").and_then(Value::as_str) != Some("text") {
            return None;
        }
        ProviderEventKind::MessageDelta {
            role: MessageRole::Assistant,
            content: properties
                .get("delta")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        }
    } else if event_type == "message.part.updated" {
        let part = properties.get("part")?;
        match part.get("type").and_then(Value::as_str) {
            Some("text") => return None,
            Some("tool") => {
                let state = part.get("state").unwrap_or(&Value::Null);
                let status = state
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("running");
                let call_id = tool_call_id.clone().unwrap_or_default();
                if matches!(status, "pending" | "running") {
                    ProviderEventKind::ToolStarted {
                        call_id,
                        name: part
                            .get("tool")
                            .and_then(Value::as_str)
                            .unwrap_or("tool")
                            .to_string(),
                        input: state
                            .get("input")
                            .cloned()
                            .unwrap_or(Value::Null)
                            .to_string(),
                    }
                } else if status == "completed" {
                    ProviderEventKind::ToolOutput {
                        call_id,
                        output: state
                            .get("output")
                            .cloned()
                            .unwrap_or(Value::Null)
                            .to_string(),
                        is_final: true,
                    }
                } else {
                    ProviderEventKind::ToolFinished {
                        call_id,
                        status: OperationStatus::Failed,
                        error: state
                            .get("error")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    }
                }
            }
            _ => ProviderEventKind::Notice {
                message: part.to_string(),
            },
        }
    } else {
        ProviderEventKind::Notice {
            message: value.to_string(),
        }
    };
    Some(ProviderEvent {
        event_id,
        provider_session_id: session_id.to_string(),
        run_id: run_id.to_string(),
        message_id,
        tool_call_id,
        kind,
    })
}

fn base64_basic(value: &header::HeaderValue) -> Result<String> {
    let bytes = value.as_bytes();
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::new();
    for chunk in bytes.chunks(3) {
        let a = chunk[0] as usize;
        let b = chunk.get(1).copied().unwrap_or(0) as usize;
        let c = chunk.get(2).copied().unwrap_or(0) as usize;
        output.push(TABLE[a >> 2] as char);
        output.push(TABLE[((a & 3) << 4) | (b >> 4)] as char);
        output.push(if chunk.len() > 1 {
            TABLE[((b & 15) << 2) | (c >> 6)] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[c & 63] as char
        } else {
            '='
        });
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::Notify,
        time::{Duration, timeout},
    };
    #[test]
    fn maps_documented_text_and_tool_events() {
        let frame = br#"data: {"id":"evt-1","type":"message.part.delta","properties":{"sessionID":"session-1","messageID":"m1","partID":"p1","field":"text","delta":"hello"}}

"#;
        let event = parse_sse(frame, "run-1", "session-1").unwrap();
        assert_eq!(event.event_id, "evt-1");
        assert_eq!(event.message_id.as_deref(), Some("m1"));
        assert!(matches!(event.kind, ProviderEventKind::MessageDelta { .. }));
    }

    #[test]
    fn maps_tool_state_and_ignores_other_sessions() {
        let frame = br#"data: {"id":"evt-tool","type":"message.part.updated","properties":{"sessionID":"session-1","messageID":"m1","part":{"type":"tool","callID":"call-1","tool":"bash","state":{"status":"running","input":{"command":"ls"}}}}}

"#;
        let event = parse_sse(frame, "run-1", "session-1").unwrap();
        assert!(matches!(event.kind, ProviderEventKind::ToolStarted { .. }));

        let other = br#"data: {"type":"session.idle","properties":{"sessionID":"other"}}

"#;
        assert!(parse_sse(other, "run-1", "session-1").is_none());
    }

    #[tokio::test]
    async fn streams_events_from_documented_fixture_routes() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let prompt_received = Arc::new(Notify::new());
        let server_prompt_received = Arc::clone(&prompt_received);
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let prompt_received = Arc::clone(&server_prompt_received);
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut chunk = [0_u8; 1024];
                    loop {
                        let count = stream.read(&mut chunk).await.ok()?;
                        if count == 0 {
                            return Some(());
                        }
                        request.extend_from_slice(&chunk[..count]);
                        if request.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let path = String::from_utf8_lossy(&request)
                        .lines()
                        .next()?
                        .split_whitespace()
                        .nth(1)?
                        .to_string();
                    if path == "/event" {
                        stream
                            .write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\r\n",
                            )
                            .await
                            .ok()?;
                        stream
                            .write_all(
                                b"data: {\"type\":\"server.connected\",\"properties\":{}}\n\n",
                            )
                            .await
                            .ok()?;
                        prompt_received.notified().await;
                        stream
                            .write_all(
                                b"data: {\"id\":\"evt-delta\",\"type\":\"message.part.delta\",\"properties\":{\"sessionID\":\"session-1\",\"messageID\":\"message-1\",\"field\":\"text\",\"delta\":\"hello\"}}\n\ndata: {\"id\":\"evt-tool\",\"type\":\"message.part.updated\",\"properties\":{\"sessionID\":\"session-1\",\"part\":{\"type\":\"tool\",\"messageID\":\"message-1\",\"callID\":\"call-1\",\"tool\":\"bash\",\"state\":{\"status\":\"running\",\"input\":{\"command\":\"pwd\"}}}}}\n\ndata: {\"id\":\"evt-idle\",\"type\":\"session.idle\",\"properties\":{\"sessionID\":\"session-1\"}}\n\n",
                            )
                            .await
                            .ok()?;
                        return Some(());
                    }
                    let (status, body) = if path == "/global/health" {
                        ("200 OK", r#"{"healthy":true,"version":"fixture"}"#)
                    } else if path == "/provider" {
                        (
                            "200 OK",
                            r#"{"all":[{"id":"fixture","name":"Fixture","models":{"model-1":{}}}],"connected":["fixture"]}"#,
                        )
                    } else if path == "/session" {
                        ("200 OK", r#"{"id":"session-1"}"#)
                    } else if path.ends_with("/prompt_async") {
                        prompt_received.notify_waiters();
                        ("204 No Content", "")
                    } else {
                        ("404 Not Found", "not found")
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{body}",
                        body.len()
                    );
                    stream.write_all(response.as_bytes()).await.ok()?;
                    Some(())
                });
            }
        });

        let client = OpenCodeClient::with_base_url(format!("http://{address}"))?;
        assert_eq!(client.health().await?, "fixture");
        assert_eq!(client.providers().await?.len(), 1);
        let request = ProviderRequest {
            conversation_id: 1,
            prompt: "hello".to_string(),
            model: Some("fixture/model-1".to_string()),
            session: crate::provider::SessionRequest::New,
            run_id: "run-fixture".to_string(),
            application_context: Default::default(),
        };
        let mut run = client.start_run(request).await?;
        let mut received = Vec::new();
        while let Ok(Some(event)) = timeout(Duration::from_secs(3), run.events.recv()).await {
            let terminal = matches!(event.kind, ProviderEventKind::Completed);
            received.push(event);
            if terminal {
                break;
            }
        }
        assert!(
            received
                .iter()
                .any(|event| { matches!(event.kind, ProviderEventKind::MessageDelta { .. }) })
        );
        assert!(
            received
                .iter()
                .any(|event| matches!(event.kind, ProviderEventKind::ToolStarted { .. }))
        );
        assert!(
            received
                .iter()
                .any(|event| matches!(event.kind, ProviderEventKind::Completed))
        );
        server.abort();
        Ok(())
    }
}
