# tuillama

Finally, a minimalist yet competent Ollama interface. 

## Features 

* Really fast and minimal
* Vim keys
* Beautiful markdown formatting and syntax highlighting on the terminal
* Dynamic compilation of messages to PDF or HTML (with LaTeX support!)
* Toggleable agentic web search mode (`Ctrl+W`) for Ollama-compatible providers

## Preview

![Preview](media/preview1.png)

![Preview 3](media/preview3.png)

## Agentic web search

Toggle on agentic web search at any time via Ctrl-W. Note that you will have to configure an Ollama API key first in the configuration file.

![Preview 4](media/preview4.png)


## Tool profiles and workspace file tools

`tuillama-server` has a tool registry with named profiles. Profiles decide which tools the model is allowed to see for a request:

| Profile | Tools exposed |
| --- | --- |
| `off` | No tools by default. `Ctrl+W` still enables the web tools for that request. |
| `web` | `web_search`, `web_fetch` |
| `files` | `file_list`, `file_read`, `file_search`, `workspace_search` |
| `workspace` | Same read-only workspace tools as `files` |
| `code` | Web tools plus read-only workspace tools |
| `full` | Web tools plus read-only workspace tools; reserved for future higher-risk tools |

Enable a profile in your config:

```toml
[tools]
profile = "workspace"

[workspace]
root = "~/projects/foo"
allow_read = true
allow_write = false
max_file_bytes = 200000

[tools.policy]
confirm_medium = false
confirm_risky = true
deny_dangerous = true
```

Or override the profile when starting the server:

```bash
cargo run --bin tuillama-server -- --tool-profile workspace
```

The current local workspace tools are read-only and scoped to `[workspace].root`. Absolute paths are rejected; ask for paths relative to the workspace root. Files larger than `max_file_bytes` are not returned.

### Asking the model to use local files

Once a file/workspace profile is enabled, you can ask natural-language questions that map to these tools:

* **List a directory** with `file_list(path, glob)`:
  * “List the files in `src/modules`.”
  * “Show Rust files under `src` matching `**/*.rs`.”
* **Read line-cited file contents** with `file_read(path, start_line, end_line)`:
  * “Read `src/main.rs` lines 1 through 80 and explain the startup flow.”
  * “Open `README.md` around the configuration section.”
* **Search selected files** with `file_search(query, glob)`:
  * “Search for `ToolCallStarted` in `**/*.rs`.”
  * “Find references to `web_search` in server code.”
* **Search the whole workspace** with `workspace_search(query)`:
  * “Search the workspace for `Attachment` and summarize where it is used.”
  * “Find TODOs across the workspace.”

Tool results include workspace-relative paths and line numbers so the assistant can cite exactly where file content came from.

### Tool-call trace events

The server emits structured lifecycle events for every tool call:

* `tool_call_started` with request/chat/tool IDs, tool name, and arguments
* `tool_call_finished` with a short result summary
* `tool_call_failed` with the error message

The client currently surfaces these as status updates, and the protocol is ready for a richer auditable trace UI.

### Attachment storage

New attachments are stored outside chat JSON under:

```text
${XDG_DATA_HOME:-$HOME/.local/share}/tuillama/attachments/<sha256>
```

Chat files keep only metadata such as the original path, MIME type, SHA-256, size, and store path. Image attachments are still loaded from the store and sent to Ollama-compatible multimodal models when needed; non-image attachments are stored as metadata for now.

## No LaTeX on the terminal? No problem

Preview mode renders Markdown + LaTeX to HTML with MathJax. Enter visual mode, select a message, and press `Enter`. In visual mode, press `y` to copy the selected message, `c` to cycle through code blocks in that message, and `y` again to copy only the highlighted code block.

![Preview 2](media/preview2.png)


## Architecture

`tuillama` now uses a client/server split with separate executables:

* **Client (`tuillama`):** handles rendering, input, navigation, and stream presentation.
* **Server (`tuillama-server`):** owns all Ollama HTTP streaming, emits structured events, and tags every stream with `chat_id` + `request_id`.

This prevents streamed tokens from spilling into a different chat when switching views and supports multiple chats generating concurrently.

## Requirements

* Rust toolchain
* Ollama
* `pandoc` and a default browser (`xdg-open` on Linux) for previewing messages
* `xclip` for clipboard integration
* `lualatex` and the LaTeX toolkit for previewing messages (I will eventually make the LaTeX engine configurable)

Optional

* A font with good Unicode/emoji support

## Build and run

```bash
cargo build --release

# pull or start a model
ollama pull llama3
# or: ollama run llama3

# run server (separate terminal)
cargo run --bin tuillama-server

# run client
cargo run --bin tuillama
```

Environment overrides (take precedence over config):

```bash
export OLLAMA_MODEL=llama3
export OLLAMA_CHAT_URL=http://localhost:11434/api/chat
export TUILLAMA_SERVER_ADDR=127.0.0.1:7878
export OLLAMA_API_KEY=<optional_api_key>
# optional: enable server debug traces for WEB mode
export TUILLAMA_DEBUG_WEB=1
```

## Configuration

Config file path (XDG):

* `${XDG_CONFIG_HOME:-$HOME/.config}/tuillama/config.toml`

Precedence: environment variables > config file > built-ins.

All keys under `[options]` are forwarded to Ollama's `options` field.

You can set `ollama_api_key` in config (or `OLLAMA_API_KEY` in env) for hosted providers that require bearer auth.

Press `Ctrl+W` in the client UI to toggle server-side agentic web search per request. In this mode the server loops over `web_search`/`web_fetch` tool calls and returns the final answer to the client.
If your prompt includes one or more full `http://` or `https://` URLs, WEB mode now fetches those exact pages first (instead of searching) so you can ask for targeted extraction/summarization of a specific source.
When debugging WEB mode, run the server with `TUILLAMA_DEBUG_WEB=1` to print per-request traces (tool calls, endpoints, statuses, extracted URLs) to stderr.

You can also set the server endpoint in config with `server_addr = "127.0.0.1:7878"`.
Set `render_emojis = false` in config to suppress emoji graphemes in rendered Markdown when your terminal/font does not handle them well.

Command mode opens from normal mode with `:` and executes commands with Enter. Supported commands include `:attach /path/to/image` to queue an image attachment for the next message, `:web on` to enable web search, and `:set <option> <value>` to change config-backed settings for the current session without writing the config file. For example, use `:set model qwen3.6:27b`, `:set api_url http://localhost:11434/api/chat`, `:set num_ctx 32768`, `:set temperature 0.7`, `:set syntax.enabled false`, `:set colors.stats_accent green`, or `:set preview.preview_fmt pdf`. Bare unknown `:set` keys are treated as Ollama `[options]` entries, so `:set num_ctx 32768` and `:set options.num_ctx 32768` are equivalent.

## Limitations (for now)

* The UI is still minimal and rough around the edges
* Attachment support is still evolving; non-image attachments are stored as metadata but are not yet injected into model context automatically.
* A bunch of other stuff, probably, since the app is so minimal
