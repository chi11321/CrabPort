//! Timeouts and result caps for the agent's tools, in one place.

/// How many lines of terminal output a `terminal_read` (and the execute
/// output wait) samples. Plenty for a screenful plus recent scrollback.
pub const AGENT_READ_LINES: usize = 400;

/// How often the execute wait re-samples the terminal's output.
pub const OUTPUT_POLL: std::time::Duration = std::time::Duration::from_millis(120);
/// How long the output must stop changing before a command counts as done.
pub const OUTPUT_QUIET: std::time::Duration = std::time::Duration::from_millis(360);
/// Upper bound on the wait: a command that keeps printing (a log tail, a
/// build) gets this long before its output-so-far is handed over.
pub const OUTPUT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);

/// Cap on the text one tool result may carry back to the model, in bytes. A
/// command that prints a whole file would otherwise fill the conversation's
/// context with one answer; the *tail* is kept, which is where a command's
/// interesting output (errors, the final summary) usually is.
pub const MAX_TOOL_RESULT_BYTES: usize = 16 * 1024;

/// How long a captured (`terminal_exec`) command may run before its backend
/// gives up and hands over the output so far. Generous enough for a normal
/// non-interactive command, bounded so a `tail -f`-style slip-up cannot
/// wedge the conversation forever.
pub const EXEC_CAPTURE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Most bytes a `read_file` loads before the result is capped for the model.
/// Big enough for any source file or config, small enough that pointing the
/// tool at a video cannot stall the panel.
pub const READ_FILE_LIMIT: u64 = 256 * 1024;

/// Most entries a `read_directory` lists before it says there were more.
pub const READ_DIR_LIMIT: usize = 500;

/// `fetch`'s overall HTTP timeout — a hung request must not wedge the
/// conversation any more than a hung command may.
pub const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How long `sftp_list` waits for the backend's navigation to land before it
/// reports the directory as unreadable (the backend logs failures silently).
/// The first listing on a session also pays the SFTP subsystem handshake, so
/// this is generous.
pub const SFTP_LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long an SFTP transfer may run before the tool call reports what has
/// happened so far. Long — transfers can genuinely take minutes — but bounded.
pub const SFTP_TRANSFER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// How long a tunnel's start may stay in "starting" before `tunnel_open`
/// reports it failed.
pub const TUNNEL_START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
