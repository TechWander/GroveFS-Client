//! WinFsp-free mapping logic for `grovefs-win`.
//!
//! This module holds every piece of the Windows↔GroveFS translation that does
//! **not** need a WinFsp type: path decomposition, the `FILETIME` epoch math,
//! the stable Windows file-attribute bits, the NTSTATUS codes we return, and
//! the projection of a GroveFS [`NodeInfo`] onto the fields WinFsp wants. It
//! compiles on any host (it only depends on the `grovefs` `remote-only` seam),
//! so the tricky conversions are unit-tested on Linux rather than only inside
//! the `#[cfg(windows)]` glue in [`crate::fs`].
//!
//! The WinFsp-specific layer (`fs.rs`) is deliberately thin: it converts a
//! `U16CStr` name into a Rust `String`, delegates to the pure helpers here,
//! then copies the resulting plain integers into WinFsp's `FileInfo` /
//! `FileSecurity` and wraps NTSTATUS codes as `FspError::NTSTATUS(i32)`.

use grovefs::meta::{MetaError, MetaProvider, NodeInfo, NodeKind, ROOT_INO};
use std::time::{SystemTime, UNIX_EPOCH};

// ---- time ----------------------------------------------------------------

/// Windows `FILETIME` epoch offset: 1601-01-01 expressed in Unix seconds.
/// GroveFS stores `mtime`/`atime`/`ctime` as Unix seconds; WinFsp wants
/// 100-nanosecond intervals since 1601-01-01 (UTC).
pub const WIN_EPOCH_OFFSET_SECS: i64 = 11_644_473_600;

/// Convert Unix seconds to a WinFsp `FILETIME` (100-ns ticks since 1601). A
/// non-positive timestamp (GroveFS uses 0 for "unknown") maps to 0 (null time),
/// which WinFsp renders as "no timestamp".
pub fn unix_secs_to_filetime(t: i64) -> u64 {
    if t <= 0 {
        0
    } else {
        (t + WIN_EPOCH_OFFSET_SECS) as u64 * 10_000_000
    }
}

/// Inverse of [`unix_secs_to_filetime`]: a `FILETIME` back to Unix seconds. A
/// zero/null `FILETIME` maps to 0.
pub fn filetime_to_unix_secs(t: u64) -> i64 {
    if t == 0 {
        0
    } else {
        (t / 10_000_000) as i64 - WIN_EPOCH_OFFSET_SECS
    }
}

/// Current wall-clock time in Unix seconds (0 if the clock is before 1970).
pub fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---- Windows file attributes (stable Microsoft bit values) ---------------

/// `FILE_ATTRIBUTE_READONLY` — set for a regular file whose POSIX mode has no
/// owner-write bit, so Explorer presents it as read-only.
pub const FILE_ATTRIBUTE_READONLY: u32 = 0x0000_0001;
/// `FILE_ATTRIBUTE_SYSTEM`.
#[allow(dead_code)]
pub const FILE_ATTRIBUTE_SYSTEM: u32 = 0x0000_0004;
/// `FILE_ATTRIBUTE_DIRECTORY`.
pub const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;

// ---- NTSTATUS codes (stable Microsoft values, as i32) --------------------
//
// WinFsp callbacks report outcomes as NTSTATUS. The `winfsp` crate exposes
// `FspError::NTSTATUS(i32)` as a publicly-constructible variant, so we carry
// the handful of codes we return as plain `i32` constants here — that keeps
// this module (and its unit tests) free of the `windows`/`winfsp-sys` crates
// while the `#[cfg(windows)]` glue wraps them with `FspError::NTSTATUS(code)`.

#[allow(dead_code)]
pub const STATUS_UNSUCCESSFUL: i32 = 0xC000_0001u32 as i32;
#[allow(dead_code)]
pub const STATUS_INVALID_PARAMETER: i32 = 0xC000_000Du32 as i32;
pub const STATUS_INVALID_DEVICE_REQUEST: i32 = 0xC000_0010u32 as i32;
pub const STATUS_ACCESS_DENIED: i32 = 0xC000_0022u32 as i32;
pub const STATUS_OBJECT_TYPE_MISMATCH: i32 = 0xC000_0024u32 as i32;
pub const STATUS_OBJECT_NAME_NOT_FOUND: i32 = 0xC000_0034u32 as i32;
pub const STATUS_OBJECT_NAME_COLLISION: i32 = 0xC000_0035u32 as i32;
pub const STATUS_OBJECT_PATH_NOT_FOUND: i32 = 0xC000_003Au32 as i32;
pub const STATUS_MEDIA_WRITE_PROTECTED: i32 = 0xC000_00A2u32 as i32;
pub const STATUS_INTERNAL_ERROR: i32 = 0xC000_00E9u32 as i32;
pub const STATUS_DIRECTORY_NOT_EMPTY: i32 = 0xC000_0101u32 as i32;

// ---- path decomposition --------------------------------------------------

/// Split a WinFsp volume-relative path (`\a\b\file.txt`) into its components,
/// dropping the leading separator and any empty segments. A trailing
/// `:stream` alternate-data-stream suffix on the final component is stripped:
/// GroveFS has no named streams, so `\f:foo` addresses the file `\f`.
///
/// The input is already a UTF-8 `String` (the caller converts the `U16CStr`);
/// this is pure string surgery, hence host-testable.
pub fn path_parts(win_path: &str) -> Vec<String> {
    let trimmed = win_path.trim_start_matches('\\');
    let mut parts: Vec<String> = trimmed
        .split('\\')
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
        .collect();
    // Strip an alternate-data-stream suffix (`name:stream`) from the last
    // component, then drop the component entirely if only `:` remained.
    if let Some(last) = parts.last_mut() {
        if let Some(colon) = last.find(':') {
            last.truncate(colon);
        }
    }
    parts.retain(|p| !p.is_empty());
    parts
}

// ---- namespace resolution (MetaProvider, so no WinFsp types) -------------

/// Walk the view tree from the root along `parts`, returning the final node.
/// An empty `parts` resolves to the root. `None` if any component is missing.
pub fn resolve(meta: &dyn MetaProvider, parts: &[String]) -> Option<NodeInfo> {
    if parts.is_empty() {
        return meta.getattr(ROOT_INO);
    }
    let mut parent = ROOT_INO;
    let mut node = None;
    for comp in parts {
        let found = meta.lookup(parent, comp)?;
        parent = found.id;
        node = Some(found);
    }
    node
}

/// Resolve the parent directory inode for a full path's components. A
/// one-component path lives in the root; deeper paths resolve their prefix.
/// `None` if the parent chain does not exist.
pub fn parent_ino(meta: &dyn MetaProvider, parts: &[String]) -> Option<i64> {
    if parts.len() <= 1 {
        return Some(ROOT_INO);
    }
    Some(resolve(meta, &parts[..parts.len() - 1])?.id)
}

// ---- NodeInfo → the integers WinFsp wants --------------------------------

/// The plain-integer projection of a GroveFS node onto WinFsp's file-info
/// fields. `fs.rs` copies these verbatim into a `FileInfo`; keeping the shape
/// here lets us assert the mapping on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileAttrs {
    pub is_dir: bool,
    pub file_attributes: u32,
    pub size: u64,
    pub creation_time: u64,
    pub last_access_time: u64,
    pub last_write_time: u64,
    pub change_time: u64,
    pub index_number: u64,
    pub hard_links: u32,
}

/// Build the [`FileAttrs`] for a node: directory vs. read-only bits, the size
/// (0 for directories), `FILETIME`-converted timestamps, and the inode id as
/// the FS-assigned `index_number` (a stable, per-view-unique reference).
pub fn node_attrs(node: &NodeInfo) -> FileAttrs {
    let is_dir = node.kind == NodeKind::Dir;
    let size = if is_dir { 0 } else { node.size };
    let mut file_attributes = 0u32;
    if is_dir {
        file_attributes |= FILE_ATTRIBUTE_DIRECTORY;
    } else if node.mode & 0o200 == 0 {
        // No owner-write bit → present as read-only.
        file_attributes |= FILE_ATTRIBUTE_READONLY;
    }
    FileAttrs {
        is_dir,
        file_attributes,
        size,
        creation_time: unix_secs_to_filetime(node.ctime),
        last_access_time: unix_secs_to_filetime(node.atime),
        last_write_time: unix_secs_to_filetime(node.mtime),
        change_time: unix_secs_to_filetime(node.mtime),
        index_number: node.id as u64,
        hard_links: node.nlink.min(u32::MAX as u64) as u32,
    }
}

/// Map a [`MetaError`] to the closest NTSTATUS WinFsp understands.
pub fn meta_err_to_status(e: MetaError) -> i32 {
    match e {
        MetaError::Exists => STATUS_OBJECT_NAME_COLLISION,
        MetaError::NotFound => STATUS_OBJECT_NAME_NOT_FOUND,
        MetaError::NotEmpty => STATUS_DIRECTORY_NOT_EMPTY,
        MetaError::TypeMismatch => STATUS_OBJECT_TYPE_MISMATCH,
        MetaError::OpNotPermitted => STATUS_ACCESS_DENIED,
        MetaError::BadParentCycle => STATUS_UNSUCCESSFUL,
        MetaError::Db(_) => STATUS_INTERNAL_ERROR,
    }
}

// ---- write gating (the two flags the `M_OPEN` ack carries) ----------------

/// Which kind of mutation a WinFsp callback performs, as seen by the volume.
///
/// The kernel mount (`grovefs/src/fs_impl.rs`) enforces the SAME split, and it
/// is the server's view governance that defines it: `read_only` is the whole
/// volume being read-only (a DERIVED lens, a read-only API key, or `--ro`),
/// while `content_ro` is a FORK — the tree is the caller's to organize, the
/// bytes are shared with the base view, so only the BYTES are off limits.
/// Lumping the two together is what makes a fork mount useless; letting content
/// through is what lets a fork damage its base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mutation {
    /// Changes an object's BYTES: a data write, or a truncate/extend of the
    /// backing object (`set_file_size`).
    Content,
    /// Changes the TREE only: create, delete, rename, attributes. Creating an
    /// empty file is a tree operation — the fork may lay out its own shape, and
    /// the first BYTES written into it are what the content gate refuses (the
    /// same order the kernel mount documents: an empty new file, then `EROFS`
    /// on its first `write(2)`).
    Tree,
}

/// The gate every mutating callback runs through: `Some(NTSTATUS)` to refuse,
/// `None` to serve. Content needs both flags clear; tree mutations only need
/// the volume itself to be writable.
pub fn refuse_mutation(read_only: bool, content_ro: bool, m: Mutation) -> Option<i32> {
    match m {
        Mutation::Tree if read_only => Some(STATUS_MEDIA_WRITE_PROTECTED),
        Mutation::Content if read_only || content_ro => Some(STATUS_MEDIA_WRITE_PROTECTED),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: i64, kind: NodeKind, mode: u32, size: u64) -> NodeInfo {
        NodeInfo {
            id,
            parent_id: ROOT_INO,
            name: format!("n{id}"),
            kind,
            data_ref: if kind == NodeKind::Regular { Some(format!("key{id}")) } else { None },
            link_target: None,
            mode,
            uid: 0,
            gid: 0,
            mtime: 1_700_000_000,
            atime: 1_700_000_001,
            ctime: 1_700_000_002,
            nlink: 2,
            size,
        }
    }

    #[test]
    fn filetime_round_trips() {
        let secs = 1_700_000_000i64;
        let ft = unix_secs_to_filetime(secs);
        assert_eq!(ft, (secs + WIN_EPOCH_OFFSET_SECS) as u64 * 10_000_000);
        assert_eq!(filetime_to_unix_secs(ft), secs);
    }

    #[test]
    fn null_time_maps_to_zero_both_ways() {
        assert_eq!(unix_secs_to_filetime(0), 0);
        assert_eq!(unix_secs_to_filetime(-5), 0);
        assert_eq!(filetime_to_unix_secs(0), 0);
    }

    #[test]
    fn path_parts_basics() {
        assert_eq!(path_parts(""), Vec::<String>::new());
        assert_eq!(path_parts("\\"), Vec::<String>::new());
        assert_eq!(path_parts("\\"), Vec::<String>::new());
        assert_eq!(path_parts("a"), ["a"]);
        assert_eq!(path_parts("a\\b\\c.txt"), ["a", "b", "c.txt"]);
        // multiple / trailing separators collapse
        assert_eq!(path_parts("\\a\\\\b\\"), ["a", "b"]);
    }

    #[test]
    fn path_parts_strips_stream_suffix_on_last_component() {
        assert_eq!(path_parts("file:stream"), ["file"]);
        assert_eq!(path_parts("dir\\file:$DATA"), ["dir", "file"]);
        // `file:` names the file's default stream — the file is still `file`.
        assert_eq!(path_parts("dir\\file:"), ["dir", "file"]);
        // a bare `:` component (no file name) collapses away entirely.
        assert_eq!(path_parts("dir\\:"), ["dir"]);
    }

    #[test]
    fn directory_gets_dir_attribute_and_zero_size() {
        let a = node_attrs(&node(7, NodeKind::Dir, 0o755, 4096));
        assert!(a.is_dir);
        assert_eq!(a.file_attributes, FILE_ATTRIBUTE_DIRECTORY);
        assert_eq!(a.size, 0);
        assert_eq!(a.index_number, 7);
    }

    #[test]
    fn writable_regular_file_has_no_attribute_bits() {
        let a = node_attrs(&node(9, NodeKind::Regular, 0o644, 123));
        assert!(!a.is_dir);
        assert_eq!(a.file_attributes, 0);
        assert_eq!(a.size, 123);
    }

    #[test]
    fn no_owner_write_mode_sets_readonly() {
        let a = node_attrs(&node(3, NodeKind::Regular, 0o444, 5));
        assert_eq!(a.file_attributes, FILE_ATTRIBUTE_READONLY);
    }

    #[test]
    fn timestamps_are_filetimes() {
        let a = node_attrs(&node(1, NodeKind::Regular, 0o644, 0));
        assert_eq!(a.last_write_time, unix_secs_to_filetime(1_700_000_000));
        assert_eq!(a.creation_time, unix_secs_to_filetime(1_700_000_002));
    }

    #[test]
    fn meta_errors_map_to_distinct_statuses() {
        assert_eq!(meta_err_to_status(MetaError::Exists), STATUS_OBJECT_NAME_COLLISION);
        assert_eq!(meta_err_to_status(MetaError::NotFound), STATUS_OBJECT_NAME_NOT_FOUND);
        assert_eq!(meta_err_to_status(MetaError::NotEmpty), STATUS_DIRECTORY_NOT_EMPTY);
        assert_eq!(meta_err_to_status(MetaError::Db("x".into())), STATUS_INTERNAL_ERROR);
        // the status values are negative i32 (high bit set), as NTSTATUS requires
        assert!(STATUS_OBJECT_NAME_NOT_FOUND < 0);
    }

    #[test]
    fn content_gate_is_stricter_than_tree_gate() {
        // Writable volume: both served.
        assert_eq!(refuse_mutation(false, false, Mutation::Tree), None);
        assert_eq!(refuse_mutation(false, false, Mutation::Content), None);
        // FORK (content read-only): the tree stays writable, the bytes do not.
        assert_eq!(refuse_mutation(false, true, Mutation::Tree), None);
        assert_eq!(
            refuse_mutation(false, true, Mutation::Content),
            Some(STATUS_MEDIA_WRITE_PROTECTED)
        );
        // Whole volume read-only (derived lens / --ro): both refused.
        assert_eq!(
            refuse_mutation(true, false, Mutation::Tree),
            Some(STATUS_MEDIA_WRITE_PROTECTED)
        );
        assert_eq!(
            refuse_mutation(true, true, Mutation::Content),
            Some(STATUS_MEDIA_WRITE_PROTECTED)
        );
    }

    #[test]
    fn known_status_codes_match_microsoft_values() {
        // Sanity guard against transcription slips on the constants we return.
        assert_eq!(STATUS_INVALID_DEVICE_REQUEST, -1073741808i32); // 0xC0000010
        assert_eq!(STATUS_OBJECT_NAME_NOT_FOUND, -1073741772i32); // 0xC0000034
        assert_eq!(STATUS_ACCESS_DENIED, -1073741790i32); // 0xC0000022
    }
}
