use std::env;

use anyhow::{Context, Result};
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
        let mut builder = Client::builder();
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
        Ok(all
            .iter()
            .filter_map(|provider| {
                let id = provider.get("id")?.as_str()?.to_string();
                let display_name = provider
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_string();
                let models = provider.get("models").and_then(Value::as_object);
                models
                    .and_then(|models| models.iter().next())
                    .map(|(model, _)| ProviderDescriptor {
                        id,
                        display_name,
                        model: Some(model.clone()),
                        capabilities: Default::default(),
                    })
            })
            .collect())
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
        let message = if status == StatusCode::UNAUTHORIZED {
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
            let prompt = client.prompt_async(&session_for_task, &request);
            let mut prompt = Box::pin(prompt);
            let mut response = match client.event_stream().await {
                Ok(response) => response,
                Err(error) => {
                    let _ = sender
                        .send(failed_event(
                            &task_run_id,
                            &session_for_task,
                            error.to_string(),
                        ))
                        .await;
                    return;
                }
            };
            let mut buffer = Vec::new();
            let mut prompt_done = false;
            while !prompt_done {
                tokio::select! {
                    result = &mut prompt => {
                        if let Err(error) = result { let _ = sender.send(failed_event(&task_run_id, &session_for_task, error.to_string())).await; return; }
                        prompt_done = true;
                    }
                    chunk = response.chunk() => match chunk {
                        Ok(Some(chunk)) => {
                            buffer.extend_from_slice(&chunk);
                            while let Some(position) = buffer.windows(2).position(|window| window == b"\n\n") {
                                let frame = buffer.drain(..position + 2).collect::<Vec<_>>();
                                if let Some(event) = parse_sse(&frame, &task_run_id, &session_for_task) { let _ = sender.send(event).await; }
                            }
                        }
                        Ok(None) => { let _ = sender.send(failed_event(&task_run_id, &session_for_task, "OpenCode event stream ended".to_string())).await; return; }
                        Err(error) => { let _ = sender.send(failed_event(&task_run_id, &session_for_task, error.to_string())).await; return; }
                    }
                }
            }
            let _ = sender
                .send(event(
                    &task_run_id,
                    &session_for_task,
                    "session.completed",
                    ProviderEventKind::Completed,
                ))
                .await;
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
        Box::pin(async move {
            self.client
                .start_run(request)
                .await
                .map_err(|error| ProviderError::Transport {
                    message: error.to_string(),
                })
        })
    }
    fn interrupt<'a>(
        &'a self,
        _session: &'a ProviderSession,
    ) -> ProviderFuture<'a, InterruptResult> {
        Box::pin(async {
            Ok(InterruptResult {
                provider_acknowledged: false,
                final_status: OperationStatus::Uncertain,
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

fn map_event(value: &Value, run_id: &str, session_id: &str) -> Option<ProviderEvent> {
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let properties = value.get("properties").unwrap_or(&Value::Null);
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
    let message_id = properties
        .get("messageID")
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
        ProviderEventKind::Failed {
            error: ProviderError::Failed {
                message: properties.to_string(),
            },
        }
    } else if event_type == "message.part.updated" {
        let part = properties.get("part")?;
        match part.get("type").and_then(Value::as_str) {
            Some("text") => ProviderEventKind::MessageDelta {
                role: MessageRole::Assistant,
                content: part
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            },
            Some("tool") => ProviderEventKind::ToolOutput {
                call_id: tool_call_id.clone().unwrap_or_default(),
                output: part.to_string(),
                is_final: part
                    .get("state")
                    .and_then(|state| state.get("status"))
                    .and_then(Value::as_str)
                    .map(|status| status != "running")
                    .unwrap_or(false),
            },
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
    use super::*;
    #[test]
    fn maps_documented_text_and_tool_events() {
        let frame = br#"data: {"type":"message.part.updated","properties":{"messageID":"m1","part":{"type":"text","text":"hello"}}}

"#;
        let event = parse_sse(frame, "run-1", "session-1").unwrap();
        assert_eq!(event.message_id.as_deref(), Some("m1"));
        assert!(matches!(event.kind, ProviderEventKind::MessageDelta { .. }));
    }
}
