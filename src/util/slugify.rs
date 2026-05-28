/// Turn a free-form description into a git-branch-safe slug.
///
/// Behavior (mirrors the Bash cw dispatcher):
/// - lowercase
/// - non-alphanumeric runs collapse to `-`
/// - leading/trailing `-` trimmed
/// - capped at 50 chars on a word boundary when possible
pub fn slugify(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut last_dash = true;
    for ch in lower.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    while out.starts_with('-') {
        out.remove(0);
    }
    if out.len() > 50 {
        let cut = out[..50].rfind('-').unwrap_or(50);
        out.truncate(cut);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::slugify;

    #[test]
    fn simple() {
        assert_eq!(slugify("Fix login bug"), "fix-login-bug");
    }

    #[test]
    fn preserves_alphanum() {
        assert_eq!(slugify("PROJ-1234 investigate"), "proj-1234-investigate");
    }

    #[test]
    fn collapses_punct() {
        assert_eq!(slugify("foo!!  bar??"), "foo-bar");
    }

    #[test]
    fn caps_at_50_on_word() {
        let long = "one two three four five six seven eight nine ten eleven twelve";
        let s = slugify(long);
        assert!(s.len() <= 50, "{} too long", s);
        assert!(!s.ends_with('-'));
    }
}
