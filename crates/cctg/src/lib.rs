//! Library side of the `cctg` binary, so integration tests and later
//! subcommands can use the hub modules.

pub mod agent;
pub mod channel;
pub mod device;
pub mod hook;
pub mod hub;
pub mod keys;
pub mod proctree;
pub mod spool;
pub mod statusline;
pub mod supervise;
pub mod tail;
pub mod wire;
