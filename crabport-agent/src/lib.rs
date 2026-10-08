//! The CrabPort agent: everything the built-in assistant does *beyond*
//! talking — the prompt it runs under, the tools it may call, and the
//! implementations behind those tools (captured and visible command
//! execution, local file reads, HTTP fetches, tunnel lifecycle, SFTP).
//!
//! Free of GPUI by construction: the UI implements [`session::AgentSession`]
//! over its terminal view and shared handles, and [`agent::Agent`] runs the
//! tools — including their polling loops — on background threads. The UI only
//! awaits the result and renders it.
//!
//! Translations: `t!` resolves a crate-local backend, so this crate loads the
//! app's locale files itself (the keys are shared with `crabport-ui/i18n`;
//! `set_locale` is process-global, so language switches stay in sync).

rust_i18n::i18n!("../crabport-ui/i18n", fallback = "en");

pub mod agent;
pub mod compaction;
pub mod fetch;
pub mod limits;
pub mod local_fs;
pub mod prompt;
pub mod session;
pub mod sftp;
pub mod shell;
pub mod text;
pub mod tools;
pub mod tunnel;

pub use agent::{Agent, Executed, ToolOutcome};
pub use compaction::{
    COMPACTED_HEADER, COMPACTION_PROMPT, CompactCall, CompactTurn, TurnRole, compaction_cut,
    compaction_transcript,
};
pub use prompt::system_prompt;
pub use session::{AgentSession, RegistryTunnel};
pub use tools::{TOOL_NAMES, ToolKind, agent_tools};
