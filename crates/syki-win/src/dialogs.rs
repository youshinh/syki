//! Native Win32 file/folder dialogs (`IFileOpenDialog` / `IFileSaveDialog`),
//! URL validation, and shell open/reveal helpers (#04).

use std::path::{Path, PathBuf};
use std::process::Command;

use windows::core::{w, HRESULT, HSTRING, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FileOpenDialog, FileSaveDialog, IFileDialog, IFileOpenDialog, IFileSaveDialog, IShellItem,
    SHCreateItemFromParsingName, ShellExecuteW, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM,
    FOS_OVERWRITEPROMPT, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

const HRESULT_ERROR_CANCELLED: HRESULT = HRESULT(0x800704C7u32 as i32);

/// Validates that an external URL uses an allowed scheme (`http://`, `https://`, or `vscode://`).
pub fn validate_external_url(url: &str) -> Result<&str, String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("external url is empty".to_string());
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("vscode://")
    {
        Ok(trimmed)
    } else {
        Err(format!("disallowed external url scheme: {trimmed}"))
    }
}

/// Opens a validated external URL via Win32 `ShellExecuteW`.
pub fn open_external_url(url: &str) -> Result<(), String> {
    let valid = validate_external_url(url)?;
    let url_h = HSTRING::from(valid);
    let res = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &url_h,
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if (res.0 as isize) <= 32 {
        return Err(format!("ShellExecuteW failed with code {}", res.0 as isize));
    }
    Ok(())
}

/// Opens an existing local file or folder in the default Windows shell handler.
pub fn open_path_in_shell(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("path does not exist: {}", path.display()));
    }
    let path_h = HSTRING::from(path.as_os_str());
    let res = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &path_h,
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if (res.0 as isize) <= 32 {
        return Err(format!("ShellExecuteW failed with code {}", res.0 as isize));
    }
    Ok(())
}

/// Reveals a file in Windows Explorer (`/select,<path>`), or opens its parent folder if missing.
pub fn reveal_in_explorer(path: &Path) -> Result<(), String> {
    if path.exists() {
        let arg = format!("/select,{}", path.display());
        Command::new("explorer.exe")
            .arg(arg)
            .spawn()
            .map_err(|e| format!("failed to launch explorer.exe: {e}"))?;
        return Ok(());
    }
    if let Some(parent) = path.parent().filter(|p| p.exists()) {
        Command::new("explorer.exe")
            .arg(parent)
            .spawn()
            .map_err(|e| format!("failed to launch explorer.exe: {e}"))?;
        return Ok(());
    }
    Err(format!("path does not exist: {}", path.display()))
}

/// Strips path separators and Windows-invalid filename characters, falling back to `untitled.md`.
pub fn sanitize_filename(name: &str) -> String {
    let base = Path::new(name.trim())
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches('.');
    if trimmed.is_empty() {
        "untitled.md".to_string()
    } else {
        trimmed.to_string()
    }
}

fn ensure_com_initialized() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
}

fn set_initial_folder(dialog: &IFileDialog, dir: &Path) {
    if !dir.is_dir() {
        return;
    }
    let dir_h = HSTRING::from(dir.as_os_str());
    let item: windows::core::Result<IShellItem> =
        unsafe { SHCreateItemFromParsingName(&dir_h, None) };
    if let Ok(folder) = item {
        unsafe {
            let _ = dialog.SetFolder(&folder);
        }
    }
}

fn read_shell_item_path(item: &IShellItem) -> Result<PathBuf, String> {
    unsafe {
        let pwstr = item
            .GetDisplayName(SIGDN_FILESYSPATH)
            .map_err(|e| format!("GetDisplayName failed: {e}"))?;
        let text = pwstr.to_string().unwrap_or_default();
        CoTaskMemFree(Some(pwstr.0 as *const _));
        if text.is_empty() {
            Err("dialog returned empty path".to_string())
        } else {
            Ok(PathBuf::from(text))
        }
    }
}

fn show_dialog_and_get_path(dialog: &IFileDialog, owner: Option<HWND>) -> Result<Option<PathBuf>, String> {
    let show_res = unsafe { dialog.Show(owner) };
    match show_res {
        Ok(()) => {
            let item = unsafe {
                dialog
                    .GetResult()
                    .map_err(|e| format!("GetResult failed: {e}"))?
            };
            read_shell_item_path(&item).map(Some)
        }
        Err(err) if err.code() == HRESULT_ERROR_CANCELLED => Ok(None),
        Err(err) => Err(format!("dialog Show failed: {err}")),
    }
}

/// Opens a native Win32 File Open dialog for Markdown/text notes.
pub fn pick_open_file(
    owner: Option<HWND>,
    start_dir: Option<&Path>,
) -> Result<Option<PathBuf>, String> {
    ensure_com_initialized();
    let dialog: IFileOpenDialog = unsafe {
        CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("CoCreateInstance(FileOpenDialog) failed: {e}"))?
    };
    let file_dialog: IFileDialog = windows::core::Interface::cast(&dialog)
        .map_err(|e| format!("cast IFileDialog failed: {e}"))?;

    unsafe {
        let opts = file_dialog.GetOptions().unwrap_or_default();
        let _ = file_dialog.SetOptions(opts | FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST);
        let filters = [
            COMDLG_FILTERSPEC {
                pszName: w!("Markdown / Text (*.md;*.markdown;*.txt)"),
                pszSpec: w!("*.md;*.markdown;*.txt"),
            },
            COMDLG_FILTERSPEC {
                pszName: w!("All Files (*.*)"),
                pszSpec: w!("*.*"),
            },
        ];
        let _ = file_dialog.SetFileTypes(&filters);
        let _ = file_dialog.SetFileTypeIndex(1);
    }

    if let Some(dir) = start_dir {
        set_initial_folder(&file_dialog, dir);
    }

    show_dialog_and_get_path(&file_dialog, owner)
}

/// Opens a native Win32 File Save dialog for Markdown or plain-text export.
pub fn pick_save_file(
    owner: Option<HWND>,
    suggested_path: Option<&str>,
    plain_text: bool,
) -> Result<Option<PathBuf>, String> {
    ensure_com_initialized();
    let dialog: IFileSaveDialog = unsafe {
        CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("CoCreateInstance(FileSaveDialog) failed: {e}"))?
    };
    let file_dialog: IFileDialog = windows::core::Interface::cast(&dialog)
        .map_err(|e| format!("cast IFileDialog failed: {e}"))?;

    unsafe {
        let opts = file_dialog.GetOptions().unwrap_or_default();
        let _ = file_dialog.SetOptions(opts | FOS_FORCEFILESYSTEM | FOS_OVERWRITEPROMPT);
        if plain_text {
            let filters = [
                COMDLG_FILTERSPEC {
                    pszName: w!("Plain Text (*.txt)"),
                    pszSpec: w!("*.txt"),
                },
                COMDLG_FILTERSPEC {
                    pszName: w!("All Files (*.*)"),
                    pszSpec: w!("*.*"),
                },
            ];
            let _ = file_dialog.SetFileTypes(&filters);
            let _ = file_dialog.SetDefaultExtension(w!("txt"));
        } else {
            let filters = [
                COMDLG_FILTERSPEC {
                    pszName: w!("Markdown (*.md)"),
                    pszSpec: w!("*.md;*.markdown"),
                },
                COMDLG_FILTERSPEC {
                    pszName: w!("Plain Text (*.txt)"),
                    pszSpec: w!("*.txt"),
                },
                COMDLG_FILTERSPEC {
                    pszName: w!("All Files (*.*)"),
                    pszSpec: w!("*.*"),
                },
            ];
            let _ = file_dialog.SetFileTypes(&filters);
            let _ = file_dialog.SetDefaultExtension(w!("md"));
        }
    }

    if let Some(suggested) = suggested_path.map(str::trim).filter(|s| !s.is_empty()) {
        let p = Path::new(suggested);
        if let Some(parent) = p.parent().filter(|d| !d.as_os_str().is_empty() && d.is_dir()) {
            set_initial_folder(&file_dialog, parent);
        }
        if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            let clean = sanitize_filename(name);
            let name_h = HSTRING::from(clean);
            unsafe {
                let _ = file_dialog.SetFileName(&name_h);
            }
        }
    }

    show_dialog_and_get_path(&file_dialog, owner)
}

/// Opens a native Win32 Folder Picker dialog (`FOS_PICKFOLDERS`).
pub fn pick_folder(
    owner: Option<HWND>,
    start_dir: Option<&Path>,
) -> Result<Option<PathBuf>, String> {
    ensure_com_initialized();
    let dialog: IFileOpenDialog = unsafe {
        CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("CoCreateInstance(FileOpenDialog) failed: {e}"))?
    };
    let file_dialog: IFileDialog = windows::core::Interface::cast(&dialog)
        .map_err(|e| format!("cast IFileDialog failed: {e}"))?;

    unsafe {
        let opts = file_dialog.GetOptions().unwrap_or_default();
        let _ = file_dialog.SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM);
    }

    if let Some(dir) = start_dir {
        set_initial_folder(&file_dialog, dir);
    }

    show_dialog_and_get_path(&file_dialog, owner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_external_url_allows_only_safe_schemes() {
        assert_eq!(
            validate_external_url("  https://example.com/docs ").unwrap(),
            "https://example.com/docs"
        );
        assert_eq!(
            validate_external_url("http://localhost:3000/").unwrap(),
            "http://localhost:3000/"
        );
        assert_eq!(
            validate_external_url("vscode://file/C:/notes/a.md").unwrap(),
            "vscode://file/C:/notes/a.md"
        );

        assert!(validate_external_url("").is_err());
        assert!(validate_external_url("   ").is_err());
        assert!(validate_external_url("file:///C:/Windows/win.ini").is_err());
        assert!(validate_external_url("javascript:alert(1)").is_err());
        assert!(validate_external_url("data:text/html,hi").is_err());
    }

    #[test]
    fn sanitize_filename_strips_invalid_chars_and_paths() {
        assert_eq!(sanitize_filename("2026-11-21_note.md"), "2026-11-21_note.md");
        assert_eq!(sanitize_filename("a/b\\c:d*e?f.md"), "c_d_e_f.md");
        assert_eq!(sanitize_filename("   "), "untitled.md");
        assert_eq!(sanitize_filename("..."), "untitled.md");
    }
}
