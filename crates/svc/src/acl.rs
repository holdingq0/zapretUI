use std::os::windows::fs::MetadataExt;
use std::path::Path;

use anyhow::bail;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LUID, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows::Win32::Security::{
    ACL, AdjustTokenPrivileges, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
    LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{BOOL, HSTRING, PCWSTR, w};

// Owned by Administrators: whoever created a file here before the install (a standard user could)
// loses the implicit right of an owner to change its permissions back.
const DATA_DIR_SDDL: &str = "O:BAD:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
const REPARSE_POINT: u32 = 0x400;

pub fn secure(path: &Path) -> anyhow::Result<()> {
    refuse_links(path)?;
    std::fs::create_dir_all(path)?;
    // Files a standard user created may not let even an administrator in: restore and take-ownership
    // rights set owner and permissions regardless of what is there now.
    enable(w!("SeRestorePrivilege"));
    enable(w!("SeTakeOwnershipPrivilege"));
    apply_tree(path)
}

fn enable(name: PCWSTR) {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token).is_err() {
            return;
        }
        let mut luid = LUID::default();
        if LookupPrivilegeValueW(PCWSTR::null(), name, &mut luid).is_ok() {
            let privileges = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES { Luid: luid, Attributes: SE_PRIVILEGE_ENABLED }],
            };
            let _ = AdjustTokenPrivileges(token, false, Some(&privileges), 0, None, None);
        }
        let _ = CloseHandle(token);
    }
}

fn refuse_links(path: &Path) -> anyhow::Result<()> {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    if meta.file_attributes() & REPARSE_POINT != 0 {
        bail!("{} — ссылка или точка монтирования. Удалите её и повторите", path.display());
    }
    if meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            refuse_links(&entry?.path())?;
        }
    }
    Ok(())
}

/// Checks every entry right before it is changed: a link swapped in after `refuse_links` must not
/// carry the change somewhere else.
fn apply_tree(path: &Path) -> anyhow::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_attributes() & REPARSE_POINT != 0 {
        bail!("{} — ссылка или точка монтирования. Удалите её и повторите", path.display());
    }
    apply(path)?;
    if meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            apply_tree(&entry?.path())?;
        }
    }
    Ok(())
}

fn apply(path: &Path) -> anyhow::Result<()> {
    let sddl = HSTRING::from(DATA_DIR_SDDL);
    let target = HSTRING::from(path.as_os_str());
    unsafe {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(&sddl, SDDL_REVISION_1, &mut sd, None)?;
        let mut present = BOOL(0);
        let mut defaulted = BOOL(0);
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut owner = PSID::default();
        let mut owner_defaulted = BOOL(0);
        let result = GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted)
            .and_then(|_| GetSecurityDescriptorOwner(sd, &mut owner, &mut owner_defaulted))
            .and_then(|_| {
                SetNamedSecurityInfoW(
                    &target,
                    SE_FILE_OBJECT,
                    OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    Some(owner),
                    None,
                    Some(dacl),
                    None,
                )
                .ok()
            });
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        result?;
    }
    Ok(())
}
