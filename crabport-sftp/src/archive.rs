use anyhow::Result;

/// Build a `.tar.gz` of `local_dir` at `out_path`.
///
/// The archive's top-level entry is named `archive_name`, so unpacking with
/// `tar xzf ... -C <dst>` yields `<dst>/<archive_name>/...` rather than
/// flattening the contents into `<dst>/...`.
pub(crate) fn build_tar_gz(
    local_dir: &str,
    archive_name: &str,
    out_path: &std::path::Path,
) -> Result<()> {
    let file = std::fs::File::create(out_path)?;
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    builder.append_dir_all(archive_name, local_dir)?;
    // Finish the tar header stream, then flush the gzip encoder.
    let encoder = builder.into_inner()?;
    encoder.finish()?;
    Ok(())
}

/// Build a unique local tmp path with the given suffix.
///
/// Uses a `crabport-` prefix and a per-process counter + timestamp so two
/// concurrent transfers don't collide. Lives under `std::env::temp_dir()`.
pub(crate) fn local_tmp_path(suffix: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let token = nanos ^ ((pid as u64) << 32) ^ (n << 16);
    std::env::temp_dir().join(format!("crabport-{token:016x}{suffix}"))
}

/// Validate that `path` is a complete, decodable gzip stream. Catches the
/// common SFTP-corruption case where the downloaded bytes aren't actually
/// gzip at all (e.g. a seek/read offset bug returns wrong data).
///
/// Reads the whole file through a `GzDecoder` and checks the CRC + size
/// trailer — if either mismatches, the stream was truncated or corrupt.
pub(crate) fn validate_gzip(path: &std::path::Path) -> Result<()> {
    let file = std::fs::File::open(path)?;
    let mut decoder = flate2::read::GzDecoder::new(std::io::BufReader::new(file));
    // Drain the decoder fully — `GzDecoder` verifies the CRC32 and isize
    // trailer when it reaches EOF, returning a `CorruptGzipStream` error
    // if they don't match.
    let mut sink = std::io::sink();
    std::io::copy(&mut decoder, &mut sink)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "crabport-archive-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn tar_gz_roundtrip_preserves_layout_and_contents() {
        let src = tmp_dir("src");
        std::fs::write(src.join("a.txt"), b"hello").unwrap();
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("sub/b.txt"), b"world").unwrap();

        let out = local_tmp_path(".tar.gz");
        build_tar_gz(src.to_str().unwrap(), "bundle", &out).unwrap();

        // The produced stream is complete, valid gzip.
        validate_gzip(&out).unwrap();

        // Unpack and verify the top-level entry name + file contents.
        let dst = tmp_dir("dst");
        let file = std::fs::File::open(&out).unwrap();
        let decoder = flate2::read::GzDecoder::new(file);
        tar::Archive::new(decoder).unpack(&dst).unwrap();
        assert_eq!(
            std::fs::read(dst.join("bundle/a.txt")).unwrap(),
            b"hello"
        );
        assert_eq!(
            std::fs::read(dst.join("bundle/sub/b.txt")).unwrap(),
            b"world"
        );

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn validate_gzip_rejects_garbage_and_truncation() {
        // Not gzip at all.
        let garbage = local_tmp_path(".bin");
        std::fs::write(&garbage, b"just some text, no gzip magic").unwrap();
        assert!(validate_gzip(&garbage).is_err());
        let _ = std::fs::remove_file(&garbage);

        // Valid gzip cut short → CRC/size trailer check must fail.
        let src = tmp_dir("trunc-src");
        std::fs::write(src.join("f.txt"), vec![b'x'; 65536]).unwrap();
        let full = local_tmp_path(".tar.gz");
        build_tar_gz(src.to_str().unwrap(), "t", &full).unwrap();
        let bytes = std::fs::read(&full).unwrap();
        let truncated = local_tmp_path(".tar.gz");
        std::fs::write(&truncated, &bytes[..bytes.len() / 2]).unwrap();
        assert!(validate_gzip(&truncated).is_err());

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_file(&full);
        let _ = std::fs::remove_file(&truncated);
    }

    #[test]
    fn local_tmp_paths_are_unique_and_carry_suffix() {
        let a = local_tmp_path(".tar.gz");
        let b = local_tmp_path(".tar.gz");
        assert_ne!(a, b);
        assert!(a.file_name().unwrap().to_str().unwrap().starts_with("crabport-"));
        assert!(a.to_str().unwrap().ends_with(".tar.gz"));
    }
}
