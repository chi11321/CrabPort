use crabport_terminal::terminal::{SftpTransferKind, SftpTransferStage};

use super::handle::SftpTransferHandle;
use super::path::{remote_tmp_path, shell_quote};
use crate::monitor::exec_with_status;

/// Download a single remote file into `local_path` using gzip staging.
///
/// Steps:
///   1. `ssh exec`: `gzip -c -- <remote> > /tmp/crabport-XXXX.gz`
///   2. SFTP-stream the .gz down with in-flight gunzip into `local_path`.
///   3. `ssh exec`: `rm -f -- /tmp/crabport-XXXX.gz`
pub(crate) async fn sftp_download_file_impl(
    backend: &SftpTransferHandle,
    remote_path: &str,
    local_path: &str,
    _original_size: u64,
) -> anyhow::Result<()> {
    // gzip-staging download flow:
    //   1. Remote `gzip -c` the file into a tmp `.gz` (off-loads compression
    //      to the server, doesn't touch the original).
    //   2. `download_file_gz` downloads the `.gz` via parallel segmented
    //      SFTP reads (full throughput) and decompresses locally — no
    //      network round-trip per decompressed chunk.
    //   3. Clean up the remote tmp.
    //
    // The previous streaming version (`GzipDecoder` over the live SFTP file)
    // was slow because each `decoder.read()` blocked on a network round-trip
    // and gzip can't be split into parallel segments. Splitting download and
    // decompress into two phases lets the download phase hit full SFTP
    // throughput (4× parallel `SSH_FXP_READ`).
    let tmp = remote_tmp_path();

    // Acquire the SSH handle for the exec step.
    let handle_guard = backend.handle.lock().await;
    let shared = handle_guard
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("SSH handle not available"))?
        .clone();
    drop(handle_guard);
    let h = shared.lock().await;

    // 1. Compress remotely into the tmp file.
    backend
        .emit_progress(
            SftpTransferKind::Download,
            SftpTransferStage::Compress,
            remote_path,
        )
        .await;
    let cmd = format!(
        "gzip -c -- {remote_q} > {tmp_q} && printf ok",
        remote_q = shell_quote(remote_path),
        tmp_q = shell_quote(&tmp),
    );
    #[cfg(debug_assertions)]
    tracing::info!("SFTP download file: compress cmd={cmd}");
    let (code, out) = exec_with_status(&h, &cmd).await;
    if code != 0 || !out.ends_with("ok") {
        return Err(anyhow::anyhow!("remote gzip failed (exit {code}): {out}"));
    }
    drop(h);

    // 2. Stat the remote `.gz` for the progress bar total.
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
    let res = s
        .download_file_gz(&tmp, local_path, Some(progress_cb))
        .await;
    #[cfg(debug_assertions)]
    if let Err(ref e) = res {
        tracing::warn!("SFTP download file: transfer failed: {e}");
    }
    backend.return_sftp(s, res).await?;

    // 3. Clean up the remote tmp regardless of step 2's outcome.
    backend
        .emit_progress(SftpTransferKind::Download, SftpTransferStage::CleanUp, &tmp)
        .await;
    let h = shared.lock().await;
    let _ = exec_with_status(&h, &format!("rm -f -- {}", shell_quote(&tmp))).await;
    Ok(())
}

/// Upload a single local file to `remote_path` using gzip staging.
///
/// Steps:
///   1. SFTP-stream-upload the local file with in-flight gzip into
///      `/tmp/crabport-XXXX.gz`.
///   2. `ssh exec`: `gunzip -c -- /tmp/crabport-XXXX.gz > <remote>`
///   3. `ssh exec`: `rm -f -- /tmp/crabport-XXXX.gz` (folded into step 2 on
///      success, run separately on failure)
pub(crate) async fn sftp_upload_file_impl(
    backend: &SftpTransferHandle,
    local_path: &str,
    remote_path: &str,
) -> anyhow::Result<()> {
    let tmp = remote_tmp_path();

    // Stat the local file so we have a byte total for the progress bar.
    // `upload_file_gz` reports bytes fed into the encoder (original size),
    // so the total is the original local file size.
    let total = std::fs::metadata(local_path).map(|m| m.len()).unwrap_or(0);
    let progress_cb = backend.make_byte_progress_cb(
        SftpTransferKind::Upload,
        SftpTransferStage::Transfer,
        local_path.to_string(),
        total,
    );
    progress_cb(0);

    // 1. Stream-compress the local file up to the remote tmp .gz.
    #[cfg(debug_assertions)]
    tracing::info!(
        "SFTP upload file: step 1 transfer+compress local={local_path} -> remote_tmp={tmp} total={total}"
    );
    let s = backend.take_or_open_sftp().await?;
    let upload_res = s.upload_file_gz(local_path, &tmp, Some(progress_cb)).await;
    backend.return_sftp(s, upload_res).await?;

    // Acquire the SSH handle for the exec step.
    let handle_guard = backend.handle.lock().await;
    let shared = handle_guard
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("SSH handle not available"))?
        .clone();
    drop(handle_guard);
    let h = shared.lock().await;

    // 2. Decompress remotely into the final destination.
    backend
        .emit_progress(
            SftpTransferKind::Upload,
            SftpTransferStage::Decompress,
            remote_path,
        )
        .await;
    let cmd = format!(
        "gunzip -c -- {tmp_q} > {remote_q} && rm -f -- {tmp_q} && printf ok",
        tmp_q = shell_quote(&tmp),
        remote_q = shell_quote(remote_path),
    );
    let (code, out) = exec_with_status(&h, &cmd).await;
    if code != 0 || !out.ends_with("ok") {
        // Best-effort cleanup of the tmp file on failure.
        backend
            .emit_progress(SftpTransferKind::Upload, SftpTransferStage::CleanUp, &tmp)
            .await;
        let _ = exec_with_status(&h, &format!("rm -f -- {}", shell_quote(&tmp))).await;
        return Err(anyhow::anyhow!("remote gunzip failed (exit {code}): {out}"));
    }

    Ok(())
}
