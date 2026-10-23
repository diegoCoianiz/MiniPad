//! Crash-recovery snapshots.
//!
//! While a document has unsaved changes, MiniPad periodically writes a full
//! copy to `%APPDATA%\MiniPad\recovery` using the document's original format
//! (when known). The `.fwd` format is deliberately avoided for imported
//! `.docx` files because it would drop unknown package parts.

use crate::doc::Document;
use crate::{docx, io};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const VERSION: &str = env!("CARGO_PKG_VERSION");
static SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryMeta {
    /// Original document path, or `None` for an untitled document.
    pub original: Option<PathBuf>,
    /// Display title at snapshot time (file name or `Document1`).
    pub title: String,
    /// Snapshot file name, relative to the recovery directory.
    pub snapshot: String,
    /// Unix time in milliseconds.
    pub saved_at: u64,
    pub app_version: String,
}

#[derive(Clone, Debug)]
pub struct RecoveryEntry {
    pub meta: RecoveryMeta,
    pub json: PathBuf,
    pub snapshot: PathBuf,
}

impl RecoveryEntry {
    pub fn saved_at_text(&self) -> String {
        use chrono::TimeZone;
        chrono::Local
            .timestamp_millis_opt(self.meta.saved_at as i64)
            .single()
            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default()
    }
}

fn app_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from).map(|p| p.join("MiniPad"))
}

pub fn dir() -> Option<PathBuf> {
    app_dir().map(|p| p.join("recovery"))
}

fn extension(original: Option<&Path>) -> &'static str {
    match original
        .and_then(|p| p.extension())
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .as_deref()
    {
        Some("fwd") => "fwd",
        Some("txt") => "txt",
        _ => "docx",
    }
}

fn save_as(doc: &Document, original: Option<&Path>, path: &Path) -> Result<(), String> {
    match extension(original) {
        "fwd" => io::save_native(doc, path),
        "txt" => io::save_text(doc, path),
        _ => docx::save_docx(doc, path),
    }
}

pub fn restore(entry: &RecoveryEntry) -> Result<Document, String> {
    crate::app::load_any(&entry.snapshot)
}

/// Lists valid recovery snapshots, newest first.
pub fn list() -> Vec<RecoveryEntry> {
    dir().map(|d| list_in(&d)).unwrap_or_default()
}

pub fn list_in(dir: &Path) -> Vec<RecoveryEntry> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let json = e.path();
        if json.extension().is_none_or(|x| !x.to_string_lossy().eq_ignore_ascii_case("json")) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&json) else { continue };
        let Ok(meta) = serde_json::from_str::<RecoveryMeta>(&text) else { continue };
        let snapshot = dir.join(&meta.snapshot);
        if snapshot.is_file() {
            out.push(RecoveryEntry { meta, json, snapshot });
        }
    }
    out.sort_by(|a, b| b.meta.saved_at.cmp(&a.meta.saved_at));
    out
}

/// Writes a snapshot, replacing `previous` (an older snapshot from this same
/// session) after the new one is safely renamed into place.
pub fn snapshot(doc: &Document, original: Option<&Path>, title: &str, previous: Option<&Path>) -> Result<RecoveryEntry, String> {
    let dir = dir().ok_or_else(|| "APPDATA is not available".to_string())?;
    snapshot_in(&dir, doc, original, title, previous)
}

pub fn snapshot_in(
    dir: &Path,
    doc: &Document,
    original: Option<&Path>,
    title: &str,
    previous: Option<&Path>,
) -> Result<RecoveryEntry, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Could not create the recovery folder: {e}"))?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let ext = extension(original);
    let base = format!("recovery-{}-{stamp}-{seq}", std::process::id());
    let snapshot = dir.join(format!("{base}.{ext}"));
    let tmp = dir.join(format!("{base}.{ext}.tmp"));
    save_as(doc, original, &tmp)?;
    std::fs::rename(&tmp, &snapshot).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("Could not finalise the recovery snapshot: {e}")
    })?;

    let meta = RecoveryMeta {
        original: original.map(|p| p.to_path_buf()),
        title: title.to_string(),
        snapshot: snapshot.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        saved_at: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(stamp as u64),
        app_version: VERSION.to_string(),
    };
    let json = dir.join(format!("{base}.json"));
    let tmp_json = dir.join(format!("{base}.json.tmp"));
    let text = serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?;
    std::fs::write(&tmp_json, text).map_err(|e| format!("Could not write recovery metadata: {e}"))?;
    std::fs::rename(&tmp_json, &json).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_json);
        format!("Could not finalise recovery metadata: {e}")
    })?;

    if let Some(old) = previous.filter(|p| *p != json) {
        delete_json(old);
    }
    Ok(RecoveryEntry { meta, json, snapshot })
}

pub fn delete(entry: &RecoveryEntry) {
    let _ = std::fs::remove_file(&entry.snapshot);
    let _ = std::fs::remove_file(&entry.json);
}

/// Deletes a snapshot by its metadata path (used to drop the current session's
/// stale snapshot after a successful save).
pub fn delete_json(path: &Path) {
    if let Ok(text) = std::fs::read_to_string(path) {
        if let Ok(meta) = serde_json::from_str::<RecoveryMeta>(&text) {
            if let Some(dir) = path.parent() {
                let _ = std::fs::remove_file(dir.join(meta.snapshot));
            }
        }
    }
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        std::env::temp_dir().join(format!("minipad-recovery-{}-{stamp}", std::process::id()))
    }

    #[test]
    fn writes_lists_and_restores() {
        let dir = temp_dir();
        let doc = Document::new();
        let original = PathBuf::from("note.txt");
        let entry = snapshot_in(&dir, &doc, Some(&original), "note.txt", None).expect("snapshot");
        assert!(entry.snapshot.is_file());
        assert!(entry.json.is_file());
        let list = list_in(&dir);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].meta.title, "note.txt");
        let restored = restore(&list[0]).expect("restore");
        assert!(!restored.paras.is_empty());

        let second = snapshot_in(&dir, &doc, Some(&original), "note.txt", Some(&entry.json)).expect("second");
        assert!(!entry.snapshot.exists(), "previous snapshot removed");
        assert!(second.snapshot.exists());
        assert_eq!(list_in(&dir).len(), 1);

        delete(&second);
        assert!(list_in(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignores_broken_metadata() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("recovery-bad.json"), "not json").unwrap();
        assert!(list_in(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
