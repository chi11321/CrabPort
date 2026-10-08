//! SFTP tool helpers: rendering a remote directory listing for the model.

use rust_i18n::t;

use crate::limits::READ_DIR_LIMIT;

/// Format a remote SFTP directory listing for the model: the resolved
/// directory, then one line per entry (directories marked with a trailing
/// slash), capped at [`READ_DIR_LIMIT`].
pub fn format_remote_listing(cwd: &str, entries: &[crabport_sftp::FileEntry]) -> String {
    let mut out = format!("{cwd}:\n");
    for entry in entries.iter().take(READ_DIR_LIMIT) {
        if entry.is_dir {
            out.push_str(&format!("{}/\n", entry.name));
        } else {
            let size = entry
                .size
                .map(|bytes| bytes.to_string())
                .unwrap_or_else(|| "?".to_string());
            out.push_str(&format!("{}  {size}\n", entry.name));
        }
    }
    if entries.len() > READ_DIR_LIMIT {
        out.push_str(&t!(
            "ai_panel.tool_dir_truncated",
            more = entries.len() - READ_DIR_LIMIT
        ));
    }
    if entries.is_empty() {
        return t!("ai_panel.tool_dir_empty").to_string();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::format_remote_listing;

    /// A remote listing marks directories with a trailing slash, carries
    /// sizes where known, and resolves the directory it lists.
    #[test]
    fn format_remote_listing_marks_dirs_and_sizes() {
        let entry = |name: &str, is_dir: bool, size: Option<u64>| crabport_sftp::FileEntry {
            name: name.to_string(),
            is_dir,
            size,
            permissions: None,
            modified: None,
        };
        let text = format_remote_listing(
            "/var/log",
            &[entry("src", true, None), entry("a.txt", false, Some(12))],
        );
        assert!(text.starts_with("/var/log:\n"), "{text}");
        assert!(text.contains("src/\n"), "{text}");
        assert!(text.contains("a.txt  12\n"), "{text}");
    }
}
