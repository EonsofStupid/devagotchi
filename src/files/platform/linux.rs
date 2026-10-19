//! Linux file identity: byte names, `O_NOFOLLOW` opens, `/proc/self/fd`
//! handle paths, and device/inode identity.

use std::ffi::OsStr;
use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Names order by their exact bytes.
pub(crate) fn sort_key(name: &OsStr) -> Vec<u8> {
    name.as_bytes().to_vec()
}

/// Opens `path` for reading without following a final symbolic link and
/// without blocking on a FIFO or device.
pub(crate) fn open_for_read(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

/// The path of the object this handle refers to now.
pub(crate) fn opened_path(file: &File) -> io::Result<PathBuf> {
    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

pub(crate) fn identity(_file: &File, metadata: &Metadata) -> io::Result<String> {
    Ok(format!("{:x}:{:x}", metadata.dev(), metadata.ino()))
}
