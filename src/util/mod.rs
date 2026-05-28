pub mod paths;
pub mod slugify;
pub mod terminal;

/// True if `bin` is an executable file on `$PATH`. Single home for the check
/// that was copy-pasted across restack/create/cleanup.
pub fn in_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}
