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
