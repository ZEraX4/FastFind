//! Storage type detection (spinning disk vs SSD) used to tune I/O concurrency.

use std::path::Path;

/// `Some(true)` for rotational media (HDD), `Some(false)` for SSD/NVMe, `None` if unknown.
pub fn is_rotational(path: &Path) -> Option<bool> {
    imp::is_rotational(path)
}

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetVolumePathNameW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Ioctl::{
        PropertyStandardQuery, StorageDeviceSeekPenaltyProperty, DEVICE_SEEK_PENALTY_DESCRIPTOR,
        IOCTL_STORAGE_QUERY_PROPERTY, STORAGE_PROPERTY_QUERY,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;

    pub fn is_rotational(path: &Path) -> Option<bool> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut vol = [0u16; 512];
        // SAFETY: buffers are valid and NUL-terminated; lengths are passed correctly.
        unsafe {
            if GetVolumePathNameW(wide.as_ptr(), vol.as_mut_ptr(), vol.len() as u32) == 0 {
                return None;
            }
        }
        let vol = String::from_utf16_lossy(&vol[..vol.iter().position(|&c| c == 0)?]);
        // Only drive-letter volumes ("C:\") map to a \\.\C: device path.
        let letter = vol.strip_suffix('\\').filter(|v| v.len() == 2 && v.ends_with(':'))?;
        let dev: Vec<u16> = format!("\\\\.\\{letter}").encode_utf16().chain(Some(0)).collect();
        unsafe {
            let h = CreateFileW(dev.as_ptr(), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut());
            if h == INVALID_HANDLE_VALUE {
                return None;
            }
            let query = STORAGE_PROPERTY_QUERY {
                PropertyId: StorageDeviceSeekPenaltyProperty,
                QueryType: PropertyStandardQuery,
                AdditionalParameters: [0],
            };
            let mut out: DEVICE_SEEK_PENALTY_DESCRIPTOR = std::mem::zeroed();
            let mut returned = 0u32;
            let ok = DeviceIoControl(
                h,
                IOCTL_STORAGE_QUERY_PROPERTY,
                &query as *const _ as *const _,
                std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
                &mut out as *mut _ as *mut _,
                std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
                &mut returned,
                std::ptr::null_mut(),
            );
            CloseHandle(h);
            if ok == 0 || returned == 0 {
                return None;
            }
            Some(out.IncursSeekPenalty)
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    pub fn is_rotational(path: &Path) -> Option<bool> {
        let dev = std::fs::metadata(path).ok()?.dev();
        let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
        let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
        let base = std::fs::canonicalize(format!("/sys/dev/block/{major}:{minor}")).ok()?;
        // Partitions have no queue/ directory; their parent device does.
        for dir in [base.clone(), base.parent()?.to_path_buf()] {
            if let Ok(s) = std::fs::read_to_string(dir.join("queue/rotational")) {
                return Some(s.trim() == "1");
            }
        }
        None
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    use std::path::Path;

    /// Every Mac sold in the last decade boots from SSD; external HDDs are rare enough that
    /// the SSD policy is the right default.
    pub fn is_rotational(_path: &Path) -> Option<bool> {
        None
    }
}
