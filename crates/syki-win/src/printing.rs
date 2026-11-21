//! CDP `Page.printToPDF` print preview, PDF page counting, and `/api/print/preview.pdf` store (#11).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrintSettings {
    #[serde(default)]
    pub paper: String,
    #[serde(default)]
    pub landscape: bool,
    #[serde(default)]
    pub margin: String,
    #[serde(default)]
    pub scale: i32,
    #[serde(default)]
    pub pages: String,
    #[serde(default)]
    pub header_footer: bool,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub location: String,
}

impl PrintSettings {
    pub fn paper_inches(&self) -> Result<(f64, f64), String> {
        let p = if self.paper.trim().is_empty() {
            "a4"
        } else {
            self.paper.trim()
        };
        match p.to_ascii_lowercase().as_str() {
            "a4" => Ok((8.27, 11.69)),
            "a3" => Ok((11.69, 16.54)),
            "b5" => Ok((7.17, 10.12)),
            "letter" => Ok((8.5, 11.0)),
            other => Err(format!("unknown paper {other:?}")),
        }
    }

    pub fn margins(&self) -> Result<(f64, f64, f64, f64), String> {
        let m = self.margin.trim().to_ascii_lowercase();
        let (mut top, right, mut bottom, left) = match m.as_str() {
            "" | "normal" => (0.79, 0.79, 0.79, 0.79),
            "narrow" => (0.39, 0.39, 0.39, 0.39),
            other => return Err(format!("unknown margin {other:?}")),
        };
        if self.header_footer {
            if top < 0.45 {
                top = 0.45;
            }
            if bottom < 0.45 {
                bottom = 0.45;
            }
        }
        Ok((top, right, bottom, left))
    }

    /// Builds the `Page.printToPDF` CDP parameter object.
    pub fn cdp_params(&self) -> Result<Value, String> {
        let (pw, ph) = self.paper_inches()?;
        let (top, right, bottom, left) = self.margins()?;
        let scale = if self.scale == 0 { 100 } else { self.scale };
        if !(25..=200).contains(&scale) {
            return Err("the scale must be between 25 and 200 percent".to_string());
        }
        let pages = self.pages.trim();
        if pages.len() > 100
            || !pages
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, ' ' | ',' | '-'))
        {
            return Err("the pages are written like 1-3, 5".to_string());
        }

        let mut obj = json!({
            "landscape": self.landscape,
            "paperWidth": pw,
            "paperHeight": ph,
            "marginTop": top,
            "marginRight": right,
            "marginBottom": bottom,
            "marginLeft": left,
            "scale": (scale as f64) / 100.0,
            "printBackground": true,
            "preferCSSPageSize": false,
            "transferMode": "ReturnAsBase64",
            "displayHeaderFooter": self.header_footer,
        });
        if !pages.is_empty() {
            obj["pageRanges"] = json!(pages);
        }
        if self.header_footer {
            let title = html_escape(&self.title);
            let loc = html_escape(&self.location);
            let pad = format!("{:.2}in", left);
            obj["headerTemplate"] = json!(format!(
                "<div style=\"width:100%;font-size:9px;color:#666;padding:0 {pad};\">{title}</div>"
            ));
            obj["footerTemplate"] = json!(format!(
                "<div style=\"width:100%;font-size:9px;color:#666;padding:0 {pad};display:flex;justify-content:space-between;\"><span>{loc}</span><span><span class=\"pageNumber\"></span> / <span class=\"totalPages\"></span></span></div>"
            ));
        }
        Ok(obj)
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Decodes `{"data":"<base64>"}` returned by `Page.printToPDF`.
pub fn decode_pdf_result(raw_json: &str) -> Result<Vec<u8>, String> {
    let val: Value =
        serde_json::from_str(raw_json).map_err(|e| format!("invalid CDP printToPDF JSON: {e}"))?;
    let b64 = val
        .get("data")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "CDP printToPDF response missing data field".to_string())?;
    STANDARD
        .decode(b64.as_bytes())
        .map_err(|e| format!("base64 decode PDF failed: {e}"))
}

/// Estimates PDF page count by scanning for `/Type /Page` (excluding `/Type /Pages`).
pub fn count_pdf_pages(pdf: &[u8]) -> usize {
    let needle = b"/Type";
    let mut count = 0usize;
    let mut i = 0usize;
    while i + needle.len() < pdf.len() {
        if &pdf[i..i + needle.len()] == needle {
            let mut j = i + needle.len();
            while j < pdf.len() && pdf[j].is_ascii_whitespace() {
                j += 1;
            }
            if pdf[j..].starts_with(b"/Page") && !pdf[j..].starts_with(b"/Pages") {
                count += 1;
            }
        }
        i += 1;
    }
    count.max(1)
}

/// In-memory holder for `/api/print/preview.pdf`.
#[derive(Clone, Default)]
pub struct PrintPreviewStore {
    inner: Arc<Mutex<(u64, Vec<u8>)>>,
}

impl PrintPreviewStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_pdf(&self, bytes: Vec<u8>) -> (String, usize) {
        let pages = count_pdf_pages(&bytes);
        let mut guard = self.inner.lock().expect("print preview mutex");
        guard.0 += 1;
        guard.1 = bytes;
        (format!("/api/print/preview.pdf?v={}", guard.0), pages)
    }

    pub fn get_pdf(&self) -> Option<Vec<u8>> {
        let guard = self.inner.lock().expect("print preview mutex");
        if guard.1.is_empty() {
            None
        } else {
            Some(guard.1.clone())
        }
    }

    pub fn clear(&self) {
        let mut guard = self.inner.lock().expect("print preview mutex");
        guard.1.clear();
        guard.1.shrink_to_fit();
    }

    pub fn route_response(&self, rel_path: &str) -> Option<(u16, &'static str, Vec<u8>)> {
        let clean = rel_path.split('?').next().unwrap_or(rel_path);
        if clean != "/api/print/preview.pdf" {
            return None;
        }
        match self.get_pdf() {
            Some(bytes) => Some((200, "application/pdf", bytes)),
            None => Some((404, "text/plain; charset=utf-8", b"No print preview".to_vec())),
        }
    }
}

pub fn pick_pdf_save_path(suggested_name: Option<&str>) -> Result<Option<PathBuf>, String> {
    let mut name = crate::dialogs::sanitize_filename(suggested_name.unwrap_or("note.pdf"));
    if !name.to_ascii_lowercase().ends_with(".pdf") {
        if let Some(stem) = Path::new(&name).file_stem().and_then(|s| s.to_str()) {
            name = format!("{stem}.pdf");
        } else {
            name.push_str(".pdf");
        }
    }
    crate::dialogs::pick_save_file(None, Some(&name), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_settings_cdp_params_validates_and_builds_expected_fields() {
        let cfg = PrintSettings {
            paper: "a4".into(),
            landscape: true,
            margin: "narrow".into(),
            scale: 120,
            pages: "1-3, 5".into(),
            header_footer: true,
            title: "My <Note>".into(),
            location: "C:\\notes\\a.md".into(),
        };
        let p = cfg.cdp_params().expect("cdpParams");
        assert_eq!(p["landscape"], true);
        assert_eq!(p["paperWidth"], 8.27);
        assert_eq!(p["marginTop"], 0.45);
        assert_eq!(p["scale"], 1.2);
        assert_eq!(p["pageRanges"], "1-3, 5");
        assert!(p["headerTemplate"].as_str().unwrap().contains("My &lt;Note&gt;"));

        let bad_scale = PrintSettings { scale: 300, ..Default::default() };
        assert!(bad_scale.cdp_params().is_err());

        let bad_pages = PrintSettings { pages: "1;drop".into(), ..Default::default() };
        assert!(bad_pages.cdp_params().is_err());
    }

    #[test]
    fn preview_store_counts_pages_and_serves_route() {
        let store = PrintPreviewStore::new();
        assert_eq!(store.route_response("/api/print/preview.pdf").unwrap().0, 404);

        let fake_pdf = b"%PDF-1.4\n1 0 obj<</Type /Pages /Count 2>>\n2 0 obj<</Type /Page>>\n3 0 obj<</Type /Page>>\n".to_vec();
        let (url, pages) = store.set_pdf(fake_pdf.clone());
        assert!(url.starts_with("/api/print/preview.pdf?v="));
        assert_eq!(pages, 2);
        assert_eq!(
            store.route_response("/api/print/preview.pdf?v=1").unwrap(),
            (200, "application/pdf", fake_pdf)
        );
        store.clear();
        assert_eq!(store.route_response("/api/print/preview.pdf").unwrap().0, 404);
    }
}
