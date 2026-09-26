//! Library side of the `cctg` binary, so integration tests and later
//! subcommands can use the hub modules.

pub mod agent;
pub mod channel;
pub mod client;
pub mod deploy;
pub mod device;
pub mod doctor;
pub mod download;
pub mod files;
pub mod hook;
pub mod hub;
pub mod keys;
pub mod proctree;
pub mod reads;
pub mod run;
pub mod shim;
pub mod spool;
pub mod statusfile;
pub mod statusline;
pub mod supervise;
pub mod tail;
pub mod term;
pub mod tls;
pub mod update;
pub mod wire;
