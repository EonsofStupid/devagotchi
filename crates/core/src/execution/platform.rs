//! Native ownership of one root's process tree: the Linux root's inherited
//! process group, or a Windows Job Object. Each provider implements the same
//! `OwnedTree` API and the detail text recorded on its operations.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub(crate) use linux::*;
#[cfg(windows)]
pub(crate) use windows::*;
