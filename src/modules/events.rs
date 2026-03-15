#[derive(Debug)]
enum AppEvent {
    Input(KeyEvent),
    OllamaError(String),
    ServerEvent(ServerEvent),
}

