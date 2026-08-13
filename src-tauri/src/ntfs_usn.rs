//! Read-only NTFS USN Journal support.
//!
//! This module never creates, resizes, or deletes a change journal. It only
//! queries an existing journal and reads records from a saved checkpoint.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JournalCheckpoint {
    pub volume: String,
    pub journal_id: u64,
    pub next_usn: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsnChange {
    pub usn: i64,
    pub file_reference: u64,
    pub parent_reference: u64,
    pub reason: u32,
    pub file_attributes: u32,
    pub name: String,
}

/// One record returned while enumerating the NTFS master file table.
/// `estimated_size_bytes` is read from the unnamed `$DATA` attribute in the
/// raw MFT record. It is suitable for fast whole-volume estimates, but it does
/// not include alternate data streams and is not a replacement for an exact
/// filesystem metadata scan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MftRecord {
    pub file_reference: u64,
    pub parent_reference: u64,
    pub file_attributes: u32,
    pub name: String,
    pub estimated_size_bytes: Option<u64>,
}

impl UsnChange {
    pub fn is_directory(&self) -> bool {
        const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
        self.file_attributes & FILE_ATTRIBUTE_DIRECTORY != 0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CatchUpResult {
    Changes {
        checkpoint: JournalCheckpoint,
        changes: Vec<UsnChange>,
    },
    RebuildRequired {
        volume: String,
        reason: String,
    },
    Unavailable {
        volume: String,
        reason: String,
    },
}

pub fn volume_for_path(path: &Path) -> Option<String> {
    let text = path.to_string_lossy().replace('\\', "/");
    let bytes = text.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        Some(format!("{}:/", (bytes[0] as char).to_ascii_uppercase()))
    } else {
        None
    }
}

#[cfg(windows)]
pub fn checkpoint(path: &Path) -> CatchUpResult {
    let Some(volume) = volume_for_path(path) else {
        return CatchUpResult::Unavailable {
            volume: path.to_string_lossy().to_string(),
            reason: "仅支持带盘符的本地 NTFS 卷".into(),
        };
    };
    match windows_impl::VolumeHandle::open(&volume).and_then(|handle| handle.query_journal()) {
        Ok(info) => CatchUpResult::Changes {
            checkpoint: JournalCheckpoint {
                volume,
                journal_id: info.journal_id,
                next_usn: info.next_usn,
            },
            changes: vec![],
        },
        Err(error) => CatchUpResult::Unavailable {
            volume,
            reason: friendly_error(&error),
        },
    }
}

#[cfg(not(windows))]
pub fn checkpoint(path: &Path) -> CatchUpResult {
    CatchUpResult::Unavailable {
        volume: path.to_string_lossy().to_string(),
        reason: "USN Journal 仅适用于 Windows NTFS".into(),
    }
}

#[cfg(windows)]
pub fn read_since(checkpoint: &JournalCheckpoint) -> CatchUpResult {
    let volume = checkpoint.volume.clone();
    let result = (|| -> Result<CatchUpResult> {
        let handle = windows_impl::VolumeHandle::open(&volume)?;
        let current = handle.query_journal()?;
        if current.journal_id != checkpoint.journal_id {
            return Ok(CatchUpResult::RebuildRequired {
                volume,
                reason: "USN Journal ID 已变化，旧检查点失效".into(),
            });
        }
        if checkpoint.next_usn < current.first_usn {
            return Ok(CatchUpResult::RebuildRequired {
                volume,
                reason: format!(
                    "USN Journal 已裁剪旧记录（检查点 {}，当前最早 {}）",
                    checkpoint.next_usn, current.first_usn
                ),
            });
        }
        if checkpoint.next_usn > current.next_usn {
            return Ok(CatchUpResult::RebuildRequired {
                volume,
                reason: "USN 序号发生回退，卷或 Journal 可能已重建".into(),
            });
        }
        let changes =
            handle.read_changes(checkpoint.next_usn, current.next_usn, current.journal_id)?;
        Ok(CatchUpResult::Changes {
            checkpoint: JournalCheckpoint {
                volume,
                journal_id: current.journal_id,
                next_usn: current.next_usn,
            },
            changes,
        })
    })();
    match result {
        Ok(result) => result,
        Err(error) => CatchUpResult::Unavailable {
            volume: checkpoint.volume.clone(),
            reason: friendly_error(&error),
        },
    }
}

#[cfg(windows)]
pub fn enumerate_mft(
    volume: &str,
    mut on_record: impl FnMut(MftRecord) -> Result<()>,
) -> Result<JournalCheckpoint> {
    let handle = windows_impl::VolumeHandle::open(volume)?;
    let journal = handle.query_journal()?;
    // Reuse one raw-record buffer for the whole volume. Allocating 64 KiB per
    // file would erase much of the benefit of sequential MFT enumeration.
    let mut file_record_buffer = vec![0u8; 64 * 1024 + 16];
    handle.enumerate_mft(journal.next_usn, |change| {
        let estimated_size_bytes = if change.is_directory() {
            Some(0)
        } else {
            handle
                .estimated_file_size(change.file_reference, &mut file_record_buffer)
                .ok()
                .flatten()
        };
        on_record(MftRecord {
            file_reference: change.file_reference,
            parent_reference: change.parent_reference,
            file_attributes: change.file_attributes,
            name: change.name,
            estimated_size_bytes,
        })
    })?;
    Ok(JournalCheckpoint {
        volume: volume.to_string(),
        journal_id: journal.journal_id,
        next_usn: journal.next_usn,
    })
}

#[cfg(not(windows))]
pub fn enumerate_mft(
    _volume: &str,
    _on_record: impl FnMut(MftRecord) -> Result<()>,
) -> Result<JournalCheckpoint> {
    Err(anyhow!("MFT 枚举仅适用于 Windows NTFS"))
}

#[cfg(not(windows))]
pub fn read_since(checkpoint: &JournalCheckpoint) -> CatchUpResult {
    CatchUpResult::Unavailable {
        volume: checkpoint.volume.clone(),
        reason: "USN Journal 仅适用于 Windows NTFS".into(),
    }
}

#[cfg(windows)]
pub fn resolve_change_path(volume: &str, change: &UsnChange) -> Result<PathBuf> {
    let handle = windows_impl::VolumeHandle::open(volume)?;
    let parent = handle.path_for_file_id(change.parent_reference)?;
    Ok(parent.join(&change.name))
}

#[cfg(windows)]
pub fn resolve_change_paths(
    volume: &str,
    changes: Vec<UsnChange>,
) -> (Vec<(UsnChange, PathBuf)>, usize) {
    let handle = match windows_impl::VolumeHandle::open(volume) {
        Ok(handle) => handle,
        Err(_) => return (vec![], changes.len()),
    };
    let mut parents: HashMap<u64, Option<PathBuf>> = HashMap::new();
    let mut resolved = Vec::with_capacity(changes.len());
    let mut unresolved = 0;
    for change in changes {
        let parent = parents
            .entry(change.parent_reference)
            .or_insert_with(|| handle.path_for_file_id(change.parent_reference).ok());
        if let Some(parent) = parent {
            let path = parent.join(&change.name);
            resolved.push((change, path));
        } else {
            unresolved += 1;
        }
    }
    (resolved, unresolved)
}

#[cfg(not(windows))]
pub fn resolve_change_path(_volume: &str, _change: &UsnChange) -> Result<PathBuf> {
    Err(anyhow!("USN Journal 仅适用于 Windows NTFS"))
}

#[cfg(not(windows))]
pub fn resolve_change_paths(
    _volume: &str,
    changes: Vec<UsnChange>,
) -> (Vec<(UsnChange, PathBuf)>, usize) {
    (vec![], changes.len())
}

fn friendly_error(error: &anyhow::Error) -> String {
    let text = format!("{:#}", error);
    if text.contains("code 5") || text.contains("Access is denied") || text.contains("拒绝访问")
    {
        "没有读取 NTFS Journal 的权限；继续使用文件系统监听，可在提升权限的索引助手可用后启用高速恢复"
            .into()
    } else {
        text
    }
}

#[cfg(windows)]
mod windows_impl {
    use super::{parse_ntfs_unnamed_data_size, parse_usn_buffer, UsnChange};
    use anyhow::{anyhow, Context, Result};
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use std::path::PathBuf;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_HANDLE_EOF, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FileIdType, GetFinalPathNameByHandleW, OpenFileById,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_DESCRIPTOR, FILE_ID_DESCRIPTOR_0, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, VOLUME_NAME_DOS,
    };
    use windows_sys::Win32::System::Ioctl::{
        FSCTL_ENUM_USN_DATA, FSCTL_GET_NTFS_FILE_RECORD, FSCTL_QUERY_USN_JOURNAL,
        FSCTL_READ_USN_JOURNAL, MFT_ENUM_DATA_V0, NTFS_FILE_RECORD_INPUT_BUFFER,
        READ_USN_JOURNAL_DATA_V0, USN_JOURNAL_DATA_V0,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;

    const JOURNAL_BUFFER_SIZE: usize = 1024 * 1024;

    pub struct JournalInfo {
        pub journal_id: u64,
        pub first_usn: i64,
        pub next_usn: i64,
    }

    pub struct VolumeHandle(HANDLE);

    unsafe impl Send for VolumeHandle {}

    impl Drop for VolumeHandle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    impl VolumeHandle {
        pub fn open(volume: &str) -> Result<Self> {
            let drive = volume
                .chars()
                .next()
                .filter(|ch| ch.is_ascii_alphabetic())
                .ok_or_else(|| anyhow!("无效的卷路径: {}", volume))?;
            let device = format!(r"\\.\{}:", drive.to_ascii_uppercase());
            let wide: Vec<u16> = device.encode_utf16().chain(Some(0)).collect();
            let handle = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    GENERIC_READ,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    null(),
                    OPEN_EXISTING,
                    0,
                    null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("打开 NTFS 卷 {} 失败", device));
            }
            Ok(Self(handle))
        }

        pub fn query_journal(&self) -> Result<JournalInfo> {
            let mut data: USN_JOURNAL_DATA_V0 = unsafe { zeroed() };
            let mut returned = 0u32;
            let ok = unsafe {
                DeviceIoControl(
                    self.0,
                    FSCTL_QUERY_USN_JOURNAL,
                    null(),
                    0,
                    &mut data as *mut _ as *mut c_void,
                    size_of::<USN_JOURNAL_DATA_V0>() as u32,
                    &mut returned,
                    null_mut(),
                )
            };
            if ok == 0 {
                return Err(std::io::Error::last_os_error()).context("查询 USN Journal 失败");
            }
            Ok(JournalInfo {
                journal_id: data.UsnJournalID,
                first_usn: data.FirstUsn,
                next_usn: data.NextUsn,
            })
        }

        pub fn read_changes(
            &self,
            start_usn: i64,
            end_usn: i64,
            journal_id: u64,
        ) -> Result<Vec<UsnChange>> {
            if start_usn >= end_usn {
                return Ok(vec![]);
            }
            let mut cursor = start_usn;
            let mut changes = Vec::new();
            let mut buffer = vec![0u8; JOURNAL_BUFFER_SIZE];
            while cursor < end_usn {
                let mut input = READ_USN_JOURNAL_DATA_V0 {
                    StartUsn: cursor,
                    ReasonMask: u32::MAX,
                    ReturnOnlyOnClose: 0,
                    Timeout: 0,
                    BytesToWaitFor: 0,
                    UsnJournalID: journal_id,
                };
                let mut returned = 0u32;
                let ok = unsafe {
                    DeviceIoControl(
                        self.0,
                        FSCTL_READ_USN_JOURNAL,
                        &mut input as *mut _ as *mut c_void,
                        size_of::<READ_USN_JOURNAL_DATA_V0>() as u32,
                        buffer.as_mut_ptr() as *mut c_void,
                        buffer.len() as u32,
                        &mut returned,
                        null_mut(),
                    )
                };
                if ok == 0 {
                    return Err(std::io::Error::last_os_error())
                        .with_context(|| format!("从 USN {} 读取 Journal 失败", cursor));
                }
                let used = returned as usize;
                if used < size_of::<i64>() {
                    return Err(anyhow!("USN Journal 返回缓冲区过短: {}", used));
                }
                let next = i64::from_le_bytes(buffer[..8].try_into().unwrap());
                changes.extend(
                    parse_usn_buffer(&buffer[8..used])?
                        .into_iter()
                        .filter(|change| change.usn < end_usn),
                );
                if next <= cursor {
                    break;
                }
                cursor = next;
            }
            Ok(changes)
        }

        pub fn enumerate_mft(
            &self,
            high_usn: i64,
            mut on_record: impl FnMut(UsnChange) -> Result<()>,
        ) -> Result<()> {
            let mut cursor = 0u64;
            let mut buffer = vec![0u8; JOURNAL_BUFFER_SIZE];
            loop {
                let mut input = MFT_ENUM_DATA_V0 {
                    StartFileReferenceNumber: cursor,
                    LowUsn: 0,
                    HighUsn: high_usn,
                };
                let mut returned = 0u32;
                let ok = unsafe {
                    DeviceIoControl(
                        self.0,
                        FSCTL_ENUM_USN_DATA,
                        &mut input as *mut _ as *mut c_void,
                        size_of::<MFT_ENUM_DATA_V0>() as u32,
                        buffer.as_mut_ptr() as *mut c_void,
                        buffer.len() as u32,
                        &mut returned,
                        null_mut(),
                    )
                };
                if ok == 0 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() == Some(ERROR_HANDLE_EOF as i32) {
                        break;
                    }
                    return Err(error).context("枚举 NTFS MFT 失败");
                }
                let used = returned as usize;
                if used < size_of::<u64>() {
                    return Err(anyhow!("MFT 枚举返回缓冲区过短: {}", used));
                }
                let next = u64::from_le_bytes(buffer[..8].try_into().unwrap());
                for record in parse_usn_buffer(&buffer[8..used])? {
                    on_record(record)?;
                }
                if next <= cursor {
                    break;
                }
                cursor = next;
            }
            Ok(())
        }

        /// Read the unnamed `$DATA` length straight from an NTFS file record.
        /// This stays on the volume handle and avoids opening the file by path.
        pub fn estimated_file_size(
            &self,
            file_reference: u64,
            output: &mut [u8],
        ) -> Result<Option<u64>> {
            const OUTPUT_HEADER_LEN: usize = size_of::<i64>() + size_of::<u32>();
            if output.len() <= OUTPUT_HEADER_LEN {
                return Err(anyhow!("MFT 文件记录缓冲区过短"));
            }
            let mut input = NTFS_FILE_RECORD_INPUT_BUFFER {
                FileReferenceNumber: file_reference as i64,
            };
            let mut returned = 0u32;
            let ok = unsafe {
                DeviceIoControl(
                    self.0,
                    FSCTL_GET_NTFS_FILE_RECORD,
                    &mut input as *mut _ as *mut c_void,
                    size_of::<NTFS_FILE_RECORD_INPUT_BUFFER>() as u32,
                    output.as_mut_ptr() as *mut c_void,
                    output.len() as u32,
                    &mut returned,
                    null_mut(),
                )
            };
            if ok == 0 {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("读取 MFT 文件记录 {} 失败", file_reference));
            }
            let used = returned as usize;
            if used < OUTPUT_HEADER_LEN {
                return Err(anyhow!("MFT 文件记录返回缓冲区过短: {}", used));
            }
            let returned_reference = i64::from_le_bytes(output[0..8].try_into().unwrap()) as u64;
            // File references include a 16-bit sequence number above the
            // 48-bit record number. The control may return the closest record,
            // so reject anything other than the requested record number.
            const RECORD_NUMBER_MASK: u64 = 0x0000_ffff_ffff_ffff;
            if returned_reference & RECORD_NUMBER_MASK != file_reference & RECORD_NUMBER_MASK {
                return Ok(None);
            }
            let record_len = u32::from_le_bytes(output[8..12].try_into().unwrap()) as usize;
            if record_len == 0 || OUTPUT_HEADER_LEN + record_len > used {
                return Err(anyhow!("MFT 文件记录长度无效: {}", record_len));
            }
            parse_ntfs_unnamed_data_size(&output[OUTPUT_HEADER_LEN..OUTPUT_HEADER_LEN + record_len])
        }

        pub fn path_for_file_id(&self, file_reference: u64) -> Result<PathBuf> {
            let descriptor = FILE_ID_DESCRIPTOR {
                dwSize: size_of::<FILE_ID_DESCRIPTOR>() as u32,
                Type: FileIdType,
                Anonymous: FILE_ID_DESCRIPTOR_0 {
                    FileId: file_reference as i64,
                },
            };
            let handle = unsafe {
                OpenFileById(
                    self.0,
                    &descriptor,
                    0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    null(),
                    FILE_FLAG_BACKUP_SEMANTICS,
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("按文件 ID {} 打开父目录失败", file_reference));
            }
            let owned = OwnedHandle(handle);
            let needed =
                unsafe { GetFinalPathNameByHandleW(owned.0, null_mut(), 0, VOLUME_NAME_DOS) };
            if needed == 0 {
                return Err(std::io::Error::last_os_error()).context("查询文件 ID 路径长度失败");
            }
            let mut wide = vec![0u16; needed as usize + 1];
            let written = unsafe {
                GetFinalPathNameByHandleW(
                    owned.0,
                    wide.as_mut_ptr(),
                    wide.len() as u32,
                    VOLUME_NAME_DOS,
                )
            };
            if written == 0 || written as usize >= wide.len() {
                return Err(std::io::Error::last_os_error()).context("查询文件 ID 完整路径失败");
            }
            let mut path = String::from_utf16_lossy(&wide[..written as usize]);
            if let Some(rest) = path.strip_prefix(r"\\?\") {
                path = rest.to_string();
            }
            Ok(PathBuf::from(path))
        }
    }

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

/// Parse the logical length of the unnamed `$DATA` attribute in a raw NTFS
/// FILE record. Attribute-list extensions and alternate streams are omitted,
/// which is why callers expose this value as an estimate.
fn parse_ntfs_unnamed_data_size(raw_record: &[u8]) -> Result<Option<u64>> {
    const FILE_SIGNATURE: &[u8; 4] = b"FILE";
    const ATTRIBUTE_DATA: u32 = 0x80;
    const ATTRIBUTE_END: u32 = u32::MAX;
    if raw_record.len() < 24 || &raw_record[..4] != FILE_SIGNATURE {
        return Ok(None);
    }

    let mut record = raw_record.to_vec();
    apply_ntfs_update_sequence(&mut record)?;
    let mut offset = u16::from_le_bytes(record[20..22].try_into().unwrap()) as usize;
    while offset + 16 <= record.len() {
        let attribute_type = u32::from_le_bytes(record[offset..offset + 4].try_into().unwrap());
        if attribute_type == ATTRIBUTE_END {
            break;
        }
        let attribute_len =
            u32::from_le_bytes(record[offset + 4..offset + 8].try_into().unwrap()) as usize;
        if attribute_len < 16 || offset + attribute_len > record.len() {
            return Ok(None);
        }
        let non_resident = record[offset + 8] != 0;
        let name_len = record[offset + 9];
        if attribute_type == ATTRIBUTE_DATA && name_len == 0 {
            if non_resident {
                if attribute_len < 64 {
                    return Ok(None);
                }
                let lowest_vcn =
                    u64::from_le_bytes(record[offset + 16..offset + 24].try_into().unwrap());
                if lowest_vcn == 0 {
                    let size =
                        i64::from_le_bytes(record[offset + 48..offset + 56].try_into().unwrap());
                    return Ok(Some(size.max(0) as u64));
                }
            } else {
                if attribute_len < 24 {
                    return Ok(None);
                }
                let size = u32::from_le_bytes(record[offset + 16..offset + 20].try_into().unwrap());
                return Ok(Some(size as u64));
            }
        }
        offset += attribute_len;
    }
    Ok(None)
}

fn apply_ntfs_update_sequence(record: &mut [u8]) -> Result<()> {
    if record.len() < 8 {
        return Ok(());
    }
    let usa_offset = u16::from_le_bytes(record[4..6].try_into().unwrap()) as usize;
    let usa_count = u16::from_le_bytes(record[6..8].try_into().unwrap()) as usize;
    if usa_count <= 1 {
        return Ok(());
    }
    if usa_offset + usa_count * 2 > record.len() {
        return Err(anyhow!("MFT 更新序列范围无效"));
    }
    let sectors = usa_count - 1;
    if record.len() % sectors != 0 {
        return Err(anyhow!("MFT 文件记录扇区布局无效"));
    }
    let sector_size = record.len() / sectors;
    for sector in 1..=sectors {
        let target = sector * sector_size - 2;
        let replacement = usa_offset + sector * 2;
        let replacement_bytes = [record[replacement], record[replacement + 1]];
        record[target..target + 2].copy_from_slice(&replacement_bytes);
    }
    Ok(())
}

fn parse_usn_buffer(mut bytes: &[u8]) -> Result<Vec<UsnChange>> {
    const MIN_RECORD_SIZE: usize = 60;
    let mut changes = Vec::new();
    while !bytes.is_empty() {
        if bytes.len() < 8 {
            return Err(anyhow!("USN 记录头不完整"));
        }
        let record_len = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
        let major = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
        if record_len < MIN_RECORD_SIZE || record_len > bytes.len() {
            return Err(anyhow!(
                "USN 记录长度无效: {}（剩余 {}）",
                record_len,
                bytes.len()
            ));
        }
        if major != 2 {
            return Err(anyhow!("暂不支持 USN_RECORD_V{}", major));
        }
        let record = &bytes[..record_len];
        let name_len = u16::from_le_bytes(record[56..58].try_into().unwrap()) as usize;
        let name_offset = u16::from_le_bytes(record[58..60].try_into().unwrap()) as usize;
        let name_end = name_offset.saturating_add(name_len);
        if name_len % 2 != 0 || name_offset < MIN_RECORD_SIZE || name_end > record_len {
            return Err(anyhow!("USN 文件名范围无效"));
        }
        let name_units: Vec<u16> = record[name_offset..name_end]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        changes.push(UsnChange {
            file_reference: u64::from_le_bytes(record[8..16].try_into().unwrap()),
            parent_reference: u64::from_le_bytes(record[16..24].try_into().unwrap()),
            usn: i64::from_le_bytes(record[24..32].try_into().unwrap()),
            reason: u32::from_le_bytes(record[40..44].try_into().unwrap()),
            file_attributes: u32::from_le_bytes(record[52..56].try_into().unwrap()),
            name: String::from_utf16_lossy(&name_units),
        });
        bytes = &bytes[record_len..];
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_windows_drive_volume() {
        assert_eq!(
            volume_for_path(Path::new(r"c:\Users\me")),
            Some("C:/".into())
        );
        assert_eq!(volume_for_path(Path::new(r"\\server\share")), None);
    }

    #[test]
    fn parses_v2_record_without_unsafe_pointer_casts() {
        let name: Vec<u16> = "demo.txt".encode_utf16().collect();
        let record_len = 60 + name.len() * 2;
        let aligned_len = (record_len + 7) & !7;
        let mut bytes = vec![0u8; aligned_len];
        bytes[0..4].copy_from_slice(&(aligned_len as u32).to_le_bytes());
        bytes[4..6].copy_from_slice(&2u16.to_le_bytes());
        bytes[8..16].copy_from_slice(&42u64.to_le_bytes());
        bytes[16..24].copy_from_slice(&7u64.to_le_bytes());
        bytes[24..32].copy_from_slice(&99i64.to_le_bytes());
        bytes[40..44].copy_from_slice(&0x100u32.to_le_bytes());
        bytes[52..56].copy_from_slice(&0x20u32.to_le_bytes());
        bytes[56..58].copy_from_slice(&((name.len() * 2) as u16).to_le_bytes());
        bytes[58..60].copy_from_slice(&60u16.to_le_bytes());
        for (index, unit) in name.iter().enumerate() {
            bytes[60 + index * 2..62 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        let parsed = parse_usn_buffer(&bytes).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].file_reference, 42);
        assert_eq!(parsed[0].parent_reference, 7);
        assert_eq!(parsed[0].usn, 99);
        assert_eq!(parsed[0].name, "demo.txt");
    }

    #[test]
    fn parses_resident_and_nonresident_mft_data_sizes() {
        let mut resident = synthetic_file_record(24);
        resident[72..76].copy_from_slice(&321u32.to_le_bytes());
        assert_eq!(parse_ntfs_unnamed_data_size(&resident).unwrap(), Some(321));

        let mut nonresident = synthetic_file_record(64);
        nonresident[64] = 1;
        nonresident[72..80].copy_from_slice(&0u64.to_le_bytes());
        nonresident[104..112].copy_from_slice(&987_654i64.to_le_bytes());
        assert_eq!(
            parse_ntfs_unnamed_data_size(&nonresident).unwrap(),
            Some(987_654)
        );
    }

    fn synthetic_file_record(attribute_len: usize) -> Vec<u8> {
        let mut record = vec![0u8; 1024];
        record[..4].copy_from_slice(b"FILE");
        record[20..22].copy_from_slice(&56u16.to_le_bytes());
        record[56..60].copy_from_slice(&0x80u32.to_le_bytes());
        record[60..64].copy_from_slice(&(attribute_len as u32).to_le_bytes());
        let end = 56 + attribute_len;
        record[end..end + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        record
    }

    #[cfg(windows)]
    #[test]
    fn journal_probe_returns_a_structured_result() {
        let result = checkpoint(Path::new("C:/"));
        eprintln!("C: USN probe: {:?}", result);
        assert!(matches!(
            result,
            CatchUpResult::Changes { .. } | CatchUpResult::Unavailable { .. }
        ));
    }
}
