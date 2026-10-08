//! `read_file` / `read_directory` — the machine CrabPort itself runs on.

use std::io::Read as _;

use rust_i18n::t;

use crate::limits::{READ_DIR_LIMIT, READ_FILE_LIMIT};
use crate::text::cap_tool_result_head;

/// Read up to [`READ_FILE_LIMIT`] bytes of a local file; binary content is
/// reported rather than dumped. Never fails — every problem becomes text.
pub fn read_local_file(path: &str) -> String {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) => return format!("cannot open {path}: {err}"),
    };
    let size = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let mut reader = file.take(READ_FILE_LIMIT);
    let mut buf = Vec::new();
    if let Err(err) = reader.read_to_end(&mut buf) {
        return format!("reading {path} failed: {err}");
    }
    if buf.contains(&0) {
        return t!("ai_panel.tool_read_file_binary", bytes = size.to_string()).to_string();
    }
    cap_tool_result_head(&String::from_utf8_lossy(&buf))
}

/// List a local directory: one line per entry, directories marked with a
/// trailing slash, capped at [`READ_DIR_LIMIT`]. Never fails — every problem
/// becomes text.
pub fn read_local_directory(path: &str) -> String {
    let dir = match std::fs::read_dir(path) {
        Ok(dir) => dir,
        Err(err) => return format!("cannot list {path}: {err}"),
    };
    let mut rows: Vec<String> = Vec::new();
    let mut overflow = 0usize;
    for entry in dir {
        let Ok(entry) = entry else { continue };
        if rows.len() >= READ_DIR_LIMIT {
            overflow += 1;
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_dir {
            rows.push(format!("{name}/"));
        } else {
            let size = entry
                .metadata()
                .map(|meta| meta.len().to_string())
                .unwrap_or_else(|_| "?".to_string());
            rows.push(format!("{name}  {size}"));
        }
    }
    if rows.is_empty() {
        return t!("ai_panel.tool_dir_empty").to_string();
    }
    let mut out = rows.join("\n");
    if overflow > 0 {
        out.push('\n');
        out.push_str(&t!("ai_panel.tool_dir_truncated", more = overflow));
    }
    out
}
