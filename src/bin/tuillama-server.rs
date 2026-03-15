use anyhow::Result;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::{env, sync::Arc};

use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream, tcp::OwnedWriteHalf},
    sync::Mutex,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Role {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Message {
    role: Role,
    content: String,
}

include!("../modules/protocol.rs");
include!("../modules/ollama_payloads.rs");

async fn send_server_event(writer: &Arc<Mutex<OwnedWriteHalf>>, ev: &ServerEvent) -> Result<()> {
    let payload = serde_json::to_string(ev)?;
    let mut guard = writer.lock().await;
    guard.write_all(payload.as_bytes()).await?;
    guard.write_all(b"\n").await?;
    guard.flush().await?;
    Ok(())
}

async fn process_stream_request(
    writer: Arc<Mutex<OwnedWriteHalf>>,
    request_id: String,
    chat_id: String,
    api_url: String,
    model: String,
    options: Option<JsonValue>,
    ollama_api_key: Option<String>,
    web_search: bool,
    messages: Vec<Message>,
) {
    let web_search_tools = [OllamaTool {
        kind: "function",
        function: OllamaToolFunction { name: "web_search" },
    }];
    let req = OllamaChatRequest {
        model: &model,
        messages: &messages,
        stream: true,
        options: options.as_ref(),
        tools: if web_search {
            Some(&web_search_tools)
        } else {
            None
        },
    };

    let mut headers = HeaderMap::new();
    if let Some(key) = ollama_api_key.as_deref() {
        if let Ok(mut val) = HeaderValue::from_str(&format!("Bearer {key}")) {
            val.set_sensitive(true);
            headers.insert(AUTHORIZATION, val);
        }
    }

    let client = reqwest::Client::new();
    let resp = match client
        .post(api_url)
        .headers(headers)
        .json(&req)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let _ = send_server_event(
                &writer,
                &ServerEvent::Error {
                    request_id,
                    chat_id,
                    message: e.to_string(),
                },
            )
            .await;
            return;
        }
    };

    if !resp.status().is_success() {
        let text = resp
            .text()
            .await
            .unwrap_or_else(|_| "unknown error".to_string());
        let _ = send_server_event(
            &writer,
            &ServerEvent::Error {
                request_id,
                chat_id,
                message: format!("HTTP error: {}", text),
            },
        )
        .await;
        return;
    }

    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();

    while let Some(item) = stream.next().await {
        match item {
            Ok(chunk) => {
                buf.extend_from_slice(&chunk);
                while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                    let line = buf.drain(..=pos).collect::<Vec<u8>>();
                    let line = &line[..line.len().saturating_sub(1)];
                    if line.is_empty() {
                        continue;
                    }
                    if let Ok(obj) = serde_json::from_slice::<OllamaChatStreamChunk>(line) {
                        if let Some(err) = obj.error {
                            let _ = send_server_event(
                                &writer,
                                &ServerEvent::Error {
                                    request_id,
                                    chat_id,
                                    message: err,
                                },
                            )
                            .await;
                            return;
                        }
                        if let Some(msg) = obj.message {
                            let _ = send_server_event(
                                &writer,
                                &ServerEvent::Chunk {
                                    request_id: request_id.clone(),
                                    chat_id: chat_id.clone(),
                                    delta: msg.content,
                                },
                            )
                            .await;
                        }
                        if obj.done {
                            let _ = send_server_event(
                                &writer,
                                &ServerEvent::Done {
                                    request_id: request_id.clone(),
                                    chat_id: chat_id.clone(),
                                },
                            )
                            .await;
                            return;
                        }
                    }
                }
            }
            Err(e) => {
                let _ = send_server_event(
                    &writer,
                    &ServerEvent::Error {
                        request_id,
                        chat_id,
                        message: e.to_string(),
                    },
                )
                .await;
                return;
            }
        }
    }
}

async fn handle_client(stream: TcpStream) -> Result<()> {
    let (reader_half, writer_half) = stream.into_split();
    let writer = Arc::new(Mutex::new(writer_half));
    let mut reader = BufReader::new(reader_half);
    let mut line = String::new();

    while reader.read_line(&mut line).await? > 0 {
        let req: ClientRequest = serde_json::from_str(line.trim_end())?;
        line.clear();

        match req {
            ClientRequest::StartStream {
                request_id,
                chat_id,
                api_url,
                model,
                options,
                ollama_api_key,
                web_search,
                messages,
                ..
            } => {
                let writer2 = writer.clone();
                tokio::spawn(async move {
                    process_stream_request(
                        writer2,
                        request_id,
                        chat_id,
                        api_url,
                        model,
                        options,
                        ollama_api_key,
                        web_search,
                        messages,
                    )
                    .await;
                });
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let server_addr =
        env::var("TUILLAMA_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:7878".to_string());
    let listener = TcpListener::bind(&server_addr).await?;
    eprintln!("tuillama-server listening on {}", server_addr);

    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream).await {
                eprintln!("client handler error: {e}");
            }
        });
    }
}
