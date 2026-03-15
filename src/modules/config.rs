#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct SyntaxSection {
    enabled: Option<bool>,
    theme_name: Option<String>,
    custom: Option<toml::value::Table>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Performance {
    stream_fps: Option<u64>,    // throttle redraws while streaming (default 30)
    input_poll_ms: Option<u64>, // terminal input poll period (default 250)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PreviewCfg {
    preview_fmt: Option<String>,   // "html" or "pdf"
    preview_open: Option<String>,  // e.g. "zathura", default xdg-open
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct AppConfig {
    model: Option<String>,
    api_url: Option<String>,
    ollama_api_key: Option<String>,
    server_addr: Option<String>,
    options: Option<toml::value::Table>,
    system_prompt: Option<String>,
    bold_selection: Option<bool>,
    colors: Option<toml::value::Table>,
    syntax: Option<SyntaxSection>,
    syntax_theme: Option<String>, // legacy
    performance: Option<Performance>,
    preview: Option<PreviewCfg>,
}

fn config_path() -> Result<PathBuf> {
    let proj = ProjectDirs::from("dev", "example", "tuillama")
        .ok_or_else(|| anyhow!("unable to resolve config dir"))?;
    Ok(proj.config_dir().join("config.toml"))
}

fn load_config(path: &PathBuf) -> Result<AppConfig> {
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let s = fs::read_to_string(path).with_context(|| "read config".to_string())?;
    let cfg: AppConfig = toml::from_str(&s).with_context(|| "parse TOML".to_string())?;
    Ok(cfg)
}
