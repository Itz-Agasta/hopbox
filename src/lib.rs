//! hopbox: sync a repo to a remote machine over SSH, run a command there, stream the result back.

pub mod cli;
pub mod config;
pub mod doctor;
pub mod exit;
pub mod quote;
pub mod remote_script;
pub mod run;
pub mod status;
pub mod sync;
pub mod transport;
pub mod workspace;
