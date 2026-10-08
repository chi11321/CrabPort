//! SFTP tool helpers: rendering a remote directory listing for the model.

use rust_i18n::t;

use crate::agent::ToolOutcome;
use crate::limits::READ_DIR_LIMIT;

/// Format a remote SFTP directory listing: one table row per entry
/// (name / type / size / permissions, as the model sees them), capped at
/// [`READ_DIR_LIMIT`].
pub fn format_remote_listing(entries: &[crabport_sftp::FileEntry]) -> ToolOutcome {
    let rows: Vec<Vec<String>> = entries
        .iter()
        .take(READ_DIR_LIMIT)
        .map(|entry| {
            vec![
                entry.name.clone(),
                if entry.is_dir { "dir" } else { "file" }.to_string(),
                entry
                    .size
                    .map(|bytes| bytes.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                entry.permissions.clone().unwrap_or_else(|| "-".to_string()),
            ]
        })
        .collect();
    if rows.is_empty() {
        return ToolOutcome::text(t!("ai_panel.tool_dir_empty"));
    }
    let mut outcome = ToolOutcome::table(&["name", "type", "size", "permissions"], rows);
    if entries.len() > READ_DIR_LIMIT {
        outcome.text.push_str(&format!(
            "\n{}",
            t!(
                "ai_panel.tool_dir_truncated",
                more = entries.len() - READ_DIR_LIMIT
            )
        ));
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::format_remote_listing;

    /// A remote listing carries one row per entry with the kind marked and
    /// sizes/permissions where known; the model's copy is JSON.
    #[test]
    fn format_remote_listing_marks_dirs_and_sizes() {
        let entry = |name: &str, is_dir: bool, size: Option<u64>| crabport_sftp::FileEntry {
            name: name.to_string(),
            is_dir,
            size,
            permissions: None,
            modified: None,
        };
        let outcome =
            format_remote_listing(&[entry("src", true, None), entry("a.txt", false, Some(12))]);
        assert!(outcome.text.contains(r#""name":"src""#), "{}", outcome.text);
        assert!(outcome.text.contains(r#""type":"dir""#), "{}", outcome.text);
        assert!(outcome.text.contains(r#""size":"12""#), "{}", outcome.text);
        let table = outcome.table.expect("table present");
        assert_eq!(table.headers, ["name", "type", "size", "permissions"]);
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0][1], "dir");
    }
}
