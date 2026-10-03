//! The project is written to disk after every change and read back on
//! launch. There is no Save button to forget.

use std::path::PathBuf;

use compypal_core::Project;

/// `$XDG_DATA_HOME/compypal/autosave.json`, else `~/.local/share/...`.
pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("compypal").join("autosave.json"))
}

pub fn load() -> Option<Project> {
    let text = std::fs::read_to_string(path()?).ok()?;
    match Project::from_json(&text) {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("autosave unreadable, starting fresh: {e}");
            None
        }
    }
}

/// Writes to a temporary file and renames it over the old one, so a crash
/// mid-write never leaves a half-written project.
pub fn save(project: &Project) -> std::io::Result<()> {
    let Some(path) = path() else { return Ok(()) };
    std::fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, project.to_json().map_err(std::io::Error::other)?)?;
    std::fs::rename(tmp, path)
}

/// App settings that outlive a session (not part of any project).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub listening: bool,
}

fn settings_path() -> Option<PathBuf> {
    Some(path()?.with_file_name("settings.json"))
}

pub fn load_settings() -> Settings {
    settings_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_settings(s: &Settings) {
    if let (Some(p), Ok(text)) = (settings_path(), serde_json::to_string_pretty(s)) {
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        let _ = std::fs::write(p, text);
    }
}

/// Where the always-on journal lives.
pub fn journal_dir() -> Option<PathBuf> {
    Some(path()?.with_file_name("journal"))
}
