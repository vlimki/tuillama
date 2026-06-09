#[derive(Debug)]
enum AppEvent {
    Input(KeyEvent),
    Resize,
    RefreshScreen,
    OllamaError(String),
    ServerEvent(ServerEvent),
}

