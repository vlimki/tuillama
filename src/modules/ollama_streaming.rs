async fn stream_ollama(
    api_url: String,
    model: String,
    options: Option<JsonValue>,
    messages: Vec<Message>,
    tx: UnboundedSender<AppEvent>,
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
            let _ = tx.send(AppEvent::OllamaError(e.to_string()));
            return;
        }
    };

    if !resp.status().is_success() {
        let text = resp
            .text()
            .await
            .unwrap_or_else(|_| "unknown error".to_string());
        let _ = tx.send(AppEvent::OllamaError(format!("HTTP error: {}", text)));
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
                    match serde_json::from_slice::<OllamaChatStreamChunk>(line) {
                        Ok(obj) => {
                            if let Some(err) = obj.error {
                                let _ = tx.send(AppEvent::OllamaError(err));
                            }
                            if let Some(msg) = obj.message {
                                let _ = tx.send(AppEvent::OllamaChunk(msg.content));
                            }
                            if obj.done {
                                let _ = tx.send(AppEvent::OllamaDone);
                            }
                        }
                        Err(_) => {}
                    }
                }
            }
            Err(e) => {
                let _ = tx.send(AppEvent::OllamaError(e.to_string()));
                break;
            }
        }
    }
}

