//! Subprocess helpers. See [`detach`] for daemonizing the background setup
//! chain. Direct `std::process::Command` use elsewhere is intentional.

pub mod detach;
