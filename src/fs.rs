//! The WinFsp `FileSystemContext` that presents a GroveFS *view* as a native
//! Windows volume. This is the Windows mirror of the Linux `fuser` bridge in
//! `grovefs/src/fs_impl.rs`: instead of FUSE callbacks it implements the WinFsp
//! callbacks, and instead of a local backend it drives the SAME two traits over
//! the LAN — [`MetaProvider`] (the authoritative view tree, via `RemoteMeta`)
//! and [`StorageBackend`] (object bytes, via `RemoteBackend`). No NFS, no kernel
//! FUSE: WinFsp's signed driver forwards IRPs to this user-mode process.
//!
//! This file is intentionally thin and `#[cfg(windows)]`-only. Every decision
//! that does not require a WinFsp type — path parsing, time math, attribute and
//! NTSTATUS computation — lives in the host-testable [`crate::mapping`] module;
//! here we only marshal `U16CStr` names and WinFsp's `FileInfo`/`FileSecurity`
//! structs to and from those plain values.
//!
//! Namespace model: a Windows path `\a\b\file.txt` is resolved by walking the
//! view tree from `ROOT_INO` with `MetaProvider::lookup`. A resolved node's
//! `data_ref` is the opaque object key the `StorageBackend` reads/writes. Each
//! open handle becomes a [`GroveFileContext`]; since WinFsp hands it back only
//! as `&` across driver threads, the fields it mutates (the delete flag, the
//! driver-managed directory buffer) use interior mutability.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use grovefs::backend::StorageBackend;
use grovefs::meta::{MetaProvider, NodeInfo};

use winfsp::filesystem::{
    DirBuffer, DirInfo, DirMarker, FileSecurity, FileInfo, FileSystemContext, OpenFileInfo,
    VolumeInfo, WideNameInfo,
};
use winfsp::{FspError, Result, U16CStr};

use crate::mapping;

/// Wrap an NTSTATUS code (as computed in [`crate::mapping`]) as a WinFsp error.
/// `FspError::NTSTATUS` is a publicly-constructible variant, so we never need
/// the `windows` crate here.
fn nt(code: i32) -> FspError {
    FspError::NTSTATUS(code)
}

/// The filesystem context: one instance, shared across every callback and
/// thread (so it must be `Sync` — both trait objects are `Send + Sync`).
pub struct GroveWinFs {
    meta: Arc<dyn MetaProvider>,
    backend: Arc<dyn StorageBackend>,
    /// Client-side read-only mount (`--ro`) OR a server-derived lens: every
    /// mutating callback is refused with a write-protected status.
    read_only: bool,
}

impl GroveWinFs {
    pub fn new(
        meta: Arc<dyn MetaProvider>,
        backend: Arc<dyn StorageBackend>,
        read_only: bool,
    ) -> Self {
        Self { meta, backend, read_only }
    }

    // ---- internal helpers ------------------------------------------------

    /// Decompose a WinFsp name (`\a\b\file`, UTF-16 C string) into path
    /// components, deferring all the string surgery to the host-tested
    /// [`mapping::path_parts`].
    fn parts(name: &U16CStr) -> Vec<String> {
        mapping::path_parts(&name.to_string_lossy())
    }

    /// Resolve a full path to a node (root when empty). `None` if missing.
    fn resolve(&self, name: &U16CStr) -> Option<NodeInfo> {
        mapping::resolve(&*self.meta, &Self::parts(name))
    }

    /// Copy a node's WinFsp-facing integers into a `FileInfo`.
    fn fill(node: &NodeInfo, fi: &mut FileInfo) {
        let a = mapping::node_attrs(node);
        fi.file_attributes = a.file_attributes;
        fi.reparse_tag = 0;
        fi.file_size = a.size;
        fi.allocation_size = a.size;
        fi.creation_time = a.creation_time;
        fi.last_access_time = a.last_access_time;
        fi.last_write_time = a.last_write_time;
        fi.change_time = a.change_time;
        fi.index_number = a.index_number;
        fi.hard_links = a.hard_links;
        fi.ea_size = 0;
    }

    /// Refuse a mutation when mounted read-only.
    fn check_writable(&self) -> Result<()> {
        if self.read_only {
            Err(nt(mapping::STATUS_MEDIA_WRITE_PROTECTED))
        } else {
            Ok(())
        }
    }

    fn open_ctx(node: &NodeInfo) -> GroveFileContext {
        GroveFileContext {
            ino: node.id,
            key: node.data_ref.as_ref().map(|k| k.as_str().into()),
            delete_pending: AtomicBool::new(false),
            dirbuffer: DirBuffer::new(),
        }
    }
}

/// A single open file/directory handle. Reached only behind `&` from driver
/// threads, so mutable state uses atomics / `DirBuffer`'s interior mutability.
pub struct GroveFileContext {
    /// The inode this handle was opened for (stable identity within the view).
    ino: i64,
    /// Object key for a regular file (`data_ref`), used by read/write. `None`
    /// for directories and symlinks (so those handles reject data IO by itself).
    key: Option<Arc<str>>,
    /// Set by `set_delete`; honoured in `cleanup` (WinFsp forbids deleting in
    /// `set_delete` itself).
    delete_pending: AtomicBool,
    /// Driver-managed enumeration buffer, only meaningful for directories.
    dirbuffer: DirBuffer,
}

impl GroveFileContext {
    /// The object key as a `&str`, or a `STATUS_INVALID_DEVICE_REQUEST` error
    /// for handles that have no data object (directories / symlinks).
    fn data_key(&self) -> Result<&str> {
        self.key.as_deref().ok_or_else(|| nt(mapping::STATUS_INVALID_DEVICE_REQUEST))
    }
}

impl FileSystemContext for GroveWinFs {
    type FileContext = GroveFileContext;

    fn get_security_by_name(
        &self,
        file_name: &U16CStr,
        _security_descriptor: Option<&mut [c_void]>,
        _reparse_point_resolver: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
    ) -> Result<FileSecurity> {
        // GroveFS carries no NT ACLs: report only existence + attributes. A
        // missing node is signalled by STATUS_OBJECT_NAME_NOT_FOUND, which is
        // how WinFsp learns "does not exist yet, but the parent does" so a
        // following `create` is allowed.
        match self.resolve(file_name) {
            Some(node) => Ok(FileSecurity {
                reparse: false,
                sz_security_descriptor: 0,
                attributes: mapping::node_attrs(&node).file_attributes,
            }),
            None => Err(nt(mapping::STATUS_OBJECT_NAME_NOT_FOUND)),
        }
    }

    fn open(
        &self,
        file_name: &U16CStr,
        _create_options: u32,
        _granted_access: u32,
        file_info: &mut OpenFileInfo,
    ) -> Result<Self::FileContext> {
        let node = self
            .resolve(file_name)
            .ok_or_else(|| nt(mapping::STATUS_OBJECT_NAME_NOT_FOUND))?;
        Self::fill(&node, file_info.as_mut());
        Ok(Self::open_ctx(&node))
    }

    #[allow(clippy::too_many_arguments)]
    fn create(
        &self,
        file_name: &U16CStr,
        _create_options: u32,
        _granted_access: u32,
        file_attributes: u32,
        _security_descriptor: Option<&[c_void]>,
        _allocation_size: u64,
        _extra_buffer: Option<&[u8]>,
        _extra_buffer_is_reparse_point: bool,
        file_info: &mut OpenFileInfo,
    ) -> Result<Self::FileContext> {
        self.check_writable()?;

        let parts = Self::parts(file_name);
        let name = parts
            .last()
            .cloned()
            .ok_or_else(|| nt(mapping::STATUS_INVALID_PARAMETER))?;
        // The parent directory must already exist; a one-component path lives
        // in the root.
        let parent_ino = mapping::parent_ino(&*self.meta, &parts)
            .ok_or_else(|| nt(mapping::STATUS_OBJECT_PATH_NOT_FOUND))?;

        // FILE_ATTRIBUTE_DIRECTORY on the create request means "make a dir".
        // (`FILE_FLAGS_AND_ATTRIBUTES` is a `u32` alias, so mask directly.)
        let want_dir = (file_attributes & mapping::FILE_ATTRIBUTE_DIRECTORY) != 0;

        let node = if want_dir {
            let ino = self
                .meta
                .create_node(parent_ino, &name, true, None, 0o755, 0, 0)
                .map_err(|e| nt(mapping::meta_err_to_status(e)))?;
            self.meta
                .getattr(ino)
                .ok_or_else(|| nt(mapping::STATUS_INTERNAL_ERROR))?
        } else {
            // A fresh object: mint a uuid key, register the empty object with
            // the backend, and bind it to a new regular-file node.
            let key: Arc<str> = self.backend.new_key(&uuid::Uuid::new_v4().simple().to_string()).into();
            self.backend.create(&key).map_err(FspError::from)?;
            let ino = self
                .meta
                .create_node(parent_ino, &name, false, Some(&*key), 0o644, 0, 0)
                .map_err(|e| nt(mapping::meta_err_to_status(e)))?;
            self.meta
                .getattr(ino)
                .ok_or_else(|| nt(mapping::STATUS_INTERNAL_ERROR))?
        };

        Self::fill(&node, file_info.as_mut());
        Ok(Self::open_ctx(&node))
    }

    fn read(&self, context: &Self::FileContext, buffer: &mut [u8], offset: u64) -> Result<u32> {
        let key = context.data_key()?;
        let data = self.backend.read(key, offset, buffer.len()).map_err(FspError::from)?;
        let n = data.len().min(buffer.len());
        buffer[..n].copy_from_slice(&data[..n]);
        Ok(n as u32)
    }

    fn write(
        &self,
        context: &Self::FileContext,
        buffer: &[u8],
        offset: u64,
        _write_to_eof: bool,
        _constrained_io: bool,
        file_info: &mut FileInfo,
    ) -> Result<u32> {
        self.check_writable()?;
        let key = context.data_key()?;
        let written = self.backend.write(key, offset, buffer).map_err(FspError::from)? as u64;
        // Reflect the fresh size/mtime so the driver caches the updated view.
        if let Some(node) = self.meta.getattr(context.ino) {
            Self::fill(&node, file_info);
        } else {
            file_info.file_size = offset + written;
            file_info.last_write_time = mapping::unix_secs_to_filetime(mapping::now_unix_secs());
        }
        Ok(written as u32)
    }

    fn get_file_info(&self, context: &Self::FileContext, file_info: &mut FileInfo) -> Result<()> {
        let node = self
            .meta
            .getattr(context.ino)
            .ok_or_else(|| nt(mapping::STATUS_OBJECT_NAME_NOT_FOUND))?;
        Self::fill(&node, file_info);
        Ok(())
    }

    fn read_directory(
        &self,
        context: &Self::FileContext,
        _pattern: Option<&U16CStr>,
        marker: DirMarker,
        buffer: &mut [u8],
    ) -> Result<u32> {
        // First call of an enumeration (no marker yet): rebuild the listing
        // into the driver-managed buffer. Later calls just drain from `marker`.
        if marker.is_none() {
            let lock = context.dirbuffer.acquire(true, None)?;
            for node in self.meta.readdir(context.ino) {
                // 255 = the crate's default DirInfo name-buffer size; the const
                // generic does not participate in inference and must be pinned.
                let mut di = DirInfo::<255>::new();
                Self::fill(&node, di.file_info_mut());
                di.set_name(&node.name)?;
                lock.write(&mut di)?;
            }
        }
        // Stream buffered entries back to the caller from the marker position.
        Ok(context.dirbuffer.read(marker, buffer))
    }

    fn set_file_size(
        &self,
        context: &Self::FileContext,
        new_size: u64,
        _set_allocation_size: bool,
        file_info: &mut FileInfo,
    ) -> Result<()> {
        self.check_writable()?;
        let key = context.data_key()?;
        self.backend.truncate(key, new_size).map_err(FspError::from)?;
        if let Some(node) = self.meta.getattr(context.ino) {
            Self::fill(&node, file_info);
        } else {
            file_info.file_size = new_size;
            file_info.allocation_size = new_size;
            file_info.last_write_time = mapping::unix_secs_to_filetime(mapping::now_unix_secs());
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn set_basic_info(
        &self,
        context: &Self::FileContext,
        _file_attributes: u32,
        _creation_time: u64,
        _last_access_time: u64,
        last_write_time: u64,
        _last_change_time: u64,
        file_info: &mut FileInfo,
    ) -> Result<()> {
        self.check_writable()?;
        // Convert the Windows timestamp back to Unix seconds and persist mtime.
        let secs = mapping::filetime_to_unix_secs(last_write_time);
        self.meta.set_times(context.ino, Some(secs), Some(secs));
        if let Some(node) = self.meta.getattr(context.ino) {
            Self::fill(&node, file_info);
        }
        Ok(())
    }

    fn set_delete(
        &self,
        context: &Self::FileContext,
        _file_name: &U16CStr,
        delete_file: bool,
    ) -> Result<()> {
        self.check_writable()?;
        // WinFsp: never delete here — only record the intent, honoured in
        // `cleanup` when the last handle closes. Interior mutability via the
        // atomic flag (we only have `&context`).
        context.delete_pending.store(delete_file, Ordering::SeqCst);
        Ok(())
    }

    fn rename(
        &self,
        _context: &Self::FileContext,
        file_name: &U16CStr,
        new_file_name: &U16CStr,
        _replace_if_exists: bool,
    ) -> Result<()> {
        self.check_writable()?;
        let old_parts = Self::parts(file_name);
        let old_name = old_parts
            .last()
            .cloned()
            .ok_or_else(|| nt(mapping::STATUS_INVALID_PARAMETER))?;
        let old_parent = mapping::parent_ino(&*self.meta, &old_parts)
            .ok_or_else(|| nt(mapping::STATUS_OBJECT_PATH_NOT_FOUND))?;
        let new_parts = Self::parts(new_file_name);
        let new_name = new_parts
            .last()
            .cloned()
            .ok_or_else(|| nt(mapping::STATUS_INVALID_PARAMETER))?;
        let new_parent = mapping::parent_ino(&*self.meta, &new_parts)
            .ok_or_else(|| nt(mapping::STATUS_OBJECT_PATH_NOT_FOUND))?;
        // GroveFS rename: noreplace=false gives Windows overwrite semantics.
        self.meta
            .rename(old_parent, &old_name, new_parent, &new_name, false)
            .map_err(|e| nt(mapping::meta_err_to_status(e)))?;
        Ok(())
    }

    fn flush(&self, context: Option<&Self::FileContext>, _file_info: &mut FileInfo) -> Result<()> {
        if let Some(ctx) = context {
            if let Ok(key) = ctx.data_key() {
                self.backend.fsync(key).map_err(FspError::from)?;
            }
        }
        Ok(())
    }

    fn cleanup(&self, context: &Self::FileContext, _file_name: Option<&U16CStr>, _flags: u32) {
        // Perform the deferred delete requested via `set_delete`.
        if !context.delete_pending.load(Ordering::SeqCst) || self.read_only {
            return;
        }
        if let Some(node) = self.meta.getattr(context.ino) {
            // The returned key (if the last reference went away) is GC'd by the
            // data plane; here we only drop the namespace entry.
            let _ = self.meta.unlink(node.parent_id, &node.name);
        }
    }

    fn close(&self, _context: Self::FileContext) {
        // The view tree and object store own their state; nothing per-handle to
        // release. `DirBuffer` (inside the context) drops with the value.
    }

    fn get_volume_info(&self, out_volume_info: &mut VolumeInfo) -> Result<()> {
        let st = self.backend.statvfs().map_err(FspError::from)?;
        // `VolumeInfo` sizes are BYTES (not sectors): total and free capacity.
        out_volume_info.total_size = st.tblocks.saturating_mul(st.bsize);
        out_volume_info.free_size = st.bavail.saturating_mul(st.bsize);
        Ok(())
    }
}
