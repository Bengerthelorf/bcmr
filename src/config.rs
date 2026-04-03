use config::{Config as ConfigLoader, ConfigError, File};
use directories::ProjectDirs;
use once_cell::sync::Lazy;
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};

// Set once at startup, read thereafter.
static JSON_MODE: AtomicBool = AtomicBool::new(false);

pub fn set_json_mode(enabled: bool) {
    JSON_MODE.store(enabled, Ordering::Relaxed);
}

pub fn is_json_mode() -> bool {
    JSON_MODE.load(Ordering::Relaxed)
}

use parking_lot::Mutex;
use std::path::PathBuf;

static LOG_FILE: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn set_log_file(path: PathBuf) {
    *LOG_FILE.lock() = Some(path);
}

pub fn log_file() -> Option<PathBuf> {
    LOG_FILE.lock().clone()
}

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub progress: ProgressConfig,
    #[serde(default)]
    pub copy: CopyConfig,
    #[serde(default)]
    pub scp: ScpConfig,
    #[serde(default)]
    pub update_check: UpdateCheck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateCheck {
    #[default]
    Notify,
    Quiet,
    Off,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CopyConfig {
    #[serde(default = "default_reflink")]
    pub reflink: String,
    #[serde(default = "default_sparse")]
    pub sparse: String,
}

impl Default for CopyConfig {
    fn default() -> Self {
        Self {
            reflink: default_reflink(),
            sparse: default_sparse(),
        }
    }
}

fn default_reflink() -> String {
    "auto".to_string()
}

fn default_sparse() -> String {
    "auto".to_string()
}

fn default_parallel_transfers() -> usize {
    4
}

#[derive(Debug, Deserialize, Clone)]
pub struct ScpConfig {
    #[serde(default = "default_parallel_transfers")]
    pub parallel_transfers: usize,
    #[serde(default = "default_compression")]
    pub compression: String,
}

impl Default for ScpConfig {
    fn default() -> Self {
        Self {
            parallel_transfers: default_parallel_transfers(),
            compression: default_compression(),
        }
    }
}

fn default_compression() -> String {
    "auto".to_string()
}

#[derive(Debug, Deserialize, Clone)]
pub struct ProgressConfig {
    pub style: String,
    pub theme: ThemeConfig,
    pub layout: LayoutConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ThemeConfig {
    pub bar_complete_char: String,
    pub bar_incomplete_char: String,
    pub bar_gradient: Vec<String>,
    pub text_color: String,
    pub border_color: String,
    pub title_color: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LayoutConfig {
    pub box_style: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            progress: ProgressConfig {
                style: "fancy".to_string(),
                theme: ThemeConfig {
                    bar_complete_char: "█".to_string(),
                    bar_incomplete_char: "░".to_string(),
                    bar_gradient: vec!["#CABBE9".to_string(), "#7E6EAC".to_string()],
                    text_color: "reset".to_string(),
                    border_color: "#9E8BCA".to_string(),
                    title_color: "#9E8BCA".to_string(),
                },
                layout: LayoutConfig {
                    box_style: "rounded".to_string(),
                },
            },
            copy: CopyConfig::default(),
            scp: ScpConfig::default(),
            update_check: UpdateCheck::default(),
        }
    }
}

pub static CONFIG: Lazy<Config> = Lazy::new(|| Config::new().unwrap_or_else(|_| Config::default()));

impl Config {
    pub fn new() -> Result<Self, ConfigError> {
        let mut s = ConfigLoader::builder();

        let defaults = Config::default();

        s = s
            .set_default("progress.style", defaults.progress.style)
            .unwrap()
            .set_default(
                "progress.theme.bar_complete_char",
                defaults.progress.theme.bar_complete_char,
            )
            .unwrap()
            .set_default(
                "progress.theme.bar_incomplete_char",
                defaults.progress.theme.bar_incomplete_char,
            )
            .unwrap()
            .set_default(
                "progress.theme.bar_gradient",
                defaults.progress.theme.bar_gradient,
            )
            .unwrap()
            .set_default(
                "progress.theme.text_color",
                defaults.progress.theme.text_color,
            )
            .unwrap()
            .set_default(
                "progress.theme.border_color",
                defaults.progress.theme.border_color,
            )
            .unwrap()
            .set_default(
                "progress.theme.title_color",
                defaults.progress.theme.title_color,
            )
            .unwrap()
            .set_default(
                "progress.layout.box_style",
                defaults.progress.layout.box_style,
            )
            .unwrap()
            .set_default("copy.reflink", defaults.copy.reflink)
            .unwrap()
            .set_default("copy.sparse", defaults.copy.sparse)
            .unwrap()
            .set_default(
                "scp.parallel_transfers",
                defaults.scp.parallel_transfers as i64,
            )
            .unwrap()
            .set_default("scp.compression", defaults.scp.compression)
            .unwrap()
            .set_default("update_check", "notify")
            .unwrap();

        if let Some(user_dirs) = directories::UserDirs::new() {
            let config_dir = user_dirs.home_dir().join(".config").join("bcmr");
            let config_path = config_dir.join("config.toml");
            if config_path.exists() {
                s = s.add_source(File::from(config_path));
            }
            let yaml_path = config_dir.join("config.yaml");
            if yaml_path.exists() {
                s = s.add_source(File::from(yaml_path));
            }
        }

        if let Some(proj_dirs) = ProjectDirs::from("com", "bcmr", "bcmr") {
            let config_dir = proj_dirs.config_dir();
            let user_config_dir =
                directories::UserDirs::new().map(|u| u.home_dir().join(".config").join("bcmr"));
            let is_duplicate = user_config_dir
                .as_ref()
                .is_some_and(|u| config_dir == u.as_path());
            if !is_duplicate {
                let config_path = config_dir.join("config.toml");
                if config_path.exists() {
                    s = s.add_source(File::from(config_path));
                }
                let yaml_path = config_dir.join("config.yaml");
                if yaml_path.exists() {
                    s = s.add_source(File::from(yaml_path));
                }
            }
        }

        s.build()?.try_deserialize()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let cfg = Config::default();
        assert_eq!(cfg.progress.style, "fancy");
        assert_eq!(cfg.update_check, UpdateCheck::Notify);
    }

    #[test]
    fn test_default_theme() {
        let cfg = Config::default();
        assert_eq!(cfg.progress.theme.bar_gradient.len(), 2);
        assert_eq!(cfg.progress.layout.box_style, "rounded");
    }

    #[test]
    fn test_config_new_loads_defaults() {
        let cfg = Config::new().unwrap();
        assert!(!cfg.progress.style.is_empty());
    }

    #[test]
    fn test_static_config() {
        assert!(!CONFIG.progress.style.is_empty());
    }
}
