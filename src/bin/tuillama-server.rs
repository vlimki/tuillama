use anyhow::{Context, Result, anyhow};
use futures_util::StreamExt;
use regex::Regex;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
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
}

include!("../modules/protocol.rs");
include!("../modules/ollama_payloads.rs");

const MAX_TOOL_ITERS: usize = 8;
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

fn tool_arg_string(args: &JsonValue, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(v) = args.get(*key).and_then(|x| x.as_str()) {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    if let Some(v) = args.as_str() {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

fn decode_duckduckgo_redirect(raw_href: &str) -> Option<String> {
    if raw_href.starts_with("http://") || raw_href.starts_with("https://") {
        return Some(raw_href.to_string());
    }
    if raw_href.starts_with("//") {
        return Some(format!("https:{}", raw_href));
    }
    let full = if raw_href.starts_with('/') {
        format!("https://duckduckgo.com{raw_href}")
    } else {
        format!("https://duckduckgo.com/{raw_href}")
    };
    let url = reqwest::Url::parse(&full).ok()?;
    for (k, v) in url.query_pairs() {
        if k == "uddg" {
            let out = v.to_string();
            if out.starts_with("http://") || out.starts_with("https://") {
                return Some(out);
            }
        }
    }
    None
}

async fn fallback_web_search(
    client: &reqwest::Client,
    query: &str,
    request_id: &str,
    debug: bool,
) -> JsonValue {
    let mut url = match reqwest::Url::parse("https://duckduckgo.com/html/") {
        Ok(u) => u,
        Err(e) => {
            return json!({"error": format!("fallback search url parse failed: {e}")});
        }
    };
    url.query_pairs_mut().append_pair("q", query);

    debug_log(
        debug,
        request_id,
        format!("fallback web_search url={}", url),
    );

    let resp = match client
        .get(url)
        .header(USER_AGENT, "tuillama/0.5 (+https://github.com)")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json!({"error": format!("fallback search request failed: {e}")}),
    };

    if !resp.status().is_success() {
        let body = resp
            .text()
            .await
            .unwrap_or_else(|_| "unknown error".to_string());
        return json!({"error": format!("fallback search HTTP error: {body}")});
    }

    let html = match resp.text().await {
        Ok(t) => t,
        Err(e) => return json!({"error": format!("fallback search body read failed: {e}")}),
    };

    let href_re = match Regex::new(r#"href="([^"]+)""#) {
        Ok(r) => r,
        Err(e) => return json!({"error": format!("fallback search regex failed: {e}")}),
    };

    let mut urls: Vec<String> = Vec::new();
    for cap in href_re.captures_iter(&html) {
        let raw = &cap[1];
        if let Some(decoded) = decode_duckduckgo_redirect(raw) {
            if (decoded.starts_with("http://") || decoded.starts_with("https://"))
                && !urls.iter().any(|u| u == &decoded)
            {
                urls.push(decoded);
                if urls.len() >= 5 {
                    break;
                }
            }
        }
    }

    json!({"query": query, "results": urls})
}

async fn fallback_web_fetch(
    client: &reqwest::Client,
    target_url: &str,
    request_id: &str,
    debug: bool,
) -> JsonValue {
    debug_log(
        debug,
        request_id,
        format!("fallback web_fetch url={target_url}"),
    );

    let resp = match client
        .get(target_url)
        .header(USER_AGENT, "tuillama/0.5 (+https://github.com)")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return json!({"error": format!("fallback fetch request failed: {e}")}),
    };

    if !resp.status().is_success() {
        let body = resp
            .text()
            .await
            .unwrap_or_else(|_| "unknown error".to_string());
        return json!({"error": format!("fallback fetch HTTP error: {body}")});
    }

    let html = match resp.text().await {
        Ok(t) => t,
        Err(e) => return json!({"error": format!("fallback fetch body read failed: {e}")}),
    };

    let strip_re = match Regex::new(r"<[^>]+>") {
        Ok(r) => r,
        Err(e) => return json!({"error": format!("fallback fetch regex failed: {e}")}),
    };
    let text = strip_re.replace_all(&html, " ").to_string();
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let snippet = truncate_for_context(&compact, TOOL_RESULT_LIMIT);

    json!({"url": target_url, "content": snippet})
}

async fn run_tool_call(
    client: &reqwest::Client,
    api_base: &str,
    headers: &HeaderMap,
    request_id: &str,
    debug: bool,
    tool_name: &str,
    args: JsonValue,
) -> JsonValue {
    let normalized = normalize_tool_name(tool_name);
    let endpoint = match normalized {
        "web_search" => format!("{api_base}/api/web_search"),
        "web_fetch" => format!("{api_base}/api/web_fetch"),
        other => {
            return json!({
                "error": format!("Tool {other} not found")
            });
        }
    };

    let payload = if args.is_object() {
        args
    } else {
        json!({ "query": args })
    };

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
        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_else(|_| "unknown error".to_string());
        debug_log(
            debug,
            request_id,
            format!("tool call http error body: name={normalized} body={body}"),
        );

        if status == reqwest::StatusCode::NOT_FOUND {
            debug_log(
                debug,
                request_id,
                format!(
                    "tool endpoint not found for {normalized}; using built-in fallback retriever"
                ),
            );
            return match normalized {
                "web_search" => {
                    let query = tool_arg_string(&payload, &["query", "q", "text", "prompt"])
                        .unwrap_or_default();
                    if query.is_empty() {
                        json!({"error": "fallback web_search: missing query"})
                    } else {
                        fallback_web_search(client, &query, request_id, debug).await
                    }
                }
                "web_fetch" => {
                    let url =
                        tool_arg_string(&payload, &["url", "link", "href"]).unwrap_or_default();
                    if url.is_empty() {
                        json!({"error": "fallback web_fetch: missing url"})
                    } else {
                        fallback_web_fetch(client, &url, request_id, debug).await
                    }
                }
                _ => json!({"error": format!("tool HTTP error: {body}")}),
            };
        }

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
    request_id: &str,
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

    let tools = vec![
        json!({ "type": "function", "function": { "name": "web_search" } }),
        json!({ "type": "function", "function": { "name": "web_fetch" } }),
    ];

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

        let web_context = json!({
            "search": search_result,
            "fetch": fetches,
        });

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
            &request_id,
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
