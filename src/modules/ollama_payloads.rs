#[derive(Debug, Serialize)]
struct OllamaChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<&'a JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [OllamaTool]>,
}

#[derive(Debug, Serialize)]
struct OllamaTool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: OllamaToolFunction,
}

#[derive(Debug, Serialize)]
struct OllamaToolFunction {
    name: &'static str,
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
    #[serde(default)]
    thinking: String,
    content: String,
}
