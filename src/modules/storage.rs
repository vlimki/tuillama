fn data_root() -> Result<PathBuf> {
    let proj = ProjectDirs::from("dev", "example", "tuillama")
        .ok_or_else(|| anyhow!("unable to resolve data dir"))?;
    Ok(proj.data_dir().to_path_buf())
}

fn chats_dir_path() -> Result<PathBuf> {
    let dir = data_root()?.join("chats");
    fs::create_dir_all(&dir).with_context(|| "create chats dir".to_string())?;
    Ok(dir)
}

fn chat_file_path(id: &str) -> Result<PathBuf> {
    Ok(chats_dir_path()?.join(format!("{}.json", id)))
}

fn list_chats() -> Result<Vec<ChatMeta>> {
    let mut metas = Vec::new();
    for entry in fs::read_dir(chats_dir_path()?)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        if let Some(ext) = entry.path().extension() {
            if ext != "json" {
                continue;
            }
        }
        let s = fs::read_to_string(entry.path());
        if let Ok(s) = s {
            if let Ok(chat) = serde_json::from_str::<Chat>(&s) {
                metas.push(ChatMeta {
                    id: chat.id,
                    title: chat.title,
                    updated_ts: chat.updated_ts,
                });
            }
        }
    }
    metas.sort_by(|a, b| b.updated_ts.cmp(&a.updated_ts));
    Ok(metas)
}

fn load_chat(id: &str) -> Result<Chat> {
    let p = chat_file_path(id)?;
    let s = fs::read_to_string(&p).with_context(|| "read chat".to_string())?;
    let chat: Chat = serde_json::from_str(&s).with_context(|| "parse chat json".to_string())?;
    Ok(chat)
}

fn save_chat(chat: &Chat) -> Result<()> {
    let p = chat_file_path(&chat.id)?;
    let mut f = File::create(&p).with_context(|| "create chat file".to_string())?;
    let s = serde_json::to_string_pretty(chat)?;
    f.write_all(s.as_bytes())?;
    Ok(())
}

fn delete_chat_file(id: &str) -> Result<()> {
    let p = chat_file_path(id)?;
    if p.exists() {
        fs::remove_file(&p).with_context(|| format!("remove {}", p.display()))?;
    }
    Ok(())
}

fn gen_chat_id() -> String {
    format!("{}-{:08x}", now_sec(), random::<u32>())
}

fn derive_title(messages: &[Message]) -> String {
    let first = messages
        .iter()
        .find(|m| matches!(m.role, Role::User))
        .or(messages.first());
    let raw = first.map(|m| m.content.trim()).unwrap_or("");
    let oneline = raw.lines().next().unwrap_or("").trim();
    let mut title = oneline.chars().take(60).collect::<String>();
    if title.is_empty() {
        title = "Untitled chat".to_string();
    }
    title
}

