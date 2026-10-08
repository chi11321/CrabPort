//! Truncation rules for tool results: what goes back to the model when a
//! command, file or HTTP body is bigger than the conversation should carry.

use crate::limits::MAX_TOOL_RESULT_BYTES;

/// The part of `after` that appeared since `before`, used to answer an
/// `execute` call with the command's own output instead of the whole screen.
///
/// Terminals scroll, so the interesting text is the suffix the two dumps
/// don't share; the common prefix is computed on bytes and then walked back
/// to a char boundary so the slice can't panic on multi-byte output. The
/// result is capped at [`MAX_TOOL_RESULT_BYTES`] (keeping the tail, which is
/// where the interesting part of a long output lives) so a chatty command
/// can't blow up the conversation's context.
pub fn new_since(before: &str, after: &str) -> String {
    let before = before.as_bytes();
    let after_bytes = after.as_bytes();
    let mut ix = 0;
    while ix < before.len() && ix < after_bytes.len() && before[ix] == after_bytes[ix] {
        ix += 1;
    }
    while ix > 0 && !after.is_char_boundary(ix) {
        ix -= 1;
    }
    cap_tool_result(after[ix..].trim_matches('\n'))
}

/// Cap a tool result at [`MAX_TOOL_RESULT_BYTES`], keeping the tail (where a
/// command's errors and summary usually are) and noting how much was dropped.
/// The cut is walked to a char boundary so the slice can't panic on
/// multi-byte output.
pub fn cap_tool_result(text: &str) -> String {
    if text.len() <= MAX_TOOL_RESULT_BYTES {
        return text.to_string();
    }
    let mut cut = text.len() - MAX_TOOL_RESULT_BYTES;
    while cut < text.len() && !text.is_char_boundary(cut) {
        cut += 1;
    }
    format!("(earlier output omitted — {} bytes)\n{}", cut, &text[cut..])
}

/// Cap a tool result at [`MAX_TOOL_RESULT_BYTES`] keeping the *head* — used
/// where the beginning of the text is the interesting part (HTTP bodies, file
/// contents) — and note how much was dropped. The cut is walked back to a
/// char boundary so the slice can't panic on multi-byte output.
pub fn cap_tool_result_head(text: &str) -> String {
    if text.len() <= MAX_TOOL_RESULT_BYTES {
        return text.to_string();
    }
    let mut end = MAX_TOOL_RESULT_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n(later output omitted — {} bytes)",
        &text[..end],
        text.len() - end
    )
}

#[cfg(test)]
mod tests {
    use super::{cap_tool_result, cap_tool_result_head};
    use crate::limits::MAX_TOOL_RESULT_BYTES;

    #[test]
    fn cap_tool_result_keeps_the_tail_on_a_char_boundary() {
        let short = "hello";
        assert_eq!(cap_tool_result(short), short);

        // Multi-byte output longer than the cap: the cut must not split a
        // char, and it must land near the end.
        let long = "⇒".repeat(MAX_TOOL_RESULT_BYTES);
        let capped = cap_tool_result(&long);
        assert!(capped.len() < long.len());
        assert!(capped.starts_with("(earlier output omitted"));
        assert!(capped.ends_with("⇒"));
    }

    /// The head-keeping cap is for HTTP bodies and file contents: the
    /// beginning survives, the dropped amount is noted, and multi-byte
    /// output is never split.
    #[test]
    fn cap_tool_result_head_keeps_the_front_on_a_char_boundary() {
        let short = "hello";
        assert_eq!(cap_tool_result_head(short), short);

        let long = "⇒".repeat(MAX_TOOL_RESULT_BYTES * 2);
        let capped = cap_tool_result_head(&long);
        assert!(capped.len() < long.len());
        assert!(capped.contains("(later output omitted"));
        assert!(capped.starts_with("⇒"));
    }
}
