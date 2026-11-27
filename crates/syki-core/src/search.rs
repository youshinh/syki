//! Parallel daily-scrap full-text search and tag/date filter extraction (#07).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::storage::{NoteFileStore, ScrapStore};

/// Adaptive TTL applied only when directory enumeration is slow (e.g. Google Drive / network mount).
const SLOW_SCAN_THRESHOLD: Duration = Duration::from_millis(25);
const SLOW_SCAN_TTL: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
struct CachedFile {
    path: PathBuf,
    file_name: String,
    stem: String,
    mtime_ns: u128,
    len: u64,
    content: String,
    content_lower: String,
    tags_lower: BTreeSet<String>,
    tag_counts: Vec<(String, usize)>,
}

#[derive(Debug, Clone)]
struct DirCache {
    last_scan: Instant,
    slow_drive: bool,
    root_mtime_ns: u128,
    entries: Vec<Arc<CachedFile>>,
}

static SEARCH_CACHE: OnceLock<RwLock<HashMap<PathBuf, DirCache>>> = OnceLock::new();

fn search_cache() -> &'static RwLock<HashMap<PathBuf, DirCache>> {
    SEARCH_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Invalidates the cached directory scan TTL for `scrap_dir` so the next search picks up writes immediately.
pub fn invalidate_search_cache(scrap_dir: &Path) {
    let resolved = ScrapStore::resolve_scrap_dir(scrap_dir);
    if let Ok(mut map) = search_cache().write() {
        if let Some(dc) = map.get_mut(&resolved) {
            dc.slow_drive = false;
            dc.root_mtime_ns = 0;
        }
    }
}

fn dir_mtime_ns(dir: &Path) -> u128 {
    std::fs::metadata(dir)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn meta_mtime_len(meta: &std::fs::Metadata) -> (u128, u64) {
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    (mtime_ns, meta.len())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SearchMatch {
    pub line: usize,
    pub col: usize,
    pub text: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub context: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub file_path: String,
    pub file_name: String,
    pub matches: Vec<SearchMatch>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrapFilter {
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: String,
}

impl ScrapFilter {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed == "null" {
            return Ok(Self::default());
        }
        serde_json::from_str::<Self>(trimmed).map_err(|e| format!("invalid filter JSON: {e}"))
    }

    pub fn is_empty(&self) -> bool {
        self.tags.is_empty() && self.from.trim().is_empty() && self.to.trim().is_empty()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TagCount {
    pub tag: String,
    pub files: usize,
    pub entries: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScrapFilterOptions {
    pub tags: Vec<TagCount>,
    pub files: usize,
    pub undated: usize,
}

fn collect_md_meta_recursive(dir: &Path, out: &mut Vec<(PathBuf, u128, u64)>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        let p = entry.path();
        if ft.is_dir() {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.starts_with('.') && name != "node_modules" {
                collect_md_meta_recursive(&p, out);
            }
        } else if ft.is_file()
            && p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("md"))
                .unwrap_or(false)
        {
            let (mtime_ns, len) = entry
                .metadata()
                .map(|m| meta_mtime_len(&m))
                .unwrap_or((0, 0));
            out.push((p, mtime_ns, len));
        }
    }
}

fn build_cached_file(path: PathBuf, mtime_ns: u128, len: u64) -> Option<Arc<CachedFile>> {
    let loaded = NoteFileStore::read_note(&path).ok()?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let content_lower = loaded.content.to_ascii_lowercase();
    let mut tags_lower = BTreeSet::new();
    let mut counts: HashMap<String, usize> = HashMap::new();
    for line in loaded.content.lines() {
        for tag in extract_tags_from_line(line) {
            tags_lower.insert(tag.to_ascii_lowercase());
            *counts.entry(tag).or_insert(0) += 1;
        }
    }
    let tag_counts = counts.into_iter().collect();
    Some(Arc::new(CachedFile {
        path,
        file_name,
        stem,
        mtime_ns,
        len,
        content: loaded.content,
        content_lower,
        tags_lower,
        tag_counts,
    }))
}

fn load_cached_entries(scrap_dir: &Path) -> Vec<Arc<CachedFile>> {
    let resolved = ScrapStore::resolve_scrap_dir(scrap_dir);
    let now = Instant::now();
    let root_mtime = dir_mtime_ns(&resolved);

    // Fast path: reuse cached directory snapshot within TTL on slow drives (e.g. Google Drive)
    // as long as the root directory mtime has not changed.
    let prev_by_path: HashMap<PathBuf, Arc<CachedFile>> = {
        if let Ok(guard) = search_cache().read() {
            if let Some(dc) = guard.get(&resolved) {
                if dc.slow_drive
                    && now.duration_since(dc.last_scan) < SLOW_SCAN_TTL
                    && root_mtime != 0
                    && dc.root_mtime_ns == root_mtime
                {
                    return dc.entries.clone();
                }
                dc.entries
                    .iter()
                    .map(|e| (e.path.clone(), Arc::clone(e)))
                    .collect()
            } else {
                HashMap::new()
            }
        } else {
            HashMap::new()
        }
    };

    let scan_start = Instant::now();
    let mut raw_files = Vec::new();
    collect_md_meta_recursive(&resolved, &mut raw_files);
    let scan_elapsed = scan_start.elapsed();
    // Newest files first (YYYY-MM-DD.md descending).
    raw_files.sort_by(|a, b| b.0.file_name().cmp(&a.0.file_name()));

    let mut to_load: Vec<(usize, PathBuf, u128, u64)> = Vec::new();
    let mut slots: Vec<Option<Arc<CachedFile>>> = vec![None; raw_files.len()];

    for (idx, (path, mtime_ns, len)) in raw_files.into_iter().enumerate() {
        if let Some(prev) = prev_by_path.get(&path) {
            if mtime_ns != 0 && prev.mtime_ns == mtime_ns && prev.len == len {
                slots[idx] = Some(Arc::clone(prev));
                continue;
            }
        }
        to_load.push((idx, path, mtime_ns, len));
    }

    if !to_load.is_empty() {
        let workers = std::thread::available_parallelism()
            .map(|n| n.get().min(8))
            .unwrap_or(4)
            .min(to_load.len());
        let next_idx = AtomicUsize::new(0);
        let mut loaded_parts: Vec<(usize, Arc<CachedFile>)> = Vec::new();
        std::thread::scope(|s| {
            let mut handles = Vec::with_capacity(workers);
            for _ in 0..workers {
                handles.push(s.spawn(|| {
                    let mut local = Vec::new();
                    loop {
                        let i = next_idx.fetch_add(1, Ordering::Relaxed);
                        if i >= to_load.len() {
                            break;
                        }
                        let (slot_idx, ref p, mtime_ns, len) = to_load[i];
                        if let Some(cf) = build_cached_file(p.clone(), mtime_ns, len) {
                            local.push((slot_idx, cf));
                        }
                    }
                    local
                }));
            }
            for h in handles {
                if let Ok(part) = h.join() {
                    loaded_parts.extend(part);
                }
            }
        });
        for (slot_idx, cf) in loaded_parts {
            slots[slot_idx] = Some(cf);
        }
    }

    let entries: Vec<Arc<CachedFile>> = slots.into_iter().flatten().collect();
    if let Ok(mut guard) = search_cache().write() {
        guard.insert(
            resolved,
            DirCache {
                last_scan: Instant::now(),
                slow_drive: scan_elapsed >= SLOW_SCAN_THRESHOLD,
                root_mtime_ns: root_mtime,
                entries: entries.clone(),
            },
        );
    }
    entries
}

fn filter_entries(entries: &[Arc<CachedFile>], filter: &ScrapFilter) -> Vec<Arc<CachedFile>> {
    let from = filter.from.trim();
    let to = filter.to.trim();
    entries
        .iter()
        .filter(|e| {
            if from.is_empty() && to.is_empty() {
                return true;
            }
            if !is_date_stem(&e.stem) {
                return false;
            }
            if !from.is_empty() && e.stem.as_str() < from {
                return false;
            }
            if !to.is_empty() && e.stem.as_str() > to {
                return false;
            }
            true
        })
        .cloned()
        .collect()
}

fn is_date_stem(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

fn extract_tags_from_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '#' {
            let prev_ok = i == 0 || chars[i - 1].is_whitespace();
            // Skip markdown headings (`# `, `## `).
            if prev_ok && i + 1 < chars.len() && chars[i + 1] != '#' && !chars[i + 1].is_whitespace()
            {
                let start = i + 1;
                let mut end = start;
                while end < chars.len()
                    && !chars[end].is_whitespace()
                    && !matches!(chars[end], ',' | '.' | ';' | ':' | ')' | ']' | '"' | '\'')
                {
                    end += 1;
                }
                if end > start {
                    let tag: String = chars[start..end].iter().collect();
                    out.push(tag);
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn file_matches_tags_cached(file: &CachedFile, required_tags: &[String]) -> bool {
    if required_tags.is_empty() {
        return true;
    }
    required_tags
        .iter()
        .all(|req| file.tags_lower.contains(&req.trim_start_matches('#').to_ascii_lowercase()))
}

fn search_cached_file(file: &CachedFile, query_lower: &str, terms: &[String], required_tags: &[String]) -> Option<SearchResult> {
    if !file_matches_tags_cached(file, required_tags) {
        return None;
    }
    let lines: Vec<&str> = file.content.lines().collect();
    let lower_lines: Vec<&str> = file.content_lower.lines().collect();
    let mut matches = Vec::new();

    if !query_lower.is_empty() {
        for (idx, (line, lower)) in lines.iter().zip(lower_lines.iter()).enumerate() {
            if let Some(pos) = lower.find(query_lower) {
                let mut context = Vec::new();
                if idx > 0 {
                    context.push(lines[idx - 1].to_string());
                }
                if idx + 1 < lines.len() {
                    context.push(lines[idx + 1].to_string());
                }
                matches.push(SearchMatch {
                    line: idx + 1,
                    col: pos + 1,
                    text: (*line).to_string(),
                    context,
                    score: None,
                });
            }
        }
    } else if !required_tags.is_empty() {
        // Filter-only browse: return first non-empty line as preview.
        if let Some((idx, line)) = lines.iter().enumerate().find(|(_, l)| !l.trim().is_empty()) {
            matches.push(SearchMatch {
                line: idx + 1,
                col: 1,
                text: (*line).to_string(),
                context: Vec::new(),
                score: None,
            });
        }
    }

    // Multi-term scored fallback when exact phrase yields no hit.
    if matches.is_empty() && terms.len() > 1 {
        for (idx, (line, lower)) in lines.iter().zip(lower_lines.iter()).enumerate() {
            let hits = terms.iter().filter(|t| lower.contains(t.as_str())).count();
            if hits > 0 {
                let score = (hits as f64) / (terms.len() as f64);
                matches.push(SearchMatch {
                    line: idx + 1,
                    col: 1,
                    text: (*line).to_string(),
                    context: Vec::new(),
                    score: Some(score),
                });
            }
        }
    }

    if matches.is_empty() {
        return None;
    }

    Some(SearchResult {
        file_path: file.path.to_string_lossy().into_owned(),
        file_name: file.file_name.clone(),
        matches,
    })
}

/// Scans `.md` files in `scrap_dir` using an in-memory `(mtime, len)` cache.
pub fn search_scraps_parallel(
    scrap_dir: &Path,
    query: &str,
    max_results: usize,
    filter: &ScrapFilter,
) -> Vec<SearchResult> {
    let all_entries = load_cached_entries(scrap_dir);
    let files = filter_entries(&all_entries, filter);
    if files.is_empty() {
        return Vec::new();
    }
    let cap = if max_results == 0 { 100 } else { max_results };
    let query_lower = query.trim().to_ascii_lowercase();
    let terms: Vec<String> = query_lower
        .split_whitespace()
        .map(str::to_string)
        .collect();

    // Small/medium corpora search faster inline without thread-spawn overhead.
    if files.len() < 128 {
        let mut results = Vec::new();
        for f in &files {
            if let Some(res) = search_cached_file(f, &query_lower, &terms, &filter.tags) {
                results.push(res);
                if results.len() >= cap {
                    break;
                }
            }
        }
        return results;
    }

    let workers = std::thread::available_parallelism()
        .map(|n| n.get().min(8))
        .unwrap_or(4)
        .min(files.len());
    let next_idx = AtomicUsize::new(0);
    let mut per_file: Vec<(usize, SearchResult)> = Vec::new();

    std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            handles.push(s.spawn(|| {
                let mut local = Vec::new();
                loop {
                    let i = next_idx.fetch_add(1, Ordering::Relaxed);
                    if i >= files.len() {
                        break;
                    }
                    if let Some(res) =
                        search_cached_file(&files[i], &query_lower, &terms, &filter.tags)
                    {
                        local.push((i, res));
                    }
                }
                local
            }));
        }
        for h in handles {
            if let Ok(part) = h.join() {
                per_file.extend(part);
            }
        }
    });

    per_file.sort_by_key(|(idx, _)| *idx);
    per_file.into_iter().take(cap).map(|(_, r)| r).collect()
}

/// Collects tag counts and file counts across `scrap_dir` for the search filter row.
pub fn scrap_filter_options(scrap_dir: &Path) -> ScrapFilterOptions {
    let entries = load_cached_entries(scrap_dir);
    let mut undated = 0usize;
    let mut tag_map: HashMap<String, (usize, usize)> = HashMap::new();

    for file in &entries {
        if !is_date_stem(&file.stem) {
            undated += 1;
        }
        for (tag, cnt) in &file.tag_counts {
            let entry = tag_map.entry(tag.clone()).or_insert((0, 0));
            entry.0 += 1;
            entry.1 += *cnt;
        }
    }

    let mut tags: Vec<TagCount> = tag_map
        .into_iter()
        .map(|(tag, (files_cnt, entries))| TagCount {
            tag,
            files: files_cnt,
            entries,
        })
        .collect();
    tags.sort_by(|a, b| b.entries.cmp(&a.entries).then_with(|| a.tag.cmp(&b.tag)));
    tags.truncate(40);

    ScrapFilterOptions {
        tags,
        files: entries.len(),
        undated,
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
        let dir = std::env::temp_dir().join(format!("syki-search-{label}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn parallel_search_respects_date_and_tag_filters_and_newest_order() {
        let dir = temp_dir("parallel");
        std::fs::write(dir.join("2026-10-01.md"), "## [09:00] #work\nRust compiler notes\n")
            .unwrap();
        std::fs::write(dir.join("2026-10-03.md"), "## [10:00] #work\nRust SIMD search\n")
            .unwrap();
        std::fs::write(dir.join("2026-10-04.md"), "## [11:00] #life\nRust gardening\n")
            .unwrap();

        let all = search_scraps_parallel(&dir, "rust", 10, &ScrapFilter::default());
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].file_name, "2026-10-04.md");

        let filtered = search_scraps_parallel(
            &dir,
            "rust",
            10,
            &ScrapFilter {
                tags: vec!["work".into()],
                from: "2026-10-02".into(),
                to: "2026-10-05".into(),
            },
        );
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].file_name, "2026-10-03.md");

        let opts = scrap_filter_options(&dir);
        assert_eq!(opts.files, 3);
        assert_eq!(opts.undated, 0);
        assert!(opts.tags.iter().any(|t| t.tag == "work" && t.files == 2));

        let _ = std::fs::remove_dir_all(dir);
    }
}
