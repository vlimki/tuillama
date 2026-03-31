use anyhow::{Context, Result, anyhow};
use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use std::{env, sync::Arc};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream, tcp::OwnedWriteHalf},
    sync::Mutex,
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
}

include!("../modules/protocol.rs");
include!("../modules/ollama_payloads.rs");

const MAX_TOOL_ITERS: usize = 7;
const TOOL_RESULT_LIMIT: usize = 8_000;

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

fn web_tools() -> Vec<JsonValue> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Search the web for up-to-date information. Always pass the user's search text in the required `query` string field.",
                "parameters": {
                    "type": "object",
                    "required": ["query"],
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "The search query to send to the web search API."
                        }
                    },
                    "additionalProperties": false
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "web_fetch",
                "description": "Fetch the contents of a specific web page. Always pass the target page in the required `url` string field.",
                "parameters": {
                    "type": "object",
                    "required": ["url"],
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "The absolute http or https URL to fetch."
                        }
                    },
                    "additionalProperties": false
                }
            }
        }),
    ]
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

async fn run_tool_call(
    client: &reqwest::Client,
    _api_base: &str,
    headers: &HeaderMap,
    request_id: &str,
    debug: bool,
    tool_name: &str,
    args: JsonValue,
) -> JsonValue {
    let normalized = normalize_tool_name(tool_name);
    let endpoint = match normalized {
        // Use ollama.com instead of local API for these
        "web_search" => format!("https://ollama.com/api/web_search"),
        "web_fetch" => format!("https://ollama.com/api/web_fetch"),
        other => {
            return json!({
                "error": format!("Tool {other} not found")
            });
        }
    };

    let payload = normalize_tool_payload(normalized, args);

    debug_log(
        debug,
        request_id,
        format!("tool call start: name={normalized} endpoint={endpoint} payload={payload}"),
    );

    let resp = match client
        .post(endpoint)
        .headers(headers.clone())
        .json(&payload)
        .send()
        .await
    {
        Ok(r) => {
            debug_log(
                debug,
                request_id,
                format!(
                    "tool call http status: name={normalized} status={}",
                    r.status()
                ),
            );
            r
        }
        Err(e) => {
            debug_log(
                debug,
                request_id,
                format!("tool call network error: name={normalized} err={e}"),
            );
            return json!({
                "error": format!("tool request failed: {e}")
            });
        }
    };

    if !resp.status().is_success() {
        let body = resp
            .text()
            .await
            .unwrap_or_else(|_| "unknown error".to_string());
        debug_log(
            debug,
            request_id,
            format!("tool call http error body: name={normalized} body={body}"),
        );
        return json!({
            "error": format!("tool HTTP error: {body}")
        });
    }

    let body = match resp.text().await {
        Ok(b) => b,
        Err(e) => {
            debug_log(
                debug,
                request_id,
                format!("tool call read body error: name={normalized} err={e}"),
            );
            return json!({
                "error": format!("failed to read tool response: {e}")
            });
        }
    };

    let trimmed = truncate_for_context(&body, TOOL_RESULT_LIMIT);
    let parsed =
        serde_json::from_str::<JsonValue>(&trimmed).unwrap_or_else(|_| json!({ "text": trimmed }));
    debug_log(
        debug,
        request_id,
        format!("tool call parsed response: name={normalized} parsed={parsed}"),
    );
    parsed
}

async fn process_agentic_web_search(
    writer: &Arc<Mutex<OwnedWriteHalf>>,
    request_id: &str,
    chat_id: &str,
    api_url: String,
    model: String,
    options: Option<JsonValue>,
    ollama_api_key: Option<String>,
    messages: Vec<Message>,
    debug: bool,
) -> Result<String> {
    let client = reqwest::Client::new();
    let headers = build_auth_headers(ollama_api_key.as_deref());
    let api_base = api_base_from_chat_url(&api_url);
    debug_log(
        debug,
        request_id,
        format!(
            "agentic mode start: model={model} api_url={api_url} api_base={api_base} message_count={}",
            messages.len()
        ),
    );

    let tools = web_tools();

    let mut convo: Vec<JsonValue> = messages
        .iter()
        .map(|m| {
            json!({
                "role": role_to_wire(&m.role),
                "content": m.content,
            })
        })
        .collect();

    // Force at least one real web retrieval in WEB ON mode, so providers/models
    // that don't autonomously emit tool calls still get fresh context.
    if let Some(query) = latest_user_query(&messages) {
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
            let search_result = run_tool_call(
                &client,
                &api_base,
                &headers,
                request_id,
                debug,
                "web_search",
                json!({ "query": query }),
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
                let fetched = run_tool_call(
                    &client,
                    &api_base,
                    &headers,
                    request_id,
                    debug,
                    "web_fetch",
                    json!({ "url": url }),
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
                let fetched = run_tool_call(
                    &client,
                    &api_base,
                    &headers,
                    request_id,
                    debug,
                    "web_fetch",
                    json!({ "url": url }),
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

    let mut final_answer = String::new();

    for iter in 0..MAX_TOOL_ITERS {
        let _ = send_status(
            writer,
            request_id,
            chat_id,
            format!(
                "Reasoning over retrieved context (step {} of {})",
                iter + 1,
                MAX_TOOL_ITERS
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
            "tools": &tools,
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

        if let Some(content) = msg.get("content").and_then(|v| v.as_str()) {
            if !content.is_empty() {
                final_answer = content.to_string();
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
            if final_answer.is_empty() {
                final_answer = "No response content returned by model.".to_string();
            }
            return Ok(final_answer);
        }

        for tc in tool_calls {
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

            let tool_result = run_tool_call(
                &client, &api_base, &headers, request_id, debug, tool_name, args,
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

    if final_answer.is_empty() {
        Err(anyhow!(
            "agentic search exceeded iteration limit without final content"
        ))
    } else {
        Ok(final_answer)
    }
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
    fn advertises_required_tool_parameters() {
        let tools = web_tools();
        assert_eq!(tools.len(), 2);
        assert_eq!(
            tools[0]["function"]["parameters"]["required"],
            json!(["query"])
        );
        assert_eq!(
            tools[1]["function"]["parameters"]["required"],
            json!(["url"])
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
    let req = OllamaChatRequest {
        model: &model,
        messages: &messages,
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
    request_id: String,
    chat_id: String,
    api_url: String,
    model: String,
    options: Option<JsonValue>,
    ollama_api_key: Option<String>,
    web_search: bool,
    messages: Vec<Message>,
) {
    let debug = debug_enabled();
    debug_log(
        debug,
        &request_id,
        format!("start stream: chat_id={chat_id} web_search={web_search} model={model}"),
    );
    if web_search {
        match process_agentic_web_search(
            &writer,
            &request_id,
            &chat_id,
            api_url,
            model,
            options,
            ollama_api_key,
            messages,
            debug,
        )
        .await
        {
            Ok(answer) => {
                let _ = send_server_event(
                    &writer,
                    &ServerEvent::Chunk {
                        request_id: request_id.clone(),
                        chat_id: chat_id.clone(),
                        delta: answer,
                    },
                )
                .await;
                let _ = send_server_event(
                    &writer,
                    &ServerEvent::Done {
                        request_id,
                        chat_id,
                    },
                )
                .await;
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
            }
        }
        return;
    }

    process_normal_stream(
        writer,
        request_id,
        chat_id,
        api_url,
        model,
        options,
        ollama_api_key,
        messages,
    )
    .await;
}

async fn handle_client(stream: TcpStream) -> Result<()> {
    let (reader_half, writer_half) = stream.into_split();
    let writer = Arc::new(Mutex::new(writer_half));
    let mut reader = BufReader::new(reader_half);
    let mut line = String::new();

    while reader.read_line(&mut line).await? > 0 {
        let req: ClientRequest = serde_json::from_str(line.trim_end())?;
        line.clear();

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
                let writer2 = writer.clone();
                tokio::spawn(async move {
                    process_stream_request(
                        writer2,
                        request_id,
                        chat_id,
                        api_url,
                        model,
                        options,
                        ollama_api_key,
                        web_search,
                        messages,
                    )
                    .await;
                });
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let server_addr =
        env::var("TUILLAMA_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:7878".to_string());
    let listener = TcpListener::bind(&server_addr).await?;
    eprintln!("tuillama-server listening on {server_addr}");

    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream).await {
                eprintln!("client handler error: {e}");
            }
        });
    }
}
