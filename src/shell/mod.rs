//! Wrapper-record contract: the binary emits machine-readable records on
//! stdout when `CW_WRAPPER=1` is set. The shell function reads them and takes
//! action (cd, terminal title, argv exec, close tab).
//!
//! Format: `CW\t<KIND>\t<field>\t<field>...\n` where any tab / backslash /
//! newline in a field is backslash-escaped. The wrapper unescapes and invokes
//! argv directly (no eval).

pub mod init;

use std::io::Write;

const ENV: &str = "CW_WRAPPER";

pub struct Emitter {
    enabled: bool,
}

impl Emitter {
    pub fn from_env() -> Self {
        Self {
            enabled: std::env::var_os(ENV).is_some_and(|v| !v.is_empty() && v != "0"),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn emit(&mut self, rec: Record) {
        if !self.enabled {
            return;
        }
        let stdout = std::io::stdout();
        let mut w = stdout.lock();
        let _ = write!(w, "CW");
        for field in rec.fields() {
            let _ = write!(w, "\t{}", escape(field));
        }
        let _ = writeln!(w);
    }
}

pub enum Record<'a> {
    Cd(&'a str),
    Title(&'a str),
    Msg(&'a str),
    /// Foreground argv exec. First field is the program, rest are args.
    Exec(&'a [String]),
    /// Background argv exec, same shape.
    ExecBg(&'a [String]),
    CloseTab,
}

impl Record<'_> {
    fn fields(&self) -> Vec<&str> {
        match self {
            Record::Cd(p) => vec!["CD", p],
            Record::Title(t) => vec!["TITLE", t],
            Record::Msg(m) => vec!["MSG", m],
            Record::Exec(argv) => {
                let mut v = vec!["EXEC"];
                v.extend(argv.iter().map(String::as_str));
                v
            }
            Record::ExecBg(argv) => {
                let mut v = vec!["EXEC_BG"];
                v.extend(argv.iter().map(String::as_str));
                v
            }
            Record::CloseTab => vec!["CLOSE_TAB", "1"],
        }
    }
}

/// Escape `\`, `\t`, `\n` so fields survive round-tripping through the shell
/// wrapper's `IFS=$'\t' read`.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str(r"\\"),
            '\t' => out.push_str(r"\t"),
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_preserves_normal() {
        assert_eq!(escape("hello world"), "hello world");
    }

    #[test]
    fn escape_tab_backslash_newline() {
        assert_eq!(escape("a\tb\\c\nd"), r"a\tb\\c\nd");
    }

    #[test]
    fn emitter_disabled_by_default() {
        std::env::remove_var(ENV);
        let e = Emitter::from_env();
        assert!(!e.enabled());
    }

    #[test]
    fn emitter_enabled_when_env_set() {
        std::env::set_var(ENV, "1");
        let e = Emitter::from_env();
        assert!(e.enabled());
        std::env::remove_var(ENV);
    }
}
