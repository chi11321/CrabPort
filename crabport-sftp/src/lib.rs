//! SFTP backend for CrabPort.
//!
//! Provides [`SftpBackend`], a thin wrapper over `russh-sftp` that adds
//! segmented / compressing file and directory transfers on top of the basic
//! SFTP operations defined by [`CrabPortSftp`].

mod api;
mod archive;
mod backend;
mod entry;
mod transfer;

pub use api::CrabPortSftp;
pub use backend::SftpBackend;
pub use entry::FileEntry;
