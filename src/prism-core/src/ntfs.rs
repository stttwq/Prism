//! NTFS MFT enumeration and USN journal replay.

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

#[derive(Debug, Clone)]
pub struct VolumeDescriptor {
    pub drive_letter: char,
    pub id: VolumeId,
    pub mount_path: String,
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
        let name =
            String::from_utf16(&utf16).map_err(|error| format!("invalid UTF-16: {error}"))?;
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
    let snapshot = volume.snapshot_mutations(records.iter().map(|record| record.frn))?;
    let result = (|| {
        for record in records {
            if record.reason & USN_REASON_FILE_DELETE != 0 {
                // Delete is terminal when NTFS coalesces several reasons into one record.
                volume.delete(record.frn)?;
            } else if record.reason & (USN_REASON_FILE_CREATE | USN_REASON_RENAME_NEW_NAME) != 0
                && volume.upsert(
                    record.frn,
                    record.parent_frn,
                    &record.name,
                    record.is_directory,
                )? == ApplyOutcome::RebuildRequired
            {
                return Ok(ApplyOutcome::RebuildRequired);
            }
            // RENAME_OLD carries the old name. The following RENAME_NEW updates the same slot.
        }
        volume.next_usn = next_usn;
        Ok(ApplyOutcome::Applied)
    })();
    if !matches!(result, Ok(ApplyOutcome::Applied)) {
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
        CloseHandle, ERROR_JOURNAL_NOT_ACTIVE, GENERIC_READ, GENERIC_WRITE, HANDLE,
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
        let handle = open_volume(descriptor, true)?;
        let checkpoint = query_or_create_journal(&handle)?;
        let mut raw_records = enumerate_mft(&handle)?;
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
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .max()
            .unwrap_or(root_record);
        volume.prepare_initial_capacity(max_record, raw_records.len())?;
        raw_records.sort_by_key(|record| {
            VolumeIndex::split_frn(record.frn)
                .map(|v| v.0)
                .unwrap_or(u32::MAX)
        });
        let mut pending = raw_records;
        for _ in 0..64 {
            if pending.is_empty() {
                break;
            }
            let mut next = Vec::new();
            let before = pending.len();
            for record in pending {
                if record.name.is_empty() {
                    continue;
                }
                match volume.upsert(
                    record.frn,
                    record.parent_frn,
                    &record.name,
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
        if current.journal_id != checkpoint.journal_id || checkpoint.next_usn < current.first_usn {
            return Err("USN journal wrapped during MFT enumeration".into());
        }
        replay_until(&handle, &mut volume, current.next_usn)?;
        volume.finish_initial_build();
        Ok(volume)
    }

    pub fn read_changes(
        handle: &VolumeHandle,
        journal_id: u64,
        start_usn: i64,
        wait: bool,
    ) -> Result<(i64, Vec<UsnRecord>), String> {
        let input = READ_USN_JOURNAL_DATA_V0 {
            StartUsn: start_usn,
            ReasonMask: WATCH_REASON_MASK,
            ReturnOnlyOnClose: 0,
            Timeout: if wait { 1 } else { 0 },
            BytesToWaitFor: if wait { 1 } else { 0 },
            UsnJournalID: journal_id,
        };
        let mut output = vec![0u8; 256 * 1024];
        let bytes = ioctl_buffer(handle.0, FSCTL_READ_USN_JOURNAL, &input, &mut output)?;
        output.truncate(bytes);
        parse_usn_buffer(&output)
    }

    fn replay_until(
        handle: &VolumeHandle,
        volume: &mut VolumeIndex,
        high_water: i64,
    ) -> Result<(), String> {
        while volume.next_usn < high_water {
            let before = volume.next_usn;
            let (next, records) = read_changes(handle, volume.journal_id, before, false)?;
            if apply_records(volume, &records, next)? == ApplyOutcome::RebuildRequired {
                return Err("excluded-directory boundary changed during MFT replay".into());
            }
            if next <= before {
                return Err("USN replay cursor did not advance".into());
            }
        }
        Ok(())
    }

    fn enumerate_mft(handle: &VolumeHandle) -> Result<Vec<UsnRecord>, String> {
        let mut input = MFT_ENUM_DATA_V0 {
            StartFileReferenceNumber: 0,
            LowUsn: 0,
            HighUsn: i64::MAX,
        };
        let mut records = Vec::with_capacity(500_000);
        let mut output = vec![0u8; 256 * 1024];
        loop {
            match ioctl_buffer(handle.0, FSCTL_ENUM_USN_DATA, &input, &mut output) {
                Ok(bytes) if bytes >= 8 => {
                    let (next, batch) = parse_usn_buffer(&output[..bytes])?;
                    records.extend(batch);
                    if next as u64 <= input.StartFileReferenceNumber {
                        break;
                    }
                    input.StartFileReferenceNumber = next as u64;
                }
                Ok(_) => break,
                Err(_error) if !records.is_empty() => break,
                Err(error) => return Err(error),
            }
        }
        Ok(records)
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

    fn record(reason: u32) -> Vec<u8> {
        let name: Vec<u16> = "hello.txt".encode_utf16().collect();
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
