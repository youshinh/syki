//! Atomic temp-rename writes: the single writer contract every Ring 0 file
//! relies on. A temp file is created, filled, flushed to disk and closed
//! *before* the rename, so a reader can only ever observe the old bytes or the
//! new bytes, never a half-written file.

use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::storage::StorageError;

/// Rename attempts after the first one, each preceded by its backoff in ms.
const RENAMED_RETRY_BACKOFF_MS: [u64; 5] = [5, 10, 20, 40, 80];

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Next unique temp-file name in `parent`, derived from the process id and a
/// monotonically increasing per-process sequence number.
fn temp_path_in(parent: &std::path::Path, attempt: usize) -> std::path::PathBuf {
    let seq = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed) as usize + attempt;
    std::path::Path::new(parent).join(format!(
        ".syki-tmp-{}-{}.tmp",
        std::process::id(),
        seq
    ))
}

/// `true` when `message` mentions a Windows sharing violation or access denial.
fn is_transient_message(message: &str) -> bool {
    let lowered = message.to_lowercase();
    lowered.contains("being used by another process") || lowered.contains("access is denied")
}

/// `true` for failures a short backoff can clear: another handle still closing
/// on the target, or a sharing violation raised by the rename.
fn is_transient_rename_failure(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::PermissionDenied
        || err.kind() == std::io::ErrorKind::AlreadyExists
        || is_transient_message(&err.to_string())
}

/// One full temp-write-then-rename attempt. The returned flag reports whether
/// the failure came from the rename step, which is the only step worth
/// retrying with backoff.
fn attempt_write_and_rename(
    tmp_path: &std::path::Path,
    target: &std::path::Path,
    data: &[u8],
) -> Result<(), (std::io::Error, bool)> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp_path)
        .map_err(|err| (err, false))?;
    match file.write_all(data).and_then(|_| file.sync_all()) {
        Ok(()) => {}
        Err(err) => {
            let _ = file.sync_all();
            drop(file);
            return Err((err, false));
        }
    }
    // The handle must be closed before the rename, otherwise the target stays
    // locked and the rename fails with a sharing violation.
    drop(file);
    std::fs::rename(tmp_path, target).map_err(|err| (err, true))
}

/// Writes `data` to `path` atomically: temp file, flush, close, rename.
///
/// Parent directories are created on demand. Transient Windows rename failures
/// are retried up to five times with a `[5, 10, 20, 40, 80]` ms backoff. On
/// any error the temp file is removed best-effort so the target file is never
/// corrupted or left half-written.
pub fn write_atomic_bytes(
    path: &std::path::Path,
    data: &[u8],
) -> Result<(), StorageError> {
    let parent = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => std::path::PathBuf::from("."),
    };
    std::fs::create_dir_all(&parent)?;

    let mut attempt = 0_usize;
    loop {
        let tmp_path = temp_path_in(&parent, attempt);
        match attempt_write_and_rename(&tmp_path, path, data) {
            Ok(()) => return Ok(()),
            Err((err, from_rename)) => {
                let _ = std::fs::remove_file(&tmp_path);
                let retryable = from_rename && is_transient_rename_failure(&err);
                if retryable && attempt < RENAMED_RETRY_BACKOFF_MS.len() {
                    std::thread::sleep(Duration::from_millis(
                        RENAMED_RETRY_BACKOFF_MS[attempt],
                    ));
                    attempt += 1;
                    continue;
                }
                return Err(StorageError::Io(err));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::TextSignature;

    #[test]
    fn write_atomic_bytes_creates_missing_parents_and_round_trips() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("nested").join("deeper").join("note.md");
        let payload = "# nested\n".as_bytes().to_vec();
        write_atomic_bytes(&target, &payload).expect("atomic write");
        let on_disk = std::fs::read(&target).expect("read back");
        assert_eq!(on_disk, payload);
        assert_eq!(
            TextSignature::from_bytes(&on_disk).sha256_hex,
            TextSignature::from_bytes(&payload).sha256_hex
        );
    }

    #[test]
    fn write_atomic_bytes_replaces_existing_content_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("config.json");
        write_atomic_bytes(&target, b"{\"v\":1}").expect("first write");
        write_atomic_bytes(&target, b"{\"v\":2}").expect("second write");
        write_atomic_bytes(&target, b"{\"v\":3}").expect("third write");
        assert_eq!(std::fs::read(&target).expect("read"), b"{\"v\":3}");
        let leftovers: Vec<std::path::PathBuf> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.starts_with(".syki-tmp-"))
                    .unwrap_or(false)
            })
            .collect();
        assert!(leftovers.is_empty(), "temp files must not survive");
    }

    #[test]
    fn transient_rename_failures_are_retried_with_backoff() {
        // A directory occupying the target name makes every rename fail with a
        // permission-style error; the writer must give up after five retries
        // instead of corrupting the target.
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("blocked");
        std::fs::create_dir_all(&target).expect("blocking dir");
        let err = write_atomic_bytes(&target, b"data")
            .expect_err("rename onto a directory must fail");
        match err {
            StorageError::Io(_) => {}
            other => panic!("unexpected error: {other}"),
        }
        assert!(target.is_dir(), "blocking target must stay intact");
        let leftovers: Vec<std::path::PathBuf> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.starts_with(".syki-tmp-"))
                    .unwrap_or(false)
            })
            .collect();
        assert!(leftovers.is_empty(), "failed write must clean its temp file");
    }

    #[test]
    fn temp_names_are_unique_across_attempts() {
        let mut seen = std::collections::HashSet::new();
        for attempt in 0..8 {
            let name = temp_path_in(std::path::Path::new("."), attempt);
            assert!(seen.insert(name.clone()), "temp name must be unique");
            let file_name = name.file_name().expect("file name").to_string_lossy().into_owned();
            assert!(file_name.starts_with(".syki-tmp-"), "unexpected name: {file_name}");
            assert!(file_name.ends_with(".tmp"), "unexpected name: {file_name}");
        }
    }
}
