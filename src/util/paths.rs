//! Workspace-number detection from a path. The pattern is always
//! `{stem}_{n}` as the final path component.

/// Extract the workspace number from a path whose **final** component is
/// `{stem}_{N}`. Returns None otherwise.
///
/// Only the leaf component is inspected — matching `{stem}_{N}` anywhere in the
/// path (as a prior version did) mis-detects when a non-workspace dir sits
/// under a `{stem}_{N}` ancestor, and a leftmost regex match would pick the
/// outer workspace over the real one. This mirrors the original
/// `detect_workspace_number`.
pub fn detect_number(path: &std::path::Path, stem: &str) -> Option<u32> {
    let name = path.file_name()?.to_str()?;
    let n = name
        .strip_prefix(&format!("{stem}_"))?
        .parse::<u32>()
        .ok()?;
    // `{stem}_0` is not a workspace: 0 is reserved for the main worktree (decided
    // by path-equality, not by name), and cw never allocates it. Mirrors the
    // original's `-gt 0` guard.
    (n > 0).then_some(n)
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
    fn ignores_non_leaf_n() {
        // Only the final component counts: a subdir under app_15 is not app_15.
        assert_eq!(detect_number(Path::new("/tmp/app_15/web/src"), "app"), None);
        // Leaf wins over an outer match (no false outer-workspace detection).
        assert_eq!(
            detect_number(Path::new("/tmp/app_2/app_15"), "app"),
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

    #[test]
    fn zero_is_not_a_workspace() {
        // {stem}_0 is reserved for the main worktree, never a numbered workspace.
        assert_eq!(detect_number(Path::new("/tmp/app_0"), "app"), None);
    }
}
