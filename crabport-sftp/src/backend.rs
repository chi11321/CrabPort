use anyhow::Result;
use russh::client::Handle;
use russh_sftp::client::SftpSession;

use crate::CrabPortSftp;

/// SFTP backend that runs over an existing SSH `client::Handle`.
///
/// Generic over the handler type so it works with any russh client
/// implementation (e.g. `crabport-ssh::backend::SshHandler`).
pub struct SftpBackend {
    pub(crate) session: SftpSession,
}

impl SftpBackend {
    /// Open an SFTP subsystem channel on an existing SSH connection.
    ///
    /// The `handle` is the russh `client::Handle` obtained after
    /// authenticating. This function requests the "sftp" subsystem,
    /// then initialises the SFTP protocol on that channel.
    pub async fn connect<H>(handle: &Handle<H>) -> Result<Self>
    where
        H: russh::client::Handler + Send,
    {
        let channel = handle.channel_open_session().await?;
        channel.request_subsystem(true, "sftp").await?;

        let sftp = SftpSession::new(channel.into_stream()).await?;
        Ok(Self { session: sftp })
    }

    /// Borrow the underlying session. Used by the segmented/compressing
    /// transfer helpers in [`crate::transfer`].
    pub(crate) fn session(&self) -> &SftpSession {
        &self.session
    }

    /// Stat a remote path. Used by the upload/download dispatch to decide
    /// whether to take the file or directory code path.
    pub async fn metadata(&self, remote_path: &str) -> Result<russh_sftp::client::fs::Metadata> {
        Ok(self.session.metadata(remote_path).await?)
    }
}

impl CrabPortSftp for SftpBackend {
    async fn write_file(&self, remote_path: &str, data: &[u8]) -> Result<()> {
        self.session.write(remote_path, data).await?;
        Ok(())
    }

    async fn read_file(&self, remote_path: &str) -> Result<Vec<u8>> {
        let data = self.session.read(remote_path).await?;
        Ok(data)
    }

    async fn remove_file(&self, remote_path: &str) -> Result<()> {
        self.session.remove_file(remote_path).await?;
        Ok(())
    }

    async fn read_dir(&self, remote_path: &str) -> Result<Vec<crate::FileEntry>> {
        let entries = self.session.read_dir(remote_path).await?;
        let mut result = Vec::new();
        for entry in entries {
            let name = entry.file_name();
            let metadata = entry.metadata();
            let is_dir = metadata.is_dir();
            let size = metadata.size;
            let permissions = metadata
                .permissions
                .map(|mode| russh_sftp::protocol::FilePermissions::from(mode).to_string());
            // mtime is u32 seconds since epoch; convert to i64.
            let modified = metadata.mtime.map(|t| t as i64);
            result.push(crate::FileEntry {
                name,
                is_dir,
                size,
                permissions,
                modified,
            });
        }
        Ok(result)
    }

    async fn create_dir(&self, remote_path: &str) -> Result<()> {
        self.session.create_dir(remote_path).await?;
        Ok(())
    }

    async fn remove_dir(&self, remote_path: &str) -> Result<()> {
        self.session.remove_dir(remote_path).await?;
        Ok(())
    }

    async fn exists(&self, remote_path: &str) -> Result<bool> {
        let exists = self.session.try_exists(remote_path).await?;
        Ok(exists)
    }

    async fn canonicalize(&self, remote_path: &str) -> Result<String> {
        let resolved = self.session.canonicalize(remote_path).await?;
        Ok(resolved)
    }

    async fn rename(&self, old_path: &str, new_path: &str) -> Result<()> {
        self.session.rename(old_path, new_path).await?;
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        self.session.close().await?;
        Ok(())
    }
}
