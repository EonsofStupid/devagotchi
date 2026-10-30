//! Win32 identity and security-descriptor checks for private broker state.
//!
//! The Windows trust boundary is the operator's user SID, as the Unix boundary
//! is the operator's UID. Private broker state, the named-pipe endpoint, and
//! journal files are created with an explicit owner and a DACL that grants only
//! that SID; existing entries are inspected through their own handles, never
//! through a reparse-point target.
// Native OS calls live here and nowhere else in this crate's dependents.
#![allow(unsafe_code)]

use std::ffi::{OsStr, c_void};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::Path;
use std::ptr::{addr_of, null_mut};
use windows_sys::Win32::Foundation::{
    ERROR_SUCCESS, FALSE, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    ConvertStringSidToSidW, GetSecurityInfo,
    SDDL_REVISION_1, SE_FILE_OBJECT, SE_OBJECT_TYPE,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, CopySid,
    DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation, GetLengthSid,
    GetSecurityDescriptorControl, GetTokenInformation, IsValidSid, OWNER_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, TOKEN_INFORMATION_CLASS,
    TOKEN_OWNER, TOKEN_QUERY, TOKEN_USER, TokenOwner, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateDirectoryW, CreateFileW,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle, READ_CONTROL,
};
use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

/// A copied security identifier, stored with the alignment its
/// sub-authorities require.
#[derive(Clone)]
pub struct Sid {
    words: Vec<u32>,
}

impl Sid {
    /// # Safety
    /// `sid` must point at a SID that stays valid for the duration of the call.
    unsafe fn copy_from(sid: PSID) -> io::Result<Self> {
        if sid.is_null() || unsafe { IsValidSid(sid) } == FALSE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Windows returned an invalid security identifier",
            ));
        }
        let length = unsafe { GetLengthSid(sid) };
        let mut words = vec![0_u32; (length as usize).div_ceil(4)];
        if unsafe { CopySid(length, words.as_mut_ptr().cast(), sid) } == FALSE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { words })
    }

    fn as_psid(&self) -> PSID {
        // EqualSid and ConvertSidToStringSidW read through this pointer only.
        self.words.as_ptr().cast_mut().cast()
    }

    /// The built-in Administrators group, `S-1-5-32-544`.
    pub fn administrators() -> io::Result<Self> {
        let text = wide(OsStr::new("S-1-5-32-544"));
        let mut sid: PSID = null_mut();
        if unsafe { ConvertStringSidToSidW(text.as_ptr(), &mut sid) } == FALSE {
            return Err(io::Error::last_os_error());
        }
        let _allocation = LocalAllocation(sid.cast());
        unsafe { Self::copy_from(sid) }
    }

    pub fn equals(&self, other: &Sid) -> bool {
        unsafe { EqualSid(self.as_psid(), other.as_psid()) != FALSE }
    }

    /// The SDDL string form, for example `S-1-5-21-…`.
    pub fn to_sddl(&self) -> io::Result<String> {
        let mut text: *mut u16 = null_mut();
        if unsafe { ConvertSidToStringSidW(self.as_psid(), &mut text) } == FALSE {
            return Err(io::Error::last_os_error());
        }
        let _allocation = LocalAllocation(text.cast());
        let mut length = 0;
        while unsafe { *text.add(length) } != 0 {
            length += 1;
        }
        let slice = unsafe { std::slice::from_raw_parts(text, length) };
        String::from_utf16(slice).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }
}

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0 as HLOCAL) };
        }
    }
}

/// The operator: the OS user this process runs as, and the owner Windows
/// assigns to objects it creates without an explicit owner. An elevated
/// administrator's default owner may be the Administrators group.
pub struct Operator {
    pub user: Sid,
    pub default_owner: Sid,
}

impl Operator {
    pub fn current() -> io::Result<Self> {
        let token = process_token(unsafe { GetCurrentProcess() })?;
        Ok(Self {
            user: token_user(&token)?,
            default_owner: token_owner(&token)?,
        })
    }
}

fn process_token(process: HANDLE) -> io::Result<OwnedHandle> {
    let mut token: HANDLE = null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == FALSE {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(token) })
}

fn token_information(token: &OwnedHandle, class: TOKEN_INFORMATION_CLASS) -> io::Result<Vec<u64>> {
    let mut length = 0_u32;
    unsafe { GetTokenInformation(token.as_raw_handle(), class, null_mut(), 0, &mut length) };
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    // u64 storage keeps the pointer-bearing token structures aligned.
    let mut buffer = vec![0_u64; (length as usize).div_ceil(8)];
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            class,
            buffer.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    } == FALSE
    {
        return Err(io::Error::last_os_error());
    }
    Ok(buffer)
}

fn token_user(token: &OwnedHandle) -> io::Result<Sid> {
    let buffer = token_information(token, TokenUser)?;
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    unsafe { Sid::copy_from(user.User.Sid) }
}

fn token_owner(token: &OwnedHandle) -> io::Result<Sid> {
    let buffer = token_information(token, TokenOwner)?;
    let owner = unsafe { &*buffer.as_ptr().cast::<TOKEN_OWNER>() };
    unsafe { Sid::copy_from(owner.Owner) }
}

/// The user SID of the process `pid` runs as.
pub fn process_user(pid: u32) -> io::Result<Sid> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    token_user(&process_token(process.as_raw_handle())?)
}

/// A security descriptor parsed from SDDL, freed with `LocalFree`.
pub struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

// The descriptor is an owned, immutable heap allocation; it is only read.
unsafe impl Send for SecurityDescriptor {}
unsafe impl Sync for SecurityDescriptor {}

impl SecurityDescriptor {
    fn from_sddl(sddl: &str) -> io::Result<Self> {
        let text = wide(OsStr::new(sddl));
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        } == FALSE
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }

    /// Owner `user` and a protected DACL granting every right to `user` only.
    /// `inheritable` passes the grant to files and directories created inside.
    pub fn private_to(user: &Sid, inheritable: bool) -> io::Result<Self> {
        let sid = user.to_sddl()?;
        let inheritance = if inheritable { "OICI" } else { "" };
        Self::from_sddl(&format!("O:{sid}D:P(A;{inheritance};FA;;;{sid})"))
    }

    /// Non-inheritable handle attributes that apply this descriptor.
    pub fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: FALSE,
        }
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0 as HLOCAL) };
    }
}

pub enum Ace {
    Allow(Sid),
    /// Any entry other than an access-allowed entry.
    Other,
}

/// The owner and DACL of one object, read through its handle.
pub struct ObjectSecurity {
    pub owner: Sid,
    pub dacl_protected: bool,
    /// `None` is a null DACL, which grants everyone full access.
    pub dacl: Option<Vec<Ace>>,
}

impl ObjectSecurity {
    /// True when a DACL exists and every entry is an allow entry for `user`.
    pub fn grants_only(&self, user: &Sid) -> bool {
        self.dacl.as_ref().is_some_and(|entries| {
            entries
                .iter()
                .all(|entry| matches!(entry, Ace::Allow(sid) if sid.equals(user)))
        })
    }
}

pub fn object_security(
    handle: RawHandle,
    object: SE_OBJECT_TYPE,
) -> io::Result<ObjectSecurity> {
    let mut owner: PSID = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    let status = unsafe {
        GetSecurityInfo(
            handle,
            object,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    // The owner and DACL pointers refer into this allocation.
    let _allocation = LocalAllocation(descriptor);
    let owner = unsafe { Sid::copy_from(owner)? };
    let mut control = 0_u16;
    let mut revision = 0_u32;
    if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == FALSE {
        return Err(io::Error::last_os_error());
    }
    let dacl = if dacl.is_null() {
        None
    } else {
        let mut size = ACL_SIZE_INFORMATION::default();
        if unsafe {
            GetAclInformation(
                dacl,
                (&mut size as *mut ACL_SIZE_INFORMATION).cast(),
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        } == FALSE
        {
            return Err(io::Error::last_os_error());
        }
        let mut entries = Vec::with_capacity(size.AceCount as usize);
        for index in 0..size.AceCount {
            let mut ace: *mut c_void = null_mut();
            if unsafe { GetAce(dacl, index, &mut ace) } == FALSE {
                return Err(io::Error::last_os_error());
            }
            let header = unsafe { &*ace.cast::<ACE_HEADER>() };
            if u32::from(header.AceType) == ACCESS_ALLOWED_ACE_TYPE {
                let allowed = ace.cast::<ACCESS_ALLOWED_ACE>();
                let sid = unsafe { addr_of!((*allowed).SidStart) }.cast_mut().cast();
                entries.push(Ace::Allow(unsafe { Sid::copy_from(sid)? }));
            } else {
                entries.push(Ace::Other);
            }
        }
        Some(entries)
    };
    Ok(ObjectSecurity {
        owner,
        dacl_protected: control & SE_DACL_PROTECTED != 0,
        dacl,
    })
}

/// Creates a directory owned by `user` whose protected, inheritable DACL
/// grants only `user`. Fails if anything already exists at `path`.
pub fn create_private_directory(path: &Path, user: &Sid) -> io::Result<()> {
    let descriptor = SecurityDescriptor::private_to(user, true)?;
    let attributes = descriptor.attributes();
    let path = wide(path.as_os_str());
    if unsafe { CreateDirectoryW(path.as_ptr(), &attributes) } == FALSE {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Creates an empty file owned by `user` whose protected DACL grants only
/// `user`. Fails if anything, including a reparse point, exists at `path`.
pub fn create_private_file(path: &Path, user: &Sid) -> io::Result<()> {
    let descriptor = SecurityDescriptor::private_to(user, false)?;
    let attributes = descriptor.attributes();
    let path = wide(path.as_os_str());
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            FILE_GENERIC_WRITE,
            0,
            &attributes,
            CREATE_NEW,
            FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    drop(unsafe { OwnedHandle::from_raw_handle(handle) });
    Ok(())
}

/// Opens the entry at `path` itself, never a reparse-point target, for
/// attribute and security queries. Other openers are not blocked.
pub fn open_entry(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

pub enum Entry {
    Directory,
    File,
}

/// Why an opened entry is not private to the operator, or `None` when it is.
///
/// A private directory is an actual directory owned by the operator's user
/// with a protected DACL that grants only that user. A private file is a
/// regular file with one link, owned by the operator (its user, the owner its
/// token gives new objects, or the Administrators group), whose DACL grants
/// only the operator's user. Neither may be a reparse point.
///
/// The Administrators group owns what an elevated administrator creates without
/// an explicit owner, and the same user later runs unelevated, or in Git Bash,
/// whose runtime makes the token's owner the user: a broker one of them started
/// must start again from the other. Only an elevated administrator or SYSTEM can
/// make an object that group owns, and either already controls every account's
/// files, so accepting it lets no one else in; the DACL must still grant the user
/// alone.
pub fn private_entry_defect(
    entry: &File,
    kind: Entry,
    operator: &Operator,
) -> io::Result<Option<&'static str>> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(entry.as_raw_handle(), &mut information) } == FALSE {
        return Err(io::Error::last_os_error());
    }
    if information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Ok(Some("is a reparse point"));
    }
    let directory = information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    let security = object_security(entry.as_raw_handle(), SE_FILE_OBJECT)?;
    match kind {
        Entry::Directory => {
            if !directory {
                return Ok(Some("is not a directory"));
            }
            if !security.owner.equals(&operator.user) {
                return Ok(Some("is not owned by the operator's user"));
            }
            if !security.dacl_protected {
                return Ok(Some("inherits permissions from its parent"));
            }
        }
        Entry::File => {
            if directory {
                return Ok(Some("is not a regular file"));
            }
            if information.nNumberOfLinks != 1 {
                return Ok(Some("has more than one link"));
            }
            if !security.owner.equals(&operator.user)
                && !security.owner.equals(&operator.default_owner)
                && !security.owner.equals(&Sid::administrators()?)
            {
                return Ok(Some("is not owned by the operator"));
            }
        }
    }
    if !security.grants_only(&operator.user) {
        return Ok(Some("grants access to another account"));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Foundation::ERROR_INVALID_OWNER;

    /// A new file with this security descriptor, opened for inspection; `None` when this
    /// process may not give it that owner.
    fn file_with(path: &Path, sddl: &str) -> Option<File> {
        let descriptor = SecurityDescriptor::from_sddl(sddl).unwrap();
        let attributes = descriptor.attributes();
        let name = wide(path.as_os_str());
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                FILE_GENERIC_WRITE,
                0,
                &attributes,
                CREATE_NEW,
                FILE_FLAG_OPEN_REPARSE_POINT,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_INVALID_OWNER as i32) {
                return None;
            }
            panic!("{}: {error}", path.display());
        }
        drop(unsafe { OwnedHandle::from_raw_handle(handle) });
        Some(open_entry(path).unwrap())
    }

    #[test]
    fn a_file_the_user_made_elevated_stays_private_to_them_unelevated() {
        let elevated = Operator::current().unwrap();
        // The same user unelevated, or in Git Bash, whose runtime makes the token's owner the
        // user: the broker one of them started must start again from the other.
        let unelevated = Operator {
            user: elevated.user.clone(),
            default_owner: elevated.user.clone(),
        };
        let user = elevated.user.to_sddl().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // What an elevated administrator's broker leaves: owned by the Administrators group.
        let Some(lock) = file_with(
            &dir.path().join("broker.lock"),
            &format!("O:BAD:P(A;;FA;;;{user})"),
        ) else {
            assert!(
                std::env::var_os("GITHUB_ACTIONS").is_none(),
                "GitHub's Windows runners are elevated administrators"
            );
            eprintln!("only an elevated administrator can make a file the Administrators group owns");
            return;
        };
        assert_eq!(
            private_entry_defect(&lock, Entry::File, &unelevated).unwrap(),
            None
        );
        // Whoever owns it, a file that grants anyone else is still refused.
        let shared = file_with(
            &dir.path().join("shared"),
            &format!("O:BAD:P(A;;FA;;;{user})(A;;FR;;;WD)"),
        )
        .unwrap();
        assert_eq!(
            private_entry_defect(&shared, Entry::File, &unelevated).unwrap(),
            Some("grants access to another account")
        );
    }
}
