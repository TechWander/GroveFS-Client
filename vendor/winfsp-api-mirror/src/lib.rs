//! OFFLINE TYPE-CHECK API-MIRROR of the `winfsp` 0.13.2 crate.
//!
//! This is **NOT** the real winfsp. It is a pure-Rust stand-in whose public
//! signatures are copied verbatim from the registry source of
//! `winfsp-0.13.2+winfsp-2.1` (`filesystem/context.rs`, `filesystem/directory.rs`,
//! `filesystem/internals/*.rs`, `error.rs`, `host/fshost.rs`,
//! `host/volumeparams.rs`, `lib.rs`), so that `cargo check --target
//! x86_64-pc-windows-*` can type-check the `#[cfg(windows)]` code of
//! `grovefs-win` on a machine without libclang / the Windows SDK / WinFsp.
//! The real `winfsp-sys` runs bindgen against the WinFsp headers and cannot
//! even build in that setting; this mirror removes the FFI while keeping the
//! *shape* the consumer code is written against:
//!
//!   * `FILE_ACCESS_RIGHTS` / `FILE_FLAGS_AND_ATTRIBUTES` are `u32` type
//!     aliases in `winfsp-sys` (lib.rs lines 18/20), so the trait below simply
//!     writes `u32` — the same underlying type the real trait resolves to.
//!   * `FspError`, `Result`, `VolumeInfo` fields, `DirMarker::is_none`,
//!     `DirBuffer::{new, acquire, read}`, `DirInfo::{new, file_info_mut}`,
//!     `WideNameInfo::set_name`, `FileSystemHost::{new, mount, start, unmount}`
//!     and every `VolumeParams` builder used by `main.rs` mirror the real API.
//!
//! Wire up via `[patch.crates-io] winfsp = { path = "vendor/winfsp-api-mirror" }`
//! ONLY while cross-`check`ing, and NEVER ship/link against it.

#![allow(dead_code, unused_variables, non_camel_case_types)]

use std::borrow::Cow;
use std::ffi::{c_void, OsStr};
use std::fmt;
use std::marker::PhantomData;

// ---- widestring stand-in (the real crate re-exports `widestring`'s types) --

/// Mirror of `widestring::U16CStr` limited to the surface `grovefs-win` uses.
#[derive(Debug)]
pub struct U16CStr {
    raw: Vec<u16>,
}

impl U16CStr {
    /// Real signature: `pub fn to_string_lossy(&self) -> Cow<'_, str>` (widestring).
    pub fn to_string_lossy(&self) -> Cow<'_, str> {
        let units: Vec<u16> = self
            .raw
            .iter()
            .copied()
            .take_while(|c| *c != 0)
            .collect();
        Cow::Owned(String::from_utf16_lossy(&units))
    }
}

/// Mirror of `widestring::U16CString` (unused by grovefs-win; kept so the
/// re-export path `winfsp::U16CString` exists like in the real lib.rs).
pub struct U16CString;

// ---- error.rs (verbatim shapes) -------------------------------------------

#[non_exhaustive]
#[derive(Debug)]
pub enum FspError {
    HRESULT(i32),
    WIN32(u32),
    NTSTATUS(i32),
    IO(std::io::ErrorKind),
}

impl fmt::Display for FspError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FspError")
    }
}

impl std::error::Error for FspError {}

impl From<std::io::Error> for FspError {
    fn from(e: std::io::Error) -> Self {
        if let Some(e) = e.raw_os_error() {
            FspError::WIN32(e as u32)
        } else {
            FspError::IO(e.kind())
        }
    }
}

pub type Result<T> = std::result::Result<T, FspError>;

/// The real trait defaults return `STATUS_INVALID_DEVICE_REQUEST.into()`.
fn unimp() -> FspError {
    FspError::NTSTATUS(0xC0000010u32 as i32)
}

// ---- init.rs ---------------------------------------------------------------

pub struct FspInit(());

pub fn winfsp_init() -> Result<FspInit> {
    Ok(FspInit(()))
}

// ---- winfsp-sys transact stand-ins ----------------------------------------

pub struct FSP_FSCTL_TRANSACT_REQ;
pub struct FSP_FSCTL_TRANSACT_RSP;

pub mod filesystem {
    use super::*;

    /// A security descriptor pointer used to update an existing descriptor.
    #[repr(transparent)]
    pub struct ModificationDescriptor(PhantomData<*const c_void>);

    #[derive(Debug)]
    pub struct FileSecurity {
        pub reparse: bool,
        pub sz_security_descriptor: u64,
        pub attributes: u32,
    }

    #[repr(C)]
    #[derive(Default, Clone, Debug)]
    /// Fields verbatim from `filesystem/internals/fileinfo.rs`.
    pub struct FileInfo {
        pub file_attributes: u32,
        pub reparse_tag: u32,
        pub allocation_size: u64,
        pub file_size: u64,
        pub creation_time: u64,
        pub last_access_time: u64,
        pub last_write_time: u64,
        pub change_time: u64,
        pub index_number: u64,
        pub hard_links: u32,
        pub ea_size: u32,
    }

    #[repr(C)]
    #[derive(Clone, Debug)]
    pub struct OpenFileInfo {
        file_info: FileInfo,
    }

    impl AsRef<FileInfo> for OpenFileInfo {
        fn as_ref(&self) -> &FileInfo {
            &self.file_info
        }
    }
    impl AsMut<FileInfo> for OpenFileInfo {
        fn as_mut(&mut self) -> &mut FileInfo {
            &mut self.file_info
        }
    }

    /// Volume fields verbatim from `internals/volumeinfo.rs`: the only public
    /// surface is `total_size`/`free_size` in BYTES + `set_volume_label`.
    #[repr(C)]
    pub struct VolumeInfo {
        pub total_size: u64,
        pub free_size: u64,
    }

    impl VolumeInfo {
        pub fn set_volume_label<P: AsRef<OsStr>>(&mut self, _volume_label: P) -> &mut Self {
            self
        }
    }

    // ---- directory.rs shapes (verbatim signatures) ------------------------

    #[derive(Debug)]
    pub struct DirBuffer(UnsafeCellShim);
    // The real DirBuffer is `UnsafeCell<PVOID>` with unsafe Send+Sync impls.
    #[derive(Debug)]
    struct UnsafeCellShim;
    // SAFETY(mirror): mirrors the real `unsafe impl Send/Sync for DirBuffer`.
    unsafe impl Send for DirBuffer {}
    unsafe impl Sync for DirBuffer {}

    impl Default for DirBuffer {
        fn default() -> Self {
            Self::new()
        }
    }

    impl DirBuffer {
        pub fn new() -> Self {
            DirBuffer(UnsafeCellShim)
        }
        pub fn acquire(&self, reset: bool, capacity_hint: Option<u32>) -> Result<DirBufferLock<'_>> {
            Ok(DirBufferLock(PhantomData))
        }
        pub fn read(&self, marker: DirMarker, buffer: &mut [u8]) -> u32 {
            0
        }
    }

    #[derive(Debug)]
    pub struct DirBufferLock<'a>(PhantomData<&'a DirBuffer>);

    impl DirBufferLock<'_> {
        pub fn write<const D: usize>(&self, dir_info: &mut DirInfo<D>) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Debug)]
    pub struct DirMarker<'a>(pub(crate) Option<&'a U16CStr>);

    impl DirMarker<'_> {
        pub fn reset(&mut self) {
            self.0.take();
        }
        pub fn is_none(&self) -> bool {
            self.0.is_none()
        }
        pub fn is_parent(&self) -> bool {
            false
        }
        pub fn is_current(&self) -> bool {
            false
        }
        pub fn inner(&self) -> Option<&[u16]> {
            None
        }
        pub fn inner_as_cstr(&self) -> Option<&U16CStr> {
            self.0
        }
    }

    #[repr(C)]
    pub struct DirInfo<const BUFFER_SIZE: usize = 255> {
        size: u16,
        file_info: FileInfo,
        file_name: [u16; BUFFER_SIZE],
    }

    impl<const BUFFER_SIZE: usize> DirInfo<BUFFER_SIZE> {
        pub fn new() -> Self {
            Self {
                size: 0,
                file_info: FileInfo::default(),
                file_name: [0; BUFFER_SIZE],
            }
        }
        pub fn file_info_mut(&mut self) -> &mut FileInfo {
            &mut self.file_info
        }
    }

    impl<const BUFFER_SIZE: usize> Default for DirInfo<BUFFER_SIZE> {
        fn default() -> Self {
            Self::new()
        }
    }

    /// Mirror of `internals/widenameinfo.rs::WideNameInfo` — `set_name` is a
    /// provided method taking `P: AsRef<OsStr>`, exactly as the real trait.
    pub trait WideNameInfo<const BUFFER_SIZE: usize = 255> {
        fn reset(&mut self);
        fn set_name<P: AsRef<OsStr>>(&mut self, file_name: P) -> Result<()> {
            Ok(())
        }
        fn set_name_cstr<P: AsRef<U16CStr>>(&mut self, file_name: P) -> Result<()> {
            Ok(())
        }
    }

    impl<const BUFFER_SIZE: usize> WideNameInfo<BUFFER_SIZE> for DirInfo<BUFFER_SIZE> {
        fn reset(&mut self) {
            *self = Self::new();
        }
    }

    // ---- context.rs: the FileSystemContext trait, signatures verbatim -----

    #[allow(unused_variables)]
    pub trait FileSystemContext: Sized {
        type FileContext: Sized;

        fn get_security_by_name(
            &self,
            file_name: &U16CStr,
            security_descriptor: Option<&mut [c_void]>,
            reparse_point_resolver: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
        ) -> Result<FileSecurity>;

        fn open(
            &self,
            file_name: &U16CStr,
            create_options: u32,
            granted_access: u32,
            file_info: &mut OpenFileInfo,
        ) -> Result<Self::FileContext>;

        fn close(&self, context: Self::FileContext);

        #[allow(clippy::too_many_arguments)]
        fn create(
            &self,
            file_name: &U16CStr,
            create_options: u32,
            granted_access: u32,
            file_attributes: u32,
            security_descriptor: Option<&[c_void]>,
            allocation_size: u64,
            extra_buffer: Option<&[u8]>,
            extra_buffer_is_reparse_point: bool,
            file_info: &mut OpenFileInfo,
        ) -> Result<Self::FileContext> {
            Err(unimp())
        }

        fn cleanup(&self, context: &Self::FileContext, file_name: Option<&U16CStr>, flags: u32) {}

        fn flush(&self, context: Option<&Self::FileContext>, file_info: &mut FileInfo) -> Result<()> {
            Err(unimp())
        }

        fn get_file_info(&self, context: &Self::FileContext, file_info: &mut FileInfo) -> Result<()> {
            Err(unimp())
        }

        fn get_security(
            &self,
            context: &Self::FileContext,
            security_descriptor: Option<&mut [c_void]>,
        ) -> Result<u64> {
            Err(unimp())
        }

        fn set_security(
            &self,
            context: &Self::FileContext,
            security_information: u32,
            modification_descriptor: ModificationDescriptor,
        ) -> Result<()> {
            Err(unimp())
        }

        fn overwrite(
            &self,
            context: &Self::FileContext,
            file_attributes: u32,
            replace_file_attributes: bool,
            allocation_size: u64,
            extra_buffer: Option<&[u8]>,
            file_info: &mut FileInfo,
        ) -> Result<()> {
            Err(unimp())
        }

        fn read_directory(
            &self,
            context: &Self::FileContext,
            pattern: Option<&U16CStr>,
            marker: DirMarker,
            buffer: &mut [u8],
        ) -> Result<u32> {
            Err(unimp())
        }

        fn rename(
            &self,
            context: &Self::FileContext,
            file_name: &U16CStr,
            new_file_name: &U16CStr,
            replace_if_exists: bool,
        ) -> Result<()> {
            Err(unimp())
        }

        #[allow(clippy::too_many_arguments)]
        fn set_basic_info(
            &self,
            context: &Self::FileContext,
            file_attributes: u32,
            creation_time: u64,
            last_access_time: u64,
            last_write_time: u64,
            last_change_time: u64,
            file_info: &mut FileInfo,
        ) -> Result<()> {
            Err(unimp())
        }

        fn set_delete(
            &self,
            context: &Self::FileContext,
            file_name: &U16CStr,
            delete_file: bool,
        ) -> Result<()> {
            Err(unimp())
        }

        fn set_file_size(
            &self,
            context: &Self::FileContext,
            new_size: u64,
            set_allocation_size: bool,
            file_info: &mut FileInfo,
        ) -> Result<()> {
            Err(unimp())
        }

        fn read(&self, context: &Self::FileContext, buffer: &mut [u8], offset: u64) -> Result<u32> {
            Err(unimp())
        }

        fn write(
            &self,
            context: &Self::FileContext,
            buffer: &[u8],
            offset: u64,
            write_to_eof: bool,
            constrained_io: bool,
            file_info: &mut FileInfo,
        ) -> Result<u32> {
            Err(unimp())
        }

        fn get_dir_info_by_name(
            &self,
            context: &Self::FileContext,
            file_name: &U16CStr,
            out_dir_info: &mut DirInfo,
        ) -> Result<()> {
            Err(unimp())
        }

        fn get_volume_info(&self, out_volume_info: &mut VolumeInfo) -> Result<()> {
            Err(unimp())
        }

        fn set_volume_label(&self, volume_label: &U16CStr, volume_info: &mut VolumeInfo) -> Result<()> {
            Err(unimp())
        }

        fn get_stream_info(&self, context: &Self::FileContext, buffer: &mut [u8]) -> Result<u32> {
            Err(unimp())
        }

        fn get_reparse_point_by_name(
            &self,
            file_name: &U16CStr,
            is_directory: bool,
            buffer: &mut [u8],
        ) -> Result<u64> {
            Err(unimp())
        }

        fn get_reparse_point(
            &self,
            context: &Self::FileContext,
            file_name: &U16CStr,
            buffer: &mut [u8],
        ) -> Result<u64> {
            Err(unimp())
        }

        fn set_reparse_point(
            &self,
            context: &Self::FileContext,
            file_name: &U16CStr,
            buffer: &[u8],
        ) -> Result<()> {
            Err(unimp())
        }

        fn delete_reparse_point(
            &self,
            context: &Self::FileContext,
            file_name: &U16CStr,
            buffer: &[u8],
        ) -> Result<()> {
            Err(unimp())
        }

        fn get_extended_attributes(
            &self,
            context: &Self::FileContext,
            buffer: &mut [u8],
        ) -> Result<u32> {
            Err(unimp())
        }

        fn set_extended_attributes(
            &self,
            context: &Self::FileContext,
            buffer: &[u8],
            file_info: &mut FileInfo,
        ) -> Result<()> {
            Err(unimp())
        }

        fn control(
            &self,
            context: &Self::FileContext,
            control_code: u32,
            input: &[u8],
            output: &mut [u8],
        ) -> Result<u32> {
            Err(unimp())
        }

        fn dispatcher_stopped(&self, normally: bool) {}
    }
}

pub mod host {
    use super::*;
    use crate::filesystem::FileSystemContext;

    /// Mirror of `host/fshost.rs` lines 29-55 (enum + both `From` impls).
    pub enum MountPoint<'a> {
        MountPoint(&'a OsStr),
        NextFreeDrive,
    }

    impl<'a, S> From<&'a S> for MountPoint<'a>
    where
        S: AsRef<OsStr> + ?Sized,
    {
        fn from(s: &'a S) -> Self {
            Self::MountPoint(s.as_ref())
        }
    }

    impl<'short, 'long, 'middle> From<&'short MountPoint<'long>> for MountPoint<'middle>
    where
        'long: 'middle,
    {
        fn from(s: &'short MountPoint<'long>) -> Self {
            match s {
                MountPoint::MountPoint(v) => MountPoint::MountPoint(v),
                MountPoint::NextFreeDrive => MountPoint::NextFreeDrive,
            }
        }
    }

    /// Every builder method mirrors `host/volumeparams.rs` (names + arg types
    /// + `&mut Self` returns, exactly as the `make_setters!` macros emit).
    pub struct VolumeParams {
        _priv: (),
    }

    impl VolumeParams {
        pub fn new() -> Self {
            VolumeParams { _priv: () }
        }
        pub fn filesystem_name<P: AsRef<OsStr>>(&mut self, _n: P) -> &mut Self {
            self
        }
        pub fn sector_size(&mut self, _n: u16) -> &mut Self {
            self
        }
        pub fn sectors_per_allocation_unit(&mut self, _n: u16) -> &mut Self {
            self
        }
        pub fn max_component_length(&mut self, _n: u16) -> &mut Self {
            self
        }
        pub fn volume_info_timeout(&mut self, _n: u32) -> &mut Self {
            self
        }
        pub fn security_timeout(&mut self, _n: u32) -> &mut Self {
            self
        }
        pub fn dir_info_timeout(&mut self, _n: u32) -> &mut Self {
            self
        }
        pub fn file_info_timeout(&mut self, _n: u32) -> &mut Self {
            self
        }
        pub fn case_preserved_names(&mut self, _n: bool) -> &mut Self {
            self
        }
        pub fn case_sensitive_search(&mut self, _n: bool) -> &mut Self {
            self
        }
        pub fn unicode_on_disk(&mut self, _n: bool) -> &mut Self {
            self
        }
        pub fn persistent_acls(&mut self, _n: bool) -> &mut Self {
            self
        }
        pub fn reparse_points(&mut self, _n: bool) -> &mut Self {
            self
        }
        pub fn named_streams(&mut self, _n: bool) -> &mut Self {
            self
        }
        pub fn read_only_volume(&mut self, _n: bool) -> &mut Self {
            self
        }
    }

    impl Default for VolumeParams {
        fn default() -> Self {
            Self::new()
        }
    }

    /// Mirror of `FileSystemHost<T, S = FineGuard>`; the guard generic is
    /// collapsed because it never appears in consumer code, while the *bounds*
    /// that gate `start()` (`T: Sync`, `FileContext: Sync`) are preserved.
    pub struct FileSystemHost<T: FileSystemContext> {
        _t: PhantomData<T>,
    }

    impl<T: FileSystemContext> FileSystemHost<T> {
        pub fn new(volume_params: VolumeParams, context: T) -> Result<Self> {
            Ok(Self { _t: PhantomData })
        }

        pub fn mount<M>(&mut self, mount: M) -> Result<()>
        where
            for<'b> &'b M: Into<MountPoint<'b>>,
        {
            Ok(())
        }

        pub fn unmount(&mut self) {}

        pub fn stop(&mut self) {}
    }

    impl<T: FileSystemContext + Sync> FileSystemHost<T>
    where
        <T as FileSystemContext>::FileContext: Sync,
    {
        pub fn start(&mut self) -> Result<()> {
            Ok(())
        }
    }
}

// The real lib.rs re-exports these from submodules; in the mirror they live at
// the crate root already, so `winfsp::{FspError, Result, U16CStr, winfsp_init}`
// paths resolve identically for consumer code.
