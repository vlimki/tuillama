#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Role {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Message {
    role: Role,
    content: String,
    #[serde(default)]
    created_ts: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<Attachment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    sources: Vec<Source>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Attachment {
    path: String,
    mime_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    data_base64: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Source {
    title: String,
    url: String,
    snippet: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Chat {
    id: String,
    title: String,
    created_ts: i64,
    updated_ts: i64,
    messages: Vec<Message>,
}

#[derive(Clone, Debug)]
struct ChatMeta {
    id: String,
    title: String,
    updated_ts: i64,
    search_excerpt: Option<String>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Mode {
    Normal,
    Insert,
    Command,
    Visual,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Focus {
    Sidebar,
    Chat,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum VisualSelection {
    Message,
    CodeBlock(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Popup {
    None,
    ConfirmDelete { id: String, title: String },
    Help,
}
