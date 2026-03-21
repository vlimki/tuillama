#[derive(Debug)]
enum AppEvent {
    Input(KeyEvent),
    Resize,
    OllamaError(String),
    ServerEvent(ServerEvent),
}

