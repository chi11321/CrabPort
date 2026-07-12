mod dir;
mod file;
mod handle;
mod path;

use crabport_sftp::CrabPortSftp;
use crabport_terminal::terminal::{BackendEvent, SftpTransferKind, SftpTransferStage};

use crate::monitor::exec_with_status;
use dir::RemoteCommandNotFound;
pub(crate) use dir::{sftp_download_dir_impl, sftp_upload_dir_impl};
pub(crate) use file::{sftp_download_file_impl, sftp_upload_file_impl};
pub(crate) use handle::SftpTransferHandle;
use path::{join_remote_path, remote_tmp_path, shell_quote, split_parent_basename};

/// Download `remote_path` into `local_path`.
///
/// Dispatches based on what `remote_path` is:
///   - regular file → single-file gzip staging ([`sftp_download_file_impl`])
///   - directory → tar.gz staging with recursive SFTP fallback
///     ([`sftp_download_dir_impl`])
pub(crate) async fn sftp_download_impl(
    backend: &SftpTransferHandle,
    remote_path: &str,
    local_path: &str,
) -> anyhow::Result<()> {
    #[cfg(debug_assertions)]
    tracing::info!("SFTP download impl: remote={remote_path} local={local_path}");
    let (is_dir, original_size) = {
        let s = backend.take_or_open_sftp().await?;
        let meta_res = s.metadata(remote_path).await;
        let (is_dir, size) = match meta_res {
            Ok(m) => {
                let is_dir = m.file_type().is_dir();
                let size = m.size.unwrap_or(0);
                (is_dir, size)
            }
            Err(e) => {
                let msg = format!("remote stat failed: {e}");
                backend
                    .return_sftp(s, Err(anyhow::anyhow!(msg.clone())))
                    .await
                    .ok();
                return Err(anyhow::anyhow!(msg));
            }
        };
        backend.return_sftp(s, Ok(())).await?;
        (is_dir, size)
    };

    if is_dir {
        sftp_download_dir_impl(backend, remote_path, local_path).await
    } else {
        sftp_download_file_impl(backend, remote_path, local_path, original_size).await
    }
}

/// Upload `local_path` to `remote_path`.
///
/// Dispatches based on what `local_path` is:
///   - regular file → single-file gzip staging ([`sftp_upload_file_impl`])
///   - directory → tar.gz staging with recursive SFTP fallback
///     ([`sftp_upload_dir_impl`])
pub(crate) async fn sftp_upload_impl(
    backend: &SftpTransferHandle,
    local_path: &str,
    remote_path: &str,
) -> anyhow::Result<()> {
    let meta = std::fs::metadata(local_path)?;
    if meta.is_dir() {
        sftp_upload_dir_impl(backend, local_path, remote_path).await
    } else {
        sftp_upload_file_impl(backend, local_path, remote_path).await
    }
}

/// Upload multiple local files to the remote in a single tar.gz transfer.
///
/// Steps:
///   1. Check if remote has `tar` (exec `which tar`).
///   2. If tar is available:
///      a. Create a local tmp tar.gz containing all the files.
///      b. Upload the tar.gz to a remote tmp path.
///      c. Extract on the remote: `tar xzf tmp -C <dest_parent>` for each file.
///      d. Clean up remote tmp.
///   3. If tar is NOT available, fall back to per-file `sftp_upload_impl`.
pub(crate) async fn sftp_upload_batch_impl(
    backend: &SftpTransferHandle,
    items: &[(String, String)],
) -> anyhow::Result<()> {
    if items.is_empty() {
        return Ok(());
    }

    // Check if remote has `tar` available.
    let tar_available = check_remote_tar(backend).await;

    if !tar_available {
        tracing::warn!(
            "SFTP batch upload: remote tar unavailable, falling back to per-file upload"
        );
        for (local_path, remote_path) in items {
            sftp_upload_impl(backend, local_path, remote_path).await?;
        }
        return Ok(());
    }

    sftp_upload_batch_via_tar(backend, items).await
}

/// Check if the remote has `tar` available by exec'ing `which tar`.
/// Returns `true` if tar is found (exit 0), `false` otherwise.
async fn check_remote_tar(backend: &SftpTransferHandle) -> bool {
    let handle_guard = backend.handle.lock().await;
    let Some(shared) = handle_guard.as_ref() else {
        return false;
    };
    let shared = shared.clone();
    drop(handle_guard);
    let h = shared.lock().await;
    let (code, _out) = exec_with_status(&h, "which tar").await;
    code == 0
}

/// Batch upload via client `tar+gz` + remote `tar xzf`.
///
/// All files are packed into a single local tar.gz (each entry named with
/// its remote basename), uploaded once via `upload_file`, then extracted
/// remotely with `tar xzf tmp -C <remote_parent>`.
async fn sftp_upload_batch_via_tar(
    backend: &SftpTransferHandle,
    items: &[(String, String)],
) -> anyhow::Result<()> {
    // All items share the same remote parent (the remote cwd). Extract the
    // parent from the first item's remote_path.
    let (remote_parent, _) = split_parent_basename(&items[0].1)?;
    let remote_tmp = remote_tmp_path();

    // 1. Build a local tmp tar.gz containing all files, each named with its
    //    remote basename so `tar xzf` places them correctly.
    backend
        .emit_progress(
            SftpTransferKind::Upload,
            SftpTransferStage::Compress,
            format!("batch upload ({} files)", items.len()),
        )
        .await;

    let local_tmp = build_batch_tar_gz(items)?;

    // 2. Upload the tar.gz via `upload_file` (data is already compressed,
    //    so no `upload_file_gz`).
    let total = std::fs::metadata(&local_tmp).map(|m| m.len()).unwrap_or(0);
    let progress_cb = backend.make_byte_progress_cb(
        SftpTransferKind::Upload,
        SftpTransferStage::Transfer,
        format!("batch upload ({} files)", items.len()),
        total,
    );
    progress_cb(0);
    let s = backend.take_or_open_sftp().await?;
    let local_tmp_str = local_tmp.to_string_lossy().into_owned();
    let res = s
        .upload_file(&local_tmp_str, &remote_tmp, Some(progress_cb))
        .await;
    backend.return_sftp(s, res).await?;

    // 3. Extract on the remote.
    backend
        .emit_progress(
            SftpTransferKind::Upload,
            SftpTransferStage::Decompress,
            &remote_parent,
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
        tmp_q = shell_quote(&remote_tmp),
    );
    let (code, out) = exec_with_status(&h, &cmd).await;
    if code == 127 {
        // Best-effort cleanup of the tmp file before falling back.
        backend
            .emit_progress(
                SftpTransferKind::Upload,
                SftpTransferStage::CleanUp,
                &remote_tmp,
            )
            .await;
        let _ = exec_with_status(&h, &format!("rm -f -- {}", shell_quote(&remote_tmp))).await;
        return Err(RemoteCommandNotFound(out).into());
    }
    if code != 0 || !out.ends_with("ok") {
        backend
            .emit_progress(
                SftpTransferKind::Upload,
                SftpTransferStage::CleanUp,
                &remote_tmp,
            )
            .await;
        let _ = exec_with_status(&h, &format!("rm -f -- {}", shell_quote(&remote_tmp))).await;
        return Err(anyhow::anyhow!(
            "remote tar xzf failed (exit {code}): {out}"
        ));
    }

    // 4. Clean up the local tmp file.
    let _ = std::fs::remove_file(&local_tmp);
    Ok(())
}

/// Build a local tar.gz containing multiple files from different local
/// paths. Each entry is named with its remote basename so that `tar xzf`
/// extracts them into the correct destination filenames.
fn build_batch_tar_gz(items: &[(String, String)]) -> anyhow::Result<std::path::PathBuf> {
    // Generate a unique local tmp path.
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let token = nanos ^ ((pid as u64) << 32) ^ (n << 16);
    let tmp = std::env::temp_dir().join(format!("crabport-batch-{token:016x}.tar.gz"));

    let file = std::fs::File::create(&tmp)?;
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);

    for (local_path, remote_path) in items {
        let local = std::path::Path::new(local_path);
        let basename = std::path::Path::new(remote_path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string());

        let meta = std::fs::metadata(local)?;
        if meta.is_dir() {
            builder.append_dir_all(&basename, local)?;
        } else {
            let mut f = std::fs::File::open(local)?;
            builder.append_file(&basename, &mut f)?;
        }
    }

    let encoder = builder.into_inner()?;
    encoder.finish()?;
    Ok(tmp)
}

/// Delete a remote file or directory. Stats the path first to choose
/// `remove_file` vs `remove_dir` — SFTP's `remove_dir` only works on empty
/// directories, so for non-empty dirs we fall back to a recursive walk that
/// deletes contents depth-first.
pub(crate) async fn sftp_delete_impl(
    backend: &SftpTransferHandle,
    remote_path: &str,
) -> anyhow::Result<()> {
    #[cfg(debug_assertions)]
    tracing::info!("SFTP delete: remote={remote_path}");
    let s = backend.take_or_open_sftp().await?;
    let meta_res = s.metadata(remote_path).await;
    let is_dir = match &meta_res {
        Ok(m) => m.file_type().is_dir(),
        Err(e) => {
            let msg = format!("remote stat failed: {e}");
            backend
                .return_sftp(s, Err(anyhow::anyhow!(msg.clone())))
                .await
                .ok();
            return Err(anyhow::anyhow!(msg));
        }
    };
    backend.return_sftp(s, meta_res.map(|_| ())).await?;

    if !is_dir {
        let s = backend.take_or_open_sftp().await?;
        let res = s.remove_file(remote_path).await;
        backend.return_sftp(s, res).await?;
        return Ok(());
    }

    let s = backend.take_or_open_sftp().await?;
    let direct = s.remove_dir(remote_path).await;
    let direct_ok = direct.is_ok();
    backend.return_sftp(s, direct).await.ok();
    if direct_ok {
        return Ok(());
    }

    sftp_delete_dir_recursive(backend, remote_path).await
}

/// Rename a remote file or directory. Thin wrapper around the SFTP
/// `rename` primitive — no staging or transfer involved.
pub(crate) async fn sftp_rename_impl(
    backend: &SftpTransferHandle,
    old_path: &str,
    new_path: &str,
) -> anyhow::Result<()> {
    #[cfg(debug_assertions)]
    tracing::info!("SFTP rename: old={old_path} new={new_path}");
    let s = backend.take_or_open_sftp().await?;
    let res = s.rename(old_path, new_path).await;
    let res = res.map_err(|e| anyhow::anyhow!("rename '{old_path}' -> '{new_path}' failed: {e}"));
    backend.return_sftp(s, res).await?;
    Ok(())
}

/// Open a remote file in the local OS default editor and re-upload on save.
///
/// Downloads the file to a temp path (preserving the extension so the OS
/// picks the right editor), launches the OS default app detached, then polls
/// the file's mtime once per second. On each detected change the file is
/// re-uploaded via [`sftp_upload_impl`]. The poll loop runs for a bounded
/// duration (30 min) and also stops early if the temp file disappears.
pub(crate) async fn sftp_edit_impl(
    backend: &SftpTransferHandle,
    remote_path: &str,
) -> anyhow::Result<()> {
    #[cfg(debug_assertions)]
    tracing::info!("SFTP edit: remote={remote_path}");

    // Derive a unique local temp path that preserves the file's extension so
    // the OS picks the right editor.
    let basename = std::path::Path::new(remote_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let token = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let local_path = std::env::temp_dir().join(format!("crabport-edit-{token}-{basename}"));
    let local_str = local_path.to_string_lossy().into_owned();

    // Download the file (gzip staging + decompression handled internally).
    backend
        .emit_progress(
            SftpTransferKind::Download,
            SftpTransferStage::Transfer,
            remote_path,
        )
        .await;
    let download_result = sftp_download_impl(backend, remote_path, &local_str).await;

    // Emit a Finished event for the download phase so the UI's progress log
    // clears immediately — the edit poll loop runs for up to 30 min, so
    // without this the toolbar would show a stale "downloading" state the
    // whole time. Uses the `Edit` kind so the UI stays silent on success
    // (no "download complete" toast for an edit flow).
    if let Some(tx) = backend.event_tx.as_ref() {
        let (success, message) = match &download_result {
            Ok(()) => (true, format!("opened {remote_path} in editor")),
            Err(e) => (false, format!("edit download failed: {e}")),
        };
        let _ = tx
            .broadcast(BackendEvent::SftpTransferFinished {
                kind: SftpTransferKind::Edit,
                success,
                message,
            })
            .await;
    }
    download_result?;

    // Record the initial mtime so we can detect saves.
    let mut last_mtime = std::fs::metadata(&local_path)
        .and_then(|m| m.modified())
        .ok();

    // Open in the OS default application, detached.
    spawn_open(&local_str);

    // Poll loop: detect mtime changes and re-upload. Bounded to 30 min.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30 * 60);
    loop {
        if std::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        // Stop early if the file was moved/deleted by the editor.
        let cur_mtime = match std::fs::metadata(&local_path) {
            Ok(m) => m.modified().ok(),
            Err(_) => break,
        };
        if cur_mtime != last_mtime {
            last_mtime = cur_mtime;
            #[cfg(debug_assertions)]
            tracing::info!("SFTP edit: re-uploading {remote_path}");
            // Emit a Transfer-stage progress event so the toolbar shows
            // "uploading" while the save is in flight.
            backend
                .emit_progress(
                    SftpTransferKind::Upload,
                    SftpTransferStage::Transfer,
                    remote_path,
                )
                .await;
            let upload_result = sftp_upload_impl(backend, &local_str, remote_path).await;
            // Emit a Finished event so the toolbar clears the progress chip.
            // Uses the `Edit` kind: success is silent, failure surfaces a
            // "save failed" toast.
            if let Some(tx) = backend.event_tx.as_ref() {
                let (success, message) = match &upload_result {
                    Ok(()) => (true, format!("saved {remote_path}")),
                    Err(e) => (false, format!("save failed: {e}")),
                };
                let _ = tx
                    .broadcast(BackendEvent::SftpTransferFinished {
                        kind: SftpTransferKind::Edit,
                        success,
                        message,
                    })
                    .await;
            }
            if let Err(e) = upload_result {
                #[cfg(debug_assertions)]
                tracing::warn!("SFTP edit: re-upload failed: {e}");
            }
        }
    }

    // Best-effort cleanup of the temp file.
    let _ = std::fs::remove_file(&local_path);
    Ok(())
}

/// Launch the OS default application for `path`, detached.
///
/// Some files (e.g. `.env`) have no associated application, in which case
/// macOS `open` exits with `kLSApplicationNotFoundErr`. We handle this by
/// falling back to `open -t` (open with the default text editor), then to
/// TextEdit explicitly. On Linux/Windows `xdg-open` / `start` already fall
/// back to a text editor for unknown types, so no extra handling is needed.
fn spawn_open(path: &str) {
    #[cfg(target_os = "macos")]
    {
        // 1. Try the default handler for the file type.
        let default = std::process::Command::new("open").arg(path).status();
        let opened = match default {
            Ok(s) if s.success() => true,
            _ => false,
        };
        if !opened {
            #[cfg(debug_assertions)]
            tracing::info!("SFTP edit: default open failed, trying text editor");
            // 2. `open -t` opens the file in the default text editor.
            let text = std::process::Command::new("open")
                .args(["-t"])
                .arg(path)
                .status();
            let opened = match text {
                Ok(s) if s.success() => true,
                _ => false,
            };
            if !opened {
                #[cfg(debug_assertions)]
                tracing::info!("SFTP edit: text editor open failed, trying TextEdit");
                // 3. Last resort: explicitly use TextEdit.
                let te = std::process::Command::new("open")
                    .args(["-a", "TextEdit"])
                    .arg(path)
                    .status();
                if let Err(e) = te {
                    #[cfg(debug_assertions)]
                    tracing::warn!("SFTP edit: failed to launch TextEdit: {e}");
                }
            }
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    let spawn_result = std::process::Command::new("xdg-open").arg(path).spawn();
    #[cfg(windows)]
    let spawn_result = std::process::Command::new("cmd")
        .args(["/C", "start", ""])
        .arg(path)
        .spawn();
    #[cfg(not(any(target_os = "macos", unix, windows)))]
    let spawn_result: Result<std::process::Child, std::io::Error> = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no editor launcher for this platform",
    ));

    #[cfg(not(target_os = "macos"))]
    if let Err(e) = spawn_result {
        #[cfg(debug_assertions)]
        tracing::warn!("SFTP edit: failed to launch editor: {e}");
    }
}

/// Recursively delete a non-empty remote directory: list entries, delete
/// each child (files directly, subdirs recursively), then remove the now-
/// empty directory itself. Depth-first so the final `remove_dir` succeeds.
async fn sftp_delete_dir_recursive(
    backend: &SftpTransferHandle,
    remote_path: &str,
) -> anyhow::Result<()> {
    let s = backend.take_or_open_sftp().await?;
    let entries_res = s.read_dir(remote_path).await;
    let entries = match entries_res {
        Ok(e) => {
            backend.return_sftp(s, Ok(())).await?;
            e
        }
        Err(e) => {
            let msg = format!("read_dir failed: {e}");
            backend.return_sftp(s, Err(e)).await.ok();
            return Err(anyhow::anyhow!(msg));
        }
    };

    for entry in entries {
        if entry.name == "." || entry.name == ".." {
            continue;
        }
        let child = join_remote_path(remote_path, &entry.name);
        if entry.is_dir {
            Box::pin(sftp_delete_dir_recursive(backend, &child)).await?;
        } else {
            let s = backend.take_or_open_sftp().await?;
            let res = s.remove_file(&child).await;
            backend.return_sftp(s, res).await?;
        }
    }

    // Now the directory should be empty — remove it.
    let s = backend.take_or_open_sftp().await?;
    let res = s.remove_dir(remote_path).await;
    backend.return_sftp(s, res).await?;
    Ok(())
}
