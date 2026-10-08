//! `read_file` / `read_directory` — the machine CrabPort itself runs on.

use std::io::Read as _;

use rust_i18n::t;

use crate::agent::ToolOutcome;
use crate::limits::{READ_DIR_LIMIT, READ_FILE_LIMIT};
use crate::text::cap_tool_result_head;

/// Read up to [`READ_FILE_LIMIT`] bytes of a local file; binary content is
/// reported rather than dumped. Never fails — every problem becomes text.
pub fn read_local_file(path: &str) -> ToolOutcome {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) => return ToolOutcome::text(format!("cannot open {path}: {err}")),
    };
    let size = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let mut reader = file.take(READ_FILE_LIMIT);
    let mut buf = Vec::new();
    if reader.read_to_end(&mut buf).is_err() {
        return ToolOutcome::text(format!("reading {path} failed: I/O error"));
    }
    if buf.contains(&0) {
        return ToolOutcome::text(t!(
            "ai_panel.tool_read_file_binary",
            bytes = size.to_string()
        ));
    }
    ToolOutcome::text(cap_tool_result_head(&String::from_utf8_lossy(&buf)))
}

/// List a local directory: one table row per entry (name / type / size,
/// as the model sees them — directories say `dir`), capped at
/// [`READ_DIR_LIMIT`]. Never fails — every problem becomes text.
pub fn read_local_directory(path: &str) -> ToolOutcome {
    let dir = match std::fs::read_dir(path) {
        Ok(dir) => dir,
        Err(err) => return ToolOutcome::text(format!("cannot list {path}: {err}")),
    };
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut overflow = 0usize;
    for entry in dir {
        let Ok(entry) = entry else { continue };
        if rows.len() >= READ_DIR_LIMIT {
            overflow += 1;
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = if is_dir { "dir" } else { "file" }.to_string();
        let size = entry
            .metadata()
            .map(|meta| meta.len().to_string())
            .unwrap_or_else(|_| "?".to_string());
        rows.push(vec![name, kind, size]);
    }
    if rows.is_empty() {
        return ToolOutcome::text(t!("ai_panel.tool_dir_empty"));
    }
    let mut outcome = ToolOutcome::table(&["name", "type", "size"], rows);
    if overflow > 0 {
        outcome.text.push_str(&format!(
            "\n{}",
            t!("ai_panel.tool_dir_truncated", more = overflow)
        ));
    }
    outcome
}
