use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::TcpStream,
};

async fn connect_server(
    tx: UnboundedSender<AppEvent>,
    server_addr: &str,
) -> Result<UnboundedSender<ClientRequest>> {
    let stream = TcpStream::connect(server_addr).await?;
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

    tokio::spawn(async move {
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => {
                    let _ = tx.send(AppEvent::OllamaError("Server disconnected".to_string()));
                    break;
                }
                Ok(_) => {
                    if let Ok(ev) = serde_json::from_str::<ServerEvent>(line.trim_end()) {
                        let _ = tx.send(AppEvent::ServerEvent(ev));
                    }
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::OllamaError(format!("Server read error: {}", e)));
                    break;
                }
            }
        }
    });

    Ok(req_tx)
}
