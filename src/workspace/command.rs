use crate::cli::{RemoveArgs, WorkspaceAction, WorkspaceArgs};
use crate::shell::Emitter;
use anyhow::Result;

pub fn default_dispatch(_rest: Vec<String>, _emitter: &mut Emitter) -> Result<()> {
    Err(anyhow::anyhow!(
        "bare `cw <description|N|PR#|branch>` lands in steps 4-5"
    ))
}

pub fn open(_target: Option<String>, _emitter: &mut Emitter) -> Result<()> {
    Err(anyhow::anyhow!("`cw open` lands in step 5"))
}

pub fn remove(_args: RemoveArgs, _emitter: &mut Emitter) -> Result<()> {
    Err(anyhow::anyhow!("`cw remove` lands in step 7"))
}

pub fn dispatch(args: WorkspaceArgs, _emitter: &mut Emitter) -> Result<()> {
    match args.action {
        WorkspaceAction::List => Err(anyhow::anyhow!("`cw workspace list` lands in step 11")),
        WorkspaceAction::Resolve { .. } => {
            Err(anyhow::anyhow!("`cw workspace resolve` lands in step 5"))
        }
    }
}
