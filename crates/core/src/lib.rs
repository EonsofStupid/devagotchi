//! The Devagotchi broker core: admission and operation lifecycle (`broker`),
//! native process supervision (`execution`), and the durable SQLite journal.
//! Wire types live in `devagotchi-contracts`; local IPC in `devagotchi-ipc`.
//! These modules build where a native execution provider exists; on other
//! targets the crate is empty and the broker binary reports `unsupported_platform`.

#[cfg(any(target_os = "linux", windows))]
pub mod broker;
#[cfg(any(target_os = "linux", windows))]
pub mod execution;
#[cfg(any(target_os = "linux", windows))]
pub(crate) mod files;
#[cfg(any(target_os = "linux", windows))]
pub mod journal;
