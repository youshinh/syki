//! Memory-mapped binary semantic vector index (`SYKISEM1`) + AVX2 SIMD cosine similarity (#07).

use std::fs::File;
use std::path::{Path, PathBuf};

use memmap2::Mmap;
use serde::{Deserialize, Serialize};

use crate::search::{SearchMatch, SearchResult};
use crate::storage::atomic::write_atomic_bytes;
use crate::storage::NoteFileStore;

const MAGIC: &[u8; 8] = b"SYKISEM1";
const HEADER_LEN: usize = 16; // 8-byte magic + u32 dim + u32 count

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
        (index_dir.join("vectors.bin"), index_dir.join("chunks.json"))
    }

    /// Writes `chunks` and `vectors` (`count * dim` f32s) atomically to `index_dir`.
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
        let (vec_path, meta_path) = Self::index_paths(index_dir);

        let mut bin = Vec::with_capacity(HEADER_LEN + vectors.len() * 4);
        bin.extend_from_slice(MAGIC);
        bin.extend_from_slice(&(dim as u32).to_le_bytes());
        bin.extend_from_slice(&(chunks.len() as u32).to_le_bytes());
        for v in vectors {
            bin.extend_from_slice(&v.to_le_bytes());
        }
        write_atomic_bytes(&vec_path, &bin).map_err(|e| e.to_string())?;

        let meta_bytes = serde_json::to_vec(chunks).map_err(|e| e.to_string())?;
        write_atomic_bytes(&meta_path, &meta_bytes).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Opens an existing index using OS `mmap`.
    pub fn open_mmap(index_dir: &Path) -> Result<Self, String> {
        let (vec_path, meta_path) = Self::index_paths(index_dir);
        let file = File::open(&vec_path).map_err(|e| format!("open vectors.bin: {e}"))?;
        let mmap = unsafe { Mmap::map(&file).map_err(|e| format!("mmap vectors.bin: {e}"))? };
        if mmap.len() < HEADER_LEN || &mmap[..8] != MAGIC {
            return Err("invalid vectors.bin header".to_string());
        }
        let dim = u32::from_le_bytes(mmap[8..12].try_into().unwrap()) as usize;
        let count = u32::from_le_bytes(mmap[12..16].try_into().unwrap()) as usize;
        let expected_len = HEADER_LEN + dim.saturating_mul(count).saturating_mul(4);
        if mmap.len() < expected_len {
            return Err("truncated vectors.bin".to_string());
        }

        let meta_raw = std::fs::read(&meta_path).map_err(|e| format!("read chunks.json: {e}"))?;
        let chunks: Vec<SemanticChunkMeta> =
            serde_json::from_slice(&meta_raw).map_err(|e| format!("parse chunks.json: {e}"))?;
        if chunks.len() != count {
            return Err("chunks.json count mismatch".to_string());
        }
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
pub fn embed_local_hash(text: &str, dim: usize) -> Vec<f32> {
    let mut vec = vec![0.0f32; dim];
    for token in text.split_whitespace() {
        let mut h = 2166136261u32;
        for b in token.to_ascii_lowercase().bytes() {
            h ^= b as u32;
            h = h.wrapping_mul(16777619);
        }
        vec[(h as usize) % dim] += 1.0;
    }
    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-12 {
        for x in &mut vec {
            *x /= norm;
        }
    }
    vec
}

/// Indexes all `.md` files in `scrap_dir` into `index_dir` using `dim`-dimensional local embeddings.
pub fn build_local_semantic_index(scrap_dir: &Path, index_dir: &Path) -> Result<(usize, usize), String> {
    let Ok(rd) = std::fs::read_dir(scrap_dir) else {
        return Ok((0, 0));
    };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("md"))
                    .unwrap_or(false)
        })
        .collect();
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
}
