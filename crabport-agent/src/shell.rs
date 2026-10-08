//! Shell text helpers shared by the execution tools.

/// Prefix a captured command with `cd <dir> &&` when the shell has reported
/// its directory.
///
/// An out-of-band command starts in the login shell's directory (usually
/// `$HOME`), not where the user's terminal is — so without this a `ls` would
/// list the wrong directory. A visible run needs no prefix: it is typed into
/// the very shell the user is looking at.
pub fn with_cwd(command: &str, cwd: Option<&str>) -> String {
    match cwd {
        Some(dir) if !dir.is_empty() => format!("cd {} && {}", quote_shell(dir), command),
        _ => command.to_string(),
    }
}

/// Single-quote `text` for a POSIX shell, closing and reopening around
/// embedded quotes (`'` → `'\''`). Enough for the directory names
/// interpolated here, and the same quoting a user would expect from a shell.
pub fn quote_shell(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::{quote_shell, with_cwd};

    /// A captured command starts where the user's terminal is, not in the
    /// login shell's default directory — and paths with quotes/quotes-spaces
    /// must survive the interpolation.
    #[test]
    fn with_cwd_prefixes_quoted_directory() {
        assert_eq!(with_cwd("ls", None), "ls");
        assert_eq!(with_cwd("ls", Some("")), "ls");
        assert_eq!(with_cwd("ls", Some("/var/log")), "cd '/var/log' && ls");
        assert_eq!(
            with_cwd("cat file", Some("/tmp/it's here")),
            "cd '/tmp/it'\\''s here' && cat file"
        );
    }

    #[test]
    fn quote_shell_escapes_single_quotes() {
        assert_eq!(quote_shell("plain"), "'plain'");
        assert_eq!(quote_shell("a'b"), "'a'\\''b'");
    }
}
