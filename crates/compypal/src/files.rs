//! Where things live. A song is a file the user saves where they like
//! (`<name>.compypal`, see `compypal_io::project_file`); new ones are
//! offered the projects folder (`COMPYPAL_PROJECTS`, else
//! `~/Music/compypal`).
//!
//! The app's own state lives under `$XDG_DATA_HOME/compypal` (else
//! `~/.local/share/compypal`): `settings.json` (the open song, recent
//! songs, …), `recovery.json` (unsaved changes, written on every edit, so a
//! crash or a kill loses nothing) and `journal/`.

use std::path::{Path, PathBuf};

use compypal_core::Project;
use compypal_io::project_file;

pub fn data_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".local/share")))?;
    Some(base.join("compypal"))
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Where new songs are saved unless the user picks somewhere else.
pub fn projects_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("COMPYPAL_PROJECTS") {
        return PathBuf::from(dir);
    }
    home().map_or_else(|| PathBuf::from("."), |h| h.join("Music").join("compypal"))
}

/// Where the always-on journal lives.
pub fn journal_dir() -> Option<PathBuf> {
    Some(data_dir()?.join("journal"))
}

/// Songs listed under Open Recent, newest first.
const RECENT: usize = 12;

/// Remembers the open song's file (`None`: a new song, not saved yet) and
/// puts it first among the recent ones.
pub fn remember_open(file: Option<&Path>) {
    update_settings(|s| {
        s.open = file.map(Path::to_path_buf);
        if let Some(f) = file {
            s.recent.retain(|r| r != f);
            s.recent.insert(0, f.to_path_buf());
            s.recent.truncate(RECENT);
        }
    });
}

/// The recent songs and the projects folder's, newest first.
pub fn song_files() -> Vec<project_file::Info> {
    project_file::list(&load_settings().recent, Some(&projects_dir()))
}

fn recovery_path() -> Option<PathBuf> {
    Some(data_dir()?.join("recovery.json"))
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Recovery {
    /// The song's file; `None` if it was never saved.
    file: Option<PathBuf>,
    project: Project,
}

/// Keeps the open song's unsaved changes, for the next launch if this one
/// never gets to save them.
pub fn write_recovery(file: Option<&Path>, project: &Project) {
    let Some(path) = recovery_path() else { return };
    let r = Recovery { file: file.map(Path::to_path_buf), project: project.clone() };
    let Ok(text) = serde_json::to_string(&r) else { return };
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, &path)).is_err() {
        eprintln!("couldn't write {}", path.display());
    }
}

pub fn clear_recovery() {
    if let Some(path) = recovery_path() {
        let _ = std::fs::remove_file(path);
    }
}

/// The song to open at launch.
pub struct Opened {
    pub project: Project,
    pub file: Option<PathBuf>,
    /// What its file has: unlike `project` when unsaved changes were
    /// recovered.
    pub saved: Project,
    pub note: Option<String>,
}

/// Unsaved changes left by the last run, else the song open last time,
/// else a new one (the demo, the first time).
pub fn open_initial() -> Opened {
    migrate_old_projects();
    let settings = load_settings();
    let recovered = recovery_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<Recovery>(&t).ok());
    if let Some(r) = recovered {
        let saved = r.file.as_deref().and_then(|f| project_file::load(f).ok()).unwrap_or_else(|| Project::new(""));
        let note = format!("Recovered unsaved changes to {}", r.project.name);
        return Opened { project: r.project, file: r.file, saved, note: Some(note) };
    }
    if let Some(file) = settings.open {
        match project_file::load(&file) {
            Ok(p) => return Opened { saved: p.clone(), project: p, file: Some(file), note: None },
            Err(e) => eprintln!("{e}"),
        }
    }
    // The demo the first time; after that, a blank song.
    let p = if settings.recent.is_empty() {
        compypal_core::demo::project()
    } else {
        let mut p = Project::new("Untitled");
        p.add_track("Piano", 0);
        p
    };
    Opened { saved: p.clone(), project: p, file: None, note: None }
}

/// Songs used to live, one JSON file each, in the data folder, saved on
/// every change. They move to the projects folder once, as song files, and
/// show up under Open Recent; the old folder is kept as
/// `projects.migrated`.
fn migrate_old_projects() {
    let Some(old) = data_dir().map(|d| d.join("projects")) else { return };
    let Ok(entries) = std::fs::read_dir(&old) else { return };
    let dest = projects_dir();
    let current = load_settings().current;
    let mut moved: Vec<(PathBuf, bool)> = Vec::new();
    for path in entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")) {
        let Ok(p) = project_file::load(&path) else { continue };
        let to = project_file::unused_path(&dest, &p.name);
        if let Err(e) = project_file::save(&to, &p) {
            eprintln!("couldn't move {} to {}: {e}", path.display(), to.display());
            return;
        }
        let was_open = path.file_stem().map(|s| s.to_string_lossy().into_owned()) == current;
        eprintln!("moved {} to {}", path.display(), to.display());
        moved.push((to, was_open));
    }
    let _ = std::fs::rename(&old, old.with_extension("migrated"));
    update_settings(|s| {
        for (to, was_open) in moved {
            if was_open {
                s.open = Some(to.clone());
            }
            if !s.recent.contains(&to) {
                s.recent.push(to);
            }
        }
        s.current = None;
    });
}

/// App settings that outlive a session (not part of any song).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub listening: bool,
    /// The song that was open, by file.
    #[serde(default)]
    pub open: Option<PathBuf>,
    #[serde(default)]
    pub recent: Vec<PathBuf>,
    /// Before song files: the open project, by stem in the old projects
    /// folder. Read once, by the move.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    /// The agent the panel runs, by id: `claude`, `gemini`, … or
    /// `custom:<command>`.
    #[serde(default)]
    pub agent: Option<String>,
    /// The audio input to hear, by device id.
    #[serde(default)]
    pub audio_in: Option<String>,
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
