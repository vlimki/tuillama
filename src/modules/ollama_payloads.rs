#[derive(Debug, Serialize)]
struct OllamaChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<&'a JsonValue>,
}

#[derive(Debug, Deserialize)]
struct OllamaChatStreamChunk {
    done: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    message: Option<OllamaChatMessage>,
}
#[derive(Debug, Deserialize)]
struct OllamaChatMessage {
    #[allow(dead_code)]
    role: String,
    content: String,
}

