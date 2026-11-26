//! Memory-mapped binary semantic vector index (`SYKISEM1`) + AVX2 SIMD cosine similarity (#07).

use std::fs::File;
use std::path::{Path, PathBuf};

use memmap2::Mmap;
use serde::{Deserialize, Serialize};

use crate::search::{SearchMatch, SearchResult};
use crate::storage::atomic::write_atomic_bytes;
use crate::storage::NoteFileStore;

const MAGIC_MDSV: &[u8; 4] = b"MDSV";
const MAGIC_LEGACY: &[u8; 8] = b"SYKISEM1";
const HEADER_LEN: usize = 16; // 16-byte header for both MDSV and SYKISEM1

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SemanticChunkMeta {
    pub file_path: String,
    pub file_name: String,
    pub line: usize,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SemanticStatusReport {
    pub enabled: bool,
    pub state: String,
    pub files_indexed: usize,
    pub chunks_indexed: usize,
    pub index_bytes: u64,
    pub simd_backend: &'static str,
}

/// Computes cosine similarity using AVX2 on `x86_64` when available, else scalar.
pub fn cosine_similarity_f32(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("avx2") {
            // Safety: runtime-guarded by `is_x86_feature_detected!("avx2")`.
            return unsafe { cosine_similarity_avx2(a, b) };
        }
    }
    cosine_similarity_scalar(a, b)
}

pub fn cosine_similarity_scalar(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (&x, &y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom <= 1e-12 {
        0.0
    } else {
        dot / denom
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn cosine_similarity_avx2(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let n = a.len();
    let mut i = 0usize;
    let mut acc_dot = _mm256_setzero_ps();
    let mut acc_na = _mm256_setzero_ps();
    let mut acc_nb = _mm256_setzero_ps();

    while i + 8 <= n {
        let va = _mm256_loadu_ps(a.as_ptr().add(i));
        let vb = _mm256_loadu_ps(b.as_ptr().add(i));
        acc_dot = _mm256_add_ps(acc_dot, _mm256_mul_ps(va, vb));
        acc_na = _mm256_add_ps(acc_na, _mm256_mul_ps(va, va));
        acc_nb = _mm256_add_ps(acc_nb, _mm256_mul_ps(vb, vb));
        i += 8;
    }

    let mut buf_dot = [0.0f32; 8];
    let mut buf_na = [0.0f32; 8];
    let mut buf_nb = [0.0f32; 8];
    _mm256_storeu_ps(buf_dot.as_mut_ptr(), acc_dot);
    _mm256_storeu_ps(buf_na.as_mut_ptr(), acc_na);
    _mm256_storeu_ps(buf_nb.as_mut_ptr(), acc_nb);

    let mut dot: f32 = buf_dot.iter().sum();
    let mut na: f32 = buf_na.iter().sum();
    let mut nb: f32 = buf_nb.iter().sum();

    while i < n {
        let x = *a.get_unchecked(i);
        let y = *b.get_unchecked(i);
        dot += x * y;
        na += x * x;
        nb += y * y;
        i += 1;
    }

    let denom = na.sqrt() * nb.sqrt();
    if denom <= 1e-12 {
        0.0
    } else {
        dot / denom
    }
}

pub fn simd_backend_name() -> &'static str {
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("avx2") {
            return "avx2";
        }
    }
    "scalar"
}

/// Memory-mapped binary vector index paired with chunk metadata JSON.
pub struct MmapVectorIndex {
    mmap: Mmap,
    dim: usize,
    count: usize,
    chunks: Vec<SemanticChunkMeta>,
}

impl MmapVectorIndex {
    pub fn index_paths(index_dir: &Path) -> (PathBuf, PathBuf) {
        let jsonl = index_dir.join("chunks.jsonl");
        let meta = if jsonl.exists() {
            jsonl
        } else {
            index_dir.join("chunks.json")
        };
        (index_dir.join("vectors.bin"), meta)
    }

    /// Writes `chunks` and `vectors` (`count * dim` f32s) atomically in Go v1 (`MDSV` + `chunks.jsonl`) format.
    pub fn write_atomic(
        index_dir: &Path,
        dim: usize,
        chunks: &[SemanticChunkMeta],
        vectors: &[f32],
    ) -> Result<(), String> {
        if dim == 0 || vectors.len() != chunks.len() * dim {
            return Err("vector matrix dimension mismatch".to_string());
        }
        std::fs::create_dir_all(index_dir).map_err(|e| e.to_string())?;
        let vec_path = index_dir.join("vectors.bin");
        let meta_path = index_dir.join("chunks.jsonl");

        let mut bin = Vec::with_capacity(HEADER_LEN + vectors.len() * 4);
        bin.extend_from_slice(MAGIC_MDSV);
        bin.extend_from_slice(&1u32.to_le_bytes()); // storeVersion = 1
        bin.extend_from_slice(&(dim as u32).to_le_bytes());
        bin.extend_from_slice(&0u32.to_le_bytes()); // reserved
        for v in vectors {
            bin.extend_from_slice(&v.to_le_bytes());
        }
        write_atomic_bytes(&vec_path, &bin).map_err(|e| e.to_string())?;

        let mut jsonl = String::new();
        for (i, c) in chunks.iter().enumerate() {
            let row = serde_json::json!({
                "rel": c.file_name,
                "path": c.file_path,
                "line": c.line,
                "text": c.text,
                "v": i,
            });
            jsonl.push_str(&row.to_string());
            jsonl.push('\n');
        }
        write_atomic_bytes(&meta_path, jsonl.as_bytes()).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Opens an existing index (`MDSV` + `chunks.jsonl` or legacy `SYKISEM1` + `chunks.json`) using OS `mmap`.
    pub fn open_mmap(index_dir: &Path) -> Result<Self, String> {
        let (vec_path, meta_path) = Self::index_paths(index_dir);
        let file = File::open(&vec_path).map_err(|e| format!("open vectors.bin: {e}"))?;
        let mmap = unsafe { Mmap::map(&file).map_err(|e| format!("mmap vectors.bin: {e}"))? };
        if mmap.len() < HEADER_LEN {
            return Err("truncated vectors.bin header".to_string());
        }

        let (dim, count_hint) = if &mmap[..4] == MAGIC_MDSV {
            let dim = u32::from_le_bytes(mmap[8..12].try_into().unwrap()) as usize;
            if dim == 0 {
                return Err("invalid MDSV dim=0".to_string());
            }
            let payload_bytes = mmap.len() - HEADER_LEN;
            let count = payload_bytes / (dim * 4);
            (dim, Some(count))
        } else if &mmap[..8] == MAGIC_LEGACY {
            let dim = u32::from_le_bytes(mmap[8..12].try_into().unwrap()) as usize;
            let count = u32::from_le_bytes(mmap[12..16].try_into().unwrap()) as usize;
            let expected_len = HEADER_LEN + dim.saturating_mul(count).saturating_mul(4);
            if mmap.len() < expected_len {
                return Err("truncated vectors.bin".to_string());
            }
            (dim, Some(count))
        } else {
            return Err("invalid vectors.bin header".to_string());
        };

        let chunks: Vec<SemanticChunkMeta> = if meta_path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            let raw = std::fs::read_to_string(&meta_path).map_err(|e| format!("read chunks.jsonl: {e}"))?;
            let mut out = Vec::new();
            for line in raw.lines() {
                let t = line.trim();
                if t.is_empty() {
                    continue;
                }
                let val: serde_json::Value = serde_json::from_str(t).map_err(|e| format!("parse chunks.jsonl: {e}"))?;
                let rel = val.get("rel").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let file_path = val.get("path").and_then(|v| v.as_str()).unwrap_or(&rel).to_string();
                let line_num = val.get("line").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
                let text = val.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
                out.push(SemanticChunkMeta {
                    file_path,
                    file_name: rel,
                    line: line_num,
                    text,
                });
            }
            out
        } else {
            let meta_raw = std::fs::read(&meta_path).map_err(|e| format!("read chunks.json: {e}"))?;
            serde_json::from_slice(&meta_raw).map_err(|e| format!("parse chunks.json: {e}"))?
        };

        let count = count_hint.unwrap_or(chunks.len()).min(chunks.len());
        Ok(Self {
            mmap,
            dim,
            count,
            chunks,
        })
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn vector_at(&self, idx: usize) -> Vec<f32> {
        let start = HEADER_LEN + idx * self.dim * 4;
        let slice = &self.mmap[start..start + self.dim * 4];
        slice
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }

    /// Ranks all chunks against `query_vec` via SIMD cosine similarity and returns top `limit`.
    pub fn search(&self, query_vec: &[f32], limit: usize) -> Vec<SearchResult> {
        if query_vec.len() != self.dim || self.count == 0 {
            return Vec::new();
        }
        let mut scored: Vec<(usize, f32)> = (0..self.count)
            .map(|i| {
                let v = self.vector_at(i);
                (i, cosine_similarity_f32(query_vec, &v))
            })
            .collect();
        scored.retain(|&(_, s)| s > 0.001);
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        scored
            .into_iter()
            .take(limit.max(1))
            .map(|(idx, score)| {
                let meta = &self.chunks[idx];
                SearchResult {
                    file_path: meta.file_path.clone(),
                    file_name: meta.file_name.clone(),
                    matches: vec![SearchMatch {
                        line: meta.line,
                        col: 1,
                        text: meta.text.clone(),
                        context: Vec::new(),
                        score: Some(score as f64),
                    }],
                }
            })
            .collect()
    }
}

/// Deterministic lightweight feature-hash embedding (64-dim) for local offline fallback & indexing.
/// Hashes both whitespace-delimited tokens and character 2-grams/3-grams so CJK/Japanese queries match.
pub fn embed_local_hash(text: &str, dim: usize) -> Vec<f32> {
    let mut vec = vec![0.0f32; dim];
    let hash_bytes = |bytes: &[u8]| -> usize {
        let mut h = 2166136261u32;
        for &b in bytes {
            h ^= b as u32;
            h = h.wrapping_mul(16777619);
        }
        (h as usize) % dim
    };
    for token in text.split_whitespace() {
        let lower = token.to_ascii_lowercase();
        vec[hash_bytes(lower.as_bytes())] += 1.0;
        let chars: Vec<char> = lower.chars().collect();
        for w in 2..=3 {
            if chars.len() >= w {
                for win in chars.windows(w) {
                    let s: String = win.iter().collect();
                    vec[hash_bytes(s.as_bytes())] += 0.7;
                }
            }
        }
    }
    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-12 {
        for x in &mut vec {
            *x /= norm;
        }
    }
    vec
}

fn collect_semindex_md(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.starts_with('.') && name != "node_modules" {
                collect_semindex_md(&p, out);
            }
        } else if p.is_file()
            && p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("md"))
                .unwrap_or(false)
        {
            out.push(p);
        }
    }
}

/// Indexes all `.md` files in `scrap_dir` (recursively) into `index_dir` using `dim`-dimensional local embeddings.
pub fn build_local_semantic_index(scrap_dir: &Path, index_dir: &Path) -> Result<(usize, usize), String> {
    let mut files = Vec::new();
    collect_semindex_md(scrap_dir, &mut files);
    files.sort();

    const DIM: usize = 64;
    let mut chunks = Vec::new();
    let mut vectors = Vec::new();
    let mut file_count = 0usize;

    for path in files {
        let Ok(loaded) = NoteFileStore::read_note(&path) else {
            continue;
        };
        file_count += 1;
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        for (idx, line) in loaded.content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed == "---" {
                continue;
            }
            chunks.push(SemanticChunkMeta {
                file_path: path.to_string_lossy().into_owned(),
                file_name: file_name.clone(),
                line: idx + 1,
                text: trimmed.chars().take(200).collect(),
            });
            vectors.extend_from_slice(&embed_local_hash(trimmed, DIM));
        }
    }

    if !chunks.is_empty() {
        MmapVectorIndex::write_atomic(index_dir, DIM, &chunks, &vectors)?;
    }
    Ok((file_count, chunks.len()))
}

pub fn semantic_status(index_dir: &Path, enabled: bool) -> SemanticStatusReport {
    let (vec_path, _) = MmapVectorIndex::index_paths(index_dir);
    let index_bytes = std::fs::metadata(&vec_path).map(|m| m.len()).unwrap_or(0);
    match MmapVectorIndex::open_mmap(index_dir) {
        Ok(idx) => {
            let mut unique_files = std::collections::BTreeSet::new();
            for c in &idx.chunks {
                unique_files.insert(&c.file_path);
            }
            SemanticStatusReport {
                enabled,
                state: "ready".to_string(),
                files_indexed: unique_files.len(),
                chunks_indexed: idx.len(),
                index_bytes,
                simd_backend: simd_backend_name(),
            }
        }
        Err(_) => SemanticStatusReport {
            enabled,
            state: if enabled { "empty" } else { "disabled" }.to_string(),
            files_indexed: 0,
            chunks_indexed: 0,
            index_bytes,
            simd_backend: simd_backend_name(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("syki-sem-{label}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn avx2_and_scalar_cosine_similarity_agree_on_non_aligned_dims() {
        let a: Vec<f32> = (0..773).map(|i| ((i * 7) % 19) as f32 * 0.1).collect();
        let b: Vec<f32> = (0..773).map(|i| ((i * 11) % 23) as f32 * 0.1).collect();
        let simd = cosine_similarity_f32(&a, &b);
        let scalar = cosine_similarity_scalar(&a, &b);
        assert!((simd - scalar).abs() < 1e-5, "simd={simd}, scalar={scalar}");
    }

    #[test]
    fn mmap_vector_index_write_open_and_search_roundtrip() {
        let dir = temp_dir("mmap");
        let scrap_dir = dir.join("scraps");
        let idx_dir = dir.join("semindex");
        std::fs::create_dir_all(&scrap_dir).unwrap();
        std::fs::write(
            scrap_dir.join("2026-11-21.md"),
            "Rust memory mapped vector search\nCoffee and croissants\n",
        )
        .unwrap();

        let (files, chunks) = build_local_semantic_index(&scrap_dir, &idx_dir).unwrap();
        assert_eq!(files, 1);
        assert_eq!(chunks, 2);

        let idx = MmapVectorIndex::open_mmap(&idx_dir).unwrap();
        let q = embed_local_hash("vector search", idx.dim());
        let hits = idx.search(&q, 1);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].matches[0].text.contains("vector search"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn opens_and_searches_go_v1_semindex_fixture() {
        let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("evals")
            .join("fixtures")
            .join("semindex");
        let idx = MmapVectorIndex::open_mmap(&fixture_dir).expect("open Go v1 MDSV fixture");
        assert_eq!(idx.dim(), 4);
        assert_eq!(idx.len(), 3);
        let hits = idx.search(&[1.0, 0.0, 0.0, 0.0], 1);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file_name, "2026-09-17.md");
        assert!(hits[0].matches[0].text.contains("Batch upload"));
    }
}
