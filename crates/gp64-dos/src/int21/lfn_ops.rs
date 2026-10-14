//! DOS 7 / Windows 95 long-filename operations which do not belong to the
//! ordinary directory-search implementation.

use super::*;
use crate::files::PathResolver;
use crate::filesystem::{FileAttributes, FileMetadata};
use std::io::{Seek, SeekFrom};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::host_local_time::{self, host_utc_offset_at};

const FILETIME_UNIX_EPOCH: i128 = 116_444_736_000_000_000;
const FILETIME_TICKS_PER_SECOND: i128 = 10_000_000;
const FILETIME_TICKS_PER_HUNDREDTH: i128 = 100_000;
const DOS_LFN_VOLUME_FLAGS: u16 = 0x4002; // LFN support + case preservation

#[derive(Clone, Copy)]
enum TrueNameKind {
    Canonical,
    Short,
    Long,
}

fn io_error_code(error: &anyhow::Error, default: u16) -> u16 {
    match error
        .downcast_ref::<std::io::Error>()
        .map(std::io::Error::kind)
    {
        Some(std::io::ErrorKind::NotFound) => 2,
        Some(std::io::ErrorKind::PermissionDenied) => 5,
        Some(std::io::ErrorKind::AlreadyExists) => 80,
        Some(std::io::ErrorKind::InvalidInput) => 1,
        _ => default,
    }
}

fn write_ascii_z(memory: &mut Memory, address: usize, text: &str, max_bytes: usize) {
    if max_bytes == 0 {
        return;
    }
    let capacity = max_bytes;
    let bytes = text.as_bytes();
    let count = bytes.len().min(capacity.saturating_sub(1));
    for (index, byte) in bytes.iter().copied().take(count).enumerate() {
        memory.write_u8(address + index, byte);
    }
    memory.write_u8(address + count, 0);
}

fn read_qword(memory: &Memory, address: usize) -> u64 {
    u64::from(memory.read_u32(address)) | (u64::from(memory.read_u32(address + 4)) << 32)
}

fn write_qword(memory: &mut Memory, address: usize, value: u64) {
    memory.write_u32(address, value as u32);
    memory.write_u32(address + 4, (value >> 32) as u32);
}

fn write_failure(dos: &mut Dos, cpu: &mut Cpu, code: u16) {
    dos.set_extended_error(code);
    cpu.regs.set_ax(code);
    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
}

fn write_success(dos: &mut Dos, cpu: &mut Cpu) {
    dos.clear_extended_error();
    cpu.regs.set_ax(0);
    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
}

fn days_in_month(year: i32, month: u32) -> u32 {
    if !(1..=12).contains(&month) {
        return 0;
    }
    let first = Dos::ymd_to_days(year, month, 1);
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    (Dos::ymd_to_days(next_year, next_month, 1) - first) as u32
}

/// Convert a UTC FILETIME into DOS time/date/hundredths.
///
/// DOS accepts 1980-01-01 through 2107-12-31, inclusive.  Keeping this
/// conversion independent of the host clock makes AX=71A7h a total, testable
/// API instead of inheriting the clamping behaviour of the older DOS helper.
fn filetime_to_dos(filetime: u64) -> Option<(u16, u16, u8)> {
    let min = FILETIME_UNIX_EPOCH
        + i128::from(Dos::ymd_to_days(1980, 1, 1)) * 86_400 * FILETIME_TICKS_PER_SECOND;
    let max = FILETIME_UNIX_EPOCH
        + i128::from(Dos::ymd_to_days(2107, 12, 31)) * 86_400 * FILETIME_TICKS_PER_SECOND
        + 86_398 * FILETIME_TICKS_PER_SECOND
        + 199 * FILETIME_TICKS_PER_HUNDREDTH;
    let filetime = i128::from(filetime);
    if !(min..=max).contains(&filetime) {
        return None;
    }

    let unix_ticks = filetime - FILETIME_UNIX_EPOCH;
    let seconds = unix_ticks / FILETIME_TICKS_PER_SECOND;
    let hundredths =
        ((unix_ticks % FILETIME_TICKS_PER_SECOND) / FILETIME_TICKS_PER_HUNDREDTH) as u8;
    let days = seconds / 86_400;
    let day_seconds = (seconds % 86_400) as u32;
    let (year, month, day) = Dos::days_to_ymd(days as i64);
    if !(1980..=2107).contains(&year) || day == 0 || month == 0 {
        return None;
    }

    let hours = day_seconds / 3_600;
    let minutes = (day_seconds % 3_600) / 60;
    let seconds = day_seconds % 60;
    let date = (((year - 1980) as u16) << 9) | ((month as u16) << 5) | day as u16;
    let time = ((hours as u16) << 11) | ((minutes as u16) << 5) | (seconds as u16 / 2);
    Some((date, time, (seconds as u8 % 2) * 100 + hundredths))
}

/// Convert DOS time/date/hundredths to a UTC FILETIME.
fn dos_to_filetime(date: u16, time: u16, hundredths: u16) -> Option<u64> {
    let year = 1980 + i32::from((date >> 9) & 0x7F);
    let month = u32::from((date >> 5) & 0x0F);
    let day = u32::from(date & 0x1F);
    let hours = u32::from((time >> 11) & 0x1F);
    let minutes = u32::from((time >> 5) & 0x3F);
    let seconds = u32::from(time & 0x1F) * 2;

    if !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hours > 23
        || minutes > 59
        || seconds > 59
        || hundredths > 199
    {
        return None;
    }

    let unix_seconds = Dos::ymd_to_days(year, month, day) * 86_400
        + i64::from(hours) * 3_600
        + i64::from(minutes) * 60
        + i64::from(seconds);
    let filetime = FILETIME_UNIX_EPOCH
        + i128::from(unix_seconds) * FILETIME_TICKS_PER_SECOND
        + i128::from(hundredths) * FILETIME_TICKS_PER_HUNDREDTH;
    u64::try_from(filetime).ok()
}

fn filetime_to_systemtime(filetime: u64) -> Option<SystemTime> {
    let unix_ticks = i128::from(filetime) - FILETIME_UNIX_EPOCH;
    if unix_ticks < 0 {
        return None;
    }
    let seconds = u64::try_from(unix_ticks / FILETIME_TICKS_PER_SECOND).ok()?;
    let nanos = u32::try_from((unix_ticks % FILETIME_TICKS_PER_SECOND) * 100).ok()?;
    Some(UNIX_EPOCH + Duration::new(seconds, nanos))
}

/// A host (UTC) timestamp as AX=7143h reports it: DOS packed LOCAL time.
fn systemtime_to_dos(time: SystemTime) -> Option<(u16, u16, u8)> {
    let local = host_local_time::systemtime_utc_to_local_with(time, host_utc_offset_at);
    filetime_to_dos(Dos::systemtime_to_filetime(local))
}

/// DOS packed LOCAL time from AX=7143h to the host (UTC) timestamp to store.
fn dos_to_systemtime(date: u16, time: u16, hundredths: u16) -> Option<SystemTime> {
    let local = dos_to_filetime(date, time, hundredths).and_then(filetime_to_systemtime)?;
    Some(host_local_time::systemtime_local_to_utc_with(
        local,
        host_utc_offset_at,
    ))
}

impl Dos {
    pub(crate) fn lfn_reset_drive(&mut self, cpu: &mut Cpu) {
        let action = cpu.regs.cx();
        if action > 2 {
            write_failure(self, cpu, 1);
            return;
        }
        let drive = match cpu.regs.dx() {
            0 => Some(self.path_resolver.get_current_drive()),
            drive => drive
                .checked_sub(1)
                .and_then(|drive| u8::try_from(drive).ok())
                .filter(|drive| *drive < 26),
        };
        let Some(drive) = drive else {
            write_failure(self, cpu, 15);
            return;
        };
        let Some(mounted) = self.drives.get_mut(&drive) else {
            write_failure(self, cpu, 15);
            return;
        };
        match mounted.flush() {
            Ok(()) => write_success(self, cpu),
            Err(error) => write_failure(self, cpu, io_error_code(&error, 5)),
        }
    }

    pub(crate) fn lfn_attributes(&mut self, cpu: &mut Cpu, memory: &mut Memory) {
        let action = cpu.regs.bl();
        let path = self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_dx(cpu));
        trace_dos_path("LFN_ATTR", cpu.regs.ds, self.dos_offset_dx(cpu), &path);
        let resolved = self.path_resolver.resolve_lfn(&path);

        let Some(drive) = self.drives.get_mut(&resolved.drive) else {
            write_failure(self, cpu, 3);
            return;
        };

        match action {
            0x00 => match drive.get_attributes(&resolved.path) {
                Ok(attrs) => {
                    cpu.regs.set_cx(dos_attr_bits(&attrs));
                    write_success(self, cpu);
                }
                Err(error) => write_failure(self, cpu, io_error_code(&error, 2)),
            },
            0x01 => {
                let attrs = dos_attrs_from_bits(cpu.regs.cx());
                match drive.set_attributes(&resolved.path, attrs) {
                    Ok(()) => write_success(self, cpu),
                    Err(error) => write_failure(self, cpu, dos_set_attr_error_code(&error)),
                }
            }
            0x02 => match drive.get_file_metadata(&resolved.path) {
                Ok(metadata) => {
                    let size = metadata.attributes.size;
                    cpu.regs.set_dx((size >> 16) as u16);
                    cpu.regs.set_ax(size as u16);
                    self.clear_extended_error();
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                }
                Err(error) => write_failure(self, cpu, io_error_code(&error, 2)),
            },
            0x03 => {
                let Some(time) = dos_to_systemtime(cpu.regs.di(), cpu.regs.cx(), 0) else {
                    write_failure(self, cpu, 1);
                    return;
                };
                match drive.set_file_time(&resolved.path, time) {
                    Ok(()) => write_success(self, cpu),
                    Err(error) => write_failure(self, cpu, io_error_code(&error, 5)),
                }
            }
            0x04 => match drive.get_file_metadata(&resolved.path) {
                Ok(metadata) => match systemtime_to_dos(metadata.last_write_time) {
                    Some((date, time, _)) => {
                        cpu.regs.set_cx(time);
                        cpu.regs.set_di(date);
                        write_success(self, cpu);
                    }
                    None => write_failure(self, cpu, 2),
                },
                Err(error) => write_failure(self, cpu, io_error_code(&error, 2)),
            },
            0x05 => {
                let Some(time) = dos_to_systemtime(cpu.regs.di(), 0, 0) else {
                    write_failure(self, cpu, 1);
                    return;
                };
                match drive.set_file_access_time(&resolved.path, time) {
                    Ok(()) => write_success(self, cpu),
                    Err(error) => write_failure(self, cpu, io_error_code(&error, 5)),
                }
            }
            0x06 => match drive.get_file_metadata(&resolved.path) {
                Ok(metadata) => match systemtime_to_dos(metadata.last_access_time) {
                    Some((date, _, _)) => {
                        cpu.regs.set_di(date);
                        write_success(self, cpu);
                    }
                    None => write_failure(self, cpu, 2),
                },
                Err(error) => write_failure(self, cpu, io_error_code(&error, 2)),
            },
            0x07 => {
                let Some(time) = dos_to_systemtime(cpu.regs.di(), cpu.regs.cx(), cpu.regs.si())
                else {
                    write_failure(self, cpu, 1);
                    return;
                };
                match drive.set_file_creation_time(&resolved.path, time) {
                    Ok(()) => write_success(self, cpu),
                    Err(error) => write_failure(self, cpu, io_error_code(&error, 5)),
                }
            }
            0x08 => match drive.get_file_metadata(&resolved.path) {
                Ok(metadata) => match systemtime_to_dos(metadata.creation_time) {
                    Some((date, time, hundredths)) => {
                        cpu.regs.set_cx(time);
                        cpu.regs.set_di(date);
                        cpu.regs.set_si(u16::from(hundredths));
                        write_success(self, cpu);
                    }
                    None => write_failure(self, cpu, 2),
                },
                Err(error) => write_failure(self, cpu, io_error_code(&error, 2)),
            },
            _ => write_failure(self, cpu, 1),
        }
    }

    /// Implement AX=7141h, including the API's explicit wildcard mode. A
    /// normal LFN delete treats SI=0 as one literal filename; SI=1 enumerates
    /// the parent directory, applies both name and attribute masks, and then
    /// deletes every matching regular file.
    pub(crate) fn lfn_delete_file(&mut self, cpu: &mut Cpu, memory: &mut Memory) {
        let wildcard_mode = cpu.regs.si();
        if wildcard_mode > 1 {
            write_failure(self, cpu, 1);
            return;
        }

        let path = self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_dx(cpu));
        trace_dos_path("LFN_DELETE", cpu.regs.ds, self.dos_offset_dx(cpu), &path);
        let resolved = self.path_resolver.resolve_lfn(&path);

        if wildcard_mode == 0 {
            let Some(drive) = self.drives.get_mut(&resolved.drive) else {
                write_failure(self, cpu, 3);
                return;
            };
            match drive.get_attributes(&resolved.path) {
                Ok(attributes) if attributes.directory || attributes.read_only => {
                    write_failure(self, cpu, 5)
                }
                Ok(_) => match drive.remove_file(&resolved.path) {
                    Ok(()) => write_success(self, cpu),
                    Err(error) => write_failure(self, cpu, io_error_code(&error, 5)),
                },
                Err(error) => write_failure(self, cpu, io_error_code(&error, 2)),
            }
            return;
        }

        let split = resolved.path.rfind('\\').unwrap_or(0);
        let file_pattern = &resolved.path[split + 1..];
        let dir_path = resolved.path[..split].to_string();
        let attr_filter = cpu.regs.cx();
        let Some(drive) = self.drives.get_mut(&resolved.drive) else {
            write_failure(self, cpu, 3);
            return;
        };
        let directory_entries = match drive.read_dir(&dir_path) {
            Ok(entries) => entries,
            Err(error) => {
                let code = match error
                    .downcast_ref::<std::io::Error>()
                    .map(std::io::Error::kind)
                {
                    Some(std::io::ErrorKind::PermissionDenied) => 5,
                    _ => 3,
                };
                write_failure(self, cpu, code);
                return;
            }
        };
        let names: Vec<String> = directory_entries
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        let aliases: std::collections::HashMap<_, _> = crate::sfn::assign_short_names_in_dir(
            drive.resolve_host_path(&dir_path).as_deref(),
            &names,
        )
        .into_iter()
        .collect();
        let matching_paths: Vec<(String, bool)> = directory_entries
            .iter()
            .filter_map(|(long_name, attributes)| {
                if attributes.directory || !Self::matches_lfn_attr_filter(attributes, attr_filter) {
                    return None;
                }
                let short_name = aliases
                    .get(long_name)
                    .map(String::as_str)
                    .unwrap_or(long_name.as_str());
                if !Self::matches_lfn_pattern(file_pattern, long_name)
                    && !Self::matches_lfn_pattern(file_pattern, short_name)
                {
                    return None;
                }
                let path = if dir_path == r"\" {
                    format!(r"\{long_name}")
                } else {
                    format!(r"{dir_path}\{long_name}")
                };
                Some((path, attributes.read_only))
            })
            .collect();

        if matching_paths.is_empty() {
            write_failure(self, cpu, 2);
            return;
        }
        for (matching_path, read_only) in matching_paths {
            if read_only {
                write_failure(self, cpu, 5);
                return;
            }
            if let Err(error) = drive.remove_file(&matching_path) {
                write_failure(self, cpu, io_error_code(&error, 5));
                return;
            }
        }
        write_success(self, cpu);
    }

    fn lfn_path_variant(
        &mut self,
        resolved: &crate::files::ResolvedPath,
        kind: TrueNameKind,
    ) -> Result<String, u16> {
        let components: Vec<&str> = resolved
            .path
            .trim_start_matches('\\')
            .split('\\')
            .filter(|component| !component.is_empty())
            .collect();
        let Some(drive) = self.drives.get_mut(&resolved.drive) else {
            return Err(3);
        };

        let mut parent = String::from("\\");
        let mut result = String::from("\\");
        for (index, component) in components.iter().enumerate() {
            let is_final = index + 1 == components.len();
            let directory_entries = drive
                .read_dir(&parent)
                .map_err(|error| io_error_code(&error, 3))?;
            let names: Vec<String> = directory_entries
                .iter()
                .map(|(name, _)| name.clone())
                .collect();
            let aliases: std::collections::HashMap<_, _> = crate::sfn::assign_short_names_in_dir(
                drive.resolve_host_path(&parent).as_deref(),
                &names,
            )
            .into_iter()
            .collect();
            let found = directory_entries.iter().find(|(name, _)| {
                name.eq_ignore_ascii_case(component)
                    || aliases
                        .get(name)
                        .is_some_and(|alias| alias.eq_ignore_ascii_case(component))
            });

            let selected = if let Some((name, _)) = found {
                match kind {
                    TrueNameKind::Long => name.clone(),
                    TrueNameKind::Canonical | TrueNameKind::Short => aliases
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| name.to_uppercase()),
                }
            } else if is_final {
                match kind {
                    TrueNameKind::Long => (*component).to_string(),
                    TrueNameKind::Canonical => {
                        crate::sfn::generate_short_name_without_tail(component).ok_or(2u16)?
                    }
                    TrueNameKind::Short => return Err(2),
                }
            } else {
                return Err(2);
            };

            if result.len() > 1 {
                result.push('\\');
            }
            result.push_str(&selected);
            let host_name = found.map(|(name, _)| name.as_str()).unwrap_or(component);
            parent = if parent == "\\" {
                format!(r"\{host_name}")
            } else {
                format!(r"{parent}\{host_name}")
            };
        }
        Ok(result)
    }

    /// CL=00 has one historical quirk: an absolute source is canonicalized
    /// entirely to short names, while a relative source keeps its relative
    /// long-name components (upper-cased) after the short current directory.
    fn lfn_relative_canonical(
        &mut self,
        resolved: &crate::files::ResolvedPath,
    ) -> Result<String, u16> {
        let current_dir = self.path_resolver.get_current_dir(resolved.drive);
        let current_components: Vec<&str> = current_dir
            .trim_start_matches('\\')
            .split('\\')
            .filter(|component| !component.is_empty())
            .collect();
        let resolved_components: Vec<&str> = resolved
            .path
            .trim_start_matches('\\')
            .split('\\')
            .filter(|component| !component.is_empty())
            .collect();
        let common = current_components
            .iter()
            .zip(&resolved_components)
            .take(current_components.len())
            .take_while(|(current, resolved)| current.eq_ignore_ascii_case(resolved))
            .count();

        let short_prefix = crate::files::ResolvedPath {
            drive: resolved.drive,
            path: if common == 0 {
                String::from(r"\")
            } else {
                format!(r"\{}", resolved_components[..common].join(r"\"))
            },
        };
        let short_prefix = self.lfn_path_variant(&short_prefix, TrueNameKind::Short)?;
        let long_path = self.lfn_path_variant(resolved, TrueNameKind::Long)?;
        let long_components: Vec<&str> = long_path
            .trim_start_matches('\\')
            .split('\\')
            .filter(|component| !component.is_empty())
            .collect();

        let mut result = short_prefix;
        for component in long_components.iter().skip(common) {
            if result.len() > 1 {
                result.push('\\');
            }
            result.push_str(&component.to_uppercase());
        }
        Ok(result)
    }

    /// Qualify a DOS path and, when requested, expand the guest-visible drive
    /// letter through the LFN SUBST chain. SUBST mappings created by the shell
    /// predate this pathname metadata, so those deliberately remain qualified
    /// with their mapped drive instead of exposing a host filesystem path.
    fn lfn_qualified_path(&self, drive: u8, path: &str, expand_subst: bool) -> Result<String, u16> {
        let mut current_drive = drive;
        let mut current_path = path.to_string();
        let mut visited = [false; 26];

        if expand_subst {
            while let Some(target) = self.subst_targets.get(&current_drive) {
                if visited[usize::from(current_drive)] {
                    return Err(3);
                }
                visited[usize::from(current_drive)] = true;
                let (Some(target_drive), target_root) = PathResolver::split_drive_and_path(target)
                else {
                    return Err(3);
                };
                let target_root = if target_root.is_empty() {
                    r"\"
                } else {
                    target_root
                };
                current_path = if target_root == r"\" {
                    current_path
                } else if current_path == r"\" {
                    target_root.to_string()
                } else {
                    format!(r"{target_root}{current_path}")
                };
                current_drive = target_drive;
            }
        }

        Ok(format!(
            "{}:{}",
            (current_drive + b'A') as char,
            current_path
        ))
    }

    pub(crate) fn lfn_true_name(&mut self, cpu: &mut Cpu, memory: &mut Memory) {
        let function = cpu.regs.cl();
        let subst_mode = cpu.regs.ch();
        if subst_mode != 0 && subst_mode != 0x80 {
            write_failure(self, cpu, 1);
            return;
        }
        let source = self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_si(cpu));
        if let (Some(drive), drive_path) = PathResolver::split_drive_and_path(&source) {
            if drive_path.is_empty() {
                write_failure(
                    self,
                    cpu,
                    if self.drives.contains_key(&drive) {
                        2
                    } else {
                        3
                    },
                );
                return;
            }
        }
        let is_relative = !PathResolver::split_drive_and_path(&source)
            .1
            .starts_with('\\');
        let resolved = self.path_resolver.resolve_lfn(&source);
        let kind = match function {
            0 => TrueNameKind::Canonical,
            1 => TrueNameKind::Short,
            2 => TrueNameKind::Long,
            _ => {
                write_failure(self, cpu, 1);
                return;
            }
        };
        let path = match if function == 0 && is_relative {
            self.lfn_relative_canonical(&resolved)
        } else {
            self.lfn_path_variant(&resolved, kind)
        } {
            Ok(path) => path,
            Err(code) => {
                write_failure(self, cpu, code);
                return;
            }
        };
        let qualified = match self.lfn_qualified_path(resolved.drive, &path, subst_mode == 0) {
            Ok(path) => path,
            Err(code) => {
                write_failure(self, cpu, code);
                return;
            }
        };
        let limit = if function == 1 { 128 } else { MAX_DOS_PATH + 1 };
        if qualified.len() + 1 > limit {
            write_failure(self, cpu, 3);
            return;
        }
        let address = self.dos_translate_es_di(cpu);
        write_ascii_z(memory, address, &qualified, limit);
        write_success(self, cpu);
    }

    pub(crate) fn lfn_volume_info(&mut self, cpu: &mut Cpu, memory: &mut Memory) {
        let root = self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_dx(cpu));
        let resolved = self.path_resolver.resolve_lfn(&root);
        let buffer_size = usize::from(cpu.regs.cx());
        if !self.drives.contains_key(&resolved.drive) {
            write_failure(self, cpu, 15);
            return;
        }

        let address = self.dos_translate_es_di(cpu);
        write_ascii_z(memory, address, "FAT32", buffer_size);
        cpu.regs.set_bx(DOS_LFN_VOLUME_FLAGS);
        cpu.regs.set_cx(255);
        cpu.regs.set_dx(260);
        self.clear_extended_error();
        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
    }

    pub(crate) fn lfn_file_info(&mut self, cpu: &mut Cpu, memory: &mut Memory) {
        let handle = cpu.regs.bx();
        let Some(entry) = self.file_table.get_sft_mut(handle) else {
            write_failure(self, cpu, 6);
            return;
        };
        let path = entry.path.clone();
        let current_position = entry.file.stream_position().unwrap_or(entry.position);
        let size = entry.file.seek(SeekFrom::End(0)).unwrap_or(entry.position);
        let _ = entry.file.seek(SeekFrom::Start(current_position));
        entry.position = current_position;
        let address = self.dos_translate_ds_dx(cpu);

        let (drive_number, drive_path) = PathResolver::split_drive_and_path(&path);
        let metadata = drive_number
            .and_then(|drive| {
                self.drives
                    .get_mut(&drive)
                    .and_then(|drive| drive.get_file_metadata(drive_path).ok())
            })
            .map(|mut metadata| {
                metadata.attributes.size = size;
                metadata
            })
            .unwrap_or_else(|| {
                let attributes = FileAttributes {
                    archive: true,
                    size,
                    modified: UNIX_EPOCH,
                    ..Default::default()
                };
                FileMetadata {
                    attributes,
                    creation_time: UNIX_EPOCH,
                    last_access_time: UNIX_EPOCH,
                    last_write_time: UNIX_EPOCH,
                }
            });

        memory.write_u32(address, u32::from(dos_attr_bits(&metadata.attributes)));
        write_qword(
            memory,
            address + 4,
            Self::systemtime_to_filetime(metadata.creation_time),
        );
        write_qword(
            memory,
            address + 12,
            Self::systemtime_to_filetime(metadata.last_access_time),
        );
        write_qword(
            memory,
            address + 20,
            Self::systemtime_to_filetime(metadata.last_write_time),
        );
        let serial = 0x4D44_0000u32 | u32::from(drive_number.unwrap_or(0));
        memory.write_u32(address + 28, serial);
        memory.write_u32(address + 32, (size >> 32) as u32);
        memory.write_u32(address + 36, size as u32);
        memory.write_u32(address + 40, 1);
        let mut file_id = 0xcbf2_9ce4_8422_2325u64;
        for byte in path.bytes() {
            file_id ^= u64::from(byte);
            file_id = file_id.wrapping_mul(0x100_0000_01b3);
        }
        write_qword(memory, address + 44, file_id);
        write_success(self, cpu);
    }

    pub(crate) fn lfn_time_conversion(&mut self, cpu: &mut Cpu, memory: &mut Memory) {
        match cpu.regs.bl() {
            0 => {
                let filetime = read_qword(memory, self.dos_translate_ds_si(cpu));
                match filetime_to_dos(filetime) {
                    Some((date, time, hundredths)) => {
                        cpu.regs.set_cx(time);
                        cpu.regs.set_dx(date);
                        cpu.regs.set_bh(hundredths);
                        write_success(self, cpu);
                    }
                    None => write_failure(self, cpu, 1),
                }
            }
            1 => {
                let Some(filetime) =
                    dos_to_filetime(cpu.regs.dx(), cpu.regs.cx(), u16::from(cpu.regs.bh()))
                else {
                    write_failure(self, cpu, 1);
                    return;
                };
                write_qword(memory, self.dos_translate_es_di(cpu), filetime);
                write_success(self, cpu);
            }
            _ => write_failure(self, cpu, 1),
        }
    }

    pub(crate) fn lfn_generate_short_name(&mut self, cpu: &mut Cpu, memory: &mut Memory) {
        let format = cpu.regs.dh();
        let charsets = cpu.regs.dl();
        let long_charset = charsets & 0x0F;
        let short_charset = charsets >> 4;
        if !matches!(format, 0 | 1) || long_charset > 2 || short_charset > 2 {
            write_failure(self, cpu, 1);
            return;
        }

        let long_name = self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_si(cpu));
        if long_name.is_empty()
            || long_name
                .chars()
                .any(|character| matches!(character, '\\' | '/' | ':'))
        {
            write_failure(self, cpu, 1);
            return;
        }
        let Some(short_name) = crate::sfn::generate_short_name_without_tail(&long_name) else {
            write_failure(self, cpu, 1);
            return;
        };

        let output = if format == 0 {
            let mut fcb = [b' '; 11];
            if short_name == "." || short_name == ".." {
                fcb[..short_name.len()].copy_from_slice(short_name.as_bytes());
            } else {
                let (base, extension) = short_name
                    .split_once('.')
                    .map_or((short_name.as_str(), ""), |(base, extension)| {
                        (base, extension)
                    });
                for (slot, byte) in base.bytes().take(8).enumerate() {
                    fcb[slot] = byte;
                }
                for (slot, byte) in extension.bytes().take(3).enumerate() {
                    fcb[8 + slot] = byte;
                }
            }
            fcb.to_vec()
        } else {
            short_name.into_bytes()
        };
        let address = self.dos_translate_es_di(cpu);
        for (index, byte) in output.iter().copied().enumerate() {
            memory.write_u8(address + index, byte);
        }
        // DH=00h is the fixed-width 11-byte FCB form.  It has no terminator;
        // only the ordinary DH=01h 8.3 form is an ASCIIZ string.
        if format == 1 {
            memory.write_u8(address + output.len(), 0);
        }
        write_success(self, cpu);
    }

    pub(crate) fn lfn_open(&mut self, cpu: &mut Cpu, memory: &mut Memory) {
        let open_mode = cpu.regs.bl();
        let access_mode = open_mode & 0x07;
        let sft_access = match access_mode {
            0..=2 => access_mode,
            4 => 0, // read-only, do not update last-access time
            _ => {
                write_failure(self, cpu, 12);
                return;
            }
        };
        let share_mode = open_mode & 0x70;
        let action = cpu.regs.dx();
        if action & !0x0013 != 0 {
            write_failure(self, cpu, 1);
            return;
        }
        let create_if_missing = action & 0x0010 != 0;
        let open_if_existing = action & 0x0001 != 0;
        let truncate_if_existing = action & 0x0002 != 0;
        let attributes = cpu.regs.cx();
        let filename = self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_si(cpu));
        trace_dos_path(
            "LFN_OPENEX",
            cpu.regs.ds,
            self.dos_offset_si(cpu),
            &filename,
        );

        if let Some(device) = crate::filesystem::parse_device_name(&filename) {
            let result = self.open_named_device(device, share_mode, sft_access);
            self.set_device_open_return(cpu, result);
            if result.is_ok() {
                cpu.regs.set_cx(1);
            }
            return;
        }

        let resolved = self.path_resolver.resolve_lfn(&filename);
        let existing_attributes = match self.drives.get_mut(&resolved.drive) {
            // A few DOS backends expose a plain `anyhow!` for a missing path
            // rather than preserving an `io::Error`.  As with the legacy
            // extended-open path, any failed existence probe is therefore
            // treated as "not present" and the real create/open call decides
            // the final DOS error.
            Some(drive) => drive.get_attributes(&resolved.path).ok(),
            None => {
                write_failure(self, cpu, 3);
                return;
            }
        };
        let file_exists = existing_attributes.is_some();
        if existing_attributes.is_some_and(|attributes| attributes.directory) {
            write_failure(self, cpu, 5);
            return;
        }
        let (should_create, should_truncate, action_taken) = if file_exists {
            if truncate_if_existing {
                (false, true, 3)
            } else if open_if_existing {
                (false, false, 1)
            } else {
                write_failure(self, cpu, 80);
                return;
            }
        } else if create_if_missing {
            (true, false, 2)
        } else {
            let code = self.missing_dos_path_error(resolved.drive, &resolved.path);
            write_failure(self, cpu, code);
            return;
        };

        let write_access = matches!(access_mode, 1 | 2);
        let full_path = format!("{}:{}", (resolved.drive + b'A') as char, resolved.path);
        let identity = self.resolved_file_identity(resolved.drive, &resolved.path);
        if self.reject_pre_open_sharing_violation(cpu, &identity, share_mode, sft_access) {
            return;
        }
        if (should_create || should_truncate) && !self.file_table.has_open_capacity() {
            write_failure(self, cpu, 4);
            return;
        }

        let file_result = {
            let drive = self
                .drives
                .get_mut(&resolved.drive)
                .expect("drive disappeared during LFN open");
            if should_create {
                drive.open_file(&resolved.path, true, true, true)
            } else if should_truncate {
                drive.open_file(&resolved.path, true, false, true)
            } else {
                drive.open_file(&resolved.path, write_access, false, false)
            }
        };
        let file = match file_result {
            Ok(file) => file,
            Err(error) => {
                write_failure(self, cpu, io_error_code(&error, 5));
                return;
            }
        };
        let Some(handle) = self.file_table.allocate_with_sharing_identity(
            file,
            full_path,
            identity,
            self.current_psp,
            share_mode,
            sft_access,
        ) else {
            write_failure(self, cpu, 4);
            return;
        };

        if should_create && attributes != 0 {
            let attrs = dos_attrs_from_bits(attributes);
            let set_result = self
                .drives
                .get_mut(&resolved.drive)
                .expect("drive disappeared after LFN open")
                .set_attributes(&resolved.path, attrs);
            if let Err(error) = set_result {
                let _ = self.file_table.try_free(handle);
                write_failure(self, cpu, dos_set_attr_error_code(&error));
                return;
            }
        }
        cpu.regs.set_ax(handle);
        cpu.regs.set_cx(action_taken);
        self.clear_extended_error();
        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
    }

    pub(crate) fn lfn_subst(&mut self, cpu: &mut Cpu, memory: &mut Memory) {
        let operation = cpu.regs.bh();
        let drive_arg = cpu.regs.bl();
        match operation {
            0 => {
                let drive = if drive_arg == 0 {
                    self.path_resolver.get_current_drive()
                } else {
                    drive_arg.saturating_sub(1)
                };
                if drive >= 26 {
                    write_failure(self, cpu, 15);
                    return;
                }
                let target = self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_dx(cpu));
                let resolved = self.path_resolver.resolve_lfn(&target);
                let target_path = self
                    .drives
                    .get_mut(&resolved.drive)
                    .and_then(|drive| drive.resolve_host_path(&resolved.path));
                let Some(target_path) = target_path else {
                    write_failure(self, cpu, 3);
                    return;
                };
                let target_dos = format!("{}:{}", (resolved.drive + b'A') as char, resolved.path);
                match std::fs::metadata(&target_path) {
                    Ok(metadata) if metadata.is_dir() => {
                        match self.subst_create_with_target(drive, target_path, Some(target_dos)) {
                            Ok(()) => write_success(self, cpu),
                            Err(_) => write_failure(self, cpu, 5),
                        }
                    }
                    Ok(_) => write_failure(self, cpu, 3),
                    Err(error) => {
                        write_failure(self, cpu, io_error_code(&anyhow::Error::from(error), 3))
                    }
                }
            }
            1 => {
                let Some(drive) = drive_arg.checked_sub(1).filter(|drive| *drive < 26) else {
                    write_failure(self, cpu, 1);
                    return;
                };
                match self.subst_delete(drive) {
                    Ok(()) => write_success(self, cpu),
                    Err(_) => write_failure(self, cpu, 1),
                }
            }
            2 => {
                let Some(drive) = drive_arg.checked_sub(1).filter(|drive| *drive < 26) else {
                    write_failure(self, cpu, 1);
                    return;
                };
                let Some(root) = self
                    .subst_mappings()
                    .into_iter()
                    .find(|(mapped_drive, _)| *mapped_drive == drive)
                    .map(|(_, root)| root)
                else {
                    write_failure(self, cpu, 1);
                    return;
                };
                let target = self
                    .subst_targets
                    .get(&drive)
                    .cloned()
                    .unwrap_or_else(|| root.to_string_lossy().into_owned());
                let address = self.dos_translate_ds_dx(cpu);
                write_ascii_z(memory, address, &target, MAX_DOS_PATH + 1);
                write_success(self, cpu);
            }
            _ => write_failure(self, cpu, 1),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::tests::create_test_io;
    use tempfile::TempDir;

    const PATH_SEG: u16 = 0x2000;
    const PATH_OFF: u16 = 0x0100;
    const OUTPUT_SEG: u16 = 0x3000;
    const OUTPUT_OFF: u16 = 0x0200;

    struct Fixture {
        dos: Dos,
        cpu: Cpu,
        memory: Memory,
        vga: crate::Vga,
        keyboard: crate::Keyboard,
        _temp: TempDir,
    }

    impl Fixture {
        fn new(files: &[(&str, &[u8])]) -> Self {
            let temp = TempDir::new().unwrap();
            for (name, bytes) in files {
                std::fs::write(temp.path().join(name), bytes).unwrap();
            }
            let mut dos = Dos::new();
            dos.drives.insert(
                2,
                Box::new(crate::filesystem::HostDrive::new(temp.path().to_path_buf())),
            );
            let (vga, keyboard) = create_test_io();
            Self {
                dos,
                cpu: Cpu::new(),
                memory: Memory::new(),
                vga,
                keyboard,
                _temp: temp,
            }
        }

        fn write_string(&mut self, segment: u16, offset: u16, value: &str) {
            let address = Memory::seg_offset_to_linear(segment, offset);
            for (index, byte) in value.bytes().chain(std::iter::once(0)).enumerate() {
                self.memory.write_u8(address + index, byte);
            }
        }

        fn call(&mut self) {
            self.dos
                .int21(
                    &mut self.cpu,
                    &mut self.memory,
                    &mut self.vga,
                    &mut self.keyboard,
                    &mut crate::Pic::new(),
                )
                .unwrap();
        }

        fn output(&self) -> String {
            let address = Memory::seg_offset_to_linear(OUTPUT_SEG, OUTPUT_OFF);
            (0..MAX_DOS_PATH)
                .map(|index| self.memory.read_u8(address + index))
                .take_while(|byte| *byte != 0)
                .map(char::from)
                .collect()
        }
    }

    #[test]
    fn lfn_a7_round_trips_filetime_and_rejects_bad_dates() {
        let date = ((2024 - 1980) << 9) | (6 << 5) | 15;
        let time = (13 << 11) | (42 << 5) | (7 / 2);
        let hundredths = 123u8;
        let filetime = dos_to_filetime(date, time, u16::from(hundredths)).unwrap();

        let mut fixture = Fixture::new(&[]);
        let input_address = Memory::seg_offset_to_linear(PATH_SEG, PATH_OFF);
        write_qword(&mut fixture.memory, input_address, filetime);
        fixture.cpu.regs.set_ax(0x71A7);
        fixture.cpu.regs.set_bl(0);
        fixture.cpu.regs.ds = PATH_SEG;
        fixture.cpu.regs.set_si(PATH_OFF);
        fixture.call();
        assert!(!fixture.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(fixture.cpu.regs.cx(), time);
        assert_eq!(fixture.cpu.regs.dx(), date);
        assert_eq!(fixture.cpu.regs.bh(), hundredths);

        fixture.cpu.regs.set_ax(0x71A7);
        fixture.cpu.regs.set_bl(1);
        fixture.cpu.regs.set_cx(time);
        fixture.cpu.regs.set_dx(date);
        fixture.cpu.regs.set_bh(hundredths);
        fixture.cpu.regs.es = OUTPUT_SEG;
        fixture.cpu.regs.set_di(OUTPUT_OFF);
        fixture.call();
        assert_eq!(
            read_qword(
                &fixture.memory,
                Memory::seg_offset_to_linear(OUTPUT_SEG, OUTPUT_OFF)
            ),
            filetime
        );

        write_qword(&mut fixture.memory, input_address, 0);
        fixture.cpu.regs.set_ax(0x71A7);
        fixture.cpu.regs.set_bl(0);
        fixture.call();
        assert!(fixture.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(fixture.cpu.regs.ax(), 1);
    }

    #[test]
    fn lfn_710d_uses_default_and_one_based_drive_numbers() {
        let mut fixture = Fixture::new(&[]);
        fixture.cpu.regs.set_ax(0x710D);
        fixture.cpu.regs.set_cx(0);
        fixture.cpu.regs.set_dx(0); // default drive, which is C: in Dos::new
        fixture.call();
        assert!(!fixture.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

        fixture.cpu.regs.set_ax(0x710D);
        fixture.cpu.regs.set_dx(3); // explicit C: uses one-based numbering
        fixture.call();
        assert!(!fixture.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    }

    #[test]
    fn lfn_a8_returns_tail_free_sfn_in_both_formats() {
        let mut fixture = Fixture::new(&[]);
        fixture.write_string(PATH_SEG, PATH_OFF, "Long File Name.txt");
        fixture.cpu.regs.set_ax(0x71A8);
        fixture.cpu.regs.set_dh(1);
        fixture.cpu.regs.set_dl(0x11); // OEM long and short character sets.
        fixture.cpu.regs.ds = PATH_SEG;
        fixture.cpu.regs.set_si(PATH_OFF);
        fixture.cpu.regs.es = OUTPUT_SEG;
        fixture.cpu.regs.set_di(OUTPUT_OFF);
        fixture.call();
        assert_eq!(fixture.output(), r"LONGFILE.TXT");

        fixture.cpu.regs.set_ax(0x71A8);
        fixture.cpu.regs.set_dh(0);
        fixture.memory.write_u8(
            Memory::seg_offset_to_linear(OUTPUT_SEG, OUTPUT_OFF) + 11,
            0xA5,
        );
        fixture.call();
        let address = Memory::seg_offset_to_linear(OUTPUT_SEG, OUTPUT_OFF);
        let mut fcb = [0u8; 11];
        for (index, byte) in fcb.iter_mut().enumerate() {
            *byte = fixture.memory.read_u8(address + index);
        }
        assert_eq!(&fcb, b"LONGFILETXT");
        assert_eq!(fixture.memory.read_u8(address + 11), 0xA5);
    }

    #[test]
    fn lfn_7160_converts_between_long_and_short_paths() {
        let mut fixture = Fixture::new(&[("Long File Name.txt", b"data")]);
        fixture.write_string(PATH_SEG, PATH_OFF, r"C:\Long File Name.txt");
        fixture.cpu.regs.ds = PATH_SEG;
        fixture.cpu.regs.set_si(PATH_OFF);
        fixture.cpu.regs.es = OUTPUT_SEG;
        fixture.cpu.regs.set_di(OUTPUT_OFF);

        fixture.cpu.regs.set_ax(0x7160);
        fixture.cpu.regs.set_cl(1);
        fixture.call();
        assert_eq!(fixture.output(), r"C:\LONGFI~1.TXT");

        fixture.write_string(PATH_SEG, PATH_OFF, r"C:\LONGFI~1.TXT");
        fixture.cpu.regs.set_ax(0x7160);
        fixture.cpu.regs.set_cl(2);
        fixture.call();
        assert_eq!(fixture.output(), r"C:\Long File Name.txt");
    }

    #[test]
    fn lfn_7160_rejects_a_drive_letter_without_a_path() {
        let mut fixture = Fixture::new(&[]);
        fixture.write_string(PATH_SEG, PATH_OFF, "C:");
        fixture.cpu.regs.set_ax(0x7160);
        fixture.cpu.regs.set_cl(0);
        fixture.cpu.regs.ds = PATH_SEG;
        fixture.cpu.regs.set_si(PATH_OFF);
        fixture.cpu.regs.es = OUTPUT_SEG;
        fixture.cpu.regs.set_di(OUTPUT_OFF);
        fixture.call();

        assert!(fixture.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(fixture.cpu.regs.ax(), 2);
    }

    #[test]
    fn lfn_7160_relative_canonical_uses_short_cwd_and_long_suffix() {
        let mut fixture = Fixture::new(&[]);
        std::fs::create_dir(fixture._temp.path().join("Long Directory")).unwrap();
        std::fs::write(
            fixture
                ._temp
                .path()
                .join("Long Directory")
                .join("Long File.txt"),
            b"data",
        )
        .unwrap();
        fixture
            .dos
            .path_resolver
            .set_current_dir_lfn(r"C:\Long Directory");
        fixture.write_string(PATH_SEG, PATH_OFF, "Long File.txt");
        fixture.cpu.regs.set_ax(0x7160);
        fixture.cpu.regs.set_cl(0);
        fixture.cpu.regs.ds = PATH_SEG;
        fixture.cpu.regs.set_si(PATH_OFF);
        fixture.cpu.regs.es = OUTPUT_SEG;
        fixture.cpu.regs.set_di(OUTPUT_OFF);
        fixture.call();

        assert_eq!(fixture.output(), r"C:\LONGDI~1\LONG FILE.TXT");
    }

    #[test]
    fn lfn_a9_creates_and_reopens_a_long_name() {
        let mut fixture = Fixture::new(&[]);
        fixture.write_string(PATH_SEG, PATH_OFF, r"C:\Created Long Name.txt");
        fixture.cpu.regs.ds = PATH_SEG;
        fixture.cpu.regs.set_si(PATH_OFF);
        fixture.cpu.regs.set_bx(2); // read/write
        fixture.cpu.regs.set_cx(0x20);
        fixture.cpu.regs.set_dx(0x10); // create if missing
        fixture.cpu.regs.set_ax(0x71A9);
        fixture.call();
        assert!(!fixture.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(fixture.cpu.regs.cx(), 2);
        let handle = fixture.cpu.regs.ax();
        assert!(fixture.dos.file_table.is_valid(handle));
        assert!(fixture._temp.path().join("Created Long Name.txt").exists());
        fixture.dos.file_table.try_free(handle).unwrap();

        fixture.cpu.regs.set_ax(0x71A9);
        fixture.cpu.regs.set_bx(0);
        fixture.cpu.regs.set_dx(1); // open if exists
        fixture.call();
        assert!(!fixture.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(fixture.cpu.regs.cx(), 1);
    }

    #[test]
    fn lfn_open_rejects_directories_as_files() {
        let mut fixture = Fixture::new(&[]);
        std::fs::create_dir(fixture._temp.path().join("Existing Directory")).unwrap();
        fixture.write_string(PATH_SEG, PATH_OFF, r"C:\Existing Directory");
        fixture.cpu.regs.set_ax(0x716C);
        fixture.cpu.regs.set_bx(0);
        fixture.cpu.regs.set_cx(0);
        fixture.cpu.regs.set_dx(1); // open if exists
        fixture.cpu.regs.ds = PATH_SEG;
        fixture.cpu.regs.set_si(PATH_OFF);
        fixture.call();

        assert!(fixture.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(fixture.cpu.regs.ax(), 5);
    }

    #[test]
    fn lfn_7141_deletes_matching_long_names_when_wildcards_are_enabled() {
        let mut fixture = Fixture::new(&[
            ("Long File One.txt", b"one"),
            ("Long File Two.txt", b"two"),
            ("keep.bin", b"keep"),
        ]);
        fixture.write_string(PATH_SEG, PATH_OFF, r"C:\*.TXT");
        fixture.cpu.regs.set_ax(0x7141);
        fixture.cpu.regs.ds = PATH_SEG;
        fixture.cpu.regs.set_dx(PATH_OFF);
        fixture.cpu.regs.set_si(1); // wildcards and attribute matching enabled
        fixture.cpu.regs.set_cx(0);
        fixture.call();

        assert!(!fixture.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert!(!fixture._temp.path().join("Long File One.txt").exists());
        assert!(!fixture._temp.path().join("Long File Two.txt").exists());
        assert!(fixture._temp.path().join("keep.bin").exists());
    }

    #[test]
    fn lfn_71aa_creates_queries_and_terminates_subst() {
        let mut fixture = Fixture::new(&[]);
        std::fs::create_dir(fixture._temp.path().join("Target Directory")).unwrap();
        fixture.write_string(PATH_SEG, PATH_OFF, r"C:\Target Directory");
        fixture.cpu.regs.ds = PATH_SEG;
        fixture.cpu.regs.set_dx(PATH_OFF);
        fixture.cpu.regs.set_ax(0x71AA);
        fixture.cpu.regs.set_bh(0);
        fixture.cpu.regs.set_bl(1); // A:
        fixture.call();
        assert!(!fixture.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

        fixture.cpu.regs.set_ax(0x71AA);
        fixture.cpu.regs.set_bh(2);
        fixture.cpu.regs.set_bl(1);
        fixture.cpu.regs.es = OUTPUT_SEG;
        fixture.cpu.regs.set_di(OUTPUT_OFF);
        fixture.cpu.regs.ds = OUTPUT_SEG;
        fixture.cpu.regs.set_dx(OUTPUT_OFF);
        fixture.call();
        let address = Memory::seg_offset_to_linear(OUTPUT_SEG, OUTPUT_OFF);
        assert_eq!(
            (0..MAX_DOS_PATH)
                .map(|index| fixture.memory.read_u8(address + index))
                .take_while(|byte| *byte != 0)
                .map(char::from)
                .collect::<String>(),
            r"C:\Target Directory"
        );

        fixture.write_string(PATH_SEG, PATH_OFF, r"A:\");
        fixture.cpu.regs.set_ax(0x7160);
        fixture.cpu.regs.set_cl(2);
        fixture.cpu.regs.set_ch(0); // expand the SUBST drive
        fixture.cpu.regs.ds = PATH_SEG;
        fixture.cpu.regs.set_si(PATH_OFF);
        fixture.cpu.regs.es = OUTPUT_SEG;
        fixture.cpu.regs.set_di(OUTPUT_OFF);
        fixture.call();
        assert_eq!(fixture.output(), r"C:\Target Directory");

        fixture.cpu.regs.set_ax(0x7160);
        fixture.cpu.regs.set_cl(2);
        fixture.cpu.regs.set_ch(0x80); // retain the SUBST drive letter
        fixture.call();
        assert_eq!(fixture.output(), r"A:\");

        fixture.cpu.regs.set_ax(0x71AA);
        fixture.cpu.regs.set_bh(1);
        fixture.cpu.regs.set_bl(1);
        fixture.call();
        assert!(!fixture.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert!(fixture.dos.subst_mappings().is_empty());
    }
}
