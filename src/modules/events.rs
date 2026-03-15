#[derive(Debug)]
enum AppEvent {
    Input(KeyEvent),
    OllamaChunk(String),
    OllamaDone,
    OllamaError(String),
}

