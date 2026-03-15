use std::sync::Arc;

use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::{TcpListener, TcpStream, tcp::OwnedWriteHalf},
    sync::Mutex,
};

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
    messages: Vec<Message>,
) {
    let req = OllamaChatRequest {
        model: &model,
        messages: &messages,
        stream: true,
        options: options.as_ref(),
    };

    let client = reqwest::Client::new();
    let resp = match client.post(api_url).json(&req).send().await {
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

async fn run_server(listener: TcpListener) -> Result<()> {
    let (socket, _) = listener.accept().await?;
    let (reader_half, writer_half) = socket.into_split();
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
                messages,
                ..
            } => {
                let writer2 = writer.clone();
                tokio::spawn(async move {
                    process_stream_request(writer2, request_id, chat_id, api_url, model, options, messages).await;
                });
            }
        }
    }

    Ok(())
}

async fn spawn_client_server(tx: UnboundedSender<AppEvent>) -> Result<UnboundedSender<ClientRequest>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;

    tokio::spawn(async move {
        if let Err(e) = run_server(listener).await {
            eprintln!("server error: {e}");
        }
    });

    let stream = TcpStream::connect(addr).await?;
    let (read_half, mut write_half) = stream.into_split();

    let (req_tx, mut req_rx) = unbounded_channel::<ClientRequest>();
    tokio::spawn(async move {
        while let Some(req) = req_rx.recv().await {
            match serde_json::to_string(&req) {
                Ok(s) => {
                    if write_half.write_all(s.as_bytes()).await.is_err() {
                        break;
                    }
                    if write_half.write_all(b"\n").await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let tx_read = tx.clone();
    tokio::spawn(async move {
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    if let Ok(ev) = serde_json::from_str::<ServerEvent>(line.trim_end()) {
                        let _ = tx_read.send(AppEvent::ServerEvent(ev));
                    }
                }
                Err(_) => break,
            }
        }
    });

    Ok(req_tx)
}
