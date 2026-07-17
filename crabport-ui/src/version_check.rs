//! Startup check for a newer GitHub release.
//!
//! [`check_for_updates`] spawns a background task that queries the GitHub
//! releases API, compares the latest `tag_name` against `CARGO_PKG_VERSION`
//! using `semver`, and — when a newer version exists — shows a toast via the
//! global [`NotificationController`] with a "详情" action that opens the
//! release page in the user's default browser.
//!
//! Notes:
//!
//! - The check is fire-and-forget; failures (network down, rate limit,
//!   unparseable JSON, …) are silently swallowed. The app must never fail to
//!   start because of this.
//! - GitHub's unauthenticated REST API requires a `User-Agent` header or it
//!   returns 403. We send a fixed string.
//! - The tag is expected to look like `v0.2.0`; a leading `v` is stripped
//!   before parsing with `semver::Version`. Tags that fail to parse are
//!   treated as "no update" rather than panicking.
//! - `Duration::ZERO` disables auto-dismiss so the toast stays until the
//!   user closes it — version updates shouldn't vanish from the screen.
//! - A stable `id("update-available")` means repeated checks (e.g. across
//!   multiple `wire()` calls — shouldn't happen, but defensive) won't stack
//!   duplicate toasts: the controller evicts the oldest entry when the cap
//!   is hit, and a shared id makes transition state behave predictably.

use std::time::Duration;

use gpui::*;
use rust_i18n::t;
use semver::Version;
use tracing::{info, warn};

use crate::components::notification::{Notification, NotificationController, NotificationLevel};

/// GitHub releases API endpoint for the single latest release.
const API_URL: &str = "https://api.github.com/repos/chi11321/CrabPort/releases/latest";

/// Browser-openable release page base (the API response also carries
/// `html_url`, but we keep a fallback in case the JSON lacks that field).
const RELEASE_PAGE_BASE: &str = "https://github.com/chi11321/CrabPort/releases/latest";

/// `User-Agent` header value. GitHub's API rejects requests without one.
const USER_AGENT: &str = concat!("CrabPort/", env!("CARGO_PKG_VERSION"));

/// Subset of the GitHub releases API response — we only read `tag_name`
/// and `html_url`. All other fields are ignored.
#[derive(serde::Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: Option<String>,
}

/// Entry point. Spawns the background check; the returned `Task` can be
/// detached by the caller (we detach internally so callers don't have to).
///
/// Pass the global notification controller entity so the spawned task can
/// push the update toast back onto the main thread when the check finishes.
pub fn check_for_updates(notifications: Entity<NotificationController>, cx: &mut App) {
    let current = match Version::parse(env!("CARGO_PKG_VERSION")) {
        Ok(v) => v,
        // Our own version doesn't parse — nothing to compare against.
        Err(e) => {
            warn!(
                "version_check: failed to parse CARGO_PKG_VERSION={:?}: {e}",
                env!("CARGO_PKG_VERSION")
            );
            return;
        }
    };
    info!(
        "version_check: starting check, current version = {}",
        current
    );

    cx.spawn(async move |cx| {
        let result = run_check(&current).await;
        match &result {
            Some(info) => {
                tracing::info!(
                    "version_check: update available ({} -> {}) at {}",
                    current,
                    info.latest,
                    info.html_url
                );
                // Show the toast on the main thread. We move the latest version
                // string + release url into the action closure so it can open
                // the browser without re-querying the API.
                let latest = info.latest.clone();
                let html_url = info.html_url.clone();
                let _ = cx.update(|cx| {
                    let action_url = html_url.clone();
                    let n =
                        Notification::new(t!("version_check.update_available.title").to_string())
                            .level(NotificationLevel::Warning)
                            .id("update-available")
                            .message(t!(
                                "version_check.update_available.message",
                                current = current.to_string().as_str(),
                                latest = latest.as_str()
                            ))
                            .action(
                                t!("version_check.action.view_details").to_string(),
                                move |_w, cx| {
                                    cx.open_url(&action_url);
                                },
                            )
                            .duration(Duration::ZERO);
                    notifications.update(cx, |c, cx| c.show(n, cx));
                });
            }
            None => {
                info!(
                    "version_check: no update (current={}, latest=unknown or equal)",
                    current
                );
            }
        }
    })
    .detach();
}

/// Owned result of a successful check — the latest version string (already
/// stripped of any `v` prefix) and the release's `html_url`.
struct ReleaseInfo {
    latest: String,
    html_url: String,
}

/// Perform the HTTP fetch + compare. Returns `Some(ReleaseInfo)` when the
/// latest GitHub release is strictly newer than `current`, `None` otherwise
/// (including on every error path — failures are silent).
async fn run_check(current: &Version) -> Option<ReleaseInfo> {
    // The HTTP GET runs in a blocking `ureq` call. We offload it to a
    // background executor thread so we don't stall the gpui main thread
    // while waiting on network I/O.
    let current = current.clone();
    let release = smol::unblock(move || {
        // `http_status_as_error(false)` so a 403/404 etc. returns `Ok(resp)`
        // and we can read the body to log the actual GitHub error message
        // (rate limit, blocked, etc.) instead of just "http status: 403".
        let mut resp = match ureq::get(API_URL)
            .header("User-Agent", USER_AGENT)
            .header("Accept", "application/vnd.github+json")
            .config()
            .http_status_as_error(false)
            .build()
            .call()
        {
            Ok(r) => r,
            Err(e) => {
                warn!("version_check: HTTP request failed: {e}");
                return None;
            }
        };
        let status = resp.status();
        // Read body first so we can log it on any non-OK status or parse error.
        let body = match resp
            .body_mut()
            .with_config()
            .limit(64 * 1024)
            .read_to_string()
        {
            Ok(s) => s,
            Err(e) => {
                warn!("version_check: failed to read response body (status={status}): {e}");
                return None;
            }
        };
        if !status.is_success() {
            warn!(
                "version_check: github returned status={status}, body: {}",
                body.chars().take(500).collect::<String>()
            );
            return None;
        }
        match serde_json::from_str::<GithubRelease>(&body) {
            Ok(r) => {
                info!(
                    "version_check: github returned tag_name={:?} html_url={:?}",
                    r.tag_name, r.html_url
                );
                Some(r)
            }
            Err(e) => {
                warn!(
                    "version_check: failed to parse github json ({}): {e}",
                    body.chars().take(500).collect::<String>()
                );
                None
            }
        }
    })
    .await?;

    let tag = release.tag_name.trim();
    let tag_stripped = tag.strip_prefix('v').unwrap_or(tag);
    let latest = match Version::parse(tag_stripped) {
        Ok(v) => v,
        Err(e) => {
            warn!("version_check: failed to parse tag={:?}: {e}", tag);
            return None;
        }
    };
    info!(
        "version_check: current={}, latest(from tag={:?})={}, newer={}",
        current,
        tag,
        latest,
        latest > current
    );
    if latest > current {
        Some(ReleaseInfo {
            latest: latest.to_string(),
            html_url: release
                .html_url
                .unwrap_or_else(|| RELEASE_PAGE_BASE.to_string()),
        })
    } else {
        None
    }
}
