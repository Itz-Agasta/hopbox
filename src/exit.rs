//! Exit codes hopbox itself produces. A remote command's own code passes through unchanged.

/// Usage or config error: bad flag, unknown host, invalid TOML.
pub const USAGE: u8 = 125;
/// Connection failure, matching ssh's own convention.
pub const CONNECT: u8 = 255;
/// Lock wait or `--timeout` exceeded.
pub const TIMEOUT: u8 = 124;
/// Killed by the memory cap.
pub const OOM: u8 = 137;
/// Interrupted locally (Ctrl-C or SIGTERM).
pub const INTERRUPTED: u8 = 130;
