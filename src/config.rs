use config::{Config as ConfigLoader, ConfigError, File};
use directories::ProjectDirs;
use once_cell::sync::Lazy;
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub progress: ProgressConfig,
    #[serde(default)]
    pub copy: CopyConfig,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct CopyConfig {
    #[serde(default = "default_reflink")]
    pub reflink: String,
}

fn default_reflink() -> String {
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
        }
    }
}

pub static CONFIG: Lazy<Config> = Lazy::new(|| Config::new().unwrap_or_else(|_| Config::default()));

impl Config {
    pub fn new() -> Result<Self, ConfigError> {
        let mut s = ConfigLoader::builder();

        // Defaults
        let defaults = Config::default();

        // Set defaults manually (for overrides)
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
            .set_default(
                "copy.reflink",
                defaults.copy.reflink,
            )
            .unwrap();

        // Check config file
        // 1. XDG (~/.config/bcmr)
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

        // 2. Platform fallback
        if let Some(proj_dirs) = ProjectDirs::from("com", "bcmr", "bcmr") {
            let config_dir = proj_dirs.config_dir();
            // Avoid duplicate source
            if !config_dir.ends_with(".config/bcmr") {
                let config_path = config_dir.join("config.toml");
                if config_path.exists() {
                    s = s.add_source(File::from(config_path));
                }
            }
        }

        s.build()?.try_deserialize()
    }
}
