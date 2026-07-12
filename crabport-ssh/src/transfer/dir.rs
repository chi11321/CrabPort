use crabport_sftp::CrabPortSftp;
use crabport_terminal::terminal::{SftpTransferKind, SftpTransferStage};

use super::file::{sftp_download_file_impl, sftp_upload_file_impl};
use super::handle::SftpTransferHandle;
use super::path::{join_remote_path, remote_tmp_path, shell_quote, split_parent_basename};
use crate::monitor::exec_with_status;

/// Conventional shell exit code for "command not found". Used to detect
/// when the remote lacks `tar` so we can fall back to recursive SFTP.
const EXIT_COMMAND_NOT_FOUND: u32 = 127;

/// Download a remote directory into `local_path`.
///
/// Primary path (1A): `tar czf` remotely → SFTP download `.tar.gz` →
/// client `tar::Archive::unpack`.
///
/// Fallback (1B): if the remote `tar` is missing (exit 127), recurse via
/// pure SFTP `read_dir` + per-file [`sftp_download_file_impl`].
pub(crate) async fn sftp_download_dir_impl(
    backend: &SftpTransferHandle,
    remote_path: &str,
    local_path: &str,
) -> anyhow::Result<()> {
    match sftp_download_dir_via_tar(backend, remote_path, local_path).await {
        Ok(()) => Ok(()),
        Err(e) if e.downcast_ref::<RemoteCommandNotFound>().is_some() => {
            tracing::warn!(
                "SFTP download: remote tar unavailable, falling back to recursive SFTP ({e})"
            );
            sftp_download_dir_recursive(backend, remote_path, local_path).await
        }
        Err(e) => Err(e),
    }
}

/// Upload a local directory to `remote_path`.
///
/// Primary path (1A): client `tar+gz` → SFTP upload `.tar.gz` →
/// `tar xzf` remotely.
///
/// Fallback (1B): if the remote `tar` is missing (exit 127), recurse via
/// pure SFTP `create_dir` + per-file [`sftp_upload_file_impl`].
pub(crate) async fn sftp_upload_dir_impl(
    backend: &SftpTransferHandle,
    local_path: &str,
    remote_path: &str,
) -> anyhow::Result<()> {
    match sftp_upload_dir_via_tar(backend, local_path, remote_path).await {
        Ok(()) => Ok(()),
        Err(e) if e.downcast_ref::<RemoteCommandNotFound>().is_some() => {
            tracing::warn!(
                "SFTP upload: remote tar unavailable, falling back to recursive SFTP ({e})"
            );
            sftp_upload_dir_recursive(backend, local_path, remote_path).await
        }
        Err(e) => Err(e),
    }
}

/// Marker error type for "the remote shell reported command not found",
/// used to trigger the recursive-SFTP fallback. We implement `Error`
/// manually (rather than pulling in `thiserror`) so the dependency surface
/// stays minimal.
#[derive(Debug)]
pub(crate) struct RemoteCommandNotFound(pub String);

impl std::fmt::Display for RemoteCommandNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "remote command not found (exit 127): {}", self.0)
    }
}

impl std::error::Error for RemoteCommandNotFound {}

/// Directory download via remote `tar czf` + client unpack.
async fn sftp_download_dir_via_tar(
    backend: &SftpTransferHandle,
    remote_path: &str,
    local_path: &str,
) -> anyhow::Result<()> {
    let tmp = remote_tmp_path();

    // Split remote_path into parent + basename so we can run
    // `tar czf tmp -C <parent> <basename>`, which packs the directory
    // without its absolute path prefix.
    let (remote_parent, remote_base) = split_parent_basename(remote_path)?;

    let handle_guard = backend.handle.lock().await;
    let shared = handle_guard
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("SSH handle not available"))?
        .clone();
    drop(handle_guard);
    let h = shared.lock().await;

    // 1. Compress the remote directory into a tmp .tar.gz.
    backend
        .emit_progress(
            SftpTransferKind::Download,
            SftpTransferStage::Compress,
            remote_path,
        )
        .await;
    let cmd = format!(
        "tar czf {tmp_q} -C {parent_q} {base_q} && printf ok",
        tmp_q = shell_quote(&tmp),
        parent_q = shell_quote(&remote_parent),
        base_q = shell_quote(&remote_base),
    );
    #[cfg(debug_assertions)]
    tracing::info!("SFTP download dir: tar czf cmd={cmd}");
    let (code, out) = exec_with_status(&h, &cmd).await;
    if code == EXIT_COMMAND_NOT_FOUND {
        return Err(RemoteCommandNotFound(out).into());
    }
    if code != 0 || !out.ends_with("ok") {
        return Err(anyhow::anyhow!(
            "remote tar czf failed (exit {code}): {out}"
        ));
    }
    drop(h);

    // 2. Download + unpack. Stat the remote .tar.gz for the progress bar total.
    let total = {
        let s = backend.take_or_open_sftp().await?;
        let meta_res = s.metadata(&tmp).await;
        let total = match &meta_res {
            Ok(m) => m.size.unwrap_or(0),
            Err(_) => 0,
        };
        backend.return_sftp(s, meta_res.map(|_| ())).await?;
        total
    };
    let progress_cb = backend.make_byte_progress_cb(
        SftpTransferKind::Download,
        SftpTransferStage::Transfer,
        local_path.to_string(),
        total,
    );
    progress_cb(0);
    let s = backend.take_or_open_sftp().await?;
    // The tar archive's top-level entry is the remote basename, so unpacking
    // into `local_path` would add an extra nesting level. Unpack into
    // `local_path`'s parent instead.
    let unpack_dir = std::path::Path::new(local_path)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let unpack_dir_str = unpack_dir.to_string_lossy().into_owned();
    let res = s
        .download_dir(&tmp, &unpack_dir_str, Some(progress_cb))
        .await;
    backend.return_sftp(s, res).await?;

    // 3. Clean up the remote tmp.
    backend
        .emit_progress(SftpTransferKind::Download, SftpTransferStage::CleanUp, &tmp)
        .await;
    let h = shared.lock().await;
    let _ = exec_with_status(&h, &format!("rm -f -- {}", shell_quote(&tmp))).await;
    Ok(())
}

/// Directory upload via client `tar+gz` + remote `tar xzf`.
async fn sftp_upload_dir_via_tar(
    backend: &SftpTransferHandle,
    local_path: &str,
    remote_path: &str,
) -> anyhow::Result<()> {
    let tmp = remote_tmp_path();

    // The archive's top-level entry is the remote basename so that
    // `tar xzf tmp -C <remote_parent>` extracts to `<remote_parent>/<remote_base>`.
    let (remote_parent, remote_base) = split_parent_basename(remote_path)?;

    // 1. Client: build tar.gz and upload it to the remote tmp path.
    //    We don't know the compressed archive size until after `upload_dir`
    //    builds it internally, so pass total=0 — the progress bar will
    //    render indeterminate for directory uploads. (The byte counter still
    //    ticks, just without a meaningful percentage.)
    let progress_cb = backend.make_byte_progress_cb(
        SftpTransferKind::Upload,
        SftpTransferStage::Transfer,
        local_path.to_string(),
        0,
    );
    progress_cb(0);
    let s = backend.take_or_open_sftp().await?;
    let res = s
        .upload_dir(local_path, &tmp, &remote_base, Some(progress_cb))
        .await;
    backend.return_sftp(s, res).await?;

    // 2. Remote: ensure the target parent exists, then extract.
    backend
        .emit_progress(
            SftpTransferKind::Upload,
            SftpTransferStage::Decompress,
            remote_path,
        )
        .await;
    let handle_guard = backend.handle.lock().await;
    let shared = handle_guard
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("SSH handle not available"))?
        .clone();
    drop(handle_guard);
    let h = shared.lock().await;

    let cmd = format!(
        "mkdir -p {parent_q} && tar xzf {tmp_q} -C {parent_q} && rm -f -- {tmp_q} && printf ok",
        parent_q = shell_quote(&remote_parent),
        tmp_q = shell_quote(&tmp),
    );
    let (code, out) = exec_with_status(&h, &cmd).await;
    if code == EXIT_COMMAND_NOT_FOUND {
        // Best-effort cleanup of the tmp file before falling back.
        backend
            .emit_progress(SftpTransferKind::Upload, SftpTransferStage::CleanUp, &tmp)
            .await;
        let _ = exec_with_status(&h, &format!("rm -f -- {}", shell_quote(&tmp))).await;
        return Err(RemoteCommandNotFound(out).into());
    }
    if code != 0 || !out.ends_with("ok") {
        backend
            .emit_progress(SftpTransferKind::Upload, SftpTransferStage::CleanUp, &tmp)
            .await;
        let _ = exec_with_status(&h, &format!("rm -f -- {}", shell_quote(&tmp))).await;
        return Err(anyhow::anyhow!(
            "remote tar xzf failed (exit {code}): {out}"
        ));
    }

    Ok(())
}

/// Fallback directory download: recurse via pure SFTP, no remote `tar`.
async fn sftp_download_dir_recursive(
    backend: &SftpTransferHandle,
    remote_path: &str,
    local_path: &str,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(local_path)?;

    let s = backend.take_or_open_sftp().await?;
    // Borrow the session for the listing, then return it. We re-acquire per
    // file below (the per-file impl does its own take/return).
    let entries = s.read_dir(remote_path).await;
    // Capture whether the listing succeeded so we can return the session
    // without consuming the entries we want to iterate.
    let entries = match entries {
        Ok(e) => {
            backend.return_sftp(s, Ok(())).await?;
            e
        }
        Err(e) => {
            let msg = format!("remote read_dir failed: {e}");
            backend.return_sftp(s, Err(e)).await?;
            return Err(anyhow::anyhow!(msg));
        }
    };

    for entry in entries {
        if entry.name == "." || entry.name == ".." {
            continue;
        }
        let remote_child = join_remote_path(remote_path, &entry.name);
        let local_child = std::path::Path::new(local_path).join(&entry.name);
        if entry.is_dir {
            Box::pin(sftp_download_dir_recursive(
                backend,
                &remote_child,
                local_child.to_str().unwrap(),
            ))
            .await?;
        } else {
            // Recursive fallback: we don't pre-stat each child, so pass 0
            // as the size — the progress bar will render indeterminate
            // (no total) for these per-file transfers.
            sftp_download_file_impl(backend, &remote_child, local_child.to_str().unwrap(), 0)
                .await?;
        }
    }
    Ok(())
}

/// Fallback directory upload: recurse via pure SFTP, no remote `tar`.
async fn sftp_upload_dir_recursive(
    backend: &SftpTransferHandle,
    local_path: &str,
    remote_path: &str,
) -> anyhow::Result<()> {
    // Ensure the remote target directory exists.
    {
        let s = backend.take_or_open_sftp().await?;
        let res = s.create_dir(remote_path).await;
        // `create_dir` fails if the dir already exists — treat that as ok.
        let res = res.or_else(|e| {
            // SFTP returns Failure(4) for "failure" which covers
            // already-exists; we don't have the status code here so just
            // log and continue.
            tracing::debug!("remote mkdir {remote_path} returned {e} (assuming exists)");
            Ok(())
        });
        backend.return_sftp(s, res).await?;
    }

    let mut entries = tokio::fs::read_dir(local_path).await?;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        let local_child = entry.path();
        let remote_child = join_remote_path(remote_path, &name);
        let file_type = entry.file_type().await?;
        if file_type.is_dir() {
            Box::pin(sftp_upload_dir_recursive(
                backend,
                local_child.to_str().unwrap(),
                &remote_child,
            ))
            .await?;
        } else if file_type.is_file() {
            sftp_upload_file_impl(backend, local_child.to_str().unwrap(), &remote_child).await?;
        }
        // Symlinks and other types are skipped.
    }
    Ok(())
}
