use std::any::Any;
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
pub mod ipc;
use std::sync::OnceLock;
use std::sync::Mutex;

use sysinfo::Disks;
use widestring::U16CStr;
use windows_sys::Win32::Storage::FileSystem::FILE_ACCESS_RIGHTS;
use winfsp::filesystem::{
    DirBuffer, DirInfo, DirMarker, FileInfo, FileSecurity, FileSystemContext, OpenFileInfo,
    VolumeInfo, WideNameInfo,
};
use winfsp::host::{FileSystemHost, VolumeParams};
use winfsp::{winfsp_init, FspError};

const DIR_ATTR: u32 = 0x10; // FILE_ATTRIBUTE_DIRECTORY
const NORMAL_ATTR: u32 = 0x80; // FILE_ATTRIBUTE_NORMAL
const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;
const FSP_CLEANUP_DELETE: u32 = 0x01;
const ALLOC_UNIT: u64 = 4096;

const NOT_FOUND: i32 = 0xC000_0034u32 as i32; // STATUS_OBJECT_NAME_NOT_FOUND
const NAME_COLLISION: i32 = 0xC000_0035u32 as i32; // STATUS_OBJECT_NAME_COLLISION
const ACCESS_DENIED: i32 = 0xC000_0022u32 as i32; // STATUS_ACCESS_DENIED
const DIR_NOT_EMPTY: i32 = 0xC000_0101u32 as i32; // STATUS_DIRECTORY_NOT_EMPTY
const NOT_A_DIRECTORY: i32 = 0xC000_0103u32 as i32; // STATUS_NOT_A_DIRECTORY
const UNSUCCESSFUL: i32 = 0xC000_0001u32 as i32; // STATUS_UNSUCCESSFUL
const DISK_FULL: i32 = 0xC000_007Fu32 as i32; // STATUS_DISK_FULL

const VIRTUAL_DRIVE_FILE_SYSTEM_NAME: &str = "NTFS";
const PHYSICAL_DRIVE_FOLDER_NAME: &str = "Virtual_Drive_Pool";

pub static CREATE_PREVILAGE_VIRTUAL_DRIVE: AtomicBool = AtomicBool::new(false);

pub static SELECTED_ROOTS: Mutex<Vec<(PathBuf, u64)>> = Mutex::new(Vec::new());

pub fn set_selected_roots(roots: Vec<(PathBuf, u64)>) {
    *SELECTED_ROOTS.lock().unwrap() = roots;
}

pub fn selected_roots() -> Vec<(PathBuf, u64)> {
    SELECTED_ROOTS.lock().unwrap().clone()
}


#[derive(Clone, Debug)]
pub struct DriveInfo {
    pub mount: PathBuf,
    pub name: String,
    pub total: u64,
    pub free: u64,
}

// ---------------------------------------------------------------- helpers

fn to_fsp(e: io::Error) -> FspError {
    let status = match e.kind() {
        io::ErrorKind::NotFound => NOT_FOUND,
        io::ErrorKind::AlreadyExists => NAME_COLLISION,
        io::ErrorKind::PermissionDenied => ACCESS_DENIED,
        _ => match e.raw_os_error() {
            Some(145) => DIR_NOT_EMPTY, // ERROR_DIR_NOT_EMPTY
            _ => UNSUCCESSFUL,
        },
    };
    FspError::NTSTATUS(status)
}

fn now_filetime() -> u64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    (secs + 11_644_473_600) * 10_000_000
}

fn round_up(n: u64) -> u64 {
    n.div_ceil(ALLOC_UNIT) * ALLOC_UNIT
}

fn fill_root(fi: &mut FileInfo) {
    let t = now_filetime();
    fi.file_attributes = DIR_ATTR;
    fi.file_size = 0;
    fi.allocation_size = 0;
    fi.creation_time = t;
    fi.last_access_time = t;
    fi.last_write_time = t;
    fi.change_time = t;
}

/// Fill a FileInfo from real physical metadata.
fn fill_from_meta(meta: &std::fs::Metadata, fi: &mut FileInfo) {
    let is_dir = meta.is_dir();
    let attrs = meta.file_attributes();
    fi.file_attributes = if is_dir {
        attrs | DIR_ATTR
    } else if attrs == 0 {
        NORMAL_ATTR
    } else {
        attrs
    };
    let size = if is_dir { 0 } else { meta.len() };
    fi.file_size = size;
    fi.allocation_size = round_up(size);
    // MetadataExt already returns Windows FILETIME values.
    fi.creation_time = meta.creation_time();
    fi.last_access_time = meta.last_access_time();
    fi.last_write_time = meta.last_write_time();
    fi.change_time = meta.last_write_time();
    fi.index_number = 0;
    fi.hard_links = 0;
    fi.reparse_tag = 0;
}

fn fill_from_file(f: &std::fs::File, fi: &mut FileInfo) -> Result<(), FspError> {
    let meta = f.metadata().map_err(to_fsp)?;
    fill_from_meta(&meta, fi);
    Ok(())
}

/// Total bytes of all files under `p` (used to seed a root's usage at mount).
fn dir_size(p: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else { return 0 };
    rd.flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

// ---------------------------------------------------------------- FS state

/// One physical directory that contributes `quota` bytes to the virtual drive.
pub struct PoolRoot {
    pub path: PathBuf,
    pub quota: u64,
    used: AtomicU64,
}

impl PoolRoot {
    pub fn new(path: PathBuf, quota: u64) -> io::Result<Self> {
        std::fs::create_dir_all(&path)?;
        let used = dir_size(&path);
        Ok(Self {
            path,
            quota,
            used: AtomicU64::new(used),
        })
    }

    fn remaining(&self) -> u64 {
        self.quota.saturating_sub(self.used.load(Ordering::Relaxed))
    }

    /// Reserve `n` bytes of quota or fail with DISK_FULL.
    fn reserve(&self, n: u64) -> Result<(), FspError> {
        if n == 0 {
            return Ok(());
        }
        let ok = self
            .used
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |u| {
                if u.saturating_add(n) <= self.quota {
                    Some(u + n)
                } else {
                    None
                }
            })
            .is_ok();
        if ok {
            Ok(())
        } else {
            Err(FspError::NTSTATUS(DISK_FULL))
        }
    }

    fn release(&self, n: u64) {
        let _ = self
            .used
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |u| {
                Some(u.saturating_sub(n))
            });
    }
}

pub struct PoolFs {
    total_size: Arc<AtomicU64>,
    roots: Vec<PoolRoot>,
    rr: AtomicUsize, // round-robin cursor
}

impl PoolFs {
    /// roots: (physical directory, quota in bytes). The drive's total size is
    /// the sum of the quotas.
    pub fn new(roots: Vec<(PathBuf, u64)>) -> io::Result<Self> {
        if roots.is_empty() {
            return Err(io::Error::other("at least one root is required"));
        }
        let roots = roots
            .into_iter()
            .map(|(p, q)| PoolRoot::new(p, q))
            .collect::<io::Result<Vec<_>>>()?;
        let total = roots.iter().map(|r| r.quota).sum();
        Ok(Self {
            total_size: Arc::new(AtomicU64::new(total)),
            roots,
            rr: AtomicUsize::new(0),
        })
    }

    pub fn total_size(&self) -> u64 {
        self.total_size.load(Ordering::Relaxed)
    }

    pub fn set_total_size(&self, v: u64) {
        self.total_size.store(v, Ordering::Relaxed);
    }

    /// Virtual name -> validated relative path ("" = drive root).
    fn rel(&self, file_name: &U16CStr) -> Option<String> {
        let s = file_name.to_string_lossy();
        let rel = s.trim_start_matches(['\\', '/']).replace('\\', "/");
        if rel.is_empty() {
            return Some(rel);
        }
        if rel.split('/').any(|c| c == ".." || c == "." || c.is_empty()) {
            return None; // path traversal / malformed: reject
        }
        Some(rel)
    }

    fn phys(&self, idx: usize, rel: &str) -> PathBuf {
        if rel.is_empty() {
            self.roots[idx].path.clone()
        } else {
            self.roots[idx].path.join(rel)
        }
    }

    /// First root that already has this path.
    fn find(&self, rel: &str) -> Option<(usize, PathBuf)> {
        (0..self.roots.len())
            .map(|i| (i, self.phys(i, rel)))
            .find(|(_, p)| p.exists())
    }

    /// Free space on the physical disk that holds root `idx`.
    fn phys_free(&self, idx: usize, disks: &Disks) -> u64 {
        disks
            .iter()
            .filter(|d| self.roots[idx].path.starts_with(d.mount_point()))
            .max_by_key(|d| d.mount_point().as_os_str().len())
            .map(|d| d.available_space())
            .unwrap_or(0)
    }

    /// Round-robin over the roots that can actually hold `size_hint` bytes.
    /// Full roots are skipped automatically.
    fn pick_root(&self, size_hint: u64) -> Option<usize> {
        let disks = Disks::new_with_refreshed_list();
        let need = size_hint.max(1);

        let usable: Vec<usize> = (0..self.roots.len())
            .filter(|&i| self.roots[i].remaining() >= need && self.phys_free(i, &disks) >= need)
            .collect();
        if usable.is_empty() {
            return None;
        }

        let k = self.rr.fetch_add(1, Ordering::Relaxed);
        Some(usable[k % usable.len()])
    }
}

pub struct PoolFile {
    dir_buffer: Option<DirBuffer>, // Some for directories
    file: Option<std::fs::File>,   // Some for files
    path: PathBuf,                 // physical path (first match for directories)
    rel: String,                   // virtual relative path ("" = root)
    root_idx: usize,               // which root holds this file
    is_dir: bool,
}

// ---------------------------------------------------------------- WinFsp callbacks

impl FileSystemContext for PoolFs {
    type FileContext = PoolFile;

    fn get_security_by_name(
        &self,
        file_name: &U16CStr,
        _security_descriptor: Option<&mut [std::ffi::c_void]>,
        _reparse_point_resolver: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
    ) -> Result<FileSecurity, FspError> {
        let rel = self.rel(file_name).ok_or(FspError::NTSTATUS(NOT_FOUND))?;
        let attributes = if rel.is_empty() {
            DIR_ATTR
        } else {
            let (_, path) = self.find(&rel).ok_or(FspError::NTSTATUS(NOT_FOUND))?;
            let meta = std::fs::metadata(&path).map_err(to_fsp)?;
            if meta.is_dir() {
                DIR_ATTR
            } else {
                let a = meta.file_attributes();
                if a == 0 { NORMAL_ATTR } else { a }
            }
        };
        Ok(FileSecurity {
            reparse: false,
            sz_security_descriptor: 0,
            attributes,
        })
    }

    fn create(
        &self,
        file_name: &U16CStr,
        create_options: u32,
        _granted_access: FILE_ACCESS_RIGHTS,
        _file_attributes: u32,
        _security_descriptor: Option<&[std::ffi::c_void]>,
        allocation_size: u64,
        _extra_buffer: Option<&[u8]>,
        _extra_buffer_is_reparse_point: bool,
        file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<Self::FileContext> {
        let rel = self.rel(file_name).ok_or(FspError::NTSTATUS(NOT_FOUND))?;
        if self.find(&rel).is_some() {
            return Err(FspError::NTSTATUS(NAME_COLLISION));
        }

        if create_options & FILE_DIRECTORY_FILE != 0 {
            // Directories exist in every root so files can land anywhere under them.
            for i in 0..self.roots.len() {
                std::fs::create_dir_all(self.phys(i, &rel)).map_err(to_fsp)?;
            }
            let path = self.phys(0, &rel);
            let meta = std::fs::metadata(&path).map_err(to_fsp)?;
            fill_from_meta(&meta, file_info.as_mut());
            return Ok(PoolFile {
                dir_buffer: Some(DirBuffer::default()),
                file: None,
                path,
                rel,
                root_idx: 0,
                is_dir: true,
            });
        }

        let idx = self
            .pick_root(allocation_size)
            .ok_or(FspError::NTSTATUS(DISK_FULL))?;
        let path = self.phys(idx, &rel);
        if let Some(parent) = path.parent() {
            // The parent may not exist in this particular root yet.
            std::fs::create_dir_all(parent).map_err(to_fsp)?;
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true) // never silently reuse an existing file
            .open(&path)
            .map_err(to_fsp)?;

        fill_from_file(&file, file_info.as_mut())?;

        Ok(PoolFile {
            dir_buffer: None,
            file: Some(file),
            path,
            rel,
            root_idx: idx,
            is_dir: false,
        })
    }

    fn open(
        &self,
        file_name: &U16CStr,
        _create_options: u32,
        _granted_access: FILE_ACCESS_RIGHTS,
        file_info: &mut OpenFileInfo,
    ) -> Result<Self::FileContext, FspError> {
        let rel = self.rel(file_name).ok_or(FspError::NTSTATUS(NOT_FOUND))?;

        if rel.is_empty() {
            fill_root(file_info.as_mut());
            return Ok(PoolFile {
                dir_buffer: Some(DirBuffer::default()),
                file: None,
                path: self.phys(0, ""),
                rel,
                root_idx: 0,
                is_dir: true,
            });
        }

        let (idx, path) = self.find(&rel).ok_or(FspError::NTSTATUS(NOT_FOUND))?;
        let meta = std::fs::metadata(&path).map_err(to_fsp)?;

        if meta.is_dir() {
            fill_from_meta(&meta, file_info.as_mut());
            return Ok(PoolFile {
                dir_buffer: Some(DirBuffer::default()),
                file: None,
                path,
                rel,
                root_idx: idx,
                is_dir: true,
            });
        }

        // Try read/write, fall back to read-only (e.g. read-only attribute).
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .or_else(|_| OpenOptions::new().read(true).open(&path))
            .map_err(to_fsp)?;

        fill_from_file(&file, file_info.as_mut())?;

        Ok(PoolFile {
            dir_buffer: None,
            file: Some(file),
            path,
            rel,
            root_idx: idx,
            is_dir: false,
        })
    }

    fn close(&self, _context: Self::FileContext) {
        // File handle is dropped (closed) here.
    }

    fn cleanup(&self, context: &Self::FileContext, _file_name: Option<&U16CStr>, flags: u32) {
        // Delete-on-close is performed in cleanup, not close.
        if flags & FSP_CLEANUP_DELETE == 0 {
            return;
        }
        if context.is_dir {
            for i in 0..self.roots.len() {
                let _ = std::fs::remove_dir(self.phys(i, &context.rel));
            }
        } else {
            let len = std::fs::metadata(&context.path).map(|m| m.len()).unwrap_or(0);
            if std::fs::remove_file(&context.path).is_ok() {
                self.roots[context.root_idx].release(len);
            }
        }
    }

    fn set_delete(
        &self,
        context: &Self::FileContext,
        _file_name: &U16CStr,
        delete_file: bool,
    ) -> Result<(), FspError> {
        if delete_file && context.is_dir {
            for i in 0..self.roots.len() {
                if let Ok(mut rd) = std::fs::read_dir(self.phys(i, &context.rel)) {
                    if rd.next().is_some() {
                        return Err(FspError::NTSTATUS(DIR_NOT_EMPTY));
                    }
                }
            }
        }
        Ok(())
    }

    fn get_file_info(
        &self,
        context: &Self::FileContext,
        file_info: &mut FileInfo,
    ) -> Result<(), FspError> {
        if context.rel.is_empty() {
            fill_root(file_info);
            return Ok(());
        }
        let meta = std::fs::metadata(&context.path).map_err(to_fsp)?;
        fill_from_meta(&meta, file_info);
        Ok(())
    }

    fn read(
        &self,
        context: &Self::FileContext,
        buffer: &mut [u8],
        offset: u64,
    ) -> Result<u32, FspError> {
        let f = context.file.as_ref().ok_or(FspError::NTSTATUS(NOT_FOUND))?;
        let n = f.seek_read(buffer, offset).map_err(to_fsp)?;
        Ok(n as u32)
    }

    fn write(
        &self,
        context: &Self::FileContext,
        buffer: &[u8],
        offset: u64,
        write_to_eof: bool,
        constrained_io: bool,
        file_info: &mut FileInfo,
    ) -> Result<u32, FspError> {
        let f = context.file.as_ref().ok_or(FspError::NTSTATUS(NOT_FOUND))?;
        let len = f.metadata().map_err(to_fsp)?.len();

        let offset = if write_to_eof { len } else { offset };

        // Constrained IO: never extend the file.
        let buf: &[u8] = if constrained_io {
            if offset >= len {
                fill_from_file(f, file_info)?;
                return Ok(0);
            }
            let max = (len - offset) as usize;
            &buffer[..buffer.len().min(max)]
        } else {
            buffer
        };

        // Charge the growth to this file's root; DISK_FULL if its quota is gone.
        let growth = (offset + buf.len() as u64).saturating_sub(len);
        let root = &self.roots[context.root_idx];
        root.reserve(growth)?;

        match f.seek_write(buf, offset) {
            Ok(n) => {
                fill_from_file(f, file_info)?;
                Ok(n as u32)
            }
            Err(e) => {
                root.release(growth);
                Err(to_fsp(e))
            }
        }
    }

    fn flush(
        &self,
        context: Option<&Self::FileContext>,
        file_info: &mut FileInfo,
    ) -> Result<(), FspError> {
        if let Some(ctx) = context {
            if let Some(f) = ctx.file.as_ref() {
                f.sync_all().map_err(to_fsp)?;
                fill_from_file(f, file_info)?;
            }
        }
        Ok(())
    }

    fn set_file_size(
        &self,
        context: &Self::FileContext,
        new_size: u64,
        set_allocation_size: bool,
        file_info: &mut FileInfo,
    ) -> Result<(), FspError> {
        let f = context.file.as_ref().ok_or(FspError::NTSTATUS(NOT_FOUND))?;
        let len = f.metadata().map_err(to_fsp)?.len();
        let root = &self.roots[context.root_idx];

        // Allocation-size changes only shrink; end-of-file changes grow or shrink.
        if !set_allocation_size || new_size < len {
            if new_size > len {
                root.reserve(new_size - len)?;
            }
            if let Err(e) = f.set_len(new_size) {
                if new_size > len {
                    root.release(new_size - len);
                }
                return Err(to_fsp(e));
            }
            if new_size < len {
                root.release(len - new_size);
            }
        }
        fill_from_file(f, file_info)
    }

    fn overwrite(
        &self,
        context: &Self::FileContext,
        _file_attributes: windows_sys::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES,
        _replace_file_attributes: bool,
        _allocation_size: u64,
        _extra_buffer: Option<&[u8]>,
        file_info: &mut FileInfo,
    ) -> Result<(), FspError> {
        let f = context.file.as_ref().ok_or(FspError::NTSTATUS(NOT_FOUND))?;
        let len = f.metadata().map_err(to_fsp)?.len();
        f.set_len(0).map_err(to_fsp)?;
        self.roots[context.root_idx].release(len);
        fill_from_file(f, file_info)
    }

    fn set_basic_info(
        &self,
        context: &Self::FileContext,
        _file_attributes: u32,
        _creation_time: u64,
        _last_access_time: u64,
        _last_write_time: u64,
        _last_change_time: u64,
        file_info: &mut FileInfo,
    ) -> Result<(), FspError> {
        // Accept and ignore (Explorer calls this after copies). Timestamps and
        // attributes are simply re-read from the physical file.
        if context.rel.is_empty() {
            fill_root(file_info);
            return Ok(());
        }
        let meta = std::fs::metadata(&context.path).map_err(to_fsp)?;
        fill_from_meta(&meta, file_info);
        Ok(())
    }

    fn rename(
        &self,
        context: &Self::FileContext,
        _file_name: &U16CStr,
        new_file_name: &U16CStr,
        replace_if_exists: bool,
    ) -> Result<(), FspError> {
        let new_rel = self
            .rel(new_file_name)
            .ok_or(FspError::NTSTATUS(NOT_FOUND))?;

        if let Some((ti, tp)) = self.find(&new_rel) {
            if !replace_if_exists {
                return Err(FspError::NTSTATUS(NAME_COLLISION));
            }
            // Replacing a file that may live in a different root: remove it first.
            if !context.is_dir && tp.is_file() && tp != self.phys(context.root_idx, &new_rel) {
                let len = std::fs::metadata(&tp).map(|m| m.len()).unwrap_or(0);
                std::fs::remove_file(&tp).map_err(to_fsp)?;
                self.roots[ti].release(len);
            }
        }

        // A file stays in its root; a directory is renamed in every root that has it.
        let targets: Vec<usize> = if context.is_dir {
            (0..self.roots.len())
                .filter(|&i| self.phys(i, &context.rel).exists())
                .collect()
        } else {
            vec![context.root_idx]
        };

        for i in targets {
            let new_path = self.phys(i, &new_rel);
            if let Some(parent) = new_path.parent() {
                std::fs::create_dir_all(parent).map_err(to_fsp)?;
            }
            std::fs::rename(self.phys(i, &context.rel), new_path).map_err(to_fsp)?;
        }
        Ok(())
    }

    fn read_directory(
        &self,
        context: &Self::FileContext,
        _pattern: Option<&U16CStr>,
        marker: DirMarker,
        buffer: &mut [u8],
    ) -> Result<u32, FspError> {
        if !context.is_dir {
            return Err(FspError::NTSTATUS(NOT_A_DIRECTORY));
        }
        let dir_buffer = context
            .dir_buffer
            .as_ref()
            .ok_or(FspError::NTSTATUS(NOT_FOUND))?;

        // Fill the buffer only when (re)starting enumeration.
        if let Ok(lock) = dir_buffer.acquire(marker.is_none(), None) {
            let mut seen: HashSet<String> = HashSet::new();
            'roots: for i in 0..self.roots.len() {
                let Ok(rd) = std::fs::read_dir(self.phys(i, &context.rel)) else {
                    continue;
                };
                for entry in rd.flatten() {
                    let name = entry.file_name();
                    // Directories exist in every root: list each name once.
                    if !seen.insert(name.to_string_lossy().to_lowercase()) {
                        continue;
                    }
                    let Ok(meta) = entry.metadata() else { continue };
                    let wide: Vec<u16> = name.encode_wide().collect();

                    let mut info: DirInfo<255> = DirInfo::new();
                    if info.set_name_raw(wide.as_slice()).is_err() {
                        continue;
                    }
                    fill_from_meta(&meta, info.file_info_mut());
                    if lock.write(&mut info).is_err() {
                        break 'roots;
                    }
                }
            }
            // `lock` is dropped here, which commits the entries.
        }

        Ok(dir_buffer.read(marker, buffer))
    }

    fn get_volume_info(&self, out: &mut VolumeInfo) -> Result<(), FspError> {
        let disks = Disks::new_with_refreshed_list();
        let mut free = 0u64;
        for i in 0..self.roots.len() {
            free += self.roots[i].remaining().min(self.phys_free(i, &disks));
        }
        out.total_size = self.total_size();
        out.free_size = free.min(self.total_size());
        Ok(())
    }
}

// ---------------------------------------------------------------- mounting

pub struct VirtualDrive {
    _host: Box<dyn Any>,
    _init: Box<dyn Any>,
    pub total_size: Arc<AtomicU64>,
}

/// `roots`: (physical directory, quota in bytes), e.g.
/// `vec![(r"E:\Virtual_Drive_Pool".into(), 20 * GB), (r"D:\Virtual_Drive_Pool".into(), 5 * GB)]`
pub fn create_virtual_drive(roots: Vec<(PathBuf, u64)>) -> io::Result<VirtualDrive> {
    let init = winfsp_init().map_err(|e| {
        io::Error::other(format!("WinFsp init failed (is WinFsp installed?): {e:?}"))
    })?;

    let mut volume_params = VolumeParams::new();
    volume_params
        .sector_size(4096)
        .sectors_per_allocation_unit(1)
        .case_preserved_names(true)
        .unicode_on_disk(true)
        .persistent_acls(false)
        .filesystem_name(VIRTUAL_DRIVE_FILE_SYSTEM_NAME);

    let fs = PoolFs::new(roots)?;
    let size = fs.total_size.clone();

    let mut host = FileSystemHost::new(volume_params, fs)
        .map_err(|e| io::Error::other(format!("host: {e:?}")))?;
    host.mount("V:")
        .map_err(|e| io::Error::other(format!("mount: {e:?}")))?;
    host.start()
        .map_err(|e| io::Error::other(format!("start: {e:?}")))?;

    Ok(VirtualDrive {
        _host: Box::new(host),
        _init: Box::new(init),
        total_size: size,
    })
}

pub fn format_bytes(bytes: u64) -> String {
    let suffixes = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes == 0 {
        return "0 B".to_string();
    }
    let i = (bytes as f64).log(1024.0).floor() as usize;
    let i = i.min(suffixes.len() - 1);
    let value = bytes as f64 / 1024.0_f64.powi(i as i32);
    format!("{:.2} {}", value, suffixes[i])
}

pub fn calculate_total_free_diskspaces() -> u64 {
    let disks = Disks::new_with_refreshed_list();
    disks.iter().map(|d| d.available_space()).sum()
}

/// Build the pool folder path for a drive mount point, e.g. `E:\` -> `E:\Virtual_Drive_Pool`.
pub fn pool_folder_for(mount_point: &Path) -> PathBuf {
    mount_point.join(PHYSICAL_DRIVE_FOLDER_NAME)
}


/// Physical drives the user can pool (the virtual V: drive itself is excluded).
pub fn list_drives() -> Vec<DriveInfo> {
    let disks = Disks::new_with_refreshed_list();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for d in disks.iter() {
        let mount = d.mount_point().to_path_buf();
        let key = mount.to_string_lossy().to_uppercase();
        if key.starts_with("V:") || d.total_space() == 0 || !seen.insert(key) {
            continue;
        }
        out.push(DriveInfo {
            mount,
            name: d.name().to_string_lossy().into_owned(),
            total: d.total_space(),
            free: d.available_space(),
        });
    }
    out.sort_by(|a, b| a.mount.cmp(&b.mount));
    out
}