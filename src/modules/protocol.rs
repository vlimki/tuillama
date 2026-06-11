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
    CancelStream {
        request_id: String,
        chat_id: String,
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
    Source {
        request_id: String,
        chat_id: String,
        title: String,
        url: String,
        snippet: String,
    },
    ToolCallStarted {
        request_id: String,
        chat_id: String,
        tool_call_id: String,
        name: String,
        args: JsonValue,
    },
    ToolCallFinished {
        request_id: String,
        chat_id: String,
        tool_call_id: String,
        result_summary: String,
    },
    ToolCallFailed {
        request_id: String,
        chat_id: String,
        tool_call_id: String,
        error: String,
    },
    Done {
        request_id: String,
        chat_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
    },
    Error {
        request_id: String,
        chat_id: String,
        message: String,
    },
}
