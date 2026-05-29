//! Optional cross-worktree Claude Code memory consolidation.
//!
//! Claude Code stores per-project memory under
//! `~/.claude/projects/<encoded-worktree-path>/memory/` — a `MEMORY.md` index
//! plus one `.md` file per memory. Because the encoding is keyed on the
//! *absolute worktree path*, every worktree of the same repo gets a SEPARATE
//! memory store, so memories accumulated in one checkout are invisible to the
//! next and are destroyed when that worktree is removed.
//!
//! When `[claude] memory_merge = true`, cw bridges that:
//!   * on create — seed a new workspace with the union of sibling worktrees'
//!     memories, so a fresh checkout's agent isn't amnesiac;
//!   * on teardown — salvage the departing workspace's memories into the
//!     survivors before `rm -rf`, so `cw remove`/`cleanup` doesn't lose them.
//!
//! All writes are ADD-ONLY: existing memory files are never overwritten and
//! index lines are unioned, so the merge can't clobber or delete.

use crate::config::Config;
use crate::git::worktree;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

pub fn enabled(cfg: &Config) -> bool {
    cfg.claude.memory_merge
}

/// Encode an absolute worktree path to its `~/.claude/projects` subdir name.
/// Claude replaces `/`, `_`, `.`, and space with `-` (verified empirically: a
/// macOS temp path's underscores and the `/private/...` realpath both appear
/// dash-encoded under `~/.claude/projects/`).
fn encode_path(p: &Path) -> String {
    p.to_string_lossy()
        .chars()
        .map(|c| match c {
            '/' | '_' | '.' | ' ' => '-',
            other => other,
        })
        .collect()
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// `~/.claude/projects/<encoded>/memory` for a worktree path (canonicalized to
/// match Claude, which keys on the realpath).
fn memory_dir(worktree: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let abs = canon(worktree);
    Some(
        PathBuf::from(home)
            .join(".claude")
            .join("projects")
            .join(encode_path(&abs))
            .join("memory"),
    )
}

/// All worktree directories of the repo (via `git worktree list`).
fn all_worktree_dirs(cfg: &Config) -> Vec<PathBuf> {
    let Some(root) = cfg.runtime.repo_root.as_deref() else {
        return Vec::new();
    };
    worktree::list(root)
        .map(|wts| wts.into_iter().map(|w| w.dir).collect())
        .unwrap_or_default()
}

/// Read a memory dir's individual memory files (filename → content), excluding
/// the `MEMORY.md` index.
fn read_memory_files(mem: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(mem) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("md") {
            continue;
        }
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name == "MEMORY.md" {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(&p) {
            out.insert(name.to_string(), content);
        }
    }
    out
}

fn read_index_lines(mem: &Path) -> Vec<String> {
    std::fs::read_to_string(mem.join("MEMORY.md"))
        .map(|c| c.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Gather the union of memory files (longest content wins on a filename clash)
/// and unique index lines (first-seen order) across `sources`.
fn gather(sources: &[PathBuf]) -> (BTreeMap<String, String>, Vec<String>) {
    let mut files: BTreeMap<String, String> = BTreeMap::new();
    let mut index: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for src in sources {
        let Some(mem) = memory_dir(src) else {
            continue;
        };
        if !mem.is_dir() {
            continue;
        }
        for (name, content) in read_memory_files(&mem) {
            let keep = files
                .get(&name)
                .map(|c| content.len() > c.len())
                .unwrap_or(true);
            if keep {
                files.insert(name, content);
            }
        }
        for line in read_index_lines(&mem) {
            let key = line.trim().to_string();
            if !key.is_empty() && seen.insert(key) {
                index.push(line);
            }
        }
    }
    (files, index)
}

/// Add the gathered files + index lines into `target`'s memory dir. Add-only:
/// existing files are never overwritten, and index lines are unioned with
/// whatever the target already has (first-seen order preserved).
fn write_into(target: &Path, files: &BTreeMap<String, String>, index: &[String]) {
    let Some(mem) = memory_dir(target) else {
        return;
    };
    if std::fs::create_dir_all(&mem).is_err() {
        return;
    }
    for (name, content) in files {
        let dest = mem.join(name);
        if dest.exists() {
            continue; // never clobber an existing memory
        }
        let _ = std::fs::write(&dest, content);
    }
    // Union index lines: keep the target's, append any not already present.
    let mut lines = read_index_lines(&mem);
    let mut seen: HashSet<String> = lines.iter().map(|l| l.trim().to_string()).collect();
    for line in index {
        let key = line.trim().to_string();
        if !key.is_empty() && seen.insert(key) {
            lines.push(line.clone());
        }
    }
    if lines.is_empty() {
        return;
    }
    let mut out = lines.join("\n");
    out.push('\n');
    let _ = std::fs::write(mem.join("MEMORY.md"), out);
}

/// Seed a freshly-created workspace with the union of every sibling worktree's
/// memories. No-op unless `[claude] memory_merge` is set.
pub fn seed_new_workspace(cfg: &Config, new_worktree: &Path) {
    if !enabled(cfg) {
        return;
    }
    let new_canon = canon(new_worktree);
    let sources: Vec<PathBuf> = all_worktree_dirs(cfg)
        .into_iter()
        .filter(|d| canon(d) != new_canon)
        .collect();
    if sources.is_empty() {
        return;
    }
    let (files, index) = gather(&sources);
    if files.is_empty() && index.is_empty() {
        return;
    }
    write_into(new_worktree, &files, &index);
}

/// Salvage a departing workspace's memories into every surviving worktree
/// before it is removed. No-op unless `[claude] memory_merge` is set.
pub fn salvage_before_remove(cfg: &Config, departing: &Path) {
    if !enabled(cfg) {
        return;
    }
    let departing_buf = departing.to_path_buf();
    let (files, index) = gather(std::slice::from_ref(&departing_buf));
    if files.is_empty() && index.is_empty() {
        return;
    }
    let departing_canon = canon(departing);
    for survivor in all_worktree_dirs(cfg) {
        if canon(&survivor) == departing_canon {
            continue;
        }
        write_into(&survivor, &files, &index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn encode_path_dashes_separators() {
        // Matches the verified Claude encoding for a {stem}_{n} worktree path.
        assert_eq!(
            encode_path(Path::new("/Users/t/Code/cw")),
            "-Users-t-Code-cw"
        );
        assert_eq!(
            encode_path(Path::new("/Users/t/Code/cw_2")),
            "-Users-t-Code-cw-2"
        );
        assert_eq!(encode_path(Path::new("/a/b.c d")), "-a-b-c-d");
    }
}
