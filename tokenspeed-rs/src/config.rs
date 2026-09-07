use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

pub use crate::collectors::Agent;

/// 跟随模式：手动锁定单个 agent，或显式开启的自动跟随（切换永远由用户触发）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FollowMode {
    #[default]
    Manual,
    Auto,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowPosition {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub schema_version: u32,
    pub selected_agent: Agent,
    #[serde(default)]
    pub follow_mode: FollowMode,
    pub pinned_project: Option<String>,
    pub window_position: Option<WindowPosition>,
    pub skill_hashes: HashMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: 1,
            selected_agent: Agent::Codex,
            follow_mode: FollowMode::Manual,
            pinned_project: None,
            window_position: None,
            skill_hashes: HashMap::new(),
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Io(String),
    InvalidJson(String),
    UnsupportedSchema(u32),
    NonUtf8Path,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => write!(f, "config I/O error: {message}"),
            Self::InvalidJson(message) => write!(f, "invalid config JSON: {message}"),
            Self::UnsupportedSchema(version) => {
                write!(f, "unsupported config schema version: {version}")
            }
            Self::NonUtf8Path => f.write_str("project path is not valid UTF-8"),
        }
    }
}

impl std::error::Error for ConfigError {}

fn io_error(error: std::io::Error) -> ConfigError {
    ConfigError::Io(error.to_string())
}

#[cfg(windows)]
fn replace_file(tmp: &Path, path: &Path) -> std::io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let from = tmp
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let to = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let moved = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(tmp: &Path, path: &Path) -> std::io::Result<()> {
    fs::rename(tmp, path)
}

pub fn config_path() -> Result<PathBuf, ConfigError> {
    #[cfg(windows)]
    let root = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let root = std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/Application Support"));
    #[cfg(not(any(windows, target_os = "macos")))]
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    root.map(|path| path.join("tokenspeed").join("config.json"))
        .ok_or_else(|| ConfigError::Io("cannot resolve per-user config directory".into()))
}

impl Config {
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from(&config_path()?)
    }

    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = fs::read_to_string(path).map_err(io_error)?;
        let config: Self = serde_json::from_str(&data)
            .map_err(|error| ConfigError::InvalidJson(error.to_string()))?;
        if config.schema_version != 1 {
            return Err(ConfigError::UnsupportedSchema(config.schema_version));
        }
        Ok(config)
    }

    pub fn save(&self) -> Result<(), ConfigError> {
        self.save_to(&config_path()?)
    }

    pub fn save_to(&self, path: &Path) -> Result<(), ConfigError> {
        if self.schema_version != 1 {
            return Err(ConfigError::UnsupportedSchema(self.schema_version));
        }
        let parent = path
            .parent()
            .ok_or_else(|| ConfigError::Io("config path has no parent".into()))?;
        fs::create_dir_all(parent).map_err(io_error)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| ConfigError::Io(error.to_string()))?
            .as_nanos();
        let tmp = parent.join(format!(".config.json.{}.{}.tmp", std::process::id(), stamp));
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| ConfigError::InvalidJson(error.to_string()))?;
        let result = (|| {
            let mut file = File::create(&tmp).map_err(io_error)?;
            file.write_all(&bytes).map_err(io_error)?;
            file.sync_all().map_err(io_error)?;
            replace_file(&tmp, path).map_err(io_error)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }
}

pub fn normalize_project(path: &Path) -> Result<String, ConfigError> {
    path.to_str().ok_or(ConfigError::NonUtf8Path)?;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(io_error)?.join(path)
    };
    let normalized = if absolute.exists() {
        fs::canonicalize(absolute).map_err(io_error)?
    } else {
        lexical_normalize(absolute)
    };
    normalized
        .to_str()
        .map(str::to_owned)
        .ok_or(ConfigError::NonUtf8Path)
}

fn lexical_normalize(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() && !path.is_absolute() {
                    normalized.push("..");
                }
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}
