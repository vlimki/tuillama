use std::{
    collections::HashMap,
    fs::{self, File},
    io::Write,
    path::PathBuf,
    str::FromStr,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Local, TimeZone, Utc};
use crossterm::{
    cursor::SetCursorStyle,
    event::{self, Event as CEvent, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use directories::ProjectDirs;
use rand::random;
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};
use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

// syntect for code highlighting
use syntect::{
    easy::HighlightLines,
    highlighting::{
        Color as SynColor, FontStyle, ScopeSelectors, StyleModifier, Theme as SynTheme, ThemeItem,
        ThemeSet,
    },
    parsing::SyntaxSet,
};

include!("modules/data_models.rs");
include!("modules/protocol.rs");
include!("modules/theme.rs");
include!("modules/config.rs");
include!("modules/storage.rs");
include!("modules/app_state.rs");
include!("modules/events.rs");
include!("modules/ollama_streaming.rs");
include!("modules/sanitization.rs");
include!("modules/clipboard.rs");
include!("modules/markdown.rs");
include!("modules/syntect_theme.rs");
include!("modules/cache.rs");
include!("modules/ui.rs");
include!("modules/runtime.rs");
