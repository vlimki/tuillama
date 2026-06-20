use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    sync::Arc,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream, tcp::OwnedWriteHalf},
    sync::{Mutex, oneshot},
    task::JoinHandle,
};

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<Attachment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    sources: Vec<Source>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Attachment {
    #[serde(default)]
    id: String,
    #[serde(default)]
    original_path: String,
    mime_type: String,
    #[serde(default)]
    sha256: String,
    #[serde(default)]
    size_bytes: u64,
    #[serde(default)]
    store_path: String,
    #[allow(dead_code)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    data_base64: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Source {
    title: String,
    url: String,
    snippet: String,
}

include!("../modules/protocol.rs");
include!("../modules/ollama_payloads.rs");

const DEFAULT_MAX_TOOL_ITERS: usize = 12;
const DEFAULT_SERVER_ADDR: &str = "0.0.0.0:7878";
const TOOL_RESULT_LIMIT: usize = 8_000;

#[derive(Clone, Debug)]
struct ServerConfig {
    server_addr: String,
    max_tool_iters: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            server_addr: DEFAULT_SERVER_ADDR.to_string(),
            max_tool_iters: DEFAULT_MAX_TOOL_ITERS,
        }
    }
}

fn parse_server_config() -> Result<ServerConfig> {
    let mut config = ServerConfig::default();
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--addr" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("missing value for --addr"))?;
                config.server_addr = value;
            }
            "--max-tool-iters" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("missing value for --max-tool-iters"))?;
                let parsed = value
                    .parse::<usize>()
                    .context("invalid value for --max-tool-iters")?;
                if parsed == 0 {
                    return Err(anyhow!("--max-tool-iters must be greater than 0"));
                }
                config.max_tool_iters = parsed;
            }
            "--help" | "-h" => {
                println!(
                    "Usage: tuillama-server [--addr <IP:PORT>] [--max-tool-iters <N>]\n\nDefaults:\n  --addr {}\n  --max-tool-iters {}",
                    DEFAULT_SERVER_ADDR, DEFAULT_MAX_TOOL_ITERS
                );
                std::process::exit(0);
            }
            _ => {
                return Err(anyhow!("unknown argument: {arg}. Use --help for usage."));
            }
        }
    }

    Ok(config)
}

fn debug_enabled() -> bool {
    matches!(
        env::var("TUILLAMA_DEBUG_WEB").ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

fn debug_log(enabled: bool, request_id: &str, message: impl AsRef<str>) {
    if enabled {
        eprintln!("[tuillama-server][{request_id}] {}", message.as_ref());
    }
}

async fn send_server_event(writer: &Arc<Mutex<OwnedWriteHalf>>, ev: &ServerEvent) -> Result<()> {
    let payload = serde_json::to_string(ev)?;
    let mut guard = writer.lock().await;
    guard.write_all(payload.as_bytes()).await?;
    guard.write_all(b"\n").await?;
    guard.flush().await?;
    Ok(())
}

async fn send_status(
    writer: &Arc<Mutex<OwnedWriteHalf>>,
    request_id: &str,
    chat_id: &str,
    message: impl Into<String>,
) -> Result<()> {
    send_server_event(
        writer,
        &ServerEvent::Status {
            request_id: request_id.to_string(),
            chat_id: chat_id.to_string(),
            message: message.into(),
        },
    )
    .await
}

async fn send_thinking(
    writer: &Arc<Mutex<OwnedWriteHalf>>,
    request_id: &str,
    chat_id: &str,
    delta: impl Into<String>,
) -> Result<()> {
    send_server_event(
        writer,
        &ServerEvent::Thinking {
            request_id: request_id.to_string(),
            chat_id: chat_id.to_string(),
            delta: delta.into(),
        },
    )
    .await
}

async fn send_source(
    writer: &Arc<Mutex<OwnedWriteHalf>>,
    request_id: &str,
    chat_id: &str,
    source: &Source,
) -> Result<()> {
    send_server_event(
        writer,
        &ServerEvent::Source {
            request_id: request_id.to_string(),
            chat_id: chat_id.to_string(),
            title: source.title.clone(),
            url: source.url.clone(),
            snippet: source.snippet.clone(),
        },
    )
    .await
}

async fn emit_sources_from_value(
    writer: &Arc<Mutex<OwnedWriteHalf>>,
    request_id: &str,
    chat_id: &str,
    seen_urls: &mut HashSet<String>,
    value: &JsonValue,
) {
    for source in sources_from_value(value) {
        if seen_urls.insert(source.url.clone()) {
            let _ = send_source(writer, request_id, chat_id, &source).await;
        }
    }
}

fn sources_from_value(value: &JsonValue) -> Vec<Source> {
    let mut sources = Vec::new();
    collect_sources(value, &mut sources);
    sources
}

fn collect_sources(value: &JsonValue, out: &mut Vec<Source>) {
    match value {
        JsonValue::Object(map) => {
            let url = ["url", "link", "href"]
                .iter()
                .find_map(|key| map.get(*key).and_then(|v| v.as_str()))
                .filter(|url| url.starts_with("http://") || url.starts_with("https://"));
            if let Some(url) = url {
                if !out.iter().any(|source| source.url == url) {
                    let title = ["title", "name", "site_name"]
                        .iter()
                        .find_map(|key| map.get(*key).and_then(|v| v.as_str()))
                        .unwrap_or(url)
                        .to_string();
                    let snippet = ["snippet", "summary", "description", "content", "text"]
                        .iter()
                        .find_map(|key| map.get(*key).and_then(|v| v.as_str()))
                        .map(|text| truncate_for_context(text, 240))
                        .unwrap_or_default();
                    out.push(Source {
                        title,
                        url: url.to_string(),
                        snippet,
                    });
                }
            }
            for child in map.values() {
                collect_sources(child, out);
            }
        }
        JsonValue::Array(items) => {
            for item in items {
                collect_sources(item, out);
            }
        }
        _ => {}
    }
}

fn build_auth_headers(ollama_api_key: Option<&str>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(key) = ollama_api_key {
        if let Ok(mut val) = HeaderValue::from_str(&format!("Bearer {key}")) {
            val.set_sensitive(true);
            headers.insert(AUTHORIZATION, val);
        }
    }
    headers
}

fn api_base_from_chat_url(api_url: &str) -> String {
    if let Some((base, _)) = api_url.rsplit_once("/api/chat") {
        base.to_string()
    } else {
        api_url.trim_end_matches('/').to_string()
    }
}

fn role_to_wire(role: &Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

fn truncate_for_context(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn latest_user_query(messages: &[Message]) -> Option<String> {
    messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, Role::User) && !m.content.trim().is_empty())
        .map(|m| m.content.trim().to_string())
}

fn extract_urls_from_text(text: &str) -> Vec<String> {
    let mut urls = Vec::new();
    if let Ok(re) = regex::Regex::new(r#"https?://[^\s)\]}>\"']+"#) {
        for m in re.find_iter(text) {
            let url = m
                .as_str()
                .trim_end_matches(&['.', ',', ';', ':', '!', '?'][..]);
            if !url.is_empty() && !urls.iter().any(|u| u == url) {
                urls.push(url.to_string());
            }
        }
    }
    urls
}

fn collect_urls(value: &JsonValue, out: &mut Vec<String>) {
    match value {
        JsonValue::Object(map) => {
            for key in ["url", "link", "href"] {
                if let Some(v) = map.get(key).and_then(|x| x.as_str()) {
                    if (v.starts_with("http://") || v.starts_with("https://"))
                        && !out.iter().any(|u| u == v)
                    {
                        out.push(v.to_string());
                    }
                }
            }
            for v in map.values() {
                collect_urls(v, out);
            }
        }
        JsonValue::Array(arr) => {
            for v in arr {
                collect_urls(v, out);
            }
        }
        _ => {}
    }
}

fn normalize_tool_name(name: &str) -> &str {
    match name {
        "webfetch" => "web_fetch",
        "websearch" => "web_search",
        other => other,
    }
}

#[derive(Clone)]
struct ToolContext {
    client: reqwest::Client,
    headers: HeaderMap,
    writer: Arc<Mutex<OwnedWriteHalf>>,
    pending_client_tools: PendingClientTools,
}

type PendingClientTools = Arc<Mutex<HashMap<String, oneshot::Sender<JsonValue>>>>;

type ToolResult = Result<JsonValue>;

#[async_trait]
trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn schema(&self) -> JsonValue;
    async fn call(&self, args: JsonValue, ctx: ToolContext) -> ToolResult;
}

struct WebSearchTool;
struct WebFetchTool;
struct FileListTool;
struct FileReadTool;
struct FileSearchTool;

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &'static str {
        "web_search"
    }
    fn schema(&self) -> JsonValue {
        json!({
            "type": "function",
            "function": {
                "name": self.name(),
                "description": "Search the web for up-to-date information. Always pass the user's search text in the required `query` string field.",
                "parameters": {
                    "type": "object",
                    "required": ["query"],
                    "properties": { "query": { "type": "string", "description": "The search query to send to the web search API." } },
                    "additionalProperties": false
                }
            }
        })
    }
    async fn call(&self, args: JsonValue, ctx: ToolContext) -> ToolResult {
        call_ollama_web_tool(&ctx.client, &ctx.headers, self.name(), args).await
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &'static str {
        "web_fetch"
    }
    fn schema(&self) -> JsonValue {
        json!({
            "type": "function",
            "function": {
                "name": self.name(),
                "description": "Fetch the contents of a specific web page. Always pass the target page in the required `url` string field.",
                "parameters": {
                    "type": "object",
                    "required": ["url"],
                    "properties": { "url": { "type": "string", "description": "The absolute http or https URL to fetch." } },
                    "additionalProperties": false
                }
            }
        })
    }
    async fn call(&self, args: JsonValue, ctx: ToolContext) -> ToolResult {
        call_ollama_web_tool(&ctx.client, &ctx.headers, self.name(), args).await
    }
}

fn file_tool_schema(
    name: &'static str,
    description: &'static str,
    properties: JsonValue,
    required: Vec<&'static str>,
) -> JsonValue {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": {
                "type": "object",
                "required": required,
                "properties": properties,
                "additionalProperties": false
            }
        }
    })
}

#[async_trait]
impl Tool for FileListTool {
    fn name(&self) -> &'static str {
        "file_list"
    }
    fn schema(&self) -> JsonValue {
        file_tool_schema(
            self.name(),
            "List files in a local directory. Optional glob filters entries.",
            json!({
                "path": { "type": "string", "description": "Directory to list. Defaults to the current directory." },
                "glob": { "type": "string", "description": "Optional glob pattern such as **/*.rs." }
            }),
            vec![],
        )
    }
    async fn call(&self, _args: JsonValue, _ctx: ToolContext) -> ToolResult {
        bail!("filesystem tools must be executed by the connected client")
    }
}

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &'static str {
        "file_read"
    }
    fn schema(&self) -> JsonValue {
        file_tool_schema(
            self.name(),
            "Read a local file with line numbers for citations. Respects the server max file byte limit.",
            json!({
                "path": { "type": "string", "description": "File path to read." },
                "start_line": { "type": "integer", "minimum": 1 },
                "end_line": { "type": "integer", "minimum": 1 }
            }),
            vec!["path"],
        )
    }
    async fn call(&self, _args: JsonValue, _ctx: ToolContext) -> ToolResult {
        bail!("filesystem tools must be executed by the connected client")
    }
}

#[async_trait]
impl Tool for FileSearchTool {
    fn name(&self) -> &'static str {
        "file_search"
    }
    fn schema(&self) -> JsonValue {
        file_tool_schema(
            self.name(),
            "Search local files for a regex or literal query and return line-cited matches. Optional path and glob narrow the search.",
            json!({
                "query": { "type": "string" },
                "path": { "type": "string", "description": "Directory to search. Defaults to the current directory." },
                "glob": { "type": "string", "description": "Optional glob pattern such as **/*.rs." }
            }),
            vec!["query"],
        )
    }
    async fn call(&self, _args: JsonValue, _ctx: ToolContext) -> ToolResult {
        bail!("filesystem tools must be executed by the connected client")
    }
}

fn build_tool_registry() -> HashMap<String, Arc<dyn Tool>> {
    all_tools()
        .into_iter()
        .map(|tool| (tool.name().to_string(), tool))
        .collect()
}

fn is_client_filesystem_tool(name: &str) -> bool {
    matches!(
        normalize_tool_name(name),
        "file_list" | "file_read" | "file_search"
    )
}

fn all_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(WebSearchTool),
        Arc::new(WebFetchTool),
        Arc::new(FileListTool),
        Arc::new(FileReadTool),
        Arc::new(FileSearchTool),
    ]
}

fn tool_schemas() -> Vec<JsonValue> {
    all_tools().into_iter().map(|tool| tool.schema()).collect()
}

fn normalize_tool_payload(tool_name: &str, raw_args: JsonValue) -> JsonValue {
    let normalized_name = normalize_tool_name(tool_name);
    let args = match raw_args {
        JsonValue::String(s) => {
            serde_json::from_str::<JsonValue>(&s).unwrap_or_else(|_| match normalized_name {
                "web_fetch" => json!({ "url": s }),
                _ => json!({ "query": s }),
            })
        }
        other => other,
    };

    match normalized_name {
        "web_search" => match args {
            JsonValue::Object(map) => {
                if let Some(query) = map
                    .get("query")
                    .or_else(|| map.get("text"))
                    .or_else(|| map.get("q"))
                    .and_then(|v| v.as_str())
                {
                    json!({ "query": query.trim() })
                } else {
                    JsonValue::Object(map)
                }
            }
            JsonValue::String(s) => json!({ "query": s.trim() }),
            other => json!({ "query": other }),
        },
        "web_fetch" => match args {
            JsonValue::Object(map) => {
                if let Some(url) = map
                    .get("url")
                    .or_else(|| map.get("href"))
                    .or_else(|| map.get("link"))
                    .and_then(|v| v.as_str())
                {
                    json!({ "url": url.trim() })
                } else {
                    JsonValue::Object(map)
                }
            }
            JsonValue::String(s) => json!({ "url": s.trim() }),
            other => json!({ "url": other }),
        },
        _ => args,
    }
}

async fn call_ollama_web_tool(
    client: &reqwest::Client,
    headers: &HeaderMap,
    tool_name: &str,
    args: JsonValue,
) -> ToolResult {
    let normalized = normalize_tool_name(tool_name);
    let endpoint = match normalized {
        "web_search" => "https://ollama.com/api/web_search",
        "web_fetch" => "https://ollama.com/api/web_fetch",
        other => bail!("Tool {other} not found"),
    };
    let payload = normalize_tool_payload(normalized, args);
    let resp = client
        .post(endpoint)
        .headers(headers.clone())
        .json(&payload)
        .send()
        .await?;
    if !resp.status().is_success() {
        let body = resp
            .text()
            .await
            .unwrap_or_else(|_| "unknown error".to_string());
        bail!("tool HTTP error: {body}");
    }
    let body = resp.text().await?;
    let trimmed = truncate_for_context(&body, TOOL_RESULT_LIMIT);
    Ok(serde_json::from_str::<JsonValue>(&trimmed).unwrap_or_else(|_| json!({ "text": trimmed })))
}

async fn run_tool_call(
    registry: &HashMap<String, Arc<dyn Tool>>,
    ctx: ToolContext,
    request_id: &str,
    debug: bool,
    tool_name: &str,
    args: JsonValue,
) -> JsonValue {
    let normalized = normalize_tool_name(tool_name).to_string();
    let payload = normalize_tool_payload(&normalized, args);
    debug_log(
        debug,
        request_id,
        format!("tool call start: name={normalized} payload={payload}"),
    );
    if is_client_filesystem_tool(&normalized) {
        return json!({ "error": "filesystem tools must be run through the client-side tool bridge" });
    }
    let Some(tool) = registry.get(&normalized) else {
        return json!({ "error": format!("Tool {normalized} not found") });
    };
    match tool.call(payload, ctx).await {
        Ok(value) => {
            debug_log(
                debug,
                request_id,
                format!("tool call response: name={normalized} response={value}"),
            );
            value
        }
        Err(e) => {
            debug_log(
                debug,
                request_id,
                format!("tool call error: name={normalized} err={e}"),
            );
            json!({ "error": e.to_string() })
        }
    }
}

async fn run_client_tool_call(
    ctx: ToolContext,
    request_id: &str,
    chat_id: &str,
    tool_call_id: &str,
    name: &str,
    args: JsonValue,
) -> JsonValue {
    let (tx, rx) = oneshot::channel();
    ctx.pending_client_tools
        .lock()
        .await
        .insert(tool_call_id.to_string(), tx);
    if let Err(e) = send_server_event(
        &ctx.writer,
        &ServerEvent::ClientToolRequest {
            request_id: request_id.to_string(),
            chat_id: chat_id.to_string(),
            tool_call_id: tool_call_id.to_string(),
            name: name.to_string(),
            args,
        },
    )
    .await
    {
        ctx.pending_client_tools.lock().await.remove(tool_call_id);
        return json!({ "error": format!("failed to send client tool request: {e}") });
    }
    match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => json!({ "error": "client tool response channel closed" }),
        Err(_) => {
            ctx.pending_client_tools.lock().await.remove(tool_call_id);
            json!({ "error": "client tool timed out" })
        }
    }
}

fn summarize_tool_result(value: &JsonValue) -> String {
    if let Some(error) = value.get("error").and_then(|v| v.as_str()) {
        return format!("error: {}", truncate_for_context(error, 180));
    }
    match value {
        JsonValue::Object(map) => {
            if let Some(entries) = map.get("entries").and_then(|v| v.as_array()) {
                return format!("{} entries", entries.len());
            }
            if let Some(matches) = map.get("matches").and_then(|v| v.as_array()) {
                return format!("{} matches", matches.len());
            }
            if let Some(lines) = map.get("lines").and_then(|v| v.as_array()) {
                return format!("{} lines", lines.len());
            }
            format!("object with {} fields", map.len())
        }
        JsonValue::Array(items) => format!("{} items", items.len()),
        other => truncate_for_context(&other.to_string(), 180),
    }
}

async fn run_tool_call_with_events(
    writer: &Arc<Mutex<OwnedWriteHalf>>,
    request_id: &str,
    chat_id: &str,
    tool_call_id: String,
    registry: &HashMap<String, Arc<dyn Tool>>,
    ctx: ToolContext,
    debug: bool,
    tool_name: &str,
    args: JsonValue,
) -> JsonValue {
    let name = normalize_tool_name(tool_name).to_string();
    let payload = normalize_tool_payload(&name, args);
    let _ = send_server_event(
        writer,
        &ServerEvent::ToolCallStarted {
            request_id: request_id.to_string(),
            chat_id: chat_id.to_string(),
            tool_call_id: tool_call_id.clone(),
            name: name.clone(),
            args: payload.clone(),
        },
    )
    .await;
    let result = if is_client_filesystem_tool(&name) {
        run_client_tool_call(ctx, request_id, chat_id, &tool_call_id, &name, payload).await
    } else {
        run_tool_call(registry, ctx, request_id, debug, &name, payload).await
    };
    if let Some(error) = result.get("error").and_then(|v| v.as_str()) {
        let _ = send_server_event(
            writer,
            &ServerEvent::ToolCallFailed {
                request_id: request_id.to_string(),
                chat_id: chat_id.to_string(),
                tool_call_id,
                error: error.to_string(),
            },
        )
        .await;
    } else {
        let _ = send_server_event(
            writer,
            &ServerEvent::ToolCallFinished {
                request_id: request_id.to_string(),
                chat_id: chat_id.to_string(),
                tool_call_id,
                result_summary: summarize_tool_result(&result),
            },
        )
        .await;
    }
    result
}

async fn process_agentic_tools(
    writer: &Arc<Mutex<OwnedWriteHalf>>,
    request_id: &str,
    chat_id: &str,
    api_url: String,
    model: String,
    options: Option<JsonValue>,
    ollama_api_key: Option<String>,
    messages: Vec<Message>,
    debug: bool,
    server_config: ServerConfig,
    force_web_preflight: bool,
    pending_client_tools: PendingClientTools,
) -> Result<()> {
    let client = reqwest::Client::new();
    let headers = build_auth_headers(ollama_api_key.as_deref());
    let registry = build_tool_registry();
    let tool_schemas = tool_schemas();
    let tool_ctx = ToolContext {
        client: client.clone(),
        headers: headers.clone(),
        writer: writer.clone(),
        pending_client_tools,
    };
    let max_tool_iters = server_config.max_tool_iters;
    let api_base = api_base_from_chat_url(&api_url);
    debug_log(
        debug,
        request_id,
        format!(
            "agentic mode start: model={model} api_url={api_url} api_base={api_base} message_count={}",
            messages.len()
        ),
    );

    let mut convo: Vec<JsonValue> = messages.iter().map(message_to_ollama_json).collect();
    let mut emitted_source_urls = HashSet::new();

    // Ctrl-W still forces at least one real web retrieval, while the model
    // always receives every tool schema and can choose tools on its own.
    if force_web_preflight && let Some(query) = latest_user_query(&messages) {
        let direct_urls = extract_urls_from_text(&query);

        let web_context = if direct_urls.is_empty() {
            let _ = send_status(
                writer,
                request_id,
                chat_id,
                format!("Searching the web for: {query}"),
            )
            .await;
            debug_log(
                debug,
                request_id,
                format!("preflight web_search query={query}"),
            );
            let search_result = run_tool_call_with_events(
                writer,
                request_id,
                chat_id,
                "preflight-web-search".to_string(),
                &registry,
                tool_ctx.clone(),
                debug,
                "web_search",
                json!({ "query": query }),
            )
            .await;
            emit_sources_from_value(
                writer,
                request_id,
                chat_id,
                &mut emitted_source_urls,
                &search_result,
            )
            .await;

            let mut urls = Vec::new();
            collect_urls(&search_result, &mut urls);
            urls.truncate(3);
            debug_log(
                debug,
                request_id,
                format!("preflight extracted urls: {:?}", urls),
            );

            let mut fetches: Vec<JsonValue> = Vec::new();
            for url in urls {
                let _ = send_status(
                    writer,
                    request_id,
                    chat_id,
                    format!("Parsing website {url}"),
                )
                .await;
                let fetched = run_tool_call_with_events(
                    writer,
                    request_id,
                    chat_id,
                    format!("preflight-web-fetch-{}", fetches.len() + 1),
                    &registry,
                    tool_ctx.clone(),
                    debug,
                    "web_fetch",
                    json!({ "url": url }),
                )
                .await;
                emit_sources_from_value(
                    writer,
                    request_id,
                    chat_id,
                    &mut emitted_source_urls,
                    &fetched,
                )
                .await;
                fetches.push(fetched);
            }

            json!({
                "mode": "search_then_fetch",
                "search": search_result,
                "fetch": fetches,
            })
        } else {
            debug_log(
                debug,
                request_id,
                format!("preflight direct url fetch urls={:?}", direct_urls),
            );

            let mut fetches: Vec<JsonValue> = Vec::new();
            for url in direct_urls {
                let _ = send_status(
                    writer,
                    request_id,
                    chat_id,
                    format!("Parsing website {url}"),
                )
                .await;
                let fetched = run_tool_call_with_events(
                    writer,
                    request_id,
                    chat_id,
                    format!("preflight-web-fetch-{}", fetches.len() + 1),
                    &registry,
                    tool_ctx.clone(),
                    debug,
                    "web_fetch",
                    json!({ "url": url }),
                )
                .await;
                emit_sources_from_value(
                    writer,
                    request_id,
                    chat_id,
                    &mut emitted_source_urls,
                    &fetched,
                )
                .await;
                fetches.push(fetched);
            }

            json!({
                "mode": "direct_url_fetch",
                "fetch": fetches,
            })
        };

        let web_context_text = truncate_for_context(&web_context.to_string(), TOOL_RESULT_LIMIT);
        debug_log(
            debug,
            request_id,
            format!("preflight web context bytes={}", web_context_text.len()),
        );
        convo.push(json!({
            "role": "system",
            "content": format!(
                "Use the following live web retrieval data to answer with citations (URLs): {}",
                web_context_text
            ),
        }));
    }

    for iter in 0..max_tool_iters {
        let _ = send_status(
            writer,
            request_id,
            chat_id,
            format!(
                "Reasoning over retrieved context (step {} of {})",
                iter + 1,
                max_tool_iters
            ),
        )
        .await;
        debug_log(
            debug,
            request_id,
            format!(
                "agent loop iteration={} convo_len={}",
                iter + 1,
                convo.len()
            ),
        );
        let req = json!({
            "model": &model,
            "messages": &convo,
            "stream": false,
            "think": true,
            "options": options.clone(),
            "tools": &tool_schemas,
        });

        let resp = client
            .post(&api_url)
            .headers(headers.clone())
            .json(&req)
            .send()
            .await
            .context("chat request failed")?;

        if !resp.status().is_success() {
            let body = resp
                .text()
                .await
                .unwrap_or_else(|_| "unknown error".to_string());
            return Err(anyhow!("HTTP error: {body}"));
        }

        let parsed: JsonValue = resp.json().await.context("invalid chat response")?;

        if let Some(err) = parsed.get("error").and_then(|v| v.as_str()) {
            debug_log(
                debug,
                request_id,
                format!("chat response error field: {err}"),
            );
            return Err(anyhow!(err.to_string()));
        }

        let msg = parsed
            .get("message")
            .cloned()
            .ok_or_else(|| anyhow!("chat response missing message"))?;

        if let Some(thinking) = msg.get("thinking").and_then(|v| v.as_str()) {
            if !thinking.is_empty() {
                let _ = send_thinking(writer, request_id, chat_id, thinking).await;
            }
        }

        // Keep the assistant message exactly as returned (including tool_calls/thinking metadata)
        // so Ollama can correctly associate subsequent tool outputs.
        convo.push(msg.clone());

        let tool_calls = msg
            .get("tool_calls")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        debug_log(
            debug,
            request_id,
            format!("model tool_calls count={}", tool_calls.len()),
        );

        if tool_calls.is_empty() {
            convo.push(json!({
                "role": "system",
                "content": "Tool use is complete. Stream the final answer now. Cite sources by URL when useful."
            }));
            let _ = send_status(writer, request_id, chat_id, "Synthesizing final answer").await;
            let req = json!({
                "model": &model,
                "messages": &convo,
                "stream": true,
                "think": true,
                "options": options.clone(),
            });
            return stream_json_chat_response(writer, request_id, chat_id, &api_url, &headers, req)
                .await;
        }

        for (tool_idx, tc) in tool_calls.into_iter().enumerate() {
            let fn_obj = tc.get("function").cloned().unwrap_or_else(|| json!({}));
            let tool_name = fn_obj
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown_tool");
            let args = fn_obj
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let status = match normalize_tool_name(tool_name) {
                "web_search" => "Running a web search".to_string(),
                "web_fetch" => normalize_tool_payload(tool_name, args.clone())
                    .get("url")
                    .and_then(|v| v.as_str())
                    .map(|u| format!("Parsing website {u}"))
                    .unwrap_or_else(|| "Parsing website".to_string()),
                other => format!("Running tool {other}"),
            };
            let _ = send_status(writer, request_id, chat_id, status).await;

            let tool_call_id = tc
                .get("id")
                .and_then(|v| v.as_str())
                .map(ToString::to_string)
                .unwrap_or_else(|| format!("tool-call-{}-{}", iter + 1, tool_idx + 1));
            let tool_result = run_tool_call_with_events(
                writer,
                request_id,
                chat_id,
                tool_call_id,
                &registry,
                tool_ctx.clone(),
                debug,
                tool_name,
                args,
            )
            .await;
            emit_sources_from_value(
                writer,
                request_id,
                chat_id,
                &mut emitted_source_urls,
                &tool_result,
            )
            .await;
            let tool_content = serde_json::to_string(&tool_result)
                .unwrap_or_else(|_| "{\"error\":\"tool serialization failed\"}".to_string());

            convo.push(json!({
                "role": "tool",
                "tool_name": tool_name,
                "content": tool_content,
            }));
        }
    }

    Err(anyhow!(
        "agentic search exceeded iteration limit without final content"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_web_search_argument_aliases() {
        assert_eq!(
            normalize_tool_payload("web_search", json!({ "text": "latest rust news" })),
            json!({ "query": "latest rust news" })
        );
        assert_eq!(
            normalize_tool_payload("websearch", JsonValue::String("weather sf".into())),
            json!({ "query": "weather sf" })
        );
        assert_eq!(
            normalize_tool_payload("web_search", JsonValue::String("{\"q\":\"mars\"}".into())),
            json!({ "query": "mars" })
        );
    }

    #[test]
    fn normalizes_web_fetch_argument_aliases() {
        assert_eq!(
            normalize_tool_payload("web_fetch", json!({ "href": "https://example.com" })),
            json!({ "url": "https://example.com" })
        );
        assert_eq!(
            normalize_tool_payload("webfetch", JsonValue::String("https://example.com".into())),
            json!({ "url": "https://example.com" })
        );
    }

    #[test]
    fn advertises_all_tool_parameters() {
        let tools = tool_schemas();
        let names = tools
            .iter()
            .filter_map(|tool| tool["function"]["name"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "web_search",
                "web_fetch",
                "file_list",
                "file_read",
                "file_search"
            ]
        );
        assert_eq!(
            tools[0]["function"]["parameters"]["required"],
            json!(["query"])
        );
        assert_eq!(
            tools[1]["function"]["parameters"]["required"],
            json!(["url"])
        );
        assert_eq!(
            tools[3]["function"]["parameters"]["required"],
            json!(["path"])
        );
    }

    #[test]
    fn extracts_urls_from_text() {
        assert_eq!(
            extract_urls_from_text("Summarize https://example.com/a?x=1 and https://foo.bar/b."),
            vec![
                "https://example.com/a?x=1".to_string(),
                "https://foo.bar/b".to_string()
            ]
        );
    }

    #[test]
    fn extracts_unique_urls_from_text() {
        assert_eq!(
            extract_urls_from_text("Read https://example.com and https://example.com"),
            vec!["https://example.com".to_string()]
        );
    }
}

async fn stream_json_chat_response(
    writer: &Arc<Mutex<OwnedWriteHalf>>,
    request_id: &str,
    chat_id: &str,
    api_url: &str,
    headers: &HeaderMap,
    req: JsonValue,
) -> Result<()> {
    let client = reqwest::Client::new();
    let resp = client
        .post(api_url)
        .headers(headers.clone())
        .json(&req)
        .send()
        .await
        .context("chat stream request failed")?;

    if !resp.status().is_success() {
        let body = resp
            .text()
            .await
            .unwrap_or_else(|_| "unknown error".to_string());
        return Err(anyhow!("HTTP error: {body}"));
    }

    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(item) = stream.next().await {
        let chunk = item.context("chat stream read failed")?;
        buf.extend_from_slice(&chunk);
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line = buf.drain(..=pos).collect::<Vec<u8>>();
            let line = &line[..line.len().saturating_sub(1)];
            if line.is_empty() {
                continue;
            }
            if let Ok(obj) = serde_json::from_slice::<OllamaChatStreamChunk>(line) {
                if let Some(err) = obj.error {
                    return Err(anyhow!(err));
                }
                if let Some(msg) = obj.message {
                    if !msg.thinking.is_empty() {
                        let _ = send_server_event(
                            writer,
                            &ServerEvent::Thinking {
                                request_id: request_id.to_string(),
                                chat_id: chat_id.to_string(),
                                delta: msg.thinking,
                            },
                        )
                        .await;
                    }
                    if !msg.content.is_empty() {
                        let _ = send_server_event(
                            writer,
                            &ServerEvent::Chunk {
                                request_id: request_id.to_string(),
                                chat_id: chat_id.to_string(),
                                delta: msg.content,
                            },
                        )
                        .await;
                    }
                }
                if obj.done {
                    let _ = send_server_event(
                        writer,
                        &ServerEvent::Done {
                            request_id: request_id.to_string(),
                            chat_id: chat_id.to_string(),
                            status: None,
                        },
                    )
                    .await;
                    return Ok(());
                }
            }
        }
    }
    let _ = send_server_event(
        writer,
        &ServerEvent::Done {
            request_id: request_id.to_string(),
            chat_id: chat_id.to_string(),
            status: None,
        },
    )
    .await;
    Ok(())
}

async fn process_normal_stream(
    writer: Arc<Mutex<OwnedWriteHalf>>,
    request_id: String,
    chat_id: String,
    api_url: String,
    model: String,
    options: Option<JsonValue>,
    ollama_api_key: Option<String>,
    messages: Vec<Message>,
) {
    let wire_messages = messages
        .iter()
        .map(message_to_ollama_wire)
        .collect::<Vec<_>>();
    let req = OllamaChatRequest {
        model: &model,
        messages: &wire_messages,
        stream: true,
        think: Some(true),
        options: options.as_ref(),
        tools: None,
    };

    let headers = build_auth_headers(ollama_api_key.as_deref());
    let client = reqwest::Client::new();
    let resp = match client
        .post(api_url)
        .headers(headers)
        .json(&req)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let _ = send_server_event(
                &writer,
                &ServerEvent::Error {
                    request_id,
                    chat_id,
                    message: e.to_string(),
                },
            )
            .await;
            return;
        }
    };

    if !resp.status().is_success() {
        let text = resp
            .text()
            .await
            .unwrap_or_else(|_| "unknown error".to_string());
        let _ = send_server_event(
            &writer,
            &ServerEvent::Error {
                request_id,
                chat_id,
                message: format!("HTTP error: {text}"),
            },
        )
        .await;
        return;
    }

    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();

    while let Some(item) = stream.next().await {
        match item {
            Ok(chunk) => {
                buf.extend_from_slice(&chunk);
                while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                    let line = buf.drain(..=pos).collect::<Vec<u8>>();
                    let line = &line[..line.len().saturating_sub(1)];
                    if line.is_empty() {
                        continue;
                    }
                    if let Ok(obj) = serde_json::from_slice::<OllamaChatStreamChunk>(line) {
                        if let Some(err) = obj.error {
                            let _ = send_server_event(
                                &writer,
                                &ServerEvent::Error {
                                    request_id,
                                    chat_id,
                                    message: err,
                                },
                            )
                            .await;
                            return;
                        }
                        if let Some(msg) = obj.message {
                            if !msg.thinking.is_empty() {
                                let _ = send_server_event(
                                    &writer,
                                    &ServerEvent::Thinking {
                                        request_id: request_id.clone(),
                                        chat_id: chat_id.clone(),
                                        delta: msg.thinking,
                                    },
                                )
                                .await;
                            }
                            let _ = send_server_event(
                                &writer,
                                &ServerEvent::Chunk {
                                    request_id: request_id.clone(),
                                    chat_id: chat_id.clone(),
                                    delta: msg.content,
                                },
                            )
                            .await;
                        }
                        if obj.done {
                            let _ = send_server_event(
                                &writer,
                                &ServerEvent::Done {
                                    request_id: request_id.clone(),
                                    chat_id: chat_id.clone(),
                                    status: None,
                                },
                            )
                            .await;
                            return;
                        }
                    }
                }
            }
            Err(e) => {
                let _ = send_server_event(
                    &writer,
                    &ServerEvent::Error {
                        request_id,
                        chat_id,
                        message: e.to_string(),
                    },
                )
                .await;
                return;
            }
        }
    }
}

async fn process_stream_request(
    writer: Arc<Mutex<OwnedWriteHalf>>,
    pending_client_tools: PendingClientTools,
    request_id: String,
    chat_id: String,
    api_url: String,
    model: String,
    options: Option<JsonValue>,
    ollama_api_key: Option<String>,
    web_search: bool,
    messages: Vec<Message>,
    server_config: ServerConfig,
) {
    let debug = debug_enabled();
    debug_log(
        debug,
        &request_id,
        format!("start stream: chat_id={chat_id} web_search={web_search} model={model}"),
    );
    match process_agentic_tools(
        &writer,
        &request_id,
        &chat_id,
        api_url,
        model,
        options,
        ollama_api_key,
        messages,
        debug,
        server_config,
        web_search,
        pending_client_tools,
    )
    .await
    {
        Ok(()) => {}
        Err(e) => {
            let _ = send_server_event(
                &writer,
                &ServerEvent::Error {
                    request_id,
                    chat_id,
                    message: e.to_string(),
                },
            )
            .await;
        }
    }
    return;
}

async fn handle_client(stream: TcpStream, config: Arc<ServerConfig>) -> Result<()> {
    let (reader_half, writer_half) = stream.into_split();
    let writer = Arc::new(Mutex::new(writer_half));
    let pending_client_tools: PendingClientTools = Arc::new(Mutex::new(HashMap::new()));
    let mut active_requests: HashMap<(String, String), JoinHandle<()>> = HashMap::new();
    let mut reader = BufReader::new(reader_half);
    let mut line = String::new();

    while reader.read_line(&mut line).await? > 0 {
        let req: ClientRequest = serde_json::from_str(line.trim_end())?;
        line.clear();
        active_requests.retain(|_, handle| !handle.is_finished());

        match req {
            ClientRequest::StartStream {
                request_id,
                chat_id,
                api_url,
                model,
                options,
                ollama_api_key,
                web_search,
                messages,
                ..
            } => {
                if let Some(old) = active_requests.remove(&(chat_id.clone(), request_id.clone())) {
                    old.abort();
                }
                let writer2 = writer.clone();
                let pending2 = pending_client_tools.clone();
                let cfg = config.clone();
                let key = (chat_id.clone(), request_id.clone());
                let handle = tokio::spawn(async move {
                    process_stream_request(
                        writer2,
                        pending2,
                        request_id,
                        chat_id,
                        api_url,
                        model,
                        options,
                        ollama_api_key,
                        web_search,
                        messages,
                        (*cfg).clone(),
                    )
                    .await;
                });
                active_requests.insert(key, handle);
            }
            ClientRequest::CancelStream {
                request_id,
                chat_id,
            } => {
                if let Some(handle) = active_requests.remove(&(chat_id.clone(), request_id.clone()))
                {
                    if handle.is_finished() {
                        continue;
                    }
                    handle.abort();
                    let _ = send_server_event(
                        &writer,
                        &ServerEvent::Done {
                            request_id,
                            chat_id,
                            status: Some("cancelled".to_string()),
                        },
                    )
                    .await;
                } else {
                    let _ = send_server_event(
                        &writer,
                        &ServerEvent::Error {
                            request_id,
                            chat_id,
                            message: "cancelled".to_string(),
                        },
                    )
                    .await;
                }
            }
            ClientRequest::ClientToolResult {
                tool_call_id,
                result,
            } => {
                if let Some(tx) = pending_client_tools.lock().await.remove(&tool_call_id) {
                    let _ = tx.send(result);
                }
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Arc::new(parse_server_config()?);
    let listener = TcpListener::bind(&config.server_addr).await?;
    eprintln!("tuillama-server listening on {}", config.server_addr);

    loop {
        let (stream, _) = listener.accept().await?;
        let cfg = config.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, cfg).await {
                eprintln!("client handler error: {e}");
            }
        });
    }
}
