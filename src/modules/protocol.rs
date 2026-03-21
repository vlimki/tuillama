#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientRequest {
    StartStream {
        request_id: String,
        chat_id: String,
        created_ts: i64,
        api_url: String,
        model: String,
        options: Option<JsonValue>,
        ollama_api_key: Option<String>,
        web_search: bool,
        messages: Vec<Message>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerEvent {
    Chunk {
        request_id: String,
        chat_id: String,
        delta: String,
    },
    Thinking {
        request_id: String,
        chat_id: String,
        delta: String,
    },
    Status {
        request_id: String,
        chat_id: String,
        message: String,
    },
    Done {
        request_id: String,
        chat_id: String,
    },
    Error {
        request_id: String,
        chat_id: String,
        message: String,
    },
}
