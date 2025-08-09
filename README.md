# tuillama

Finally, a minimalist yet competent Ollama interface. Fast, Vim keys, beautiful markdown formatting on the terminal, dynamic compilation to HTML for viewing LaTeX.

---

## Requirements

* Rust toolchain
* Ollama
* `pandoc` and a default browser (`xdg-open` on Linux)
* `xclip` for clipboard integration

Optional

* A font with good Unicode/emoji support

---

## Build and run

```bash
cargo build --release

# pull or start a model
ollama pull llama3
# or: ollama run llama3

# run the app
cargo run
```

Environment overrides (take precedence over config):

```bash
export OLLAMA_MODEL=llama3
export OLLAMA_CHAT_URL=http://localhost:11434/api/chat
```

---

## Configuration

Config file path (XDG):

* `${XDG_CONFIG_HOME:-$HOME/.config}/tuillama/config.toml`

Precedence: environment variables > config file > built-ins.

All keys under `[options]` are forwarded to Ollama's `options` field.

---

## No LaTeX on the terminal? No problem

Preview mode renders Markdown + LaTeX to HTML with MathJax. Enter visual mode, select a message, and press `Enter`.

---

## Limitations (for now)

* Models and custom options require manually editing the config file and restarting the app
* No image support (this should be relatively easy to fix)
* A bunch of other stuff, probably, since the app is so minimal
