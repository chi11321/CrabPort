//! Automatic context compaction: how big a conversation has grown, where to
//! cut it, and the transcript handed to the summarizer.
//!
//! UI-free by construction: callers map their own message representation into
//! [`CompactTurn`] slices, so one set of rules applies no matter what renders
//! the conversation.

use crabport_ai::{MESSAGE_OVERHEAD_TOKENS, TOOL_CALL_OVERHEAD_TOKENS, estimate_tokens};

/// Longest single tool result fed into a compaction transcript, in bytes. The
/// transcript is the *summarizer's input*, so a handful of huge command
/// outputs must not blow up the very request meant to shrink the
/// conversation; [`cap_for_summary`] keeps the tail of a longer result, where
/// a command's errors and summary usually are.
pub const COMPACT_RESULT_CAP: usize = 4 * 1024;

/// Instruction for a compaction round: what the summary must preserve so the
/// work can continue without the original messages.
pub const COMPACTION_PROMPT: &str = "\
You maintain the working memory of a terminal-operating agent. Summarize the \
conversation transcript below into notes for yourself, so the work can \
continue without the original messages. Preserve: the user's goals and \
constraints (and anything they asked you to remember), decisions made, the \
commands that ran with their key output (paths, errors, numbers), the state \
of anything still running, and open questions. Be specific and terse — no \
pleasantries, no restating the obvious. Reply with the summary only, as \
Markdown.";

/// Preamble for a compaction summary in the wire history. Spelled out so the
/// model reads it as context rather than as a fresh instruction.
pub const COMPACTED_HEADER: &str =
    "Summary of the earlier conversation, compacted to save context:\n\n";

/// Author of one turn in the compaction view.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TurnRole {
    User,
    Assistant,
    /// A summary standing in for everything older (already compacted once).
    Summary,
}

/// One tool call inside an assistant turn, as the compaction helpers see it.
pub struct CompactCall<'a> {
    pub name: &'a str,
    pub arguments: &'a str,
    /// The result the call produced, when it ran.
    pub result: Option<&'a str>,
}

/// One conversation turn, borrowed from the caller's own message type.
pub struct CompactTurn<'a> {
    pub role: TurnRole,
    pub content: &'a str,
    pub tool_calls: Vec<CompactCall<'a>>,
}

/// Rough token size of one turn, matching what its wire form costs: the
/// content plus every tool call's arguments and result. Reasoning is not
/// counted — it is display-only and never replayed.
pub fn message_tokens(turn: &CompactTurn<'_>) -> usize {
    let mut total = MESSAGE_OVERHEAD_TOKENS + estimate_tokens(turn.content);
    for call in &turn.tool_calls {
        total += TOOL_CALL_OVERHEAD_TOKENS
            + estimate_tokens(call.name)
            + estimate_tokens(call.arguments)
            + call.result.map(estimate_tokens).unwrap_or(0);
    }
    total
}

/// Index of the first turn to keep verbatim when compacting a conversation.
///
/// Walks backwards, keeping turns until roughly `keep_tokens` is reached, then
/// snaps the cut back to the user turn that opened that block: an assistant's
/// tool calls and their results must travel together, and the kept tail reads
/// best starting with the user's ask. `0` means "nothing old enough to
/// summarize" — the caller then leaves the history alone.
pub fn compaction_cut(turns: &[CompactTurn<'_>], keep_tokens: usize) -> usize {
    let mut used = 0usize;
    for (ix, turn) in turns.iter().enumerate().rev() {
        used += message_tokens(turn);
        if used >= keep_tokens {
            return turns[..=ix]
                .iter()
                .rposition(|turn| turn.role == TurnRole::User)
                .unwrap_or(0);
        }
    }
    0
}

/// Plain-text transcript of the turns being compacted, for the summarizer.
///
/// Tool results are capped ([`COMPACT_RESULT_CAP`], tail kept) so a few huge
/// command outputs can't make the compaction request larger than the
/// conversation it is meant to shrink.
pub fn compaction_transcript(turns: &[CompactTurn<'_>]) -> String {
    let mut out = String::new();
    for turn in turns {
        match turn.role {
            TurnRole::User => {
                out.push_str("USER: ");
                out.push_str(turn.content.trim());
                out.push('\n');
            }
            TurnRole::Summary => {
                out.push_str("SUMMARY SO FAR: ");
                out.push_str(turn.content.trim());
                out.push('\n');
            }
            TurnRole::Assistant => {
                if !turn.content.trim().is_empty() {
                    out.push_str("ASSISTANT: ");
                    out.push_str(turn.content.trim());
                    out.push('\n');
                }
                for call in &turn.tool_calls {
                    out.push_str("  [");
                    out.push_str(call.name);
                    out.push_str("] ");
                    out.push_str(call.arguments.trim());
                    if let Some(result) = call.result {
                        out.push_str(" -> ");
                        out.push_str(cap_for_summary(result.trim()));
                    }
                    out.push('\n');
                }
            }
        }
    }
    out
}

/// `text` shortened to [`COMPACT_RESULT_CAP`] from the front (keeping the
/// tail, where a command's errors and summary usually are) when it is longer.
pub fn cap_for_summary(text: &str) -> &str {
    if text.len() <= COMPACT_RESULT_CAP {
        return text;
    }
    let mut cut = text.len() - COMPACT_RESULT_CAP;
    while cut < text.len() && !text.is_char_boundary(cut) {
        cut += 1;
    }
    &text[cut..]
}

#[cfg(test)]
mod tests {
    use super::{
        COMPACT_RESULT_CAP, CompactCall, CompactTurn, TurnRole, compaction_cut,
        compaction_transcript, message_tokens,
    };

    fn msg(role: TurnRole, content: &'static str) -> CompactTurn<'static> {
        CompactTurn {
            role,
            content,
            tool_calls: Vec::new(),
        }
    }

    /// Compaction keeps the recent tail and snaps its cut back to a user
    /// turn, so an assistant's tool calls never get separated from the
    /// results that answer them.
    #[test]
    fn compaction_cut_keeps_recent_turns_and_snaps_to_user() {
        let turns = vec![
            msg(TurnRole::User, "u1"),
            msg(TurnRole::Assistant, "a1"),
            msg(TurnRole::User, "u2"),
            msg(TurnRole::Assistant, "a2"),
            msg(TurnRole::User, "u3"),
        ];
        // A tiny budget keeps only the newest user turn.
        assert_eq!(compaction_cut(&turns, 1), 4);
        // A budget covering `u3` and `a2` snaps back to the turn that opened
        // the block: `a2` must not travel without `u2`.
        let keep = message_tokens(&turns[4]) + message_tokens(&turns[3]);
        assert_eq!(compaction_cut(&turns, keep), 2);
        // Everything fits: nothing old enough to summarize.
        assert_eq!(compaction_cut(&turns, usize::MAX), 0);
        assert_eq!(compaction_cut(&[], 10), 0);
    }

    /// The transcript fed to the summarizer labels roles, carries tool calls
    /// with their results, and caps huge results so the compaction request
    /// cannot be bigger than the history it is shrinking.
    #[test]
    fn compaction_transcript_labels_roles_and_caps_results() {
        let long_result = "x".repeat(COMPACT_RESULT_CAP * 2);
        let assistant = CompactTurn {
            role: TurnRole::Assistant,
            content: "done",
            tool_calls: vec![CompactCall {
                name: "terminal_exec",
                arguments: r#"{"command":"ls"}"#,
                result: Some(&long_result),
            }],
        };
        let text = compaction_transcript(&[msg(TurnRole::User, "hi"), assistant]);
        assert!(text.contains("USER: hi"), "{text}");
        assert!(
            text.contains("[terminal_exec] {\"command\":\"ls\"} -> "),
            "{text}"
        );
        // The capped result must not carry more than one cap of text.
        assert!(!text.contains(&"x".repeat(COMPACT_RESULT_CAP + 1)));
    }
}
