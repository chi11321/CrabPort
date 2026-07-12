use anyhow::Result;

/// SFTP operations over an existing SSH connection.
///
/// The segmented/compressing transfers (`download_file`, `upload_file`,
/// `upload_file_gz`, `download_file_gz`) are intentionally NOT part of this
/// trait: they depend on internals of [`crate::SftpBackend`] (access to the
/// underlying `SftpSession` for opening multiple file handles), and there is
/// currently no second implementation that would benefit from abstraction.
#[allow(async_fn_in_trait)]
pub trait CrabPortSftp: Send + Sync {
    /// Upload a file: create or overwrite `remote_path` with `data`.
    async fn write_file(&self, remote_path: &str, data: &[u8]) -> Result<()>;

    /// Download a file: read the entire contents of `remote_path`.
    async fn read_file(&self, remote_path: &str) -> Result<Vec<u8>>;

    /// Delete a remote file.
    async fn remove_file(&self, remote_path: &str) -> Result<()>;

    /// List directory entries. Returns a vec of [`crate::FileEntry`].
    async fn read_dir(&self, remote_path: &str) -> Result<Vec<crate::FileEntry>>;

    /// Create a directory on the remote host.
    async fn create_dir(&self, remote_path: &str) -> Result<()>;

    /// Remove a directory on the remote host.
    async fn remove_dir(&self, remote_path: &str) -> Result<()>;

    /// Check if a file or directory exists.
    async fn exists(&self, remote_path: &str) -> Result<bool>;

    /// Canonicalize (resolve) a path on the remote host.
    async fn canonicalize(&self, remote_path: &str) -> Result<String>;

    /// Rename/move a remote file or directory from `old_path` to `new_path`.
    /// The destination must not exist on most servers (SFTP `rename` is
    /// non-atomic and overwrites are server-dependent); callers should check
    /// `exists` first if they need overwrite semantics.
    async fn rename(&self, old_path: &str, new_path: &str) -> Result<()>;

    /// Close the SFTP session.
    async fn close(&self) -> Result<()>;
}
