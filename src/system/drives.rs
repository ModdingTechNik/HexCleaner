//! Диски компьютера и выбор папки в стандартном окне Windows.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::HWND;
use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetLogicalDriveStringsW, GetVolumeInformationW};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree};
use windows::Win32::UI::Shell::{
    FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName,
    SIGDN_FILESYSPATH,
};
use windows::core::HSTRING;

/// Диск с файловой системой.
#[derive(Clone, Debug, PartialEq)]
pub struct Drive {
    /// Корень диска: `C:\`.
    pub root: PathBuf,
    /// Метка тома; может быть пустой.
    pub label: String,
    pub total: u64,
    pub free: u64,
}

/// Диски, к которым есть доступ. Пустые дисководы и недоступные сетевые диски пропускаются.
pub fn list() -> Vec<Drive> {
    let mut buffer = [0u16; 512];
    let length = unsafe { GetLogicalDriveStringsW(Some(&mut buffer)) } as usize;
    if length == 0 || length > buffer.len() {
        return Vec::new();
    }
    // Корни дисков через ноль: `C:\␀D:\␀␀`.
    buffer[..length]
        .split(|&c| c == 0)
        .filter(|root| !root.is_empty())
        .filter_map(|root| drive(&String::from_utf16_lossy(root)))
        .collect()
}

fn drive(root: &str) -> Option<Drive> {
    let path = HSTRING::from(root);
    let (mut free, mut total) = (0u64, 0u64);
    unsafe { GetDiskFreeSpaceExW(&path, Some(&mut free), Some(&mut total), None) }.ok()?;

    let mut label = [0u16; 261];
    let label = match unsafe { GetVolumeInformationW(&path, Some(&mut label), None, None, None, None) } {
        Ok(()) => String::from_utf16_lossy(&label[..label.iter().position(|&c| c == 0).unwrap_or(label.len())]),
        Err(_) => String::new(),
    };
    Some(Drive { root: PathBuf::from(root), label, total, free })
}

/// Окно выбора папки, начиная с `initial`. `owner` — дескриптор окна программы.
/// `None` — пользователь закрыл окно или что-то пошло не так.
pub fn pick_folder(owner: Option<isize>, initial: Option<&Path>) -> Option<PathBuf> {
    unsafe {
        // Поток окна уже может быть инициализирован для COM (это делает winit) — это нормально.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        if let Some(initial) = initial
            && let Ok(folder) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(initial.as_os_str()), None)
        {
            let _ = dialog.SetFolder(&folder);
        }
        dialog.Show(owner.map(|window| HWND(window as *mut _))).ok()?;

        let item = dialog.GetResult().ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(name.0 as *const _));
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_the_system_drive() {
        let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let drives = list();
        let drive = drives.iter().find(|drive| drive.root.to_string_lossy().starts_with(&system));
        let drive = drive.unwrap_or_else(|| panic!("нет диска {system} в {drives:?}"));
        assert!(drive.total > 0 && drive.free <= drive.total, "{drive:?}");
    }
}
