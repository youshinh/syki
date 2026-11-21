//! Parallel daily-scrap full-text search and tag/date filter extraction (#07).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};

use crate::storage::{NoteFileStore, ScrapStore};

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

fn collect_scrap_files(scrap_dir: &Path, filter: &ScrapFilter) -> Vec<PathBuf> {
    let resolved = ScrapStore::resolve_scrap_dir(scrap_dir);
    let Ok(rd) = std::fs::read_dir(&resolved) else {
        return Vec::new();
    };
    let from = filter.from.trim();
    let to = filter.to.trim();
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
        .filter(|p| {
            if from.is_empty() && to.is_empty() {
                return true;
            }
            let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
                return false;
            };
            if !is_date_stem(stem) {
                return false;
            }
            if !from.is_empty() && stem < from {
                return false;
            }
            if !to.is_empty() && stem > to {
                return false;
            }
            true
        })
        .collect();
    // Newest files first (YYYY-MM-DD.md descending).
    files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    files
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

fn file_matches_tags(content: &str, required_tags: &[String]) -> bool {
    if required_tags.is_empty() {
        return true;
    }
    let mut found = BTreeSet::new();
    for line in content.lines() {
        for tag in extract_tags_from_line(line) {
            found.insert(tag.to_ascii_lowercase());
        }
    }
    required_tags
        .iter()
        .all(|req| found.contains(&req.trim_start_matches('#').to_ascii_lowercase()))
}

fn search_single_file(path: &Path, query_lower: &str, terms: &[String], required_tags: &[String]) -> Option<SearchResult> {
    let loaded = NoteFileStore::read_note(path).ok()?;
    if !file_matches_tags(&loaded.content, required_tags) {
        return None;
    }
    let lines: Vec<&str> = loaded.content.lines().collect();
    let mut matches = Vec::new();

    if !query_lower.is_empty() {
        for (idx, line) in lines.iter().enumerate() {
            let lower = line.to_ascii_lowercase();
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
        for (idx, line) in lines.iter().enumerate() {
            let lower = line.to_ascii_lowercase();
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

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    Some(SearchResult {
        file_path: path.to_string_lossy().into_owned(),
        file_name,
        matches,
    })
}

/// Scans `.md` files in `scrap_dir` in parallel using a bounded thread pool.
pub fn search_scraps_parallel(
    scrap_dir: &Path,
    query: &str,
    max_results: usize,
    filter: &ScrapFilter,
) -> Vec<SearchResult> {
    let files = collect_scrap_files(scrap_dir, filter);
    if files.is_empty() {
        return Vec::new();
    }
    let cap = if max_results == 0 { 100 } else { max_results };
    let query_lower = query.trim().to_ascii_lowercase();
    let terms: Vec<String> = query_lower
        .split_whitespace()
        .map(str::to_string)
        .collect();

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
                        search_single_file(&files[i], &query_lower, &terms, &filter.tags)
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
    let files = collect_scrap_files(scrap_dir, &ScrapFilter::default());
    let mut undated = 0usize;
    let mut tag_map: HashMap<String, (usize, usize)> = HashMap::new();

    for path in &files {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if !is_date_stem(stem) {
            undated += 1;
        }
        let Ok(loaded) = NoteFileStore::read_note(path) else {
            continue;
        };
        let mut file_tags: HashMap<String, usize> = HashMap::new();
        for line in loaded.content.lines() {
            for tag in extract_tags_from_line(line) {
                *file_tags.entry(tag).or_insert(0) += 1;
            }
        }
        for (tag, cnt) in file_tags {
            let entry = tag_map.entry(tag).or_insert((0, 0));
            entry.0 += 1;
            entry.1 += cnt;
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
        files: files.len(),
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
