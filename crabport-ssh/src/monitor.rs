use std::sync::Arc;

use parking_lot::RwLock;
use russh::{
    Channel, ChannelMsg,
    client::{self, Msg},
};
use tokio::sync::Mutex as TokioMutex;

use crabport_terminal::terminal::{
    CpuStats, DiskStats, ExecCancel, ExecOutput, MemoryStats, NetworkStats, RemoteMetrics,
    RemoteStatus,
};

use crate::backend::MonitorState;
use crate::handler::SshHandler;
use crate::session::SshConnectionInfo;

// ---------------------------------------------------------------------------
// Monitor loop — periodically collects latency / memory / network via SSH exec
// ---------------------------------------------------------------------------

pub(crate) async fn monitor_loop(
    handle: Arc<TokioMutex<client::Handle<SshHandler>>>,
    _info: SshConnectionInfo,
    monitor: Arc<RwLock<MonitorState>>,
) {
    let mut prev_net_sent: u64 = 0;
    let mut prev_net_recv: u64 = 0;
    // The first sample has no previous counters to diff against, so it reports
    // the totals as "no rate" and only seeds them (same as before the
    // combined-exec change).
    let mut first_sample = true;

    // One combined exec per sample. The collectors used to run as five
    // separate `channel_open_session` + `exec` round trips (a latency probe,
    // `cat /proc/meminfo`, `cat /proc/loadavg`, `nproc`, `df`, `cat
    // /proc/net/dev`) while holding the shared handle lock. On a
    // high-latency link that is seconds of lock time per tick for data that
    // only feeds a status chip, and each exec blocks SFTP work sharing the
    // same handle. The commands are all cheap and read-only, so they run as
    // one shell command with markers between sections and are split apart
    // locally.
    const SAMPLE_CMD: &str = concat!(
        "echo __MEM__; cat /proc/meminfo 2>/dev/null; ",
        "echo __LOAD__; cat /proc/loadavg 2>/dev/null; ",
        "echo __NCPU__; (nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null); ",
        "echo __DISK__; df -kP 2>/dev/null; ",
        "echo __NET__; cat /proc/net/dev 2>/dev/null",
    );

    /// How often the remote metrics are sampled. The toolbar only shows a
    /// handful of numbers, and every sample costs an SSH exec plus
    /// (because the lock is shared) a window where SFTP and tunnel setup
    /// queue behind it — so this is deliberately not per-second.
    const SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

    loop {
        // Skip if disconnected — checked before the exec so a session that
        // already dropped does not pay for one more round trip.
        {
            let m = monitor.read();
            if m.status == RemoteStatus::Disconnected {
                return;
            }
        }

        let sample = {
            let h = handle.lock().await;
            exec_and_read_timed(&h, SAMPLE_CMD).await
        };

        let (latency_ms, output): (Option<u32>, String) = sample.unwrap_or_default();

        let sections = split_sections(&output);
        let memory = parse_meminfo(&sections.meminfo);
        let cpu = parse_cpu(
            &sections.loadavg,
            Some(sections.ncpus.trim()).filter(|s| !s.is_empty()),
        );
        let disk = parse_df(&sections.df);
        let network = parse_net_dev(&sections.netdev).and_then(|net| {
            if first_sample {
                prev_net_sent = net.bytes_sent;
                prev_net_recv = net.bytes_recv;
                return None;
            }
            let rate_sent = net.bytes_sent.saturating_sub(prev_net_sent);
            let rate_recv = net.bytes_recv.saturating_sub(prev_net_recv);
            prev_net_sent = net.bytes_sent;
            prev_net_recv = net.bytes_recv;
            Some(NetworkStats {
                bytes_sent: rate_sent,
                bytes_recv: rate_recv,
            })
        });
        first_sample = false;

        // ---- Update shared state ----
        {
            let mut m = monitor.write();
            m.metrics = RemoteMetrics {
                latency_ms,
                memory,
                cpu,
                disk,
                network,
            };
        }

        tokio::time::sleep(SAMPLE_INTERVAL).await;
    }
}

/// The sections of one combined sample command.
#[derive(Default)]
struct Sample {
    meminfo: String,
    loadavg: String,
    ncpus: String,
    df: String,
    netdev: String,
}

/// Split a combined `echo __MARK__` sample into its sections. Marker lines
/// are matched on the trimmed line, so ordinary output cannot be mistaken for
/// a section boundary.
fn split_sections(output: &str) -> Sample {
    let mut sample = Sample::default();
    let mut target: Option<&mut String> = None;
    for line in output.lines() {
        match line.trim() {
            "__MEM__" => {
                target = Some(&mut sample.meminfo);
                continue;
            }
            "__LOAD__" => {
                target = Some(&mut sample.loadavg);
                continue;
            }
            "__NCPU__" => {
                target = Some(&mut sample.ncpus);
                continue;
            }
            "__DISK__" => {
                target = Some(&mut sample.df);
                continue;
            }
            "__NET__" => {
                target = Some(&mut sample.netdev);
                continue;
            }
            _ => {}
        }
        if let Some(buf) = target.as_deref_mut() {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    sample
}

/// Parse `/proc/meminfo` output into memory stats. Values are in kB;
/// `used` is derived as `MemTotal - MemAvailable`. Returns `None` when
/// `MemTotal` is missing/zero (not a Linux-style meminfo).
fn parse_meminfo(output: &str) -> Option<MemoryStats> {
    let mut total: u64 = 0;
    let mut available: u64 = 0;

    for line in output.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        let value = parts[1].parse::<u64>().unwrap_or(0);
        // /proc/meminfo values are in kB
        if parts[0].starts_with("MemTotal") {
            total = value * 1024;
        } else if parts[0].starts_with("MemAvailable") {
            available = value * 1024;
        }
    }

    if total == 0 {
        return None;
    }

    Some(MemoryStats {
        total,
        used: total.saturating_sub(available),
    })
}

/// Derive the usage percentage from `/proc/loadavg` output plus the raw
/// CPU-count command output (`None` / unparseable → assume 1 core). See
/// the combined sample for the rationale behind the load-average proxy.
fn parse_cpu(loadavg: &str, ncpus_raw: Option<&str>) -> Option<CpuStats> {
    let first = loadavg.split_whitespace().next()?;
    let load = first.parse::<f32>().ok()?;

    let ncpus: f32 = ncpus_raw
        .and_then(|s| s.trim().parse::<f32>().ok())
        .map(|n| if n > 0.0 { n } else { 1.0 })
        .unwrap_or(1.0);

    let usage_pct = ((load / ncpus) * 100.0).clamp(0.0, 100.0);
    Some(CpuStats { usage_pct })
}

/// Parse `df -kP` output. Picks the row mounted at `/` (or the largest
/// filesystem as a fallback) and converts 1KB blocks to bytes.
fn parse_df(output: &str) -> Option<DiskStats> {
    // `df -kP` headers:
    // Filesystem 1024-blocks Used Available Capacity Mounted on
    // Parse every row into (mount_point, total_bytes, used_bytes).
    let mut rows: Vec<(&str, u64, u64)> = Vec::new();
    for line in output.lines().skip(1) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 6 {
            // Not a complete df row — skip. This also covers the rare
            // case where `df` wraps a long device path onto two lines.
            continue;
        }
        let mount = parts[parts.len() - 1];
        let total_kb = parts[parts.len() - 5].parse::<u64>().ok();
        let used_kb = parts[parts.len() - 4].parse::<u64>().ok();
        if let (Some(total), Some(used)) = (total_kb, used_kb) {
            rows.push((mount, total * 1024, used * 1024));
        }
    }
    // Prefer the row whose mount point is `/`; else the largest total.
    let pick = rows
        .iter()
        .find(|r| r.0 == "/")
        .copied()
        .or_else(|| rows.iter().max_by_key(|r| r.1).copied())?;
    if pick.1 == 0 {
        return None;
    }
    Some(DiskStats {
        total: pick.1,
        used: pick.2,
    })
}

/// Parse `/proc/net/dev` output, summing receive/transmit byte counters
/// across all interfaces.
fn parse_net_dev(output: &str) -> Option<NetworkStats> {
    let mut bytes_recv: u64 = 0;
    let mut bytes_sent: u64 = 0;

    for line in output.lines() {
        let trimmed = line.trim();
        // Skip header lines
        if !trimmed.contains(':') {
            continue;
        }
        let parts: Vec<&str> = trimmed.split(':').collect();
        if parts.len() < 2 {
            continue;
        }
        let fields: Vec<&str> = parts[1].split_whitespace().collect();
        if fields.len() < 10 {
            continue;
        }
        // Fields: receive bytes (0) | ... | transmit bytes (8) | ...
        bytes_recv += fields[0].parse::<u64>().unwrap_or(0);
        bytes_sent += fields[8].parse::<u64>().unwrap_or(0);
    }

    Some(NetworkStats {
        bytes_sent,
        bytes_recv,
    })
}

/// Like [`exec_and_read`], but also reports the time to the first output
/// byte — a round-trip measure suitable for the connection's latency chip.
///
/// The command's first line is echoed before any real work, so the first
/// packet back arrives one round trip after the exec, independent of how long
/// the command itself takes to run.
pub(crate) async fn exec_and_read_timed(
    handle: &client::Handle<SshHandler>,
    cmd: &str,
) -> Option<(Option<u32>, String)> {
    let start = std::time::Instant::now();
    let mut ch = handle.channel_open_session().await.ok()?;
    if ch.exec(true, cmd).await.is_err() {
        return None;
    }

    let mut first_byte_ms = None;
    let mut output = Vec::new();
    loop {
        match ch.wait().await {
            Some(ChannelMsg::Data { data }) => {
                if first_byte_ms.is_none() {
                    first_byte_ms = Some(start.elapsed().as_millis() as u32);
                }
                output.extend_from_slice(&data);
            }
            Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
            _ => {}
        }
    }

    Some((first_byte_ms, String::from_utf8(output).ok()?))
}

/// Execute a command over SSH and read all its stdout output.
pub(crate) async fn exec_and_read(
    handle: &client::Handle<SshHandler>,
    cmd: &str,
) -> Option<String> {
    let mut ch = handle.channel_open_session().await.ok()?;
    if ch.exec(true, cmd).await.is_err() {
        return None;
    }

    let mut output = Vec::new();
    loop {
        match ch.wait().await {
            Some(ChannelMsg::Data { data }) => {
                output.extend_from_slice(&data);
            }
            Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
            _ => {}
        }
    }

    String::from_utf8(output).ok()
}

/// Execute a command over SSH, returning `(exit_code, combined_stdout_stderr)`.
///
/// Used by the gzip-staged transfer methods to run `gzip`/`gunzip` on the
/// remote and verify they succeeded. stdout and stderr are merged so error
/// messages reach the caller regardless of which stream the server wrote
/// them to.
///
/// If the channel dies before delivering an `ExitStatus` (e.g. the server
/// killed the process), returns `(127, <captured output>)` — 127 is the
/// conventional "command not found / abnormal exit" code.
pub(crate) async fn exec_with_status(
    handle: &client::Handle<SshHandler>,
    cmd: &str,
) -> (u32, String) {
    let mut ch = match handle.channel_open_session().await {
        Ok(ch) => ch,
        Err(e) => return (127, format!("failed to open channel: {e}")),
    };
    if ch.exec(true, cmd).await.is_err() {
        return (127, "failed to start exec".to_string());
    }

    let mut output = Vec::new();
    // Default to 127 so a missing `ExitStatus` (e.g. the channel was closed
    // by the remote before reporting one) is treated as a failure rather
    // than silently succeeding. The actual exit status, when delivered,
    // overrides this below.
    let mut exit_code = 127;
    // Track whether we've seen an explicit `ExitStatus`. russh *can*
    // deliver `Eof` before `ExitStatus` on some servers, so we must not
    // break out of the loop on `Eof` alone — we keep draining until we
    // either see `ExitStatus` or the channel is fully closed (`Close`/
    // `None`). Without this, we'd return the 127 default for commands
    // that actually succeeded.
    let mut saw_exit_status = false;
    loop {
        match ch.wait().await {
            // russh delivers stdout and stderr on separate message variants
            // — capture both into the same buffer.
            Some(ChannelMsg::Data { data }) => output.extend_from_slice(&data),
            Some(ChannelMsg::ExtendedData { data, .. }) => output.extend_from_slice(&data),
            Some(ChannelMsg::ExitStatus { exit_status }) => {
                exit_code = exit_status;
                saw_exit_status = true;
            }
            // Only break once we've either seen the exit status or the
            // channel is fully closed. `Eof` alone is not enough — the
            // `ExitStatus` may still arrive afterwards.
            Some(ChannelMsg::Close) | None => break,
            Some(ChannelMsg::Eof) if saw_exit_status => break,
            _ => {}
        }
    }

    (exit_code, String::from_utf8_lossy(&output).into_owned())
}

/// Open a session channel and start `cmd` as an exec, returning the channel
/// to read — deliberately split from [`drain_exec_capture`] so callers can
/// drop the session-handle lock before the (possibly minutes-long) drain.
///
/// The command runs without a tty: anything that prompts for input fails or
/// waits for stdin that never comes, which is the intended safety net for
/// the agent's implicit execution mode.
pub(crate) async fn start_exec_capture(
    handle: &client::Handle<SshHandler>,
    cmd: &str,
) -> Result<Channel<Msg>, String> {
    let ch = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("failed to open channel: {e}"))?;
    ch.exec(true, cmd)
        .await
        .map_err(|e| format!("failed to start exec: {e}"))?;
    Ok(ch)
}

/// How often [`drain_exec_capture`] re-checks the cancel token while it
/// waits for more output. Bounds how long a stopped command keeps running.
const CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// Read a started exec channel to completion, giving up after `timeout` or
/// as soon as `cancel` is flipped.
///
/// stdout and stderr share the output buffer (in arrival order), matching
/// [`exec_with_status`]. On timeout or cancel the channel is closed — sshd
/// terminates the remote process when its channel goes away — and the
/// output captured so far is returned with [`ExecOutput::timed_out`] /
/// [`ExecOutput::cancelled`] set.
pub(crate) async fn drain_exec_capture(
    mut ch: Channel<Msg>,
    timeout: std::time::Duration,
    cancel: ExecCancel,
) -> ExecOutput {
    let mut output = Vec::new();
    let mut exit_code = None;
    let mut saw_exit_status = false;
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if cancel.is_cancelled() {
            let _ = ch.close().await;
            return ExecOutput {
                output: String::from_utf8_lossy(&output).into_owned(),
                exit_code: None,
                timed_out: false,
                cancelled: true,
            };
        }
        // Wake up regularly while waiting so a stopped command ends within a
        // poll tick instead of at its next (possibly never arriving)
        // message. Dropping `ch.wait()` is cancel-safe: it is a
        // `tokio::mpsc` receive underneath.
        let tick = (tokio::time::Instant::now() + CANCEL_POLL).min(deadline);
        let msg = match tokio::time::timeout_at(tick, ch.wait()).await {
            Ok(msg) => msg,
            Err(_) if tokio::time::Instant::now() >= deadline => {
                let _ = ch.close().await;
                return ExecOutput {
                    output: String::from_utf8_lossy(&output).into_owned(),
                    exit_code: None,
                    timed_out: true,
                    cancelled: false,
                };
            }
            // Poll tick: loop around to re-check the cancel token.
            Err(_) => continue,
        };
        match msg {
            // russh delivers stdout and stderr on separate message variants
            // — capture both into the same buffer.
            Some(ChannelMsg::Data { data }) => output.extend_from_slice(&data),
            Some(ChannelMsg::ExtendedData { data, .. }) => output.extend_from_slice(&data),
            Some(ChannelMsg::ExitStatus { exit_status }) => {
                exit_code = Some(exit_status);
                saw_exit_status = true;
            }
            // `Eof` can arrive before `ExitStatus` on some servers — keep
            // draining until the status or a full close (same reasoning as
            // `exec_with_status`).
            Some(ChannelMsg::Close) | None => break,
            Some(ChannelMsg::Eof) if saw_exit_status => break,
            _ => {}
        }
    }
    ExecOutput {
        output: String::from_utf8_lossy(&output).into_owned(),
        exit_code,
        timed_out: false,
        cancelled: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The combined sample command is split on its marker lines, and each
    /// section is handed to the same parser the separate execs used, so the
    /// merge cannot change what the toolbar shows.
    #[test]
    fn combined_sample_splits_into_sections() {
        let output = "\n__MEM__\nMemTotal:       16384000 kB\nMemAvailable:    8192000 kB\n\
                      __LOAD__\n0.50 0.40 0.30 1/234 5678\n\
                      __NCPU__\n4\n\
                      __DISK__\nFilesystem 1024-blocks Used Available Capacity Mounted on\n\
                      /dev/sda1     10240000 5120000   5120000      50% /\n\
                      __NET__\nInter-|   Receive\n face |bytes\n    lo: 100 2 0 0 0 0 0 0 200 3 0 0 0 0 0 0\n";
        let sections = split_sections(output);

        let mem = parse_meminfo(&sections.meminfo).expect("meminfo parses");
        assert_eq!(mem.total, 16384000 * 1024);
        assert_eq!(mem.used, (16384000 - 8192000) * 1024);

        let cpu = parse_cpu(
            &sections.loadavg,
            Some(sections.ncpus.trim()).filter(|s| !s.is_empty()),
        )
        .expect("cpu parses");
        assert!((cpu.usage_pct - 12.5).abs() < 0.001);

        let disk = parse_df(&sections.df).expect("disk parses");
        assert_eq!(disk.total, 10240000 * 1024);

        let net = parse_net_dev(&sections.netdev).expect("network parses");
        assert_eq!(net.bytes_sent, 200);
        assert_eq!(net.bytes_recv, 100);
    }

    /// Output that never emits a marker yields empty sections rather than
    /// leaking one section's text into another.
    #[test]
    fn combined_sample_without_markers_is_empty() {
        let sections = split_sections("bash: cat: command not found\n");
        assert!(sections.meminfo.is_empty());
        assert!(sections.loadavg.is_empty());
        assert!(sections.df.is_empty());
        assert!(parse_meminfo(&sections.meminfo).is_none());
    }

    #[test]
    fn meminfo_parses_total_and_available() {
        let out = "MemTotal:       16384000 kB\n\
                   MemFree:         1024000 kB\n\
                   MemAvailable:    8192000 kB\n\
                   Buffers:          512000 kB\n";
        let m = parse_meminfo(out).expect("parses");
        assert_eq!(m.total, 16_384_000 * 1024);
        assert_eq!(m.used, (16_384_000 - 8_192_000) * 1024);
    }

    #[test]
    fn meminfo_without_total_is_none() {
        // e.g. a BSD host without /proc — cat prints an error to stderr and
        // nothing usable on stdout.
        assert!(parse_meminfo("").is_none());
        assert!(parse_meminfo("cat: /proc/meminfo: No such file\n").is_none());
    }

    #[test]
    fn cpu_usage_is_load_over_cores_clamped() {
        // load 2.0 on 4 cores → 50%.
        let c = parse_cpu("2.00 1.50 1.00 2/345 6789\n", Some("4\n")).unwrap();
        assert!((c.usage_pct - 50.0).abs() < 0.01);

        // Unknown core count assumes 1 core; load 0.5 → 50%.
        let c = parse_cpu("0.50 0.40 0.30 1/100 42\n", None).unwrap();
        assert!((c.usage_pct - 50.0).abs() < 0.01);

        // Overload clamps at 100%.
        let c = parse_cpu("64.0 60.0 55.0 9/999 1\n", Some("2")).unwrap();
        assert_eq!(c.usage_pct, 100.0);

        // Garbage load → None.
        assert!(parse_cpu("not-a-number", Some("4")).is_none());
        assert!(parse_cpu("", Some("4")).is_none());
    }

    #[test]
    fn df_prefers_root_mount() {
        let out = "Filesystem     1024-blocks     Used Available Capacity Mounted on\n\
                   /dev/sda1         102400      51200     51200      50% /\n\
                   /dev/sdb1        1024000     102400    921600      10% /data\n";
        let d = parse_df(out).expect("parses");
        assert_eq!(d.total, 102_400 * 1024);
        assert_eq!(d.used, 51_200 * 1024);
    }

    #[test]
    fn df_without_root_falls_back_to_largest() {
        let out = "Filesystem 1024-blocks Used Available Capacity Mounted-on\n\
                   tank/a          1000  100       900      10% /a\n\
                   tank/b          9000  900      8100      10% /b\n";
        let d = parse_df(out).expect("parses");
        assert_eq!(d.total, 9000 * 1024);
        assert_eq!(d.used, 900 * 1024);
    }

    #[test]
    fn df_with_no_rows_is_none() {
        assert!(parse_df("Filesystem 1024-blocks Used Available Capacity Mounted on\n").is_none());
        assert!(parse_df("").is_none());
    }

    #[test]
    fn net_dev_sums_all_interfaces() {
        let out = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo:    1000      10    0    0    0     0          0         0     2000      20    0    0    0     0       0          0
  eth0:    5000      50    0    0    0     0          0         0     7000      70    0    0    0     0       0          0
";
        let n = parse_net_dev(out).expect("parses");
        assert_eq!(n.bytes_recv, 6000);
        assert_eq!(n.bytes_sent, 9000);
    }

    #[test]
    fn net_dev_headers_only_sums_to_zero() {
        // Header lines carry a ':' inside "Inter-|" — the parser must not
        // treat them as interface rows.
        let n = parse_net_dev("Inter-|   Receive\n face |bytes packets\n").unwrap();
        assert_eq!(n.bytes_recv, 0);
        assert_eq!(n.bytes_sent, 0);
    }
}
