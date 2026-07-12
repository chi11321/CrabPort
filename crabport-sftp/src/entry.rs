/// A directory entry with metadata.
///
/// Carries the same information regardless of whether it came from a local
/// `std::fs` listing or a remote SFTP `read_dir`: name, type, size,
/// permissions, and modified time. Fields beyond `name` / `is_dir` are
/// `Option` because not all backends can populate them (e.g. a server that
/// omits `mtime` from its SFTP attributes).
#[derive(Clone, Debug)]
pub struct FileEntry {
    /// File or directory name (not full path).
    pub name: String,
    /// Whether this entry is a directory.
    pub is_dir: bool,
    /// File size in bytes. `None` for directories or when unavailable.
    pub size: Option<u64>,
    /// Permission string (e.g. "rwxr-xr-x"). `None` when unavailable.
    pub permissions: Option<String>,
    /// Modified time as a Unix timestamp (seconds). `None` when unavailable.
    pub modified: Option<i64>,
}
