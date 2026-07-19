use std::sync::Arc;

use parking_lot::RwLock;
use russh::{ChannelMsg, client};
use tokio::sync::Mutex as TokioMutex;

use crabport_terminal::terminal::{
    CpuStats, DiskStats, MemoryStats, NetworkStats, RemoteMetrics, RemoteStatus,
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

    // ---- First collection immediately on connection ----
    {
        let h = handle.lock().await;
        let latency_ms = measure_latency(&h).await;
        let memory = collect_memory(&h).await;
        let cpu = collect_cpu(&h).await;
        let disk = collect_disk(&h).await;
        let network = collect_network(&h).await;

        if let Some(net) = network {
            prev_net_sent = net.bytes_sent;
            prev_net_recv = net.bytes_recv;
        }

        let mut m = monitor.write();
        m.metrics = RemoteMetrics {
            latency_ms,
            memory,
            cpu,
            disk,
            network: None, // No rate on first tick
        };
    }

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        // Skip if disconnected
        {
            let m = monitor.read();
            if m.status == RemoteStatus::Disconnected {
                return;
            }
        }

        let h = handle.lock().await;

        // ---- Latency: measure RTT of a small exec command ----
        let latency_ms = measure_latency(&h).await;

        // ---- Memory: parse /proc/meminfo ----
        let memory = collect_memory(&h).await;

        // ---- CPU: parse /proc/loadavg (portable across Linux/macOS) ----
        let cpu = collect_cpu(&h).await;

        // ---- Disk: stat the root filesystem via `df` ----
        let disk = collect_disk(&h).await;

        // ---- Network: parse /proc/net/dev ----
        let raw_network = collect_network(&h).await;
        let network = raw_network.map(|net| {
            let rate_sent = net.bytes_sent.saturating_sub(prev_net_sent);
            let rate_recv = net.bytes_recv.saturating_sub(prev_net_recv);
            prev_net_sent = net.bytes_sent;
            prev_net_recv = net.bytes_recv;
            NetworkStats {
                bytes_sent: rate_sent,
                bytes_recv: rate_recv,
            }
        });

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
    }
}

/// Measure round-trip latency by executing `echo ping` over SSH.
pub(crate) async fn measure_latency(handle: &client::Handle<SshHandler>) -> Option<u32> {
    let start = std::time::Instant::now();
    match handle.channel_open_session().await {
        Ok(mut ch) => {
            if ch.exec(true, "echo ping").await.is_err() {
                return None;
            }
            // Drain output until channel closes
            loop {
                match ch.wait().await {
                    Some(ChannelMsg::Data { .. }) => {}
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                    _ => {}
                }
            }
            let elapsed = start.elapsed().as_millis() as u32;
            Some(elapsed)
        }
        Err(_) => None,
    }
}

/// Collect remote memory stats via `cat /proc/meminfo`.
async fn collect_memory(handle: &client::Handle<SshHandler>) -> Option<MemoryStats> {
    let output = exec_and_read(handle, "cat /proc/meminfo").await?;
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

/// Collect remote CPU stats. We use `/proc/loadavg` because it's the most
/// portable single source across Linux distros and macOS-likes, and it
/// doesn't require a follow-up sample (unlike `/proc/stat`, which needs
/// two reads ~1s apart to compute a usage percentage).
///
/// `usage_pct` is derived as `load_avg * 100 / num_cpus`, clamped to
/// 100.0. This isn't a true instantaneous CPU usage, but it's a
/// reasonable proxy for "how busy is this box" and matches what
/// `htop`/`top` show in their headers. We compute the CPU count from
/// `nproc` (Linux) — failing that, we fall back to assuming the load is
/// per-core (load_avg_1m * 100).
///
/// We no longer surface the raw 1-minute load average as a separate
/// field — the toolbar only shows the percentage, so carrying the extra
/// number through `RemoteMetrics` was dead weight.
async fn collect_cpu(handle: &client::Handle<SshHandler>) -> Option<CpuStats> {
    let output = exec_and_read(handle, "cat /proc/loadavg").await?;
    let first = output.split_whitespace().next()?;
    let load = first.parse::<f32>().ok()?;

    // Try to get CPU count. `nproc` is universally available on Linux;
    // `sysctl -n hw.ncpu` on macOS/BSD. We try both sequentially.
    let ncpus: f32 = exec_and_read(handle, "nproc 2>/dev/null || sysctl -n hw.ncpu")
        .await
        .and_then(|s| s.trim().parse::<f32>().ok())
        .map(|n| if n > 0.0 { n } else { 1.0 })
        .unwrap_or(1.0);

    let usage_pct = ((load / ncpus) * 100.0).clamp(0.0, 100.0);
    Some(CpuStats { usage_pct })
}

/// Collect remote disk stats by parsing `df` output for the root
/// filesystem (`/`). We use `df -k -P` to get POSIX-compatible output:
/// `-k` forces 1KB blocks, `-P` keeps the line-format stable (no splitting
/// long device paths across two lines). We pick the row whose mount point
/// is exactly `/` because that's the most universal "primary disk" on a
/// remote server. If the root isn't present (unusual but possible on
/// BSD/macOS), we fall back to the row with the largest total size.
async fn collect_disk(handle: &client::Handle<SshHandler>) -> Option<DiskStats> {
    // `df -kP` headers:
    // Filesystem 1024-blocks Used Available Capacity Mounted on
    let output = exec_and_read(handle, "df -kP 2>/dev/null || df -kP /").await?;
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

/// Collect remote network stats via `cat /proc/net/dev`.
/// Sums across all interfaces.
async fn collect_network(handle: &client::Handle<SshHandler>) -> Option<NetworkStats> {
    let output = exec_and_read(handle, "cat /proc/net/dev").await?;
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
