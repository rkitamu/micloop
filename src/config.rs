//! ユーザー設定 (~/.config/micloop/config.json)。CLIとGUIで共有する。

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// キーを押すたびON/OFF
    Toggle,
    /// キーを押している間だけON
    Hold,
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mode::Toggle => write!(f, "toggle"),
            Mode::Hold => write!(f, "hold"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Output {
    /// リアルタイムにループバック再生
    Realtime,
    /// ONの間録音し、OFFにした瞬間にまとめて再生
    Delayed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Modifier {
    Ctrl,
    Shift,
    Alt,
    Super,
}

pub const ALL_MODIFIERS: [Modifier; 4] = [
    Modifier::Ctrl,
    Modifier::Shift,
    Modifier::Alt,
    Modifier::Super,
];

impl std::fmt::Display for Modifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Modifier::Ctrl => write!(f, "Ctrl"),
            Modifier::Shift => write!(f, "Shift"),
            Modifier::Alt => write!(f, "Alt"),
            Modifier::Super => write!(f, "Super"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub key: String,
    pub modifiers: Vec<Modifier>,
    pub mode: Mode,
    pub latency_msec: u32,
    pub output: Output,
    /// delayed時の最大録音秒数。tmpfs (RAM) を食い潰さないための上限
    pub max_record_secs: u32,
    /// 録音履歴ウィンドウを開くホットキー。Noneなら無効
    pub history_key: Option<String>,
    pub history_modifiers: Vec<Modifier>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            key: "KEY_SCROLLLOCK".into(),
            modifiers: vec![Modifier::Ctrl],
            mode: Mode::Toggle,
            latency_msec: 20,
            output: Output::Realtime,
            max_record_secs: 600,
            history_key: None,
            history_modifiers: vec![],
        }
    }
}

/// 表示用のホットキー表記 (例: "Ctrl+Super+M")。
pub fn format_hotkey(modifiers: &[Modifier], key: &str) -> String {
    let mut parts: Vec<String> = modifiers.iter().map(Modifier::to_string).collect();
    parts.push(key.strip_prefix("KEY_").unwrap_or(key).to_string());
    parts.join("+")
}

impl Config {
    pub fn hotkey_label(&self) -> String {
        format_hotkey(&self.modifiers, &self.key)
    }

    pub fn history_hotkey_label(&self) -> Option<String> {
        self.history_key
            .as_deref()
            .map(|key| format_hotkey(&self.history_modifiers, key))
    }
}

pub fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config"));
    base.join("micloop").join("config.json")
}

pub fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").expect("HOME is not set"))
}

pub fn load() -> Config {
    load_from(&config_path())
}

pub fn save(cfg: &Config) -> std::io::Result<()> {
    save_to(cfg, &config_path())
}

fn load_from(path: &std::path::Path) -> Config {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_to(cfg: &Config, path: &std::path::Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut json = serde_json::to_string_pretty(cfg).expect("config serializes");
    json.push('\n');
    fs::write(path, json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = std::env::temp_dir().join("micloop-test-config");
        let path = dir.join("config.json");
        let cfg = Config {
            key: "KEY_F9".into(),
            modifiers: vec![Modifier::Ctrl, Modifier::Super],
            mode: Mode::Hold,
            latency_msec: 5,
            output: Output::Delayed,
            max_record_secs: 30,
            history_key: Some("KEY_F10".into()),
            history_modifiers: vec![Modifier::Ctrl],
        };
        save_to(&cfg, &path).unwrap();
        assert_eq!(load_from(&path), cfg);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_file_gives_defaults() {
        let path = std::path::Path::new("/nonexistent/micloop/config.json");
        assert_eq!(load_from(path), Config::default());
    }

    #[test]
    fn old_config_without_modifiers_gets_default() {
        let cfg: Config = serde_json::from_str(r#"{"key": "KEY_F9"}"#).unwrap();
        assert_eq!(cfg.key, "KEY_F9");
        assert_eq!(cfg.modifiers, vec![Modifier::Ctrl]);
    }

    #[test]
    fn hotkey_label_formats() {
        let cfg = Config {
            key: "KEY_M".into(),
            modifiers: vec![Modifier::Ctrl, Modifier::Super],
            ..Default::default()
        };
        assert_eq!(cfg.hotkey_label(), "Ctrl+Super+M");
    }
}
