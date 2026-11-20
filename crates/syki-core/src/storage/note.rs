//! Encoding-aware atomic note I/O (`UTF-8`, `Shift_JIS`, `EUC-JP`) with
//! optimistic concurrency signature checks (`expect_sig`).

use std::path::Path;

use crate::storage::{atomic, LoadedNote, StorageError, TextSignature};

/// Note file reader/writer with lossless Japanese encoding preservation.
pub struct NoteFileStore;

impl NoteFileStore {
    /// Detects encoding (`UTF-8` with/without BOM, strict `Shift_JIS`, strict `EUC-JP`)
    /// and decodes raw bytes to a UTF-8 string + canonical encoding name.
    pub fn decode_bytes(raw: &[u8]) -> (String, String) {
        if raw.is_empty() {
            return (String::new(), "UTF-8".to_string());
        }
        if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
            if let Ok(s) = std::str::from_utf8(&raw[3..]) {
                return (s.to_string(), "UTF-8".to_string());
            }
        }
        if let Ok(s) = std::str::from_utf8(raw) {
            return (s.to_string(), "UTF-8".to_string());
        }
        if let Some(cow) =
            encoding_rs::SHIFT_JIS.decode_without_bom_handling_and_without_replacement(raw)
        {
            return (cow.into_owned(), "Shift_JIS".to_string());
        }
        if let Some(cow) =
            encoding_rs::EUC_JP.decode_without_bom_handling_and_without_replacement(raw)
        {
            return (cow.into_owned(), "EUC-JP".to_string());
        }
        (
            String::from_utf8_lossy(raw).into_owned(),
            "UTF-8".to_string(),
        )
    }

    /// Encodes `content` into raw bytes for the requested encoding name.
    pub fn encode_str(content: &str, encoding: &str) -> Vec<u8> {
        let enc = encoding.trim().to_ascii_lowercase();
        match enc.as_str() {
            "shift_jis" | "shift-jis" | "sjis" | "cp932" => {
                encoding_rs::SHIFT_JIS.encode(content).0.into_owned()
            }
            "euc-jp" | "eucjp" => encoding_rs::EUC_JP.encode(content).0.into_owned(),
            _ => content.as_bytes().to_vec(),
        }
    }

    pub fn read_note(path: &Path) -> Result<LoadedNote, StorageError> {
        let raw = std::fs::read(path)?;
        let sig = TextSignature::from_bytes(&raw);
        let (content, encoding_name) = Self::decode_bytes(&raw);
        Ok(LoadedNote {
            path: path.to_path_buf(),
            content,
            encoding_name,
            sig,
        })
    }

    pub fn write_atomic(
        path: &Path,
        content: &str,
        encoding: &str,
    ) -> Result<TextSignature, StorageError> {
        let bytes = Self::encode_str(content, encoding);
        atomic::write_atomic_bytes(path, &bytes)?;
        Ok(TextSignature::from_bytes(&bytes))
    }

    pub fn write_checked(
        path: &Path,
        content: &str,
        encoding: &str,
        expect_sig: Option<&str>,
    ) -> Result<TextSignature, StorageError> {
        if let Some(expected) = expect_sig {
            if path.exists() {
                let existing = std::fs::read(path)?;
                let current_sig = TextSignature::from_bytes(&existing);
                if !current_sig
                    .sha256_hex
                    .eq_ignore_ascii_case(expected.trim())
                {
                    return Err(StorageError::Conflict { current_sig });
                }
            }
        }
        Self::write_atomic(path, content, encoding)
    }
}

/// Computes the `cyrb53` UTF-16 signature matching `DiskSync.sum(text)` in `frontend/js/disk_sync.js`.
pub fn cyrb53_utf16_sig(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut len: usize = 0;
    let mut h1: u32 = 0xdead_beef;
    let mut h2: u32 = 0x41c6_ce57;
    for ch in normalized.encode_utf16() {
        len += 1;
        h1 = (h1 ^ (ch as u32)).wrapping_mul(2654435761);
        h2 = (h2 ^ (ch as u32)).wrapping_mul(1597334677);
    }
    h1 = (h1 ^ (h1 >> 16)).wrapping_mul(2246822507)
        ^ (h2 ^ (h2 >> 13)).wrapping_mul(3266489909);
    h2 = (h2 ^ (h2 >> 16)).wrapping_mul(2246822507)
        ^ (h1 ^ (h1 >> 13)).wrapping_mul(3266489909);
    let value: u64 = 4294967296u64 * ((2097151u32 & h2) as u64) + (h1 as u64);
    format!("{len:x}-{value:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::fixture_path;
    use tempfile::tempdir;

    #[test]
    fn shift_jis_fixture_decodes_cleanly_and_round_trips() {
        let src = fixture_path("scraps/shift_jis_note.md");
        let original_bytes = std::fs::read(&src).expect("read sjis fixture");
        let loaded = NoteFileStore::read_note(&src).expect("read_note sjis");

        assert_eq!(loaded.encoding_name, "Shift_JIS");
        assert!(!loaded.content.contains('\u{FFFD}'));
        assert!(loaded.content.contains("日本語メモの読み込み互換性確認"));

        let dir = tempdir().expect("tempdir");
        let out_path = dir.path().join("sjis_out.md");
        let sig = NoteFileStore::write_atomic(&out_path, &loaded.content, &loaded.encoding_name)
            .expect("write sjis");
        assert_eq!(std::fs::read(&out_path).expect("re-read"), original_bytes);
        assert_eq!(sig, loaded.sig);
    }

    #[test]
    fn utf8_bom_is_stripped_cleanly() {
        let raw = [b"\xEF\xBB\xBF".as_slice(), "こんにちは".as_bytes()].concat();
        let (text, enc) = NoteFileStore::decode_bytes(&raw);
        assert_eq!(enc, "UTF-8");
        assert_eq!(text, "こんにちは");
    }

    #[test]
    fn write_checked_detects_external_conflict_and_preserves_disk() {
        let dir = tempdir().expect("tempdir");
        let note_path = dir.path().join("note.md");
        let sig1 =
            NoteFileStore::write_atomic(&note_path, "v1\n", "UTF-8").expect("initial write");

        NoteFileStore::write_atomic(&note_path, "external v2\n", "UTF-8")
            .expect("external overwrite");

        let err = NoteFileStore::write_checked(
            &note_path,
            "stale client edit\n",
            "UTF-8",
            Some(&sig1.sha256_hex),
        )
        .expect_err("stale sig must conflict");

        match err {
            StorageError::Conflict { current_sig } => {
                assert_eq!(
                    current_sig,
                    TextSignature::from_bytes(b"external v2\n")
                );
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(&note_path).expect("disk untouched"),
            "external v2\n"
        );

        let expected = TextSignature::from_bytes(b"external v2\n").sha256_hex;
        let sig3 = NoteFileStore::write_checked(
            &note_path,
            "merged v3\n",
            "UTF-8",
            Some(&expected),
        )
        .expect("matching sig succeeds");
        assert_eq!(sig3, TextSignature::from_bytes(b"merged v3\n"));
    }

    #[test]
    fn cyrb53_utf16_sig_matches_disk_sync_js() {
        assert_eq!(
            cyrb53_utf16_sig("hello\r\nworld"),
            cyrb53_utf16_sig("hello\nworld")
        );
        assert_eq!(cyrb53_utf16_sig("hello\n世界"), "8-5be9f81d6a121");
    }
}
