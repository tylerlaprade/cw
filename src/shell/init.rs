use crate::cli::Shell;
use anyhow::Result;

const ZSH: &str = include_str!("../../templates/zsh.sh");
const BASH: &str = include_str!("../../templates/bash.sh");
const FISH: &str = include_str!("../../templates/fish.fish");

pub fn run(shell: Shell) -> Result<()> {
    let src = match shell {
        Shell::Zsh => ZSH,
        Shell::Bash => BASH,
        Shell::Fish => FISH,
    };
    print!("{}", src);
    Ok(())
}
