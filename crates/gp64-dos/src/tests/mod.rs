mod aux_prn_devices;
mod bpb_geometry;
mod console_alias_occupancy;
mod console_and_file_io;
mod devices_and_extenders;
mod dos_core;
mod dpmi_coverage;
mod dpmi_int21;
mod fcb;
mod indos_lifecycle;
mod int21_coverage;
mod int21_edge_cases;
mod int21_functions;
mod int21_process_and_time;
mod kernel_enhancements;
mod kernel_structures;
mod lfn_and_drive_attributes;
mod metadata_pointers;
mod not_ready_drive;
mod overlays_and_tsr;
mod vshare_sharing;

use super::*;
use crate::filesystem::{VirtualFile, BRIDGE_CAPACITY};
use gp64_types::test_util::{env_lock, EnvVarGuard};
use std::io::{Cursor, Read, Seek, Write};
use std::time::{Duration, SystemTime};

fn trace_subscriber(filter: &str) -> tracing::subscriber::DefaultGuard {
    use tracing_subscriber::EnvFilter;
    let sub = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .with_writer(std::io::sink)
        .finish();
    tracing::subscriber::set_default(sub)
}

struct DiskSpaceDrive {
    total: u64,
    free: u64,
}

impl DiskSpaceDrive {
    fn new(total: u64, free: u64) -> Self {
        Self { total, free }
    }
}

struct DenyWriteDrive;
struct ReadOnlyDrive;
struct NotFoundDrive;
/// A drive that really carries a volume label, so the no-synthesis fix can be
/// shown not to have gone too far. MDD-BUG-ANVIL-00137.
struct LabelledDrive;
struct NoSuchFileDrive;
struct AllAttributesDrive;

impl VirtualDrive for DiskSpaceDrive {
    fn open_file(
        &mut self,
        _path: &str,
        _write: bool,
        _create: bool,
        _truncate: bool,
    ) -> Result<Box<dyn VirtualFile>> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn create_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn remove_file(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn remove_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn rename(&mut self, _from: &str, _to: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn get_attributes(&mut self, _path: &str) -> Result<crate::filesystem::FileAttributes> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn set_attributes(
        &mut self,
        _path: &str,
        _attrs: crate::filesystem::FileAttributes,
    ) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn set_file_time(&mut self, _path: &str, _modified: std::time::SystemTime) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn read_dir(
        &mut self,
        _path: &str,
    ) -> Result<Vec<(String, crate::filesystem::FileAttributes)>> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn get_disk_space(&self) -> Result<(u64, u64)> {
        Ok((self.total, self.free))
    }
}

struct ErrorDiskSpaceDrive;

impl VirtualDrive for ErrorDiskSpaceDrive {
    fn open_file(
        &mut self,
        _path: &str,
        _write: bool,
        _create: bool,
        _truncate: bool,
    ) -> Result<Box<dyn VirtualFile>> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn create_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn remove_file(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn remove_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn rename(&mut self, _from: &str, _to: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn get_attributes(&mut self, _path: &str) -> Result<crate::filesystem::FileAttributes> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn set_attributes(
        &mut self,
        _path: &str,
        _attrs: crate::filesystem::FileAttributes,
    ) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn set_file_time(&mut self, _path: &str, _modified: std::time::SystemTime) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn read_dir(
        &mut self,
        _path: &str,
    ) -> Result<Vec<(String, crate::filesystem::FileAttributes)>> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn get_disk_space(&self) -> Result<(u64, u64)> {
        Err(anyhow::anyhow!("disk space unavailable"))
    }
}

impl VirtualDrive for DenyWriteDrive {
    fn open_file(
        &mut self,
        _path: &str,
        _write: bool,
        _create: bool,
        _truncate: bool,
    ) -> Result<Box<dyn VirtualFile>> {
        Err(anyhow::anyhow!("permission denied"))
    }

    fn create_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn remove_file(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn remove_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn rename(&mut self, _from: &str, _to: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn get_attributes(&mut self, _path: &str) -> Result<crate::filesystem::FileAttributes> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn set_attributes(
        &mut self,
        _path: &str,
        _attrs: crate::filesystem::FileAttributes,
    ) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn set_file_time(&mut self, _path: &str, _modified: std::time::SystemTime) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn read_dir(
        &mut self,
        _path: &str,
    ) -> Result<Vec<(String, crate::filesystem::FileAttributes)>> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn get_disk_space(&self) -> Result<(u64, u64)> {
        Ok((0, 0))
    }
}

impl VirtualDrive for ReadOnlyDrive {
    fn open_file(
        &mut self,
        _path: &str,
        _write: bool,
        _create: bool,
        _truncate: bool,
    ) -> Result<Box<dyn VirtualFile>> {
        Err(anyhow::anyhow!("read only"))
    }

    fn create_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn remove_file(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn remove_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn rename(&mut self, _from: &str, _to: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn get_attributes(&mut self, _path: &str) -> Result<crate::filesystem::FileAttributes> {
        Ok(crate::filesystem::FileAttributes {
            read_only: true,
            hidden: false,
            system: false,
            volume_label: false,
            directory: false,
            archive: true,
            size: 1,
            modified: SystemTime::UNIX_EPOCH,
        })
    }

    fn set_attributes(
        &mut self,
        _path: &str,
        _attrs: crate::filesystem::FileAttributes,
    ) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn set_file_time(&mut self, _path: &str, _modified: std::time::SystemTime) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn read_dir(
        &mut self,
        _path: &str,
    ) -> Result<Vec<(String, crate::filesystem::FileAttributes)>> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn get_disk_space(&self) -> Result<(u64, u64)> {
        Ok((0, 0))
    }
}

impl VirtualDrive for NotFoundDrive {
    fn open_file(
        &mut self,
        _path: &str,
        _write: bool,
        _create: bool,
        _truncate: bool,
    ) -> Result<Box<dyn VirtualFile>> {
        Err(anyhow::anyhow!("not found"))
    }

    fn create_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn remove_file(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn remove_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn rename(&mut self, _from: &str, _to: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn get_attributes(&mut self, _path: &str) -> Result<crate::filesystem::FileAttributes> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn set_attributes(
        &mut self,
        _path: &str,
        _attrs: crate::filesystem::FileAttributes,
    ) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn set_file_time(&mut self, _path: &str, _modified: std::time::SystemTime) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn read_dir(
        &mut self,
        _path: &str,
    ) -> Result<Vec<(String, crate::filesystem::FileAttributes)>> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn get_disk_space(&self) -> Result<(u64, u64)> {
        Ok((0, 0))
    }
}

impl VirtualDrive for LabelledDrive {
    fn open_file(
        &mut self,
        _path: &str,
        _write: bool,
        _create: bool,
        _truncate: bool,
    ) -> Result<Box<dyn VirtualFile>> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn create_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn remove_file(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn remove_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn rename(&mut self, _from: &str, _to: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn get_attributes(&mut self, _path: &str) -> Result<crate::filesystem::FileAttributes> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn set_attributes(
        &mut self,
        _path: &str,
        _attrs: crate::filesystem::FileAttributes,
    ) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn set_file_time(&mut self, _path: &str, _modified: std::time::SystemTime) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn read_dir(
        &mut self,
        _path: &str,
    ) -> Result<Vec<(String, crate::filesystem::FileAttributes)>> {
        Ok(vec![(
            "REALDISK".to_string(),
            crate::filesystem::FileAttributes {
                volume_label: true,
                ..Default::default()
            },
        )])
    }

    fn get_disk_space(&self) -> Result<(u64, u64)> {
        Ok((0, 0))
    }
}

impl VirtualDrive for NoSuchFileDrive {
    fn open_file(
        &mut self,
        _path: &str,
        _write: bool,
        _create: bool,
        _truncate: bool,
    ) -> Result<Box<dyn VirtualFile>> {
        Err(anyhow::anyhow!("no such file"))
    }

    fn create_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn remove_file(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn remove_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn rename(&mut self, _from: &str, _to: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn get_attributes(&mut self, _path: &str) -> Result<crate::filesystem::FileAttributes> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn set_attributes(
        &mut self,
        _path: &str,
        _attrs: crate::filesystem::FileAttributes,
    ) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn set_file_time(&mut self, _path: &str, _modified: std::time::SystemTime) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn read_dir(
        &mut self,
        _path: &str,
    ) -> Result<Vec<(String, crate::filesystem::FileAttributes)>> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn get_disk_space(&self) -> Result<(u64, u64)> {
        Ok((0, 0))
    }
}

impl VirtualDrive for AllAttributesDrive {
    fn open_file(
        &mut self,
        _path: &str,
        _write: bool,
        _create: bool,
        _truncate: bool,
    ) -> Result<Box<dyn VirtualFile>> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn create_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn remove_file(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn remove_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn rename(&mut self, _from: &str, _to: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn get_attributes(&mut self, _path: &str) -> Result<crate::filesystem::FileAttributes> {
        Ok(crate::filesystem::FileAttributes {
            read_only: true,
            hidden: true,
            system: true,
            volume_label: true,
            directory: true,
            archive: true,
            size: 0,
            modified: SystemTime::UNIX_EPOCH,
        })
    }

    fn set_attributes(
        &mut self,
        _path: &str,
        _attrs: crate::filesystem::FileAttributes,
    ) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn set_file_time(&mut self, _path: &str, _modified: std::time::SystemTime) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn read_dir(
        &mut self,
        _path: &str,
    ) -> Result<Vec<(String, crate::filesystem::FileAttributes)>> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn get_disk_space(&self) -> Result<(u64, u64)> {
        Ok((0, 0))
    }
}

struct FailingReadFile;

impl Read for FailingReadFile {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("forced read failure"))
    }
}

impl Write for FailingReadFile {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Ok(0)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Seek for FailingReadFile {
    fn seek(&mut self, _pos: std::io::SeekFrom) -> std::io::Result<u64> {
        Ok(0)
    }
}

impl VirtualFile for FailingReadFile {
    fn set_len(&mut self, _size: u64) -> Result<()> {
        Ok(())
    }

    fn sync_all(&mut self) -> Result<()> {
        Ok(())
    }
}

struct FailingSyncFile;

impl Read for FailingSyncFile {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Ok(0)
    }
}

impl Write for FailingSyncFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        // Close drains via `flush` (MDD-BUG-FLUX-00157); this mock's
        // persistence fails on both the drain and the AH=68h barrier.
        Err(std::io::Error::other("forced flush failure"))
    }
}

impl Seek for FailingSyncFile {
    fn seek(&mut self, _pos: std::io::SeekFrom) -> std::io::Result<u64> {
        Ok(0)
    }
}

impl VirtualFile for FailingSyncFile {
    fn set_len(&mut self, _size: u64) -> Result<()> {
        Ok(())
    }

    fn sync_all(&mut self) -> Result<()> {
        Err(anyhow::anyhow!("forced sync failure"))
    }
}

struct CursorFile {
    cursor: Cursor<Vec<u8>>,
}

impl CursorFile {
    fn new(data: Vec<u8>) -> Self {
        Self {
            cursor: Cursor::new(data),
        }
    }
}

impl Read for CursorFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.cursor.read(buf)
    }
}

impl Write for CursorFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.cursor.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.cursor.flush()
    }
}

impl Seek for CursorFile {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.cursor.seek(pos)
    }
}

impl VirtualFile for CursorFile {
    fn set_len(&mut self, size: u64) -> Result<()> {
        let size = size as usize;
        let data = self.cursor.get_mut();
        if data.len() < size {
            data.resize(size, 0);
        } else {
            data.truncate(size);
        }
        Ok(())
    }

    fn sync_all(&mut self) -> Result<()> {
        Ok(())
    }
}

struct ChunkGuardReadFile {
    cursor: Cursor<Vec<u8>>,
    max_chunk: usize,
}

impl ChunkGuardReadFile {
    fn new(data: Vec<u8>, max_chunk: usize) -> Self {
        Self {
            cursor: Cursor::new(data),
            max_chunk,
        }
    }
}

impl Read for ChunkGuardReadFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        assert!(
            buf.len() <= self.max_chunk,
            "read buffer was {} bytes, expected at most {}",
            buf.len(),
            self.max_chunk
        );
        self.cursor.read(buf)
    }
}

impl Write for ChunkGuardReadFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Seek for ChunkGuardReadFile {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.cursor.seek(pos)
    }
}

impl VirtualFile for ChunkGuardReadFile {
    fn set_len(&mut self, _size: u64) -> Result<()> {
        Ok(())
    }

    fn sync_all(&mut self) -> Result<()> {
        Ok(())
    }
}

struct ChunkGuardWriteFile {
    cursor: Cursor<Vec<u8>>,
    max_chunk: usize,
}

impl ChunkGuardWriteFile {
    fn new(max_chunk: usize) -> Self {
        Self {
            cursor: Cursor::new(Vec::new()),
            max_chunk,
        }
    }
}

impl Read for ChunkGuardWriteFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.cursor.read(buf)
    }
}

impl Write for ChunkGuardWriteFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        assert!(
            buf.len() <= self.max_chunk,
            "write buffer was {} bytes, expected at most {}",
            buf.len(),
            self.max_chunk
        );
        self.cursor.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.cursor.flush()
    }
}

impl Seek for ChunkGuardWriteFile {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.cursor.seek(pos)
    }
}

impl VirtualFile for ChunkGuardWriteFile {
    fn set_len(&mut self, size: u64) -> Result<()> {
        let size = size as usize;
        let data = self.cursor.get_mut();
        if data.len() < size {
            data.resize(size, 0);
        } else {
            data.truncate(size);
        }
        Ok(())
    }

    fn sync_all(&mut self) -> Result<()> {
        Ok(())
    }
}

struct SeekErrorFile;

impl Read for SeekErrorFile {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Ok(0)
    }
}

impl Write for SeekErrorFile {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Ok(0)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Seek for SeekErrorFile {
    fn seek(&mut self, _pos: std::io::SeekFrom) -> std::io::Result<u64> {
        Err(std::io::Error::other("seek failure"))
    }
}

impl VirtualFile for SeekErrorFile {
    fn set_len(&mut self, _size: u64) -> Result<()> {
        Ok(())
    }

    fn sync_all(&mut self) -> Result<()> {
        Ok(())
    }
}

struct SetLenErrorFile {
    cursor: Cursor<Vec<u8>>,
}

impl SetLenErrorFile {
    fn new() -> Self {
        Self {
            cursor: Cursor::new(Vec::new()),
        }
    }
}

impl Read for SetLenErrorFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.cursor.read(buf)
    }
}

impl Write for SetLenErrorFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.cursor.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.cursor.flush()
    }
}

impl Seek for SetLenErrorFile {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.cursor.seek(pos)
    }
}

impl VirtualFile for SetLenErrorFile {
    fn set_len(&mut self, _size: u64) -> Result<()> {
        Err(anyhow::anyhow!("set_len failure"))
    }

    fn sync_all(&mut self) -> Result<()> {
        Ok(())
    }
}

struct PartialWriteFile {
    cursor: Cursor<Vec<u8>>,
    max_write: usize,
}

impl PartialWriteFile {
    fn new(max_write: usize) -> Self {
        Self {
            cursor: Cursor::new(Vec::new()),
            max_write,
        }
    }
}

impl Read for PartialWriteFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.cursor.read(buf)
    }
}

impl Write for PartialWriteFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = buf.len().min(self.max_write);
        self.cursor.write_all(&buf[..n])?;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.cursor.flush()
    }
}

impl Seek for PartialWriteFile {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.cursor.seek(pos)
    }
}

impl VirtualFile for PartialWriteFile {
    fn set_len(&mut self, size: u64) -> Result<()> {
        let size = size as usize;
        let data = self.cursor.get_mut();
        if data.len() < size {
            data.resize(size, 0);
        } else {
            data.truncate(size);
        }
        Ok(())
    }

    fn sync_all(&mut self) -> Result<()> {
        Ok(())
    }
}

struct ErrorWriteFile;

impl Read for ErrorWriteFile {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Ok(0)
    }
}

impl Write for ErrorWriteFile {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("write failure"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Seek for ErrorWriteFile {
    fn seek(&mut self, _pos: std::io::SeekFrom) -> std::io::Result<u64> {
        Ok(0)
    }
}

impl VirtualFile for ErrorWriteFile {
    fn set_len(&mut self, _size: u64) -> Result<()> {
        Ok(())
    }

    fn sync_all(&mut self) -> Result<()> {
        Ok(())
    }
}

struct FailingReadDrive;

impl VirtualDrive for FailingReadDrive {
    fn open_file(
        &mut self,
        _path: &str,
        _write: bool,
        _create: bool,
        _truncate: bool,
    ) -> Result<Box<dyn VirtualFile>> {
        Ok(Box::new(FailingReadFile))
    }

    fn create_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn remove_file(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn remove_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn rename(&mut self, _from: &str, _to: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn get_attributes(&mut self, _path: &str) -> Result<crate::filesystem::FileAttributes> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn set_attributes(
        &mut self,
        _path: &str,
        _attrs: crate::filesystem::FileAttributes,
    ) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn set_file_time(&mut self, _path: &str, _modified: std::time::SystemTime) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn read_dir(
        &mut self,
        _path: &str,
    ) -> Result<Vec<(String, crate::filesystem::FileAttributes)>> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn get_disk_space(&self) -> Result<(u64, u64)> {
        Ok((0, 0))
    }
}

struct ListingDrive {
    entries: Vec<(String, crate::filesystem::FileAttributes)>,
    remove_ok: bool,
    rename_ok: bool,
}

impl ListingDrive {
    fn new(entries: Vec<(String, crate::filesystem::FileAttributes)>) -> Self {
        Self {
            entries,
            remove_ok: true,
            rename_ok: true,
        }
    }

    fn with_remove_ok(mut self, remove_ok: bool) -> Self {
        self.remove_ok = remove_ok;
        self
    }

    fn with_rename_ok(mut self, rename_ok: bool) -> Self {
        self.rename_ok = rename_ok;
        self
    }
}

impl VirtualDrive for ListingDrive {
    fn open_file(
        &mut self,
        _path: &str,
        _write: bool,
        _create: bool,
        _truncate: bool,
    ) -> Result<Box<dyn VirtualFile>> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn create_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn remove_file(&mut self, _path: &str) -> Result<()> {
        if self.remove_ok {
            Ok(())
        } else {
            Err(anyhow::anyhow!("remove failed"))
        }
    }

    fn remove_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn rename(&mut self, _from: &str, _to: &str) -> Result<()> {
        if self.rename_ok {
            Ok(())
        } else {
            Err(anyhow::anyhow!("rename failed"))
        }
    }

    fn get_attributes(&mut self, _path: &str) -> Result<crate::filesystem::FileAttributes> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn set_attributes(
        &mut self,
        _path: &str,
        _attrs: crate::filesystem::FileAttributes,
    ) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn set_file_time(&mut self, _path: &str, _modified: std::time::SystemTime) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn read_dir(
        &mut self,
        _path: &str,
    ) -> Result<Vec<(String, crate::filesystem::FileAttributes)>> {
        Ok(self.entries.clone())
    }

    fn get_disk_space(&self) -> Result<(u64, u64)> {
        Ok((0, 0))
    }
}

fn attrs_with_size(size: u64) -> crate::filesystem::FileAttributes {
    crate::filesystem::FileAttributes {
        size,
        modified: std::time::SystemTime::UNIX_EPOCH,
        ..Default::default()
    }
}

struct AttrErrorDrive;

impl VirtualDrive for AttrErrorDrive {
    fn open_file(
        &mut self,
        _path: &str,
        _write: bool,
        _create: bool,
        _truncate: bool,
    ) -> Result<Box<dyn VirtualFile>> {
        Ok(Box::new(CursorFile::new(vec![])))
    }

    fn create_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn remove_file(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn remove_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn rename(&mut self, _from: &str, _to: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn get_attributes(&mut self, _path: &str) -> Result<crate::filesystem::FileAttributes> {
        Err(anyhow::anyhow!("attr failure"))
    }

    fn set_attributes(
        &mut self,
        _path: &str,
        _attrs: crate::filesystem::FileAttributes,
    ) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn set_file_time(&mut self, _path: &str, _modified: std::time::SystemTime) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn read_dir(
        &mut self,
        _path: &str,
    ) -> Result<Vec<(String, crate::filesystem::FileAttributes)>> {
        Err(anyhow::anyhow!("unsupported"))
    }

    fn get_disk_space(&self) -> Result<(u64, u64)> {
        Ok((0, 0))
    }
}

fn exercise_virtual_file(file: &mut dyn VirtualFile) {
    let mut read_buf = [0u8; 8];
    let _ = file.read(&mut read_buf);
    let _ = file.write(&[1, 2, 3, 4]);
    let _ = file.flush();
    let _ = file.seek(SeekFrom::Start(0));
    let _ = file.set_len(4);
    let _ = file.sync_all();
}

fn exercise_virtual_drive(drive: &mut dyn VirtualDrive) {
    let _ = drive.create_dir("C:\\TMP");
    let _ = drive.remove_file("C:\\TMP\\FILE.TXT");
    let _ = drive.remove_dir("C:\\TMP");
    let _ = drive.rename("C:\\OLD.TXT", "C:\\NEW.TXT");
    let _ = drive.get_attributes("C:\\FILE.TXT");
    let _ = drive.set_attributes("C:\\FILE.TXT", crate::filesystem::FileAttributes::default());
    let _ = drive.set_file_time("C:\\FILE.TXT", SystemTime::UNIX_EPOCH);
    let _ = drive.read_dir("C:\\");
    let _ = drive.get_disk_space();

    if let Ok(mut file) = drive.open_file("C:\\FILE.TXT", true, true, false) {
        exercise_virtual_file(file.as_mut());
    }
}

// KILN-00086: test_parse_trace_ah_parses_ranges_and_values removed — it asserted
// the retired AH-range env parsing (parse_trace_ah is now always None).

// KILN-00086: test_parse_trace_ah_decimal_and_invalid_range removed (retired
// AH-range parsing).

// KILN-00086: test_should_trace_ah_respects_ranges and test_parse_trace_ah_ranges
// removed — they asserted the retired AH-range filter
// (should_trace_ah now traces every AH; parse_trace_ah is always None).

// KILN-00086: test_dos_resource_sample_is_bounded_and_checksummed removed — it
// asserted the retired resource-trace sample-bytes override (the sample size is
// now the fixed default 16).

// ============================================================
// INT 21h function tests
// ============================================================

pub(crate) fn create_test_io() -> (Vga, Keyboard) {
    let vga = Vga::new();
    let keyboard = Keyboard::new();
    (vga, keyboard)
}

// ============================================================
// More INT 21h function tests for coverage
// ============================================================

// AH=3F cooked console reads wait for a complete line in every video mode.

// Additional tests for coverage improvement

/// A drive whose directory listing returns an arbitrary number of entries,
/// for exercising FindFirst capacity behaviour without creating real files.
struct ManyEntriesDrive {
    count: usize,
}

impl VirtualDrive for ManyEntriesDrive {
    fn open_file(
        &mut self,
        _path: &str,
        _write: bool,
        _create: bool,
        _truncate: bool,
    ) -> Result<Box<dyn VirtualFile>> {
        Err(anyhow::anyhow!("unsupported"))
    }
    fn create_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }
    fn remove_file(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }
    fn remove_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }
    fn rename(&mut self, _from: &str, _to: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }
    fn get_attributes(&mut self, _path: &str) -> Result<crate::filesystem::FileAttributes> {
        Err(anyhow::anyhow!("unsupported"))
    }
    fn set_attributes(
        &mut self,
        _path: &str,
        _attrs: crate::filesystem::FileAttributes,
    ) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }
    fn set_file_time(&mut self, _path: &str, _modified: std::time::SystemTime) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }
    fn read_dir(
        &mut self,
        _path: &str,
    ) -> Result<Vec<(String, crate::filesystem::FileAttributes)>> {
        Ok((0..self.count)
            .map(|i| {
                (
                    format!("F{i:05}.TXT"),
                    crate::filesystem::FileAttributes::default(),
                )
            })
            .collect())
    }
    fn get_disk_space(&self) -> Result<(u64, u64)> {
        Ok((0, 0))
    }
}

// ============================================================================
// FCB (File Control Block) Tests
// ============================================================================

/// Create a test FCB in memory and return the segment:offset
pub(crate) fn create_test_fcb(
    memory: &mut Memory,
    segment: u16,
    offset: u16,
    drive: u8,
    filename: &str,
    extension: &str,
) {
    create_test_fcb_at(
        memory,
        Memory::seg_offset_to_linear(segment, offset),
        drive,
        filename,
        extension,
    );
}

/// Create a test FCB at an already-resolved linear address.
pub(crate) fn create_test_fcb_at(
    memory: &mut Memory,
    addr: usize,
    drive: u8,
    filename: &str,
    extension: &str,
) {
    // Drive (0 = default, 1 = A, 2 = B, 3 = C)
    memory.write_u8(addr + Dos::FCB_DRIVE, drive);

    // Filename (8 bytes, space-padded)
    let name_bytes = filename.as_bytes();
    for i in 0..8 {
        let ch = if i < name_bytes.len() {
            name_bytes[i]
        } else {
            b' '
        };
        memory.write_u8(addr + Dos::FCB_FILENAME + i, ch);
    }

    // Extension (3 bytes, space-padded)
    let ext_bytes = extension.as_bytes();
    for i in 0..3 {
        let ch = if i < ext_bytes.len() {
            ext_bytes[i]
        } else {
            b' '
        };
        memory.write_u8(addr + Dos::FCB_EXTENSION + i, ch);
    }

    // Initialize other fields
    memory.write_u16(addr + Dos::FCB_CURRENT_BLOCK, 0);
    memory.write_u16(addr + Dos::FCB_RECORD_SIZE, 0); // Will be set to 128 by open
    memory.write_u32(addr + Dos::FCB_FILE_SIZE, 0);
    memory.write_u16(addr + Dos::FCB_FILE_DATE, 0);
    memory.write_u16(addr + Dos::FCB_FILE_TIME, 0);
    memory.write_u32(addr + Dos::FCB_RANDOM_RECORD, 0);
}

#[allow(clippy::too_many_arguments)]
fn create_rename_fcb(
    memory: &mut Memory,
    segment: u16,
    offset: u16,
    drive: u8,
    old_name: &str,
    old_ext: &str,
    new_name: &str,
    new_ext: &str,
) {
    create_test_fcb(memory, segment, offset, drive, old_name, old_ext);

    let addr = Memory::seg_offset_to_linear(segment, offset);
    let new_name_bytes = new_name.as_bytes();
    for i in 0..8 {
        let ch = if i < new_name_bytes.len() {
            new_name_bytes[i]
        } else {
            b' '
        };
        memory.write_u8(addr + 17 + i, ch);
    }

    let new_ext_bytes = new_ext.as_bytes();
    for i in 0..3 {
        let ch = if i < new_ext_bytes.len() {
            new_ext_bytes[i]
        } else {
            b' '
        };
        memory.write_u8(addr + 25 + i, ch);
    }
}

fn fill_file_table(dos: &mut Dos) {
    for i in 5..=0xFF {
        if dos
            .file_table
            .allocate(
                Box::new(CursorFile::new(vec![])),
                format!("C:\\FILL{i:02X}.TXT"),
                dos.current_psp,
            )
            .is_none()
        {
            break;
        }
    }
}

fn write_asciiz(memory: &mut Memory, segment: u16, offset: u16, value: &str) {
    let base = Memory::seg_offset_to_linear(segment, offset);
    for (i, &b) in value.as_bytes().iter().enumerate() {
        memory.write_u8(base + i, b);
    }
    memory.write_u8(base + value.len(), 0);
}

/// A drive whose files exist but are unreadable: open_file surfaces a real
/// io::Error (PermissionDenied), wrapped in anyhow via `.into()` exactly as
/// HostDrive does, so the EXEC search can recover the kind by downcast.
struct AccessDeniedDrive;

impl VirtualDrive for AccessDeniedDrive {
    fn open_file(
        &mut self,
        _path: &str,
        _write: bool,
        _create: bool,
        _truncate: bool,
    ) -> Result<Box<dyn VirtualFile>> {
        Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied").into())
    }
    fn create_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }
    fn remove_file(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }
    fn remove_dir(&mut self, _path: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }
    fn rename(&mut self, _from: &str, _to: &str) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }
    fn get_attributes(&mut self, _path: &str) -> Result<crate::filesystem::FileAttributes> {
        Err(anyhow::anyhow!("unsupported"))
    }
    fn set_attributes(
        &mut self,
        _path: &str,
        _attrs: crate::filesystem::FileAttributes,
    ) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }
    fn set_file_time(&mut self, _path: &str, _modified: std::time::SystemTime) -> Result<()> {
        Err(anyhow::anyhow!("unsupported"))
    }
    fn read_dir(
        &mut self,
        _path: &str,
    ) -> Result<Vec<(String, crate::filesystem::FileAttributes)>> {
        Err(anyhow::anyhow!("unsupported"))
    }
    fn get_disk_space(&self) -> Result<(u64, u64)> {
        Err(anyhow::anyhow!("unsupported"))
    }
}

// =======================================================
// DOS kernel structure tests (copy of the DOS model layout)
// =======================================================

// =========================================
// Phase 1: Overlay and TSR Support Tests
// =========================================

// Phase 2: IOCTL tests

// Phase 4: Miscellaneous DOS functions

// Phase 5: Memory Strategy Enhancement tests

// Additional IOCTL tests for complete coverage

// Create New File (5Bh) additional tests

// Overlay loading success tests

// TSR MCB ownership tests

// ========================================================================
// Phase 1 Enhancement Tests
// ========================================================================

// ==================== INT 2Fh Multiplex Tests (Extended) ====================

// ==================== Stage 5: MSCDEX IOCTL / CdAudioPlayer tests ====================

// ========== Phase 4: INT 21h Completion Tests ==========

// ==================== AH=5Dh Server Function Call Tests ====================

// ==================== AH=53h Translate BPB to DPB Tests ====================

// ==================== AH=64h Get Lookahead Buffer Tests ====================

// ==================== LFN (Long Filename) INT 21h/AH=71h Tests ====================

// ---- Round-5 branch-coverage tests (test_r5_dos_*) ----

// ============================================================
// Coverage batch 1: Simple stubs, flags, and scalar functions
// ============================================================

// ============================================================
// Coverage batch 2: Date/time functions
// ============================================================

// ============================================================
// Coverage batch 3: DPB and BPB translation
// ============================================================

// ============================================================
// Coverage batch 4: Memory strategy
// ============================================================

// ============================================================
// Coverage batch 5: Extended error, handle count, commit
// ============================================================

// ============================================================
// Coverage batch 6: File operations (delete, seek, attributes)
// ============================================================

// ============================================================
// Coverage batch 7: Rename, file datetime, server functions
// ============================================================

// ============================================================
// Coverage batch 8: Truename, country info, DBCS
// ============================================================

// ============================================================
// Coverage batch 9: Write operations
// ============================================================

// ============================================================
// Coverage batch 10: Read operations
// ============================================================

// ============================================================
// Coverage batch 11: Keyboard input functions
// ============================================================

// ============================================================
// Coverage batch 12: Console I/O functions (AH=06, 07, 08)
// ============================================================

// ============================================================
// Coverage batch 13: Disk serial number
// ============================================================

// ============================================================
// Coverage batch 14: Close file, set DTA
// ============================================================

// ============================================================
// Coverage batch 15: bda_ticks_to_time unit tests
// ============================================================

// ============================================================
// Coverage batch 16: format_dos_bytes helper
// ============================================================

// ============================================================
// Coverage batch 5: 32-bit DPMI mode paths in lib.rs
// ============================================================

/// Helper: enter protected mode and set 32-bit DPMI mode.
fn enter_dpmi_32bit(dos: &mut Dos, cpu: &mut gp64_cpu::Cpu, memory: &mut Memory) {
    cpu.regs.set_ax(1); // Request 32-bit mode
    let _ = dos.dpmi.enter_protected_mode(cpu, memory);
    // Ensure 32-bit mode is set (enter_protected_mode reads AX)
    dos.dpmi.set_cpu_mode(1);
    // A 32-bit DPMI client normally runs with a USE32 code segment.
    cpu.regs.cs_cache.big = true;
}

/// Helper: enter protected mode in 16-bit DPMI mode.
fn enter_dpmi_16bit(dos: &mut Dos, cpu: &mut gp64_cpu::Cpu, memory: &mut Memory) {
    cpu.regs.set_ax(0); // Request 16-bit mode
    let _ = dos.dpmi.enter_protected_mode(cpu, memory);
    dos.dpmi.set_cpu_mode(0);
}

fn pm16_int21(dos: &mut Dos, cpu: &mut gp64_cpu::Cpu, memory: &mut Memory, ax: u16) {
    let (mut vga, mut keyboard) = create_test_io();
    cpu.regs.set_ax(ax);
    dos.int21(cpu, memory, &mut vga, &mut keyboard, &mut Pic::new())
        .unwrap();
}

// ============================================================
// Coverage batch 6: INT 21h/3Fh and 40h in 32-bit DPMI
// ============================================================

// ============================================================
// Coverage batch 7: DPMI impl uncovered branches (via int31)
// ============================================================

// ===== Coverage: 16-bit DPMI offset functions (lines 945, 956, 967, 978, 989 false branches) =====

// ===== Coverage: dos_datetime_to_systemtime (line 7712) =====

// ===== Coverage: AH=58h AL=03 UMB link state (lines 4187, 4198) =====

// ===== Coverage: AH=25h set interrupt vector in PM (line 4364) =====

// ===== Coverage: AH=35h get interrupt vector in PM (lines 2894, 2905) =====

// ===== Coverage: dos_datetime with non-zero seconds (line 7712) =====

// ===== Coverage: INT 21h/40h write via 0x00FE alias (line 3381) =====

// ===== Coverage: AH=58h get/set allocation strategy edge cases =====

// (MSCDEX is INT 2Fh, not INT 21h - skip for lib.rs coverage)

// ===== Coverage: DOS read with 32-bit DPMI mode =====
// The 32-bit DPMI read/write paths need actual file handles in PM mode.
// Instead of full INT 21h through PM, test the helper functions directly.

// ===== Coverage: INT 21h/40h write to file handle with CX=0 (truncate, line 3428) =====

// ===== Coverage: LFN get attributes with hidden flag (line 5506) =====

// ===== Coverage: INT 21h/3Fh read from file via file table (lines 3317, 3327) =====

// ===== Coverage: AH=43h set attributes error path (line 3621) =====

// ===== Coverage: AH=43h set attributes access denied (line 3625-3626) =====

// ===== Coverage: AH=43h set attributes anyhow error (line 3628-3629) =====

// ===== Coverage: AH=4Eh find first success (line 4300) =====

// ===== Coverage: AH=43h invalid subfunction (line 3635-3637) =====

// ===== Coverage: DOS datetime conversion round-trip (line 7712) =====

// ===== Coverage batch 20: lib.rs branch coverage push =====

// ===== Coverage batch 21: LFN attribute + trace + MSCDEX + env copy =====

// ------------------------------------------------------------------
// MDD-BUG-CRUCIBLE-00593 — LFN 7143h BL=01h (Set File Attributes).
//
// The old `test_cov_lfn_7143_set_attrs` asserted only "carry is clear",
// which the defect satisfied trivially: the handler discarded CX and
// never called the drive at all. These replacements record what the
// drive actually received.
// ------------------------------------------------------------------

/// What a `RecordingAttrDrive` observed, shared with the test body.
#[derive(Default)]
struct AttrLog {
    set_calls: Vec<(String, crate::filesystem::FileAttributes)>,
    stored: Option<crate::filesystem::FileAttributes>,
}

/// A drive that records every `set_attributes` call and can be told to
/// fail with a specific `io::ErrorKind` (missing target, read-only).
struct RecordingAttrDrive {
    log: std::sync::Arc<std::sync::Mutex<AttrLog>>,
    fail_with: Option<std::io::ErrorKind>,
}

impl VirtualDrive for RecordingAttrDrive {
    fn open_file(&mut self, _: &str, _: bool, _: bool, _: bool) -> Result<Box<dyn VirtualFile>> {
        Err(anyhow::anyhow!("n"))
    }
    fn create_dir(&mut self, _: &str) -> Result<()> {
        Err(anyhow::anyhow!("n"))
    }
    fn remove_file(&mut self, _: &str) -> Result<()> {
        Err(anyhow::anyhow!("n"))
    }
    fn remove_dir(&mut self, _: &str) -> Result<()> {
        Err(anyhow::anyhow!("n"))
    }
    fn rename(&mut self, _: &str, _: &str) -> Result<()> {
        Err(anyhow::anyhow!("n"))
    }
    fn get_attributes(&mut self, _: &str) -> Result<crate::filesystem::FileAttributes> {
        Ok(self.log.lock().unwrap().stored.unwrap_or_default())
    }
    fn set_attributes(
        &mut self,
        path: &str,
        attrs: crate::filesystem::FileAttributes,
    ) -> Result<()> {
        self.log
            .lock()
            .unwrap()
            .set_calls
            .push((path.to_string(), attrs));
        if let Some(kind) = self.fail_with {
            return Err(std::io::Error::new(kind, "injected").into());
        }
        self.log.lock().unwrap().stored = Some(attrs);
        Ok(())
    }
    fn set_file_time(&mut self, _: &str, _: SystemTime) -> Result<()> {
        Ok(())
    }
    fn read_dir(&mut self, _: &str) -> Result<Vec<(String, crate::filesystem::FileAttributes)>> {
        Ok(vec![])
    }
    fn get_disk_space(&self) -> Result<(u64, u64)> {
        Ok((0, 0))
    }
}

// ------------------------------------------------------------------
// MDD-BUG-CRUCIBLE-00592 — LFN CHDIR must validate its target.
// ------------------------------------------------------------------

/// Compare tree entries by name, ignoring case and any leading path
/// separator, so a test's allow-list does not have to guess the exact
/// spelling the resolver produces.
fn normalize_tree_key(path: &str) -> String {
    path.replace('/', "\\")
        .trim_start_matches('\\')
        .to_ascii_uppercase()
}

/// A drive whose directory tree is an explicit allow-list, so a test can
/// state exactly which paths exist and which of those are directories.
struct TreeDrive {
    dirs: Vec<String>,
    files: Vec<String>,
}

impl VirtualDrive for TreeDrive {
    fn open_file(&mut self, _: &str, _: bool, _: bool, _: bool) -> Result<Box<dyn VirtualFile>> {
        Err(anyhow::anyhow!("n"))
    }
    fn create_dir(&mut self, _: &str) -> Result<()> {
        Err(anyhow::anyhow!("n"))
    }
    fn remove_file(&mut self, _: &str) -> Result<()> {
        Err(anyhow::anyhow!("n"))
    }
    fn remove_dir(&mut self, _: &str) -> Result<()> {
        Err(anyhow::anyhow!("n"))
    }
    fn rename(&mut self, _: &str, _: &str) -> Result<()> {
        Err(anyhow::anyhow!("n"))
    }
    fn get_attributes(&mut self, path: &str) -> Result<crate::filesystem::FileAttributes> {
        let key = normalize_tree_key(path);
        if self.dirs.iter().any(|d| normalize_tree_key(d) == key) {
            return Ok(crate::filesystem::FileAttributes {
                directory: true,
                ..Default::default()
            });
        }
        if self.files.iter().any(|f| normalize_tree_key(f) == key) {
            return Ok(crate::filesystem::FileAttributes {
                archive: true,
                ..Default::default()
            });
        }
        Err(std::io::Error::from(std::io::ErrorKind::NotFound).into())
    }
    fn set_attributes(&mut self, _: &str, _: crate::filesystem::FileAttributes) -> Result<()> {
        Ok(())
    }
    fn set_file_time(&mut self, _: &str, _: SystemTime) -> Result<()> {
        Ok(())
    }
    fn read_dir(&mut self, _: &str) -> Result<Vec<(String, crate::filesystem::FileAttributes)>> {
        Ok(vec![])
    }
    fn get_disk_space(&self) -> Result<(u64, u64)> {
        Ok((0, 0))
    }
}

// ========== Batch 23: lib.rs remaining branches ==========

// ========== Batch 25: targeted coverage push ==========

/// Drive that returns file attributes without archive flag set.
struct NoArchiveDrive;
impl VirtualDrive for NoArchiveDrive {
    fn open_file(
        &mut self,
        _p: &str,
        _w: bool,
        _c: bool,
        _t: bool,
    ) -> Result<Box<dyn VirtualFile>> {
        Err(anyhow::anyhow!("no"))
    }
    fn create_dir(&mut self, _p: &str) -> Result<()> {
        Err(anyhow::anyhow!("no"))
    }
    fn remove_file(&mut self, _p: &str) -> Result<()> {
        Err(anyhow::anyhow!("no"))
    }
    fn remove_dir(&mut self, _p: &str) -> Result<()> {
        Err(anyhow::anyhow!("no"))
    }
    fn rename(&mut self, _f: &str, _t: &str) -> Result<()> {
        Err(anyhow::anyhow!("no"))
    }
    fn get_attributes(&mut self, _p: &str) -> Result<crate::filesystem::FileAttributes> {
        Ok(crate::filesystem::FileAttributes {
            read_only: true,
            hidden: false,
            system: false,
            volume_label: false,
            directory: false,
            archive: false,
            size: 100,
            modified: SystemTime::UNIX_EPOCH,
        })
    }
    fn set_attributes(&mut self, _p: &str, _a: crate::filesystem::FileAttributes) -> Result<()> {
        Err(anyhow::anyhow!("no"))
    }
    fn set_file_time(&mut self, _p: &str, _m: std::time::SystemTime) -> Result<()> {
        Err(anyhow::anyhow!("no"))
    }
    fn read_dir(&mut self, _p: &str) -> Result<Vec<(String, crate::filesystem::FileAttributes)>> {
        Err(anyhow::anyhow!("no"))
    }
    fn get_disk_space(&self) -> Result<(u64, u64)> {
        Ok((1000, 500))
    }
}

// ========== Coverage Batch 27 ==========

// ---- init_environment integration tests ----

// ---- build_glide_env_vars tests ----

// ---- OVL filename matching tests ----

// ---- Virtual file table (HLD 2026.05.24 §5.2 Option C) ----

// ==================== INT 24h Critical Error Tests ====================

// ========== format_dos_bytes coverage ==========

// ========== bda_ticks_to_time coverage ==========

// ========== boot_time_bda_ticks coverage ==========

// ========== build_sound_env_vars coverage ==========

// ========== build_glide_env_vars additional coverage ==========

// ========== Dos struct accessor coverage ==========

// ============================================================
// INT 21h AH=37h - Get/Set switch character
// ============================================================

// ============================================================
// INT 21h AH=26h - Create PSP
// ============================================================

// ============================================================
// INT 21h AH=31h - Terminate and Stay Resident
// ============================================================

// ============================================================
// INT 21h AH=03/04/05 - STDAUX/STDPRN stubs
// ============================================================

// ============================================================
// INT 21h AH=0D - Reset disk
// ============================================================

// ============================================================
// INT 21h AH=0C - Clear keyboard and invoke function
// ============================================================

// ============================================================
// INT 2Fh - Multiplex interrupt tests
// ============================================================

// ============================================================
// INT 21h AH=5Eh/5Fh - Network stubs
// ============================================================

// ============================================================
// INT 21h AH=0Bh - Check input status
// ============================================================

// ============================================================
// INT 21h AH=2E - Set verify flag
// ============================================================

// ============================================================
// INT 21h AH=DCh - AZUSA_MEM stub
// ============================================================

// ============================================================
// INT 21h AH=06h - Direct console I/O (input/output modes)
// ============================================================

// ============================================================
// INT 21h AH=02 - Character output
// ============================================================

// ============================================================
// INT 21h AH=69h - Get/Set disk serial number
// ============================================================

// ============================================================
// Stdout capture tests
// ============================================================

// ============================================================
// Country info tests
// ============================================================

// ============================================================
// DTA tests
// ============================================================

// ============================================================
// INT 21h AH=3Dh - Open file device paths
// ============================================================

// ============================================================
// INT 21h AH=71h - LFN functions
// ============================================================

// ============================================================
// INT 21h - Trace-enabled paths
// ============================================================

// ============================================================
// INT 21h AH=09h - Print string with trace
// ============================================================

// ============================================================
// INT 21h AH=3Dh - Open with Glide interception
// ============================================================

// ============================================================
// INT 21h AH=54h - Get verify flag
// ============================================================

// ============================================================
// INT 21h AH=59h - Get extended error info
// ============================================================

// ============================================================
// INT 21h AH=50h/51h - Get/Set PSP
// ============================================================

// ============================================================
// INT 21h AH=62h - Get PSP (duplicate of 51h)
// ============================================================

// ============================================================
// INT 21h AH=4Dh - Get return code
// ============================================================

// ============================================================
// INT 21h AH=4Bh AL=01 - Load overlay stubs
// ============================================================

// ============================================================
// INT 21h AH=33h - Get/Set Ctrl-Break check
// ============================================================

// ============================================================
// INT 21h AH=38h - Get/Set country info
// ============================================================

// ============================================================
// INT 21h AH=25h/35h - Set/Get interrupt vector
// ============================================================

// ============================================================
// INT 21h AH=44h - IOCTL subfunctions
// ============================================================

// ============================================================
// INT 21h AH=32h - Get Drive Parameter Block
// ============================================================

// ============================================================
// INT 21h AH=34h - Get InDOS flag pointer
// ============================================================

// ============================================================
// INT 21h AH=52h - Get SysVars (List of Lists)
// ============================================================

// ============================================================
// INT 21h AH=58h - Get/Set memory allocation strategy
// ============================================================

// ============================================================
// INT 21h AH=2Ah/2Ch - Get date/time
// ============================================================

// ============================================================
// INT 21h AH=2Bh/2Dh - Set date/time (no-op)
// ============================================================

// ============================================================
// INT 21h AH=47h - Get current directory
// ============================================================

// ============================================================
// INT 21h AH=57h - Get/Set file date/time
// ============================================================

// ============================================================
// Accessor method tests
// ============================================================

// ============================================================
// INT 21h AH=2Bh/2Dh - Set date/time round-trip tests
// ============================================================

// ============================================================
// AUX / PRN character devices
// (HLD 2026.08.03 "DOS AUX and PRN character devices with an
//  LPT printer sink", acceptance criteria 1-3, 5-7, 9, 13)
// ============================================================

/// Run one INT 21h call against `dos`, returning `(AX, CF)`.
fn dev_int21(dos: &mut Dos, cpu: &mut gp64_cpu::Cpu, memory: &mut Memory) -> (u16, bool) {
    let (mut vga, mut keyboard) = create_test_io();
    dos.int21(cpu, memory, &mut vga, &mut keyboard, &mut Pic::new())
        .expect("int21 dispatch");
    (
        cpu.regs.ax(),
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
    )
}

/// Open `name` through the given open/create function (AH=3Dh or AH=3Ch),
/// with DS:DX pointing at the name. Returns `(AX, CF)`.
fn dev_open(dos: &mut Dos, memory: &mut Memory, ah: u8, name: &str, access: u8) -> (u16, bool) {
    let mut cpu = gp64_cpu::Cpu::new();
    write_asciiz(memory, 0x2000, 0x0100, name);
    cpu.regs.set_ah(ah);
    cpu.regs.set_al(access);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    dev_int21(dos, &mut cpu, memory)
}

/// Mount an empty temporary directory as drive C and return it, so a test
/// that must prove "no host file was created" is looking at a clean tree
/// rather than the crate's working directory.
fn dev_mount_empty_c(dos: &mut Dos) -> tempfile::TempDir {
    let temp_dir = tempfile::TempDir::new().unwrap();
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );
    temp_dir
}

// ------------------------------------------------------------
// Cross-family review defects (2026.08.03)
// ------------------------------------------------------------
