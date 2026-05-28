//! Workspace-number detection from a path. The pattern is always
//! `{stem}_{n}` as a path component.

use regex::Regex;

/// Extract the workspace number from a path whose final component is
/// `{stem}_{N}`. Returns None if no match.
pub fn detect_number(path: &std::path::Path, stem: &str) -> Option<u32> {
    let re = Regex::new(&format!(r"(?:^|/){}_(\d+)(?:/|$)", regex::escape(stem))).ok()?;
    let s = path.to_string_lossy();
    re.captures(&s)?.get(1)?.as_str().parse::<u32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn detects_trailing_n() {
        assert_eq!(
            detect_number(Path::new("/home/t/Code/app_3"), "app"),
            Some(3)
        );
    }

    #[test]
    fn detects_middle_n() {
        assert_eq!(
            detect_number(Path::new("/tmp/app_15/web/src"), "app"),
            Some(15)
        );
    }

    #[test]
    fn ignores_wrong_stem() {
        assert_eq!(detect_number(Path::new("/tmp/other_3"), "app"), None);
    }

    #[test]
    fn no_number() {
        assert_eq!(detect_number(Path::new("/tmp/app"), "app"), None);
    }
}
