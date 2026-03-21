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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thinking: Option<String>,
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
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Mode {
    Normal,
    Insert,
    Visual,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Focus {
    Sidebar,
    Chat,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Popup {
    None,
    ConfirmDelete { id: String, title: String },
    Help,
}
