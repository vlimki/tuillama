#[derive(Debug, Serialize)]
struct OllamaChatRequest<'a> {
    model: &'a str,
    messages: &'a [OllamaWireMessage],
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<&'a JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [OllamaTool]>,
}

#[derive(Debug, Serialize)]
struct OllamaWireMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    images: Vec<String>,
}

fn message_to_ollama_wire(message: &Message) -> OllamaWireMessage {
    OllamaWireMessage {
        role: role_to_wire(&message.role).to_string(),
        content: message.content.clone(),
        images: message
            .attachments
            .iter()
            .filter_map(|attachment| attachment.data_base64.clone())
            .collect(),
    }
}

fn message_to_ollama_json(message: &Message) -> JsonValue {
    let mut value = json!({
        "role": role_to_wire(&message.role),
        "content": message.content,
    });
    let images = message
        .attachments
        .iter()
        .filter_map(|attachment| attachment.data_base64.clone())
        .collect::<Vec<_>>();
    if !images.is_empty() {
        value["images"] = json!(images);
    }
    value
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
