//! `fetch` — one HTTP request through CrabPort's own client, optionally via
//! a proxy URL (a dynamic tunnel's socks5h endpoint is the usual case).

use crate::limits::FETCH_TIMEOUT;
use crate::text::cap_tool_result_head;

/// Most bytes of an HTTP body a `fetch` reads before capping. Bounds the
/// memory the read holds; the result is capped again for the model.
const FETCH_BODY_LIMIT: u64 = 256 * 1024;

/// Send one HTTP request with CrabPort's own HTTP client, optionally through
/// a proxy URL, and return the text to hand back to the model. Never fails:
/// every problem becomes readable text.
pub fn fetch_url(url: &str, proxy: Option<&str>, method: &str, body: Option<&str>) -> String {
    let mut config = ureq::Agent::config_builder().timeout_global(Some(FETCH_TIMEOUT));
    if let Some(proxy_url) = proxy {
        match ureq::Proxy::new(proxy_url) {
            Ok(proxy) => config = config.proxy(Some(proxy)),
            Err(err) => return format!("invalid proxy url: {err}"),
        }
    }
    let agent = ureq::Agent::new_with_config(config.build());

    let outcome = match method {
        "POST" => agent.post(url).send(body.unwrap_or_default()),
        "PUT" => agent.put(url).send(body.unwrap_or_default()),
        "DELETE" => agent.delete(url).call(),
        "HEAD" => agent.head(url).call(),
        _ => agent.get(url).call(),
    };
    match outcome {
        Ok(mut resp) => {
            let mut text = format!("HTTP {}\n", resp.status());
            for (name, value) in resp.headers() {
                text.push_str(&format!("{name}: {}\n", value.to_str().unwrap_or("…")));
            }
            match resp
                .body_mut()
                .with_config()
                .limit(FETCH_BODY_LIMIT)
                .read_to_string()
            {
                Ok(body_text) => {
                    text.push('\n');
                    text.push_str(&cap_tool_result_head(body_text.trim_end()));
                }
                Err(err) => text.push_str(&format!("\n(body unreadable: {err})")),
            }
            text
        }
        Err(err) => format!("request failed: {err}"),
    }
}
