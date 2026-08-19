//! NTFS MFT enumeration and USN journal replay.

use std::collections::BTreeMap;

use crate::hierarchy::{ApplyOutcome, VolumeId, VolumeIndex};
use crate::log;

pub const USN_REASON_FILE_CREATE: u32 = 0x0000_0100;
pub const USN_REASON_FILE_DELETE: u32 = 0x0000_0200;
pub const USN_REASON_RENAME_OLD_NAME: u32 = 0x0000_1000;
pub const USN_REASON_RENAME_NEW_NAME: u32 = 0x0000_2000;
pub const WATCH_REASON_MASK: u32 = USN_REASON_FILE_CREATE
    | USN_REASON_FILE_DELETE
    | USN_REASON_RENAME_OLD_NAME
    | USN_REASON_RENAME_NEW_NAME;

#[derive(Debug, Clone)]
pub struct UsnRecord {
    pub frn: u64,
    pub parent_frn: u64,
    pub usn: i64,
    pub reason: u32,
    pub is_directory: bool,
    pub name: String,
}

/// AUDIT-2026-08-18 R-C1: MFT 枚举的池化记录——name 不存 String 而存 (offset, len)
/// 指向单个 `Vec<u8>` 池（UTF-8 编码后的文件名字节）。每条从 ~100-130B（含堆 String）
/// 降到 ~40B（纯 POD），峰值砍半、分配从 O(n) 降到 O(1) 摊销。
/// 仅用于全量 MFT 枚举路径；增量 USN 路径量小，保留 UsnRecord（含 String）不变。
#[derive(Debug, Clone, Copy)]
pub struct MftRecord {
    pub frn: u64,
    pub parent_frn: u64,
    pub is_directory: bool,
    pub name_offset: u32,
    pub name_len: u32,
}

impl MftRecord {
    /// 从 name pool 取 `&str`（UTF-8 lossy 已在入池时完成）。
    pub fn name<'a>(&self, pool: &'a [u8]) -> &'a str {
        let bytes = &pool[self.name_offset as usize..][..self.name_len as usize];
        std::str::from_utf8(bytes).unwrap_or("")
    }
}

#[derive(Debug, Clone)]
pub struct VolumeDescriptor {
    pub drive_letter: char,
    pub id: VolumeId,
    pub mount_path: String,
}

/// Drive letter of `%SystemDrive%`, or `None` when the variable is missing or malformed.
pub fn system_drive_letter() -> Option<char> {
    let value = std::env::var("SystemDrive").ok()?;
    let letter = value.chars().next()?.to_ascii_uppercase();
    letter.is_ascii_alphabetic().then_some(letter)
}

/// Moves the system volume to the front, leaving every other volume in its original
/// relative order (R3).
///
/// The system volume is what the user searches first after an install, so it must be
/// indexed first by construction rather than by relying on `C:` happening to sort first.
/// The reorder is stable so that benchmark runs stay comparable.
pub fn order_volumes(
    descriptors: Vec<VolumeDescriptor>,
    system_drive: Option<char>,
) -> Vec<VolumeDescriptor> {
    let Some(system_drive) = system_drive else {
        return descriptors;
    };
    let mut ordered = Vec::with_capacity(descriptors.len());
    let mut rest = Vec::with_capacity(descriptors.len());
    for descriptor in descriptors {
        if descriptor.drive_letter.to_ascii_uppercase() == system_drive {
            ordered.push(descriptor);
        } else {
            rest.push(descriptor);
        }
    }
    ordered.append(&mut rest);
    ordered
}

#[derive(Debug, Clone, Copy)]
pub struct JournalInfo {
    pub journal_id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
}

pub fn parse_usn_buffer(buffer: &[u8]) -> Result<(i64, Vec<UsnRecord>), String> {
    if buffer.len() < 8 {
        return Err("USN buffer is shorter than its cursor".into());
    }
    let next_usn = i64::from_le_bytes(buffer[..8].try_into().map_err(|_| "cursor")?);
    let mut offset = 8usize;
    let mut records = Vec::new();
    const HEADER: usize = 60;
    while offset < buffer.len() {
        if buffer.len() - offset < HEADER {
            return Err("truncated USN record header".into());
        }
        let record_length = read_u32(buffer, offset)? as usize;
        if record_length < HEADER || !record_length.is_multiple_of(8) {
            return Err(format!("invalid USN record length {record_length}"));
        }
        let end = offset
            .checked_add(record_length)
            .filter(|end| *end <= buffer.len())
            .ok_or_else(|| "USN record exceeds output buffer".to_string())?;
        let major = read_u16(buffer, offset + 4)?;
        if major != 2 {
            return Err(format!("unsupported USN record major version {major}"));
        }
        let name_length = read_u16(buffer, offset + 56)? as usize;
        let name_offset = read_u16(buffer, offset + 58)? as usize;
        if !name_length.is_multiple_of(2) || !name_offset.is_multiple_of(2) || name_offset < HEADER
        {
            return Err("invalid UTF-16 file-name boundary".into());
        }
        let name_end = name_offset
            .checked_add(name_length)
            .filter(|name_end| *name_end <= record_length)
            .ok_or_else(|| "USN file name exceeds record".to_string())?;
        let utf16: Vec<u16> = buffer[offset + name_offset..offset + name_end]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        // AUDIT-2026-08-18 R-B4: NTFS 文件名允许非法代理对（文件系统不校验 Unicode 合法性），
        // from_utf16 严格版遇到孤立代理对会 Err，让 parse_usn_buffer 整批失败 → watcher 死
        // → 经 R-B2 放大为全盘重扫。lossy 单条降级（U+FFFD 替换非法代理对）是正确语义：
        // 其余条目不受影响，合法文件名完全不变。
        let name = String::from_utf16_lossy(&utf16);
        records.push(UsnRecord {
            frn: read_u64(buffer, offset + 8)?,
            parent_frn: read_u64(buffer, offset + 16)?,
            usn: read_i64(buffer, offset + 24)?,
            reason: read_u32(buffer, offset + 40)?,
            is_directory: read_u32(buffer, offset + 52)? & 0x10 != 0,
            name,
        });
        offset = end;
    }
    Ok((next_usn, records))
}

pub fn apply_records(
    volume: &mut VolumeIndex,
    records: &[UsnRecord],
    next_usn: i64,
) -> Result<ApplyOutcome, String> {
    apply_records_inner(volume, records, next_usn, false).map(|(outcome, _)| outcome)
}

fn apply_replay_records(
    volume: &mut VolumeIndex,
    records: &[UsnRecord],
    next_usn: i64,
) -> Result<(ApplyOutcome, usize), String> {
    apply_records_inner(volume, records, next_usn, true)
}

fn apply_records_inner(
    volume: &mut VolumeIndex,
    records: &[UsnRecord],
    next_usn: i64,
    tolerate_unreachable: bool,
) -> Result<(ApplyOutcome, usize), String> {
    let snapshot = volume.snapshot_mutations(records.iter().map(|record| record.frn))?;
    let result = (|| {
        let mut pending = BTreeMap::<u32, &UsnRecord>::new();
        for record in records {
            let record_number = VolumeIndex::split_frn(record.frn)?.0;
            // A later event for the same slot supersedes an earlier deferred create/rename.
            pending.remove(&record_number);
            if record.reason & USN_REASON_FILE_DELETE != 0 {
                // Delete is terminal when NTFS coalesces several reasons into one record.
                volume.delete(record.frn)?;
            } else if record.reason & (USN_REASON_FILE_CREATE | USN_REASON_RENAME_NEW_NAME) != 0 {
                match volume.upsert(
                    record.frn,
                    record.parent_frn,
                    &record.name,
                    record.is_directory,
                ) {
                    Ok(ApplyOutcome::Applied) => {}
                    Ok(ApplyOutcome::RebuildRequired) => {
                        return Ok((ApplyOutcome::RebuildRequired, 0));
                    }
                    Err(error) if error.starts_with("broken parent chain") => {
                        pending.insert(record_number, record);
                    }
                    Err(error) => return Err(error),
                }
            }
            // RENAME_OLD carries the old name. The following RENAME_NEW updates the same slot.
        }

        loop {
            let before = pending.len();
            let deferred = std::mem::take(&mut pending);
            for (record_number, record) in deferred {
                match volume.upsert(
                    record.frn,
                    record.parent_frn,
                    &record.name,
                    record.is_directory,
                ) {
                    Ok(ApplyOutcome::Applied) => {}
                    Ok(ApplyOutcome::RebuildRequired) => {
                        return Ok((ApplyOutcome::RebuildRequired, 0));
                    }
                    Err(error) if error.starts_with("broken parent chain") => {
                        pending.insert(record_number, record);
                    }
                    Err(error) => return Err(error),
                }
            }
            if pending.is_empty() || pending.len() == before {
                break;
            }
        }

        if !pending.is_empty() && !tolerate_unreachable {
            let record = pending.keys().next().copied().unwrap_or_default();
            return Err(format!("broken parent chain at record {record}"));
        }
        volume.next_usn = next_usn;
        Ok((ApplyOutcome::Applied, pending.len()))
    })();
    if !matches!(result, Ok((ApplyOutcome::Applied, _))) {
        volume.rollback_mutations(snapshot);
    }
    result
}

fn read_u16(buffer: &[u8], offset: usize) -> Result<u16, String> {
    let bytes = buffer.get(offset..offset + 2).ok_or("u16 out of bounds")?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(buffer: &[u8], offset: usize) -> Result<u32, String> {
    let bytes = buffer.get(offset..offset + 4).ok_or("u32 out of bounds")?;
    Ok(u32::from_le_bytes(bytes.try_into().map_err(|_| "u32")?))
}

fn read_u64(buffer: &[u8], offset: usize) -> Result<u64, String> {
    let bytes = buffer.get(offset..offset + 8).ok_or("u64 out of bounds")?;
    Ok(u64::from_le_bytes(bytes.try_into().map_err(|_| "u64")?))
}

fn read_i64(buffer: &[u8], offset: usize) -> Result<i64, String> {
    let bytes = buffer.get(offset..offset + 8).ok_or("i64 out of bounds")?;
    Ok(i64::from_le_bytes(bytes.try_into().map_err(|_| "i64")?))
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_HANDLE_EOF, ERROR_JOURNAL_NOT_ACTIVE, GENERIC_READ, GENERIC_WRITE,
        HANDLE,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
        GetVolumeNameForVolumeMountPointW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::Ioctl::{
        CREATE_USN_JOURNAL_DATA, FSCTL_CREATE_USN_JOURNAL, FSCTL_ENUM_USN_DATA,
        FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL, MFT_ENUM_DATA_V0,
        READ_USN_JOURNAL_DATA_V0, USN_JOURNAL_DATA_V0,
    };
    use windows::Win32::System::IO::DeviceIoControl;

    pub struct VolumeHandle(HANDLE);

    unsafe impl Send for VolumeHandle {}

    impl Drop for VolumeHandle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    pub fn discover_volumes() -> Result<Vec<VolumeDescriptor>, String> {
        let mask = unsafe { GetLogicalDrives() };
        if mask == 0 {
            return Err("GetLogicalDrives returned no volumes".into());
        }
        let mut volumes = Vec::new();
        for index in 0..26u32 {
            if mask & (1 << index) == 0 {
                continue;
            }
            let drive_letter = char::from_u32(u32::from(b'A') + index).ok_or("drive letter")?;
            let mount_path = format!("{drive_letter}:\\");
            let mount_w = wide(&mount_path);
            if unsafe { GetDriveTypeW(PCWSTR(mount_w.as_ptr())) } != 3 {
                continue;
            }
            let mut fs_name = [0u16; 32];
            let mut serial = 0u32;
            let info = unsafe {
                GetVolumeInformationW(
                    PCWSTR(mount_w.as_ptr()),
                    None,
                    Some(&mut serial),
                    None,
                    None,
                    Some(&mut fs_name),
                )
            };
            if info.is_err() || utf16z(&fs_name) != "NTFS" {
                continue;
            }
            let mut guid = [0u16; 96];
            unsafe { GetVolumeNameForVolumeMountPointW(PCWSTR(mount_w.as_ptr()), &mut guid) }
                .map_err(|error| format!("volume GUID for {mount_path}: {error}"))?;
            volumes.push(VolumeDescriptor {
                drive_letter,
                id: VolumeId {
                    guid: utf16z(&guid),
                    serial,
                },
                mount_path,
            });
        }
        Ok(volumes)
    }

    pub fn open_volume(descriptor: &VolumeDescriptor, write: bool) -> Result<VolumeHandle, String> {
        let path = wide(&format!("\\\\.\\{}:", descriptor.drive_letter));
        let access = if write {
            GENERIC_READ.0 | GENERIC_WRITE.0
        } else {
            GENERIC_READ.0
        };
        let handle = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            )
        }
        .map_err(|error| format!("open volume {}: {error}", descriptor.mount_path))?;
        Ok(VolumeHandle(handle))
    }

    pub fn query_or_create_journal(handle: &VolumeHandle) -> Result<JournalInfo, String> {
        match query_journal(handle) {
            Ok(info) => Ok(info),
            Err(error) if error.contains(&ERROR_JOURNAL_NOT_ACTIVE.0.to_string()) => {
                let create = CREATE_USN_JOURNAL_DATA {
                    MaximumSize: 32 * 1024 * 1024,
                    AllocationDelta: 8 * 1024 * 1024,
                };
                ioctl_in(handle.0, FSCTL_CREATE_USN_JOURNAL, &create)?;
                query_journal(handle)
            }
            Err(error) => Err(error),
        }
    }

    pub fn query_journal(handle: &VolumeHandle) -> Result<JournalInfo, String> {
        let mut data = USN_JOURNAL_DATA_V0::default();
        ioctl_out(handle.0, FSCTL_QUERY_USN_JOURNAL, &mut data)?;
        Ok(JournalInfo {
            journal_id: data.UsnJournalID,
            first_usn: data.FirstUsn,
            next_usn: data.NextUsn,
        })
    }

    pub fn build_volume(descriptor: &VolumeDescriptor) -> Result<VolumeIndex, String> {
        build_volume_with_progress(descriptor, &|| false, &mut |_| {})
    }

    /// Builds one volume, reporting enumerated record counts as they arrive.
    ///
    /// `on_records` is invoked once per `FSCTL_ENUM_USN_DATA` batch, not once per record,
    /// so first-build progress reporting stays off the per-record hot path (R4).
    pub fn build_volume_with_progress(
        descriptor: &VolumeDescriptor,
        should_cancel: &dyn Fn() -> bool,
        on_records: &mut dyn FnMut(u64),
    ) -> Result<VolumeIndex, String> {
        ensure_build_continues(should_cancel)?;
        let handle = open_volume(descriptor, true)?;
        let checkpoint = query_or_create_journal(&handle)?;
        let (raw_records, name_pool) = enumerate_mft(&handle, should_cancel, on_records)?;
        ensure_build_continues(should_cancel)?;
        let root_record = raw_records
            .iter()
            .find_map(|record| {
                let (frn, _) = VolumeIndex::split_frn(record.frn).ok()?;
                let (parent, _) = VolumeIndex::split_frn(record.parent_frn).ok()?;
                (frn == parent).then_some(frn)
            })
            .unwrap_or(5);
        let mut volume = VolumeIndex::new(
            descriptor.id.clone(),
            descriptor.mount_path.clone(),
            checkpoint.journal_id,
            checkpoint.next_usn,
            root_record,
        )?;
        let max_record = raw_records
            .iter()
            .map(|record| VolumeIndex::split_frn(record.frn).map(|value| value.0))
            .try_fold(root_record, |acc, item| item.map(|value| acc.max(value)))?;
        volume.prepare_initial_capacity(max_record, raw_records.len())?;
        // M1（FRESH-AUDIT-2026-08-19）: 枚举完成时名字总字节已知，预留 names
        // 容量消除 upsert 循环的倍增扩容尖峰；记录数×64 封顶防病态长名池。
        volume.reserve_names_capacity(name_pool.len().min(raw_records.len() * 64));
        let mut pending = raw_records;
        pending.sort_by_key(|record| {
            VolumeIndex::split_frn(record.frn)
                .map(|v| v.0)
                .unwrap_or(u32::MAX)
        });
        for _ in 0..64 {
            ensure_build_continues(should_cancel)?;
            if pending.is_empty() {
                break;
            }
            let mut next = Vec::new();
            let before = pending.len();
            for (position, record) in pending.into_iter().enumerate() {
                if position & 0x0fff == 0 {
                    ensure_build_continues(should_cancel)?;
                }
                let name = record.name(&name_pool);
                if name.is_empty() {
                    continue;
                }
                match volume.upsert(
                    record.frn,
                    record.parent_frn,
                    name,
                    record.is_directory,
                ) {
                    Ok(_) => {}
                    Err(error) if error.starts_with("broken parent chain") => next.push(record),
                    Err(error) => return Err(error),
                }
            }
            if next.len() == before {
                log(format!(
                    "skipping {} unreachable MFT records on {}",
                    next.len(),
                    descriptor.mount_path
                ));
                pending = Vec::new();
                break;
            }
            pending = next;
        }
        if !pending.is_empty() {
            log(format!(
                "skipping {} MFT records deeper than 64 on {}",
                pending.len(),
                descriptor.mount_path
            ));
        }

        let current = query_journal(&handle)?;
        ensure_build_continues(should_cancel)?;
        if current.journal_id != checkpoint.journal_id || checkpoint.next_usn < current.first_usn {
            return Err("USN journal wrapped during MFT enumeration".into());
        }
        replay_until(&handle, &mut volume, current.next_usn, should_cancel)?;
        volume.finish_initial_build();
        Ok(volume)
    }

    /// USN 读取缓冲大小。监听循环跨调用复用一块缓冲，
    /// 不再每毫秒分配并清零 256 KiB（空闲卷曾以此速率空转）。
    pub const USN_READ_CHUNK: usize = 256 * 1024;

    pub fn read_changes(
        handle: &VolumeHandle,
        journal_id: u64,
        start_usn: i64,
        wait: bool,
        output: &mut Vec<u8>,
    ) -> Result<(i64, Vec<UsnRecord>), String> {
        let input = READ_USN_JOURNAL_DATA_V0 {
            StartUsn: start_usn,
            ReasonMask: WATCH_REASON_MASK,
            ReturnOnlyOnClose: 0,
            // 空闲时在内核里阻塞等待而不是 1ms 超时轮询：有变更立即返回，
            // 停止/重建标志最坏 250ms 内被看到（服务关停预算 2s，余量充足）。
            Timeout: if wait { 250 } else { 0 },
            BytesToWaitFor: if wait { 1 } else { 0 },
            UsnJournalID: journal_id,
        };
        output.resize(USN_READ_CHUNK, 0);
        let bytes = ioctl_buffer(handle.0, FSCTL_READ_USN_JOURNAL, &input, output)?;
        parse_usn_buffer(&output[..bytes])
    }

    fn replay_until(
        handle: &VolumeHandle,
        volume: &mut VolumeIndex,
        high_water: i64,
        should_cancel: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        let mut cursor = volume.next_usn;
        let mut replay = Vec::new();
        let mut output = Vec::with_capacity(USN_READ_CHUNK);
        while cursor < high_water {
            ensure_build_continues(should_cancel)?;
            let before = cursor;
            let (next, mut records) =
                read_changes(handle, volume.journal_id, before, false, &mut output)?;
            if next <= before {
                return Err("USN replay cursor did not advance".into());
            }
            replay.append(&mut records);
            cursor = next;
        }
        ensure_build_continues(should_cancel)?;
        let (outcome, skipped) = apply_replay_records(volume, &replay, cursor)?;
        if outcome == ApplyOutcome::RebuildRequired {
            return Err("excluded-directory boundary changed during MFT replay".into());
        }
        if skipped > 0 {
            log(format!(
                "skipping {skipped} unreachable USN records during MFT replay on {}",
                volume.mount_path
            ));
        }
        Ok(())
    }

    /// AUDIT-2026-08-18 R-C1: 返回池化的 MftRecord + name_pool，
    /// 不再返回含堆 String 的 UsnRecord，降首建峰值。
    fn enumerate_mft(
        handle: &VolumeHandle,
        should_cancel: &dyn Fn() -> bool,
        on_records: &mut dyn FnMut(u64),
    ) -> Result<(Vec<MftRecord>, Vec<u8>), String> {
        let mut input = MFT_ENUM_DATA_V0 {
            StartFileReferenceNumber: 0,
            LowUsn: 0,
            HighUsn: i64::MAX,
        };
        // Start with a modest initial capacity and let `extend` grow it in batches.
        let mut records = Vec::with_capacity(64_000);
        // name pool: 单一大 Vec<u8> 存所有文件名的 UTF-8 字节，O(1) 摊销分配。
        let mut name_pool = Vec::with_capacity(8 * 1024 * 1024);
        let mut output = vec![0u8; 256 * 1024];
        loop {
            ensure_build_continues(should_cancel)?;
            match ioctl_buffer_code(handle.0, FSCTL_ENUM_USN_DATA, &input, &mut output) {
                Ok(bytes) if bytes >= 8 => {
                    let (next, batch) = parse_usn_buffer(&output[..bytes])?;
                    on_records(batch.len() as u64);
                    // 池化：每条 UsnRecord 的 name 转为 (offset, len) 指向 name_pool，
                    // 然后释放 batch 的 String 分配。
                    for record in batch {
                        let name_bytes = record.name.as_bytes();
                        let offset = name_pool.len() as u32;
                        name_pool.extend_from_slice(name_bytes);
                        let len = name_bytes.len() as u32;
                        records.push(MftRecord {
                            frn: record.frn,
                            parent_frn: record.parent_frn,
                            is_directory: record.is_directory,
                            name_offset: offset,
                            name_len: len,
                        });
                    }
                    if next as u64 <= input.StartFileReferenceNumber {
                        break;
                    }
                    input.StartFileReferenceNumber = next as u64;
                }
                Ok(_) => break,
                Err(error) if error.win32_code() == ERROR_HANDLE_EOF.0 => break,
                Err(error) => {
                    log(format!(
                        "MFT enumeration failed: {} (collected {} records)",
                        error.message,
                        records.len()
                    ));
                    return Err(error.message);
                }
            }
        }
        Ok((records, name_pool))
    }

    fn ensure_build_continues(should_cancel: &dyn Fn() -> bool) -> Result<(), String> {
        if should_cancel() {
            Err("volume build cancelled".into())
        } else {
            Ok(())
        }
    }

    fn ioctl_in<T>(handle: HANDLE, code: u32, input: &T) -> Result<(), String> {
        let mut returned = 0u32;
        unsafe {
            DeviceIoControl(
                handle,
                code,
                Some(input as *const T as *const c_void),
                std::mem::size_of::<T>() as u32,
                None,
                0,
                Some(&mut returned),
                None,
            )
        }
        .map_err(|error| format!("DeviceIoControl {code:#x}: {error} ({})", error.code().0))
    }

    fn ioctl_out<T>(handle: HANDLE, code: u32, output: &mut T) -> Result<(), String> {
        let mut returned = 0u32;
        unsafe {
            DeviceIoControl(
                handle,
                code,
                None,
                0,
                Some(output as *mut T as *mut c_void),
                std::mem::size_of::<T>() as u32,
                Some(&mut returned),
                None,
            )
        }
        .map_err(|error| format!("DeviceIoControl {code:#x}: {error} ({})", error.code().0))
    }

    fn ioctl_buffer<T>(
        handle: HANDLE,
        code: u32,
        input: &T,
        output: &mut [u8],
    ) -> Result<usize, String> {
        let mut returned = 0u32;
        unsafe {
            DeviceIoControl(
                handle,
                code,
                Some(input as *const T as *const c_void),
                std::mem::size_of::<T>() as u32,
                Some(output.as_mut_ptr() as *mut c_void),
                output.len() as u32,
                Some(&mut returned),
                None,
            )
        }
        .map_err(|error| format!("DeviceIoControl {code:#x}: {error} ({})", error.code().0))?;
        Ok(returned as usize)
    }

    /// An ioctl failure carrying both the formatted message and the raw HRESULT,
    /// so callers can branch on the Win32 error code without string matching.
    /// Only used by [`enumerate_mft`], which needs to distinguish
    /// `ERROR_HANDLE_EOF` (the normal end of `FSCTL_ENUM_USN_DATA`) from real
    /// failures. The shared [`ioctl_buffer`] still returns `String` to avoid
    /// touching its many call sites.
    struct IoctlError {
        message: String,
        hresult: i32,
    }

    impl IoctlError {
        /// The low 16 bits of an HRESULT in the `0x80070000` (FACILITY_WIN32)
        /// range hold the original Win32 error code.
        fn win32_code(&self) -> u32 {
            (self.hresult & 0xFFFF) as u32
        }
    }

    /// Same as [`ioctl_buffer`] but returns a typed error carrying the raw
    /// HRESULT. Used only by the MFT enumeration loop, where `ERROR_HANDLE_EOF`
    /// must be distinguished from genuine failures (audit M1).
    fn ioctl_buffer_code<T>(
        handle: HANDLE,
        code: u32,
        input: &T,
        output: &mut [u8],
    ) -> Result<usize, IoctlError> {
        let mut returned = 0u32;
        unsafe {
            DeviceIoControl(
                handle,
                code,
                Some(input as *const T as *const c_void),
                std::mem::size_of::<T>() as u32,
                Some(output.as_mut_ptr() as *mut c_void),
                output.len() as u32,
                Some(&mut returned),
                None,
            )
        }
        .map_err(|error| IoctlError {
            message: format!("DeviceIoControl {code:#x}: {error} ({})", error.code().0),
            hresult: error.code().0,
        })?;
        Ok(returned as usize)
    }

    fn wide(value: &str) -> Vec<u16> {
        Path::new(value)
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn utf16z(value: &[u16]) -> String {
        let end = value
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(value.len());
        String::from_utf16_lossy(&value[..end])
    }

    #[cfg(test)]
    mod ioctl_error_tests {
        use super::*;

        /// M1 (audit): `ERROR_HANDLE_EOF` is the normal end of
        /// `FSCTL_ENUM_USN_DATA`. The HRESULT→Win32 extraction must map it
        /// back to Win32 code 38 so the enumeration loop can treat it as a
        /// successful termination rather than a swallowed error.
        #[test]
        fn handle_eof_hresult_maps_to_win32_38() {
            let error = IoctlError {
                message: "DeviceIoControl 0x900b8: ...".into(),
                // HRESULT_FROM_WIN32(ERROR_HANDLE_EOF=38) = 0x80070026
                hresult: 0x80070026u32 as i32,
            };
            assert_eq!(error.win32_code(), ERROR_HANDLE_EOF.0);
            assert_eq!(error.win32_code(), 38);
        }

        #[test]
        fn real_ioctl_error_maps_to_its_win32_code() {
            // ERROR_NOT_FOUND (1248) as HRESULT_FROM_WIN32 = 0x800704E0
            let error = IoctlError {
                message: "DeviceIoControl 0x900b8: ...".into(),
                hresult: 0x800704E0u32 as i32,
            };
            assert_eq!(error.win32_code(), 1248);
        }
    }
}

#[cfg(windows)]
pub use platform::*;

#[cfg(not(windows))]
pub fn discover_volumes() -> Result<Vec<VolumeDescriptor>, String> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record_with_name(reason: u32, name: &[u16]) -> Vec<u8> {
        let length = (60 + name.len() * 2 + 7) & !7;
        let mut bytes = vec![0u8; 8 + length];
        bytes[..8].copy_from_slice(&99i64.to_le_bytes());
        let base = 8;
        bytes[base..base + 4].copy_from_slice(&(length as u32).to_le_bytes());
        bytes[base + 4..base + 6].copy_from_slice(&2u16.to_le_bytes());
        bytes[base + 8..base + 16].copy_from_slice(&11u64.to_le_bytes());
        bytes[base + 16..base + 24].copy_from_slice(&5u64.to_le_bytes());
        bytes[base + 24..base + 32].copy_from_slice(&98i64.to_le_bytes());
        bytes[base + 40..base + 44].copy_from_slice(&reason.to_le_bytes());
        bytes[base + 52..base + 56].copy_from_slice(&0u32.to_le_bytes());
        bytes[base + 56..base + 58].copy_from_slice(&((name.len() * 2) as u16).to_le_bytes());
        bytes[base + 58..base + 60].copy_from_slice(&60u16.to_le_bytes());
        for (index, unit) in name.iter().enumerate() {
            let offset = base + 60 + index * 2;
            bytes[offset..offset + 2].copy_from_slice(&unit.to_le_bytes());
        }
        bytes
    }

    fn record(reason: u32) -> Vec<u8> {
        let name: Vec<u16> = "hello.txt".encode_utf16().collect();
        record_with_name(reason, &name)
    }

    /// R-B4: 孤立代理对（unpaired surrogate）在 NTFS 文件名中合法——文件系统不校验
    /// Unicode 合法性。from_utf16_lossy 用 U+FFFD 替换非法代理对，单条降级而非整批失败。
    #[test]
    fn lossy_utf16_unpaired_surrogate_does_not_fail_batch() {
        // 第一条：含孤立高代理 0xD800（无低代理配对）
        let bad_name: Vec<u16> = [0x0061, 0xD800, 0x0062].to_vec(); // "a" + unpaired + "b"
        let bad_record = record_with_name(USN_REASON_FILE_CREATE, &bad_name);

        // 第二条：正常名
        let good_name: Vec<u16> = "ok.txt".encode_utf16().collect();
        let good_record = record_with_name(USN_REASON_FILE_CREATE, &good_name);

        // 拼接：cursor + bad_record + good_record
        let mut buffer = vec![0u8; 8];
        buffer.copy_from_slice(&99i64.to_le_bytes());
        buffer.extend_from_slice(&bad_record[8..]); // 跳过 bad_record 自带 cursor
        buffer.extend_from_slice(&good_record[8..]);

        let (next, records) = parse_usn_buffer(&buffer).expect("lossy decode should not fail");
        assert_eq!(next, 99);
        assert_eq!(records.len(), 2, "both records should parse");
        // 第一条含 U+FFFD 替换字符
        assert!(records[0].name.contains('\u{FFFD}'), "bad name should contain replacement char");
        // 第二条完全正常
        assert_eq!(records[1].name, "ok.txt");
    }

    #[test]
    fn parses_v2_records_and_mixed_reasons() {
        let (next, records) =
            parse_usn_buffer(&record(USN_REASON_FILE_CREATE | 0x8000_0000)).unwrap();
        assert_eq!(next, 99);
        assert_eq!(records[0].name, "hello.txt");
        assert_ne!(records[0].reason & USN_REASON_FILE_CREATE, 0);
    }

    #[test]
    fn exclusion_boundary_rebuild_does_not_advance_or_mutate_live_tree() {
        let mut volume = VolumeIndex::new(
            VolumeId {
                guid: "test".into(),
                serial: 1,
            },
            "C:\\".into(),
            7,
            10,
            5,
        )
        .unwrap();
        volume.upsert(10, 5, "visible", true).unwrap();
        volume.upsert(11, 10, "child.txt", false).unwrap();

        let outcome = apply_records(
            &mut volume,
            &[UsnRecord {
                frn: 10,
                parent_frn: 5,
                usn: 10,
                reason: USN_REASON_RENAME_NEW_NAME,
                is_directory: true,
                name: "node_modules".into(),
            }],
            11,
        )
        .unwrap();

        assert_eq!(outcome, ApplyOutcome::RebuildRequired);
        assert_eq!(volume.next_usn, 10);
        assert_eq!(volume.path_for(11).unwrap(), r"C:\visible\child.txt");
    }

    #[test]
    fn failed_or_rebuild_batches_roll_back_earlier_records() {
        let mut volume = VolumeIndex::new(
            VolumeId {
                guid: "test".into(),
                serial: 1,
            },
            "C:\\".into(),
            7,
            10,
            5,
        )
        .unwrap();
        volume.upsert(10, 5, "visible", true).unwrap();
        volume.upsert(11, 10, "child.txt", false).unwrap();

        let create = UsnRecord {
            frn: 12,
            parent_frn: 5,
            usn: 10,
            reason: USN_REASON_FILE_CREATE,
            is_directory: false,
            name: "transient.txt".into(),
        };
        let boundary = UsnRecord {
            frn: 10,
            parent_frn: 5,
            usn: 11,
            reason: USN_REASON_RENAME_NEW_NAME,
            is_directory: true,
            name: "node_modules".into(),
        };
        assert_eq!(
            apply_records(&mut volume, &[create.clone(), boundary], 12).unwrap(),
            ApplyOutcome::RebuildRequired
        );
        assert!(volume.search("transient", 10).is_empty());
        assert_eq!(volume.path_for(11).unwrap(), r"C:\visible\child.txt");
        assert_eq!(volume.next_usn, 10);

        let broken = UsnRecord {
            frn: 13,
            parent_frn: 99,
            usn: 11,
            reason: USN_REASON_FILE_CREATE,
            is_directory: false,
            name: "broken.txt".into(),
        };
        assert!(apply_records(&mut volume, &[create, broken], 12).is_err());
        assert!(volume.search("transient", 10).is_empty());
        assert_eq!(volume.next_usn, 10);
    }

    #[test]
    fn delete_is_terminal_when_reasons_are_coalesced() {
        let mut volume = VolumeIndex::new(
            VolumeId {
                guid: "test".into(),
                serial: 1,
            },
            "C:\\".into(),
            7,
            10,
            5,
        )
        .unwrap();
        volume.upsert(11, 5, "gone.txt", false).unwrap();
        apply_records(
            &mut volume,
            &[UsnRecord {
                frn: 11,
                parent_frn: 5,
                usn: 10,
                reason: USN_REASON_FILE_CREATE | USN_REASON_FILE_DELETE,
                is_directory: false,
                name: "gone.txt".into(),
            }],
            11,
        )
        .unwrap();
        assert!(volume.search("gone", 10).is_empty());
        assert_eq!(volume.next_usn, 11);
    }

    #[test]
    fn replay_resolves_child_before_parent_and_skips_stale_orphans() {
        let mut volume = VolumeIndex::new(
            VolumeId {
                guid: "test".into(),
                serial: 1,
            },
            "C:\\".into(),
            7,
            10,
            5,
        )
        .unwrap();
        let records = [
            UsnRecord {
                frn: 12,
                parent_frn: 11,
                usn: 10,
                reason: USN_REASON_FILE_CREATE,
                is_directory: false,
                name: "child.txt".into(),
            },
            UsnRecord {
                frn: 11,
                parent_frn: 5,
                usn: 11,
                reason: USN_REASON_FILE_CREATE,
                is_directory: true,
                name: "parent".into(),
            },
            UsnRecord {
                frn: 13,
                parent_frn: 99,
                usn: 12,
                reason: USN_REASON_FILE_CREATE,
                is_directory: false,
                name: "stale.txt".into(),
            },
        ];

        let (outcome, skipped) = apply_replay_records(&mut volume, &records, 13).unwrap();
        assert_eq!(outcome, ApplyOutcome::Applied);
        assert_eq!(skipped, 1);
        assert_eq!(volume.path_for(12).unwrap(), r"C:\parent\child.txt");
        assert!(volume.search("stale", 10).is_empty());
        assert_eq!(volume.next_usn, 13);
    }

    #[test]
    fn rejects_truncated_zero_length_unknown_version_and_bad_name() {
        assert!(parse_usn_buffer(&[0; 7]).is_err());
        let mut zero = record(0);
        zero[8..12].copy_from_slice(&0u32.to_le_bytes());
        assert!(parse_usn_buffer(&zero).is_err());
        let mut unknown = record(0);
        unknown[12..14].copy_from_slice(&3u16.to_le_bytes());
        assert!(parse_usn_buffer(&unknown).is_err());
        let mut bad_name = record(0);
        bad_name[64..66].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(parse_usn_buffer(&bad_name).is_err());
    }
}
