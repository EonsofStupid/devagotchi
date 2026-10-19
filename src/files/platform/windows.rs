//! Windows file identity: UTF-16 names, reparse-point-aware opens that never
//! block another writer, final handle paths, and 128-bit file IDs.

use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_NAME_NORMALIZED, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdInfo, GetFileInformationByHandleEx,
    GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
};

/// Names order by their UTF-16 code units, encoded big-endian so byte order
/// matches unit order.
pub(crate) fn sort_key(name: &OsStr) -> Vec<u8> {
    name.encode_wide().flat_map(u16::to_be_bytes).collect()
}

/// Opens `path` for reading without following a final reparse point. Read,
/// write, and delete sharing let other programs replace or remove the file
/// while it is open.
pub(crate) fn open_for_read(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// The normalized drive path of the object this handle refers to now.
pub(crate) fn opened_path(file: &File) -> io::Result<PathBuf> {
    let mut buffer = vec![0_u16; 512];
    loop {
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        } as usize;
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        if length < buffer.len() {
            buffer.truncate(length);
            break;
        }
        // Too small: `length` is the required size including the terminator.
        buffer.resize(length, 0);
    }
    let path = PathBuf::from(OsString::from_wide(&buffer));
    Ok(dunce::simplified(&path).to_path_buf())
}

pub(crate) fn identity(file: &File, _metadata: &Metadata) -> io::Result<String> {
    let mut information = FILE_ID_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            (&mut information as *mut FILE_ID_INFO).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(format!(
        "{:x}:{}",
        information.VolumeSerialNumber,
        hex::encode(information.FileId.Identifier)
    ))
}
