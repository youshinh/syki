//! Single active-file watcher resilient to both `In-Place Clear` truncation
//! and atomic temp-rename updates on Windows.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::storage::StorageError;

#[derive(Default)]
struct WatchState {
    watched_dir: Option<PathBuf>,
    target_path: Option<PathBuf>,
    target_name_lower: Option<String>,
}

/// Watches the active note's parent directory and notifies on matching file changes.
pub struct ActiveFileWatcher {
    watcher: Mutex<RecommendedWatcher>,
    state: Arc<Mutex<WatchState>>,
}

impl ActiveFileWatcher {
    pub fn start(on_change: Arc<dyn Fn(PathBuf) + Send + Sync>) -> Result<Self, StorageError> {
        let state = Arc::new(Mutex::new(WatchState::default()));
        let state_cb = Arc::clone(&state);

        let watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                let Ok(event) = res else {
                    return;
                };
                if !matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Any
                ) {
                    return;
                }

                let target = {
                    let guard = state_cb.lock().expect("watch state mutex");
                    match (guard.target_path.as_ref(), guard.target_name_lower.as_ref()) {
                        (Some(path), Some(name)) => {
                            let matched = event.paths.iter().any(|p| {
                                p.file_name()
                                    .and_then(|n| n.to_str())
                                    .map(|n| n.to_ascii_lowercase() == *name)
                                    .unwrap_or(false)
                            });
                            if matched {
                                Some(path.clone())
                            } else {
                                None
                            }
                        }
                        _ => None,
                    }
                };

                if let Some(changed_path) = target {
                    on_change(changed_path);
                }
            })
            .map_err(|e| StorageError::Watch(e.to_string()))?;

        Ok(Self {
            watcher: Mutex::new(watcher),
            state,
        })
    }

    pub fn watch(&self, path: &Path) -> Result<(), StorageError> {
        let parent_raw = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = std::fs::canonicalize(parent_raw).unwrap_or_else(|_| parent_raw.to_path_buf());
        let file_name_lower = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| StorageError::Watch("target path has no file name".into()))?
            .to_ascii_lowercase();

        let mut watcher = self.watcher.lock().expect("watcher mutex");
        let mut state = self.state.lock().expect("watch state mutex");

        if let Some(prev_dir) = state.watched_dir.take() {
            if prev_dir != parent {
                let _ = watcher.unwatch(&prev_dir);
            } else {
                state.watched_dir = Some(prev_dir);
            }
        }

        if state.watched_dir.as_ref() != Some(&parent) {
            watcher
                .watch(&parent, RecursiveMode::NonRecursive)
                .map_err(|e| StorageError::Watch(e.to_string()))?;
            state.watched_dir = Some(parent);
        }

        state.target_path = Some(path.to_path_buf());
        state.target_name_lower = Some(file_name_lower);
        Ok(())
    }

    pub fn unwatch(&self) -> Result<(), StorageError> {
        let mut watcher = self.watcher.lock().expect("watcher mutex");
        let mut state = self.state.lock().expect("watch state mutex");
        if let Some(prev_dir) = state.watched_dir.take() {
            let _ = watcher.unwatch(&prev_dir);
        }
        state.target_path = None;
        state.target_name_lower = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::NoteFileStore;
    use std::io::Write as _;
    use std::sync::mpsc;
    use std::time::Duration;
    use tempfile::tempdir;

    #[test]
    fn watcher_emits_within_500ms_on_in_place_clear_and_atomic_write_and_stops_on_unwatch() {
        let dir = tempdir().expect("tempdir");
        let note_path = dir.path().join("2026-11-21.md");
        NoteFileStore::write_atomic(&note_path, "initial body\n", "UTF-8")
            .expect("seed watched file");

        let (tx, rx) = mpsc::channel();
        let watcher = ActiveFileWatcher::start(Arc::new(move |p| {
            let _ = tx.send(p);
        }))
        .expect("start watcher");
        watcher.watch(&note_path).expect("watch file");

        // Drain any initial setup notifications before triggering In-Place Clear.
        while rx.try_recv().is_ok() {}

        {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&note_path)
                .expect("in-place truncate");
            f.write_all(b"").expect("clear");
            f.sync_all().expect("sync clear");
        }

        let got = rx
            .recv_timeout(Duration::from_millis(500))
            .expect("in-place clear event within 500ms");
        assert_eq!(got, note_path);

        while rx.try_recv().is_ok() {}
        NoteFileStore::write_atomic(&note_path, "atomic update\n", "UTF-8")
            .expect("atomic overwrite");
        let got_atomic = rx
            .recv_timeout(Duration::from_millis(500))
            .expect("atomic rename event within 500ms");
        assert_eq!(got_atomic, note_path);

        watcher.unwatch().expect("unwatch");
        while rx.try_recv().is_ok() {}
        NoteFileStore::write_atomic(&note_path, "after unwatch\n", "UTF-8")
            .expect("write after unwatch");
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "no events should arrive after unwatch"
        );
    }
}
