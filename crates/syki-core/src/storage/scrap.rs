//! Daily Scrap journal paths (`YYYY-MM-DD.md`) and append-only entries.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Local, NaiveDate};

use crate::storage::StorageError;

static SCRAP_APPEND_LOCK: Mutex<()> = Mutex::new(());

/// Daily Scrap storage and `~` directory expansion.
pub struct ScrapStore;

impl ScrapStore {
    pub fn resolve_scrap_dir(dir: &Path) -> PathBuf {
        let raw = dir.to_string_lossy();
        let trimmed = raw.trim();
        let home = home_dir_from_env();

        if trimmed.is_empty() {
            return home.join("Documents").join("syki-sok").join("scraps");
        }
        if trimmed == "~" {
            return home;
        }
        if let Some(rest) = trimmed
            .strip_prefix("~/")
            .or_else(|| trimmed.strip_prefix("~\\"))
        {
            let mut out = home;
            for part in rest.split(['/', '\\']).filter(|s| !s.is_empty()) {
                out.push(part);
            }
            return out;
        }
        PathBuf::from(trimmed)
    }

    pub fn daily_path(scrap_dir: &Path, date: NaiveDate) -> PathBuf {
        Self::resolve_scrap_dir(scrap_dir).join(format!("{}.md", date.format("%Y-%m-%d")))
    }

    pub fn format_scrap_entry(content: &str, command: &str, now: DateTime<Local>) -> String {
        let heading = match command.trim() {
            "" => "CLI Pipe",
            other => other,
        };
        let body = content.trim_end_matches(['\r', '\n']);
        format!(
            "---\n## [{}] {}\n```text\n{}\n```\n",
            now.format("%H:%M:%S"),
            heading,
            body
        )
    }

    pub fn append_entry(
        scrap_dir: &Path,
        entry: &str,
        now: DateTime<Local>,
    ) -> Result<PathBuf, StorageError> {
        let _guard = SCRAP_APPEND_LOCK.lock().expect("scrap append mutex");
        let resolved_dir = Self::resolve_scrap_dir(scrap_dir);
        std::fs::create_dir_all(&resolved_dir)?;

        let target = Self::daily_path(&resolved_dir, now.date_naive());
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&target)?;

        if file.metadata()?.len() > 0 {
            file.write_all(b"\n")?;
        }
        file.write_all(entry.as_bytes())?;
        file.sync_all()?;
        Ok(target)
    }
}

fn home_dir_from_env() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn daily_path_expands_tilde_and_formats_date() {
        let date = NaiveDate::from_ymd_opt(2026, 9, 17).expect("valid date");
        let path = ScrapStore::daily_path(Path::new("~/Documents/syki-sok/scraps"), date);
        assert!(path.ends_with(
            Path::new("Documents")
                .join("syki-sok")
                .join("scraps")
                .join("2026-09-17.md")
        ));
    }

    #[test]
    fn append_entry_creates_daily_file_and_separates_entries() {
        let dir = tempdir().expect("tempdir");
        let now = Local::now();
        let e1 = ScrapStore::format_scrap_entry("first line\n", "echo", now);
        let e2 = ScrapStore::format_scrap_entry("second line", "", now);

        let p1 = ScrapStore::append_entry(dir.path(), &e1, now).expect("append 1");
        let p2 = ScrapStore::append_entry(dir.path(), &e2, now).expect("append 2");
        assert_eq!(p1, p2);

        let body = std::fs::read_to_string(&p1).expect("read daily scrap");
        assert!(body.contains("## ["));
        assert!(body.contains("] echo\n```text\nfirst line\n```"));
        assert!(body.contains("\n\n---\n"));
        assert!(body.contains("] CLI Pipe\n```text\nsecond line\n```"));
    }
}
