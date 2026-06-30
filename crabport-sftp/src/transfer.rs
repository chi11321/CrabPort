use anyhow::{Result, anyhow};
use async_compression::tokio::write::GzipEncoder;
use std::io::Write;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::archive::{build_tar_gz, local_tmp_path, validate_gzip};
use crate::backend::SftpBackend;

/// Per-segment buffer size when streaming an upload. Larger buffers mean fewer
/// SFTP write requests but more memory; 256 KiB is a reasonable default that
/// also matches OpenSSH's typical advertised write length.
const UPLOAD_CHUNK_SIZE: usize = 256 * 1024;

impl SftpBackend {
    /// Download `remote_path` to a local file, using segmented parallel reads.
    ///
    /// The file is split into N segments fetched concurrently over the shared
    /// SFTP channel (each segment opens its own file handle on the same path
    /// and seeks to its offset). Small files fall back to a single sequential
    /// read.
    ///
    /// This relies on `russh-sftp`'s `File` sharing an `Arc<RawSftpSession>`
    /// internally, so multiple handles multiplex their `SSH_FXP_READ`
    /// requests over the same SFTP channel — no extra SSH channels are
    /// opened.
    pub async fn download_file(
        &self,
        remote_path: &str,
        local_path: &str,
        progress: Option<Arc<dyn Fn(u64) + Send + Sync>>,
    ) -> Result<()> {
        #[cfg(debug_assertions)]
        tracing::info!("SFTP download_file: remote={remote_path} local={local_path}");
        let session = self.session();

        // Stat first — we need the size to split into segments, and we want
        // to reject directories / unknown-size files early.
        #[cfg(debug_assertions)]
        let meta = session.metadata(remote_path).await?;
        #[cfg(debug_assertions)]
        let size = meta
            .size
            .ok_or_else(|| anyhow!("remote file size unknown; cannot segmented-download"))?;
        #[cfg(debug_assertions)]
        tracing::info!("SFTP download_file: remote size={size}");

        let mut local = tokio::fs::File::create(local_path).await?;
        let done = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));

        // Sequential read for both small and large files. The parallel
        // segmented path had data-corruption issues (likely a russh-sftp
        // seek/read race under concurrency); re-enable only after adding a
        // post-download integrity check.
        let mut file = session.open(remote_path).await?;
        let mut buf = vec![0u8; UPLOAD_CHUNK_SIZE];
        loop {
            let n = file.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            local.write_all(&buf[..n]).await?;
            done.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
            if let Some(ref cb) = progress {
                cb(done.load(std::sync::atomic::Ordering::Relaxed));
            }
        }
        local.flush().await?;
        #[cfg(debug_assertions)]
        tracing::info!(
            "SFTP download_file: wrote {} bytes",
            done.load(std::sync::atomic::Ordering::Relaxed)
        );
        Ok(())
    }

    /// Upload a local file to `remote_path` using chunked streaming writes.
    ///
    /// The SFTP layer pipelines writes internally (up to
    /// `max_concurrent_writes`), so we don't need to spawn parallel tasks
    /// here — we just need to avoid buffering the entire file in memory.
    pub async fn upload_file(
        &self,
        local_path: &str,
        remote_path: &str,
        progress: Option<Arc<dyn Fn(u64) + Send + Sync>>,
    ) -> Result<()> {
        let session = self.session();
        let mut local = tokio::fs::File::open(local_path).await?;
        let mut remote = session.create(remote_path).await?;

        let mut total: u64 = 0;
        let mut buf = vec![0u8; UPLOAD_CHUNK_SIZE];
        loop {
            let n = local.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            remote.write_all(&buf[..n]).await?;
            total += n as u64;
            if let Some(ref cb) = progress {
                cb(total);
            }
        }
        remote.flush().await?;
        // `shutdown` flushes pending write acks and closes the SFTP handle.
        let _ = remote.shutdown().await;
        Ok(())
    }

    /// Upload a local file, gzip-compressing it in transit. The remote file
    /// is stored **compressed** (i.e. the bytes on the server are a gzip
    /// stream), so callers should typically use a `.gz` suffix on
    /// `remote_path` to reflect that.
    ///
    /// `progress` reports compressed bytes written to the remote (the
    /// `.gz` size grows as we encode). The caller should set the progress
    /// total to the local file's original size — the compressed size is
    /// smaller, so the bar would undershoot; reporting original bytes
    /// uploaded-as-fed gives a meaningful 0→100% over the source file.
    pub async fn upload_file_gz(
        &self,
        local_path: &str,
        remote_path: &str,
        progress: Option<Arc<dyn Fn(u64) + Send + Sync>>,
    ) -> Result<()> {
        let session = self.session();
        let mut local = tokio::fs::File::open(local_path).await?;
        let remote = session.create(remote_path).await?;
        let mut encoder = GzipEncoder::new(remote);

        let mut total: u64 = 0;
        let mut buf = vec![0u8; UPLOAD_CHUNK_SIZE];
        loop {
            let n = local.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            encoder.write_all(&buf[..n]).await?;
            total += n as u64;
            if let Some(ref cb) = progress {
                cb(total);
            }
        }
        encoder.shutdown().await?;
        Ok(())
    }

    /// Download a remote file that was stored compressed (see
    /// [`SftpBackend::upload_file_gz`]) and decompress it in transit, writing
    /// the original bytes to `local_path`.
    ///
    /// This is the **two-phase** version: download the `.gz` to a local tmp
    /// file first (via the parallel-segmented [`SftpBackend::download_file`],
    /// so we get full SFTP throughput), then decompress locally with no
    /// network in the loop. The previous streaming version (`GzipDecoder`
    /// over the live SFTP file) was slow because each `decoder.read()`
    /// blocked on a network round-trip and the gzip format can't be split
    /// into parallel segments.
    ///
    /// `progress` reports compressed bytes transferred (matches the `.gz`
    /// size), so the caller should set the progress total to the `.gz` size,
    /// not the original size.
    pub async fn download_file_gz(
        &self,
        remote_path: &str,
        local_path: &str,
        progress: Option<Arc<dyn Fn(u64) + Send + Sync>>,
    ) -> Result<()> {
        #[cfg(debug_assertions)]
        tracing::info!("SFTP download_file_gz: remote={remote_path} local={local_path}");
        // 1. Download the .gz into a local tmp file.
        let tmp = local_tmp_path(".gz");
        let tmp_str = tmp.to_str().unwrap().to_string();
        if let Err(e) = self.download_file(remote_path, &tmp_str, progress).await {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }

        // 2. Decompress the local tmp .gz into the final target. Pure local
        //    I/O + CPU — no network round-trips.
        let decompress_result = self.decompress_local_gz(&tmp, local_path).await;
        #[cfg(debug_assertions)]
        if let Err(ref e) = decompress_result {
            tracing::warn!("SFTP download_file_gz: decompress failed: {e}");
        }

        // 3. Always remove the local tmp .gz.
        let _ = std::fs::remove_file(&tmp);
        decompress_result.map(|_| ())
    }

    /// Decompress a local `.gz` file into `out_path`, returning the number
    /// of decompressed bytes written. Uses `flate2`'s sync decoder over a
    /// `std::fs::File` — no async needed since it's all local I/O, and the
    /// sync path avoids the per-read `await` overhead that the tokio
    /// `GzipDecoder` would impose.
    async fn decompress_local_gz(&self, gz_path: &std::path::Path, out_path: &str) -> Result<u64> {
        // Run on the blocking pool so we don't stall the async runtime.
        let gz_path = gz_path.to_path_buf();
        let out_path = out_path.to_string();
        tokio::task::spawn_blocking(move || -> Result<u64> {
            let input = std::fs::File::open(&gz_path)?;
            let mut decoder = flate2::read::GzDecoder::new(std::io::BufReader::new(input));
            let mut output = std::fs::File::create(&out_path)?;
            let n = std::io::copy(&mut decoder, &mut output)?;
            output.flush()?;
            Ok(n)
        })
        .await?
    }

    /// Tar+gzip a local directory and upload the resulting `.tar.gz` stream
    /// to `remote_tar_gz`.
    ///
    /// This is the directory analogue of [`SftpBackend::upload_file_gz`]:
    /// the remote `remote_tar_gz` path ends up holding a tar.gz byte stream.
    /// The caller (typically `SshBackend`'s dir-upload orchestration) is
    /// responsible for extracting it on the remote via `tar xzf`.
    ///
    /// `archive_name` is the top-level entry name inside the archive. When
    /// the remote extracts with `tar xzf ... -C <remote_parent>`, the result
    /// is `<remote_parent>/<archive_name>/...`. Pass the **remote** target's
    /// basename here so the extracted directory has the name the caller
    /// asked for, regardless of the local source's name.
    ///
    /// Implementation: the `tar` crate is synchronous, so we build the
    /// archive into a local tmp file first (via `tar::Builder` + a sync
    /// `flate2::write::GzEncoder` over a `std::fs::File`), then stream-upload
    /// that tmp file via the existing [`SftpBackend::upload_file`]. This
    /// costs one local disk write of the compressed archive — an acceptable
    /// trade-off vs. writing an async `Write` bridge for `tar`.
    pub async fn upload_dir(
        &self,
        local_dir: &str,
        remote_tar_gz: &str,
        archive_name: &str,
        progress: Option<Arc<dyn Fn(u64) + Send + Sync>>,
    ) -> Result<()> {
        // 1. Build a tmp tar.gz of the local directory.
        let tmp = local_tmp_path(".tar.gz");
        let build_result = build_tar_gz(local_dir, archive_name, &tmp);
        if let Err(e) = build_result {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }

        // 2. Stream-upload the tmp file to the remote path. Use the
        //    non-gz `upload_file` because the data is already a tar.gz —
        //    wrapping it in another gzip layer would just waste CPU.
        let upload_result = self
            .upload_file(tmp.to_str().unwrap(), remote_tar_gz, progress)
            .await;

        // 3. Always remove the local tmp file, even on upload failure.
        let _ = std::fs::remove_file(&tmp);

        upload_result
    }

    /// Download a remote `.tar.gz` archive and extract it into `local_dir`.
    ///
    /// This is the directory analogue of [`SftpBackend::download_file_gz`].
    /// The caller (typically `SshBackend`'s dir-download orchestration) is
    /// responsible for creating the remote `.tar.gz` via `ssh exec`
    /// `tar czf` first.
    ///
    /// Implementation: same tmp-file approach as [`SftpBackend::upload_dir`]
    /// — download the archive to a local tmp file, then unpack with
    /// `tar::Archive`.
    pub async fn download_dir(
        &self,
        remote_tar_gz: &str,
        local_dir: &str,
        progress: Option<Arc<dyn Fn(u64) + Send + Sync>>,
    ) -> Result<()> {
        #[cfg(debug_assertions)]
        tracing::info!("SFTP download_dir: remote_tar_gz={remote_tar_gz} local_dir={local_dir}");
        // 1. Download the remote tar.gz into a local tmp file.
        let tmp = local_tmp_path(".tar.gz");
        let tmp_str = tmp.to_str().unwrap().to_string();
        if let Err(e) = self.download_file(remote_tar_gz, &tmp_str, progress).await {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }

        // Validate the downloaded .tar.gz is a valid gzip stream before
        // unpacking — catches corruption from SFTP read issues early.
        if let Err(e) = validate_gzip(&tmp) {
            let _ = std::fs::remove_file(&tmp);
            return Err(anyhow!("downloaded archive is corrupt: {e}"));
        }

        // 2. Ensure the destination directory exists, then unpack.
        let unpack_result = (|| -> Result<()> {
            std::fs::create_dir_all(local_dir)?;
            // Decompress .tar.gz into a plain .tar tmp file, then unpack.
            let tar_tmp = local_tmp_path(".tar");
            {
                let input = std::fs::File::open(&tmp)?;
                let mut decoder = flate2::read::GzDecoder::new(std::io::BufReader::new(input));
                let mut output = std::fs::File::create(&tar_tmp)?;
                std::io::copy(&mut decoder, &mut output)?;
                output.flush()?;
            }
            let mut archive = tar::Archive::new(std::fs::File::open(&tar_tmp)?);
            archive.unpack(local_dir)?;
            let _ = std::fs::remove_file(&tar_tmp);
            Ok(())
        })();
        #[cfg(debug_assertions)]
        if let Err(ref e) = unpack_result {
            tracing::warn!("SFTP download_dir: unpack failed: {e:#}");
        }

        // 3. Always remove the local tmp file.
        let _ = std::fs::remove_file(&tmp);

        unpack_result
    }
}
