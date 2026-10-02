//! Snapshot undo. Projects are small enough that cloning one per edit is
//! cheaper than keeping every operation invertible, and it means agent edits
//! and UI edits undo the same way.

use crate::model::Project;

#[derive(Clone, Debug, Default)]
pub struct History {
    past: Vec<(String, Project)>,
    future: Vec<(String, Project)>,
}

const LIMIT: usize = 200;

impl History {
    /// Records `before` as the state to return to, labelled with what the
    /// edit was. Clears the redo stack.
    pub fn push(&mut self, label: impl Into<String>, before: Project) {
        self.past.push((label.into(), before));
        if self.past.len() > LIMIT {
            self.past.remove(0);
        }
        self.future.clear();
    }

    /// Swaps `current` for the previous state. Returns the undone label.
    pub fn undo(&mut self, current: &mut Project) -> Option<String> {
        let (label, prev) = self.past.pop()?;
        self.future.push((label.clone(), std::mem::replace(current, prev)));
        Some(label)
    }

    pub fn redo(&mut self, current: &mut Project) -> Option<String> {
        let (label, next) = self.future.pop()?;
        self.past.push((label.clone(), std::mem::replace(current, next)));
        Some(label)
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.past.last().map(|(l, _)| l.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.future.last().map(|(l, _)| l.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_redo() {
        let mut h = History::default();
        let mut p = Project::new("a");
        h.push("rename", p.clone());
        p.name = "b".into();
        assert_eq!(h.undo(&mut p).as_deref(), Some("rename"));
        assert_eq!(p.name, "a");
        h.redo(&mut p);
        assert_eq!(p.name, "b");
    }
}
