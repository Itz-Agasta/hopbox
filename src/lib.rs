//! hopbox: sync a repo to a remote machine over SSH, run a command there, stream the result back.

pub mod cli;
pub mod config;
pub mod doctor;
pub mod exit;
pub mod gc;
pub mod jobs;
pub mod provider;
pub mod quote;
pub mod remote_script;
pub mod rent;
pub mod run;
pub mod state;
pub mod status;
pub mod sync;
pub mod transport;
pub mod workspace;
