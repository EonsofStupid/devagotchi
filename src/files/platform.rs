//! Native file identity for workspace queries: name ordering, opening without
//! following a final link, the opened handle's own path, and object identity.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub(crate) use linux::*;
#[cfg(windows)]
pub(crate) use windows::*;
