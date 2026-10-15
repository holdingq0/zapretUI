use std::os::windows::fs::MetadataExt;
use std::path::Path;

use anyhow::bail;
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR,
};
use windows::core::{BOOL, HSTRING};

const DATA_DIR_SDDL: &str = "D:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
const REPARSE_POINT: u32 = 0x400;

pub fn secure(path: &Path) -> anyhow::Result<()> {
    refuse_links(path)?;
    std::fs::create_dir_all(path)?;
    apply_tree(path)
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

fn apply_tree(path: &Path) -> anyhow::Result<()> {
    apply(path)?;
    if std::fs::symlink_metadata(path)?.is_dir() {
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
        let result = GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted).and_then(|_| {
            SetNamedSecurityInfoW(
                &target,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                None,
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
