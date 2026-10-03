//! Projects on disk. Each project is a file in the projects folder, written
//! after every change; there is no Save button to forget. Settings remember
//! which project was open, and whether the journal is listening.
//!
//! Everything lives under `$XDG_DATA_HOME/compypal` (else
//! `~/.local/share/compypal`): `projects/<stem>.json`, `settings.json` and
//! `journal/`.

use std::path::PathBuf;

use compypal_core::Project;

pub fn data_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("compypal"))
}

pub fn projects_dir() -> Option<PathBuf> {
    Some(data_dir()?.join("projects"))
}

/// Where the always-on journal lives.
pub fn journal_dir() -> Option<PathBuf> {
    Some(data_dir()?.join("journal"))
}

fn project_path(stem: &str) -> Option<PathBuf> {
    Some(projects_dir()?.join(format!("{stem}.json")))
}

/// A file-name-safe version of a project name: "My Song!" -> "my-song".
fn slug(name: &str) -> String {
    let s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if s.is_empty() { "untitled".into() } else { s }
}

/// A stem for `name` that no existing project uses.
pub fn unique_stem(name: &str) -> String {
    let base = slug(name);
    let taken = |s: &str| project_path(s).is_some_and(|p| p.exists());
    if !taken(&base) {
        return base;
    }
    (2..).map(|i| format!("{base}-{i}")).find(|s| !taken(s)).unwrap()
}

pub fn load_project(stem: &str) -> Option<Project> {
    let text = std::fs::read_to_string(project_path(stem)?).ok()?;
    match Project::from_json(&text) {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("project {stem} unreadable: {e}");
            None
        }
    }
}

/// Writes to a temporary file and renames it over the old one, so a crash
/// mid-write never leaves a half-written project.
pub fn save_project(stem: &str, project: &Project) -> std::io::Result<()> {
    let Some(path) = project_path(stem) else { return Ok(()) };
    std::fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, project.to_json().map_err(std::io::Error::other)?)?;
    std::fs::rename(tmp, path)
}

/// Moves a project's file to a new stem (after a rename).
pub fn move_project(from: &str, to: &str) -> std::io::Result<()> {
    match (project_path(from), project_path(to)) {
        (Some(a), Some(b)) if a.exists() => std::fs::rename(a, b),
        _ => Ok(()),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectInfo {
    pub stem: String,
    pub name: String,
    /// Unix seconds of the last save.
    pub modified: f64,
    pub tracks: usize,
    pub notes: usize,
    pub sessions: usize,
}

/// Every project, most recently changed first.
pub fn list_projects() -> Vec<ProjectInfo> {
    let Some(dir) = projects_dir() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<ProjectInfo> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "json") {
                return None;
            }
            let stem = path.file_stem()?.to_string_lossy().into_owned();
            let modified = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0.0, |d| d.as_secs_f64());
            let p = Project::from_json(&std::fs::read_to_string(&path).ok()?).ok()?;
            Some(ProjectInfo {
                stem,
                name: p.name.clone(),
                modified,
                tracks: p.tracks.len(),
                notes: p.tracks.iter().map(|t| t.absolute_notes().len()).sum(),
                sessions: p.sessions.len(),
            })
        })
        .collect();
    out.sort_by(|a, b| b.modified.total_cmp(&a.modified));
    out
}

/// The project to open at launch: the one open last time, else the old
/// single autosave (moved into the projects folder), else the demo.
pub fn open_initial() -> (Project, String) {
    let settings = load_settings();
    if let Some(stem) = settings.current.as_deref()
        && let Some(p) = load_project(stem)
    {
        return (p, stem.to_string());
    }
    let legacy = data_dir().map(|d| d.join("autosave.json"));
    let migrated = legacy
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|t| Project::from_json(&t).ok());
    let (project, stem) = match migrated {
        Some(p) => {
            let stem = unique_stem(&p.name);
            if save_project(&stem, &p).is_ok()
                && let Some(path) = &legacy
            {
                let _ = std::fs::rename(path, path.with_extension("json.migrated"));
            }
            (p, stem)
        }
        None => {
            let p = compypal_core::demo::project();
            (p, unique_stem("demo"))
        }
    };
    update_settings(|s| s.current = Some(stem.clone()));
    (project, stem)
}

/// App settings that outlive a session (not part of any project).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub listening: bool,
    /// The project that was open, by file stem.
    #[serde(default)]
    pub current: Option<String>,
}

fn settings_path() -> Option<PathBuf> {
    Some(data_dir()?.join("settings.json"))
}

pub fn load_settings() -> Settings {
    settings_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Changes settings on disk, keeping whatever else is there.
pub fn update_settings(f: impl FnOnce(&mut Settings)) {
    let mut s = load_settings();
    f(&mut s);
    if let (Some(p), Ok(text)) = (settings_path(), serde_json::to_string_pretty(&s)) {
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        let _ = std::fs::write(p, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slug("My Song!"), "my-song");
        assert_eq!(slug("  Bossa  idea #2 "), "bossa-idea-2");
        assert_eq!(slug("???"), "untitled");
    }
}
