//! Song files: a project saved where the user put it, as `<name>.compypal`
//! (JSON inside).

use std::path::{Path, PathBuf};

use compypal_core::Project;

pub const EXT: &str = "compypal";

pub fn load(path: &Path) -> Result<Project, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    Project::from_json(&text).map_err(|e| format!("{} isn't a compypal song: {e}", path.display()))
}

/// Writes to a temporary file beside it and renames that over the old one,
/// so a crash mid-write never leaves a half-written song.
pub fn save(path: &Path, project: &Project) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, project.to_json().map_err(std::io::Error::other)?)?;
    std::fs::rename(tmp, path)
}

/// Where a path someone typed points: a bare name or relative path is in
/// `dir`, `~/` is home, and the extension is added when it's missing.
pub fn resolve(dir: &Path, input: &str) -> PathBuf {
    let input = input.trim();
    let path = match input.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map_or_else(|| PathBuf::from(input), |h| PathBuf::from(h).join(rest)),
        None => PathBuf::from(input),
    };
    let path = if path.is_absolute() { path } else { dir.join(path) };
    with_ext(path)
}

pub fn with_ext(path: PathBuf) -> PathBuf {
    if path.extension().is_some_and(|x| x == EXT) { path } else {
        let mut s = path.into_os_string();
        s.push(".");
        s.push(EXT);
        PathBuf::from(s)
    }
}

/// A file-name-safe version of a song name: "My Song!" -> "My Song".
pub fn file_stem(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '(' | ')' | '\'') { c } else { ' ' })
        .collect();
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let s = s.trim_matches('.');
    if s.is_empty() { "Untitled".into() } else { s.to_string() }
}

/// `dir/<name>.compypal`, or `<name> 2.compypal` and so on if that's taken.
pub fn unused_path(dir: &Path, name: &str) -> PathBuf {
    let stem = file_stem(name);
    let first = dir.join(format!("{stem}.{EXT}"));
    if !first.exists() {
        return first;
    }
    (2..).map(|i| dir.join(format!("{stem} {i}.{EXT}"))).find(|p| !p.exists()).unwrap()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub path: PathBuf,
    pub name: String,
    /// Unix seconds of the last save.
    pub modified: f64,
    pub tracks: usize,
    pub notes: usize,
    pub sessions: usize,
}

impl Info {
    pub fn read(path: &Path) -> Option<Info> {
        let modified = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0.0, |d| d.as_secs_f64());
        let p = load(path).ok()?;
        Some(Info {
            path: path.to_path_buf(),
            name: p.name.clone(),
            modified,
            tracks: p.tracks.len(),
            notes: p.tracks.iter().map(|t| t.absolute_notes().len()).sum(),
            sessions: p.sessions.len(),
        })
    }

    /// "3 track(s), 120 notes, 2 take(s)".
    pub fn what(&self) -> String {
        format!("{} track(s), {} notes{}", self.tracks, self.notes, if self.sessions > 0 { format!(", {} take(s)", self.sessions) } else { String::new() })
    }
}

/// The songs among `recent` (paths, in any order) and in `dir`, each once,
/// most recently changed first. Files that are gone or unreadable are left
/// out.
pub fn list(recent: &[PathBuf], dir: Option<&Path>) -> Vec<Info> {
    let mut paths: Vec<PathBuf> = recent.to_vec();
    if let Some(entries) = dir.and_then(|d| std::fs::read_dir(d).ok()) {
        paths.extend(entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == EXT)));
    }
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<Info> = paths
        .into_iter()
        .filter(|p| seen.insert(std::fs::canonicalize(p).unwrap_or_else(|_| p.clone())))
        .filter_map(|p| Info::read(&p))
        .collect();
    out.sort_by(|a, b| b.modified.total_cmp(&a.modified));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_paths() {
        assert_eq!(file_stem("My Song!"), "My Song");
        assert_eq!(file_stem("  Bossa  idea #2 "), "Bossa idea 2");
        assert_eq!(file_stem("a/b"), "a b");
        assert_eq!(file_stem("??"), "Untitled");
        assert_eq!(resolve(Path::new("/songs"), "Blues"), PathBuf::from("/songs/Blues.compypal"));
        assert_eq!(resolve(Path::new("/songs"), "/tmp/x.compypal"), PathBuf::from("/tmp/x.compypal"));
        assert_eq!(resolve(Path::new("/songs"), "old/x.json"), PathBuf::from("/songs/old/x.json.compypal"));
    }

    #[test]
    fn saves_loads_and_lists() {
        let dir = std::env::temp_dir().join(format!("compypal-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut p = Project::new("Tune");
        p.add_track("Piano", 0);
        let path = unused_path(&dir, &p.name);
        assert_eq!(path, dir.join("Tune.compypal"));
        save(&path, &p).unwrap();
        assert_eq!(load(&path).unwrap(), p);
        assert_eq!(unused_path(&dir, "Tune"), dir.join("Tune 2.compypal"));
        let listed = list(std::slice::from_ref(&path), Some(&dir));
        assert_eq!(listed.len(), 1, "the recent file and the folder's are the same file");
        assert_eq!(listed[0].name, "Tune");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
