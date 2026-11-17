//! Transfer verified objects into a private same-volume namespace before shell use.

pub(crate) fn recovery_title(title: &str, recovery: &[String]) -> String {
    if recovery.is_empty() {
        title.into()
    } else {
        format!(
            "{title}; {}",
            serde_json::to_string(recovery).expect("serialize recovery strings")
        )
    }
}

#[cfg(windows)]
mod native {
    use std::ffi::c_void;
    use std::fs::{File, OpenOptions};
    use std::os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FileIdInfo, GetFileInformationByHandleEx, FILE_ID_INFO,
    };

    const STAGE_ACCESS: u32 = 0x120089;
    const STAGE_SHARE: u32 = 3;

    #[repr(C)]
    struct UnicodeString {
        length: u16,
        maximum: u16,
        buffer: *mut u16,
    }
    #[repr(C)]
    struct ObjectAttributes {
        length: u32,
        root: HANDLE,
        name: *mut UnicodeString,
        attributes: u32,
        security: *mut c_void,
        qos: *mut c_void,
    }
    #[repr(C)]
    struct IoStatus {
        status: usize,
        information: usize,
    }
    #[repr(C)]
    struct RenameInfo {
        replace: u8,
        root: HANDLE,
        length: u32,
        name: [u16; 1],
    }
    #[repr(C)]
    struct StreamData {
        size: i64,
        name: [u16; 296],
    }
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtCreateFile(
            handle: *mut HANDLE,
            access: u32,
            attributes: *mut ObjectAttributes,
            status: *mut IoStatus,
            allocation: *const i64,
            file_attributes: u32,
            share: u32,
            disposition: u32,
            options: u32,
            ea: *const c_void,
            ea_len: u32,
        ) -> i32;
        fn NtSetInformationFile(
            handle: HANDLE,
            status: *mut IoStatus,
            data: *const c_void,
            length: u32,
            class: i32,
        ) -> i32;
        fn RtlNtStatusToDosError(status: i32) -> u32;
    }
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text: *const u16,
            revision: u32,
            descriptor: *mut *mut c_void,
            size: *mut u32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
        fn CloseHandle(handle: HANDLE) -> i32;
        fn SetFileInformationByHandle(
            handle: HANDLE,
            class: i32,
            data: *const c_void,
            size: u32,
        ) -> i32;
        fn GetVolumeInformationByHandleW(
            handle: HANDLE,
            name: *mut u16,
            name_size: u32,
            serial: *mut u32,
            component: *mut u32,
            flags: *mut u32,
            filesystem: *mut u16,
            filesystem_size: u32,
        ) -> i32;
        fn FindFirstStreamW(
            path: *const u16,
            level: i32,
            data: *mut StreamData,
            flags: u32,
        ) -> HANDLE;
        fn FindNextStreamW(handle: HANDLE, data: *mut StreamData) -> i32;
        fn FindClose(handle: HANDLE) -> i32;
        #[link_name = "GetFileInformationByHandleEx"]
        fn query_info(handle: HANDLE, class: i32, data: *mut c_void, size: u32) -> i32;
    }

    fn plain_leaf(handle: HANDLE) -> Result<(), String> {
        use windows::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        };
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(handle, &mut info) }.map_err(|e| e.to_string())?;
        if info.nNumberOfLinks != 1 || info.dwFileAttributes & (0x10 | 0x40 | 0x400 | 0x1000) != 0 {
            return Err("linked_reparse_or_offline_leaf".into());
        }
        Ok(())
    }

    fn last_error() -> String {
        std::io::Error::last_os_error().to_string()
    }
    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }
    pub(crate) fn identity(handle: HANDLE) -> Result<(u64, [u8; 16]), String> {
        let mut id = FILE_ID_INFO::default();
        unsafe {
            GetFileInformationByHandleEx(
                handle,
                FileIdInfo,
                &mut id as *mut _ as _,
                std::mem::size_of_val(&id) as u32,
            )
        }
        .map_err(|e| e.to_string())?;
        Ok((id.VolumeSerialNumber, id.FileId.Identifier))
    }

    // The handle query also prevents an alias race in pathname stream enumeration.
    pub(crate) fn default_stream_only(path: &Path, handle: HANDLE) -> Result<(), String> {
        let mut data = StreamData {
            size: 0,
            name: [0; 296],
        };
        let search = unsafe { FindFirstStreamW(wide(path).as_ptr(), 0, &mut data, 0) };
        if search.0 as isize == -1 {
            if std::io::Error::last_os_error().raw_os_error() != Some(38) {
                return Err(format!("stream_enumeration_failed: {}", last_error()));
            }
        } else {
            struct Search(HANDLE);
            impl Drop for Search {
                fn drop(&mut self) {
                    unsafe {
                        FindClose(self.0);
                    }
                }
            }
            let _search = Search(search);
            loop {
                let len = data
                    .name
                    .iter()
                    .position(|&v| v == 0)
                    .ok_or("invalid_stream_name")?;
                if data.name[..len] != "::$DATA".encode_utf16().collect::<Vec<_>>() {
                    return Err("named_stream_refused".into());
                }
                if unsafe { FindNextStreamW(search, &mut data) } == 0 {
                    if std::io::Error::last_os_error().raw_os_error() != Some(38) {
                        return Err(format!("stream_enumeration_failed: {}", last_error()));
                    }
                    break;
                }
            }
        }
        let mut buffer = vec![0u64; 128];
        loop {
            if unsafe {
                query_info(
                    handle,
                    7,
                    buffer.as_mut_ptr() as _,
                    (buffer.len() * 8) as u32,
                )
            } != 0
            {
                break;
            }
            let error = std::io::Error::last_os_error();
            if !matches!(error.raw_os_error(), Some(24 | 122 | 234)) || buffer.len() >= 131072 {
                return Err(format!("stream_query_failed: {error}"));
            }
            buffer.resize(buffer.len() * 2, 0);
        }
        let bytes =
            unsafe { std::slice::from_raw_parts(buffer.as_ptr() as *const u8, buffer.len() * 8) };
        let mut offset = 0;
        loop {
            if offset + 24 > bytes.len() {
                return Err("invalid_stream_info".into());
            }
            let next = u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            let len =
                u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
            if len % 2 != 0 || offset + 24 + len > bytes.len() {
                return Err("invalid_stream_info".into());
            }
            let name: Vec<u16> = bytes[offset + 24..offset + 24 + len]
                .chunks_exact(2)
                .map(|b| u16::from_ne_bytes([b[0], b[1]]))
                .collect();
            if !name.is_empty() && name != "::$DATA".encode_utf16().collect::<Vec<_>>() {
                return Err("named_stream_refused".into());
            }
            if next == 0 {
                break;
            }
            if next < 24 + len || next % 8 != 0 {
                return Err("invalid_stream_info".into());
            }
            offset = offset.checked_add(next).ok_or("invalid_stream_info")?;
        }
        Ok(())
    }

    pub(crate) fn rename(
        handle: HANDLE,
        parent: HANDLE,
        name: &std::ffi::OsStr,
    ) -> Result<(), String> {
        let name: Vec<u16> = name.encode_wide().collect();
        if name.is_empty() || name.iter().any(|&v| matches!(v, 0 | 47 | 58 | 92)) {
            return Err("invalid_transfer_name".into());
        }
        let offset = std::mem::offset_of!(RenameInfo, name);
        let size = (offset + name.len() * 2).max(std::mem::size_of::<RenameInfo>());
        let mut buffer = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
        let info = buffer.as_mut_ptr() as *mut RenameInfo;
        unsafe {
            (*info).replace = 0;
            (*info).root = parent;
            (*info).length = (name.len() * 2) as u32;
            std::ptr::copy_nonoverlapping(
                name.as_ptr(),
                (info as *mut u8).add(offset) as *mut u16,
                name.len(),
            );
            if SetFileInformationByHandle(handle, 3, info as _, size as u32) == 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(87) {
                    return Err(format!("handle_transfer_failed: {error}"));
                }
                // Some Win32 wrappers reject relative RootDirectory names; native rename stays handle-bound.
                let mut status = IoStatus {
                    status: 0,
                    information: 0,
                };
                let result = NtSetInformationFile(handle, &mut status, info as _, size as u32, 10);
                if result < 0 {
                    return Err(format!(
                        "handle_transfer_failed: {}",
                        std::io::Error::from_raw_os_error(RtlNtStatusToDosError(result) as i32)
                    ));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn handle_path(handle: HANDLE) -> Result<PathBuf, String> {
        use windows::Win32::Storage::FileSystem::{
            GetFinalPathNameByHandleW, GETFINALPATHNAMEBYHANDLE_FLAGS,
        };
        let mut buffer = vec![0u16; 32768];
        let len = unsafe {
            GetFinalPathNameByHandleW(handle, &mut buffer, GETFINALPATHNAMEBYHANDLE_FLAGS(0))
        } as usize;
        if len == 0 || len >= buffer.len() {
            return Err("object_path_unavailable".into());
        }
        let text = String::from_utf16(&buffer[..len]).map_err(|e| e.to_string())?;
        Ok(PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text)))
    }

    fn volume_path(handle: HANDLE) -> Result<PathBuf, String> {
        use windows::Win32::Storage::FileSystem::{
            GetFinalPathNameByHandleW, GETFINALPATHNAMEBYHANDLE_FLAGS,
        };
        let mut buffer = vec![0u16; 32768];
        let len = unsafe {
            GetFinalPathNameByHandleW(handle, &mut buffer, GETFINALPATHNAMEBYHANDLE_FLAGS(1))
        } as usize;
        if len == 0 || len >= buffer.len() {
            return Err("volume_path_unavailable".into());
        }
        let text = String::from_utf16(&buffer[..len]).map_err(|e| e.to_string())?;
        if !text.starts_with(r"\\?\Volume{") {
            return Err("untrusted_volume_namespace".into());
        }
        Ok(PathBuf::from(text))
    }

    pub(crate) fn directory_at_path(handle: HANDLE, path: &Path) -> Result<(), String> {
        use windows::Win32::Storage::FileSystem::{
            GetFinalPathNameByHandleW, GETFINALPATHNAMEBYHANDLE_FLAGS,
        };
        let mut attrs = [0u32; 2];
        if unsafe { query_info(handle, 9, attrs.as_mut_ptr() as _, 8) } == 0 {
            return Err(last_error());
        }
        if attrs[0] & 0x400 != 0 || attrs[0] & 0x10 == 0 {
            return Err("unsafe_restoration_parent".into());
        }
        let mut buffer = vec![0u16; 32768];
        let len = unsafe {
            GetFinalPathNameByHandleW(handle, &mut buffer, GETFINALPATHNAMEBYHANDLE_FLAGS(0))
        } as usize;
        if len == 0 || len >= buffer.len() {
            return Err("parent_path_unavailable".into());
        }
        let actual = String::from_utf16(&buffer[..len]).map_err(|e| e.to_string())?;
        if !actual
            .strip_prefix(r"\\?\")
            .unwrap_or(&actual)
            .eq_ignore_ascii_case(&path.to_string_lossy().replace('/', "\\"))
        {
            return Err("restoration_parent_redirected".into());
        }
        Ok(())
    }

    struct Descriptor(*mut c_void);
    impl Drop for Descriptor {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
    struct Directory(HANDLE);
    impl Drop for Directory {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    struct Stage {
        path: PathBuf,
        dir: Directory,
        _root: File,
    }

    impl Drop for Stage {
        fn drop(&mut self) {
            let handle = std::mem::replace(&mut self.dir.0, HANDLE::default());
            let expected = identity(handle);
            unsafe {
                CloseHandle(handle);
            }
            let Some(parent) = self.path.parent() else {
                return;
            };
            let Ok(file) = OpenOptions::new()
                .access_mode(0x10080)
                .share_mode(STAGE_SHARE)
                .custom_flags(0x02200000)
                .open(parent)
            else {
                return;
            };
            let cleanup = HANDLE(file.as_raw_handle());
            if expected.is_err() || identity(cleanup).ok() != expected.ok() {
                return;
            }
            // Directory disposition cannot remove a retained payload or follow an alias.
            let delete = 1u8;
            unsafe {
                SetFileInformationByHandle(cleanup, 4, &delete as *const _ as _, 1);
            }
        }
    }

    impl Stage {
        fn create(source: HANDLE, base: &Path) -> Result<Self, String> {
            if !crate::winutil::is_elevated() {
                return Err("needs_admin".into());
            }
            let root = OpenOptions::new()
                .read(true)
                .share_mode(3)
                .custom_flags(0x02200000)
                .open(base)
                .map_err(|e| format!("staging_root_unavailable: {e}"))?;
            let metadata = root.metadata().map_err(|e| e.to_string())?;
            use std::os::windows::fs::MetadataExt;
            if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
                return Err("unsafe_staging_root".into());
            }
            let root_handle = HANDLE(root.as_raw_handle());
            if identity(source)?.0 != identity(root_handle)?.0 {
                return Err("staging_volume_mismatch".into());
            }
            let mut flags = 0;
            if unsafe {
                GetVolumeInformationByHandleW(
                    root_handle,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut flags,
                    std::ptr::null_mut(),
                    0,
                )
            } == 0
            {
                return Err(last_error());
            }
            if flags & 8 == 0 {
                return Err("persistent_acls_required".into());
            }
            let text: Vec<u16> = "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let mut descriptor = std::ptr::null_mut();
            if unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    text.as_ptr(),
                    1,
                    &mut descriptor,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err(last_error());
            }
            let descriptor = Descriptor(descriptor);
            let guid = unsafe { windows::Win32::System::Com::CoCreateGuid() }
                .map_err(|e| e.to_string())?;
            let name = format!(".tp-verified-recycle-{guid:?}");
            let mut name_w: Vec<u16> = name.encode_utf16().collect();
            let mut name_u = UnicodeString {
                length: (name_w.len() * 2) as u16,
                maximum: (name_w.len() * 2) as u16,
                buffer: name_w.as_mut_ptr(),
            };
            let mut attributes = ObjectAttributes {
                length: std::mem::size_of::<ObjectAttributes>() as u32,
                root: root_handle,
                name: &mut name_u,
                attributes: 0x40,
                security: descriptor.0,
                qos: std::ptr::null_mut(),
            };
            let mut status = IoStatus {
                status: 0,
                information: 0,
            };
            let mut handle = HANDLE::default();
            // Atomic create/open leaves no writable-parent replacement window.
            let result = unsafe {
                NtCreateFile(
                    &mut handle,
                    STAGE_ACCESS,
                    &mut attributes,
                    &mut status,
                    std::ptr::null(),
                    0x10,
                    STAGE_SHARE,
                    2,
                    0x00200021,
                    std::ptr::null(),
                    0,
                )
            };
            if result < 0 {
                return Err(format!(
                    "staging_create_failed: {}",
                    std::io::Error::from_raw_os_error(
                        unsafe { RtlNtStatusToDosError(result) } as i32
                    )
                ));
            }
            let dir = Directory(handle);
            if identity(handle)?.0 != identity(source)?.0 {
                return Err("staging_volume_mismatch".into());
            }
            // Volume-GUID names cannot be redirected by a mutable DOS drive mapping.
            let path = volume_path(handle)?.join("payload");
            Ok(Self {
                path,
                dir,
                _root: root,
            })
        }
    }

    pub(crate) fn recycle_verified(
        source: HANDLE,
        parent: HANDLE,
        original: &Path,
        cancel: &AtomicBool,
        validate: &dyn Fn(bool) -> Result<(), String>,
        persist: &dyn Fn(Option<&str>) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut base = PathBuf::new();
        for part in original.components().take(2) {
            base.push(part.as_os_str());
        }
        if !crate::winutil::on_fixed_drive(&base) {
            return Err("non_fixed_drive".into());
        }
        run(
            source,
            parent,
            original,
            &base,
            cancel,
            validate,
            persist,
            &shell::recycle,
        )
    }

    fn run(
        source: HANDLE,
        parent: HANDLE,
        original: &Path,
        base: &Path,
        cancel: &AtomicBool,
        validate: &dyn Fn(bool) -> Result<(), String>,
        persist: &dyn Fn(Option<&str>) -> Result<(), String>,
        recycle: &dyn Fn(&Path, HANDLE) -> Result<(), String>,
    ) -> Result<(), String> {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let stage = Stage::create(source, base)?;
        let id = identity(source)?;
        let pending = format!(
            "recovery_pending: original={}; staging={}; volume={}; file_id={:02x?}",
            original.display(),
            stage.path.display(),
            id.0,
            id.1
        );
        persist(Some(&pending))?;
        let transfer = (|| {
            validate(false)?;
            default_stream_only(original, source)?;
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            rename(source, stage.dir.0, stage.path.file_name().unwrap())
        })();
        if let Err(error) = transfer {
            persist(None)?;
            return Err(error);
        }
        let outcome = plain_leaf(source)
            .and_then(|_| default_stream_only(&stage.path, source))
            .and_then(|_| recycle(&stage.path, source));
        match outcome {
            Ok(()) => finish_recycle(source, original, &pending, persist),
            Err(error) => {
                let restore = validate(true).and_then(|_| {
                    directory_at_path(parent, original.parent().ok_or("invalid_original_parent")?)?;
                    rename(
                        source,
                        parent,
                        original.file_name().ok_or("invalid_original_name")?,
                    )
                });
                match restore {
                    Ok(()) => {
                        persist(None)?;
                        Err(format!("recycle_failed_restored: {error}"))
                    }
                    Err(restore_error) => {
                        // An unconfirmed shell move is pulled back by identity, never by its old name.
                        let restage = match volume_path(source) {
                            Ok(path) if path == stage.path => Ok(()),
                            _ => rename(source, stage.dir.0, stage.path.file_name().unwrap()),
                        };
                        let restage_error = restage
                            .err()
                            .map(|e| format!("; restaging_error={e}"))
                            .unwrap_or_default();
                        let recovery = volume_path(source)
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|e| {
                                format!("{}; recovery_path_error={e}", stage.path.display())
                            });
                        let detail = format!("{pending}; recovery={recovery}; recycle_error={error}; restoration_error={restore_error}{restage_error}");
                        if let Err(journal) = persist(Some(&detail)) {
                            return Err(format!("{detail}; history_error={journal}"));
                        }
                        Err(detail)
                    }
                }
            }
        }
    }

    fn finish_recycle(
        source: HANDLE,
        original: &Path,
        pending: &str,
        persist: &dyn Fn(Option<&str>) -> Result<(), String>,
    ) -> Result<(), String> {
        let location = volume_path(source)?;
        let id = identity(source)?;
        let retained = format!("{pending}; recycle_location={}", location.display());
        persist(Some(&retained))?;
        // A stream added after PreDelete must stay recoverable, never count as a verified success.
        plain_leaf(source)?;
        default_stream_only(&location, source)?;
        persist(Some(&format!(
            "recycled: original={}; recycle_location={}; volume={}; file_id={:02x?}",
            original.display(),
            location.display(),
            id.0,
            id.1
        )))
    }

    #[cfg(test)]
    pub(crate) fn fixture_recycle(
        source: HANDLE,
        parent: HANDLE,
        original: &Path,
        base: &Path,
        cancel: &AtomicBool,
        validate: &dyn Fn(bool) -> Result<(), String>,
        persist: &dyn Fn(Option<&str>) -> Result<(), String>,
        recycle: &dyn Fn(&Path, HANDLE) -> Result<(), String>,
    ) -> Result<(), String> {
        assert!(base
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(".tp-"));
        assert!(original.starts_with(base));
        run(
            source, parent, original, base, cancel, validate, persist, recycle,
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::cell::{Cell, RefCell};
        use std::fs;
        use std::sync::atomic::AtomicU64;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        struct Fixture(PathBuf);
        impl Fixture {
            fn new() -> Self {
                let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
                    ".tp-verified-fixture-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                fs::create_dir(&root).unwrap();
                Self(root)
            }
            fn source(&self) -> (PathBuf, File, File) {
                let path = self.0.join("source/payload");
                fs::create_dir(path.parent().unwrap()).unwrap();
                fs::write(&path, b"verified bytes").unwrap();
                let source = OpenOptions::new()
                    .read(true)
                    .access_mode(0x80010000)
                    .share_mode(5)
                    .custom_flags(0x00200000)
                    .open(&path)
                    .unwrap();
                let parent = OpenOptions::new()
                    .access_mode(0x80)
                    .share_mode(7)
                    .custom_flags(0x02200000)
                    .open(path.parent().unwrap())
                    .unwrap();
                (path, source, parent)
            }
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                fs::remove_dir_all(&self.0).expect("remove owned verified fixture only");
            }
        }
        fn elevated() -> bool {
            if crate::winutil::is_elevated() {
                true
            } else {
                eprintln!("restricted staging fixture requires elevation; not exercised");
                false
            }
        }

        #[test]
        fn native_stage_is_admin_only_or_refuses_before_transfer() {
            let f = Fixture::new();
            let (path, source, _) = f.source();
            let handle = HANDLE(source.as_raw_handle());
            let stage = Stage::create(handle, &f.0);
            if !crate::winutil::is_elevated() {
                assert_eq!(stage.err().unwrap(), "needs_admin");
                assert_eq!(handle_path(handle).unwrap(), path);
                return;
            }
            let stage = stage.unwrap();
            assert_eq!(
                identity(handle).unwrap().0,
                identity(stage.dir.0).unwrap().0
            );
            assert!(
                fs::rename(stage.path.parent().unwrap(), f.0.join("redirected-stage")).is_err()
            );
            #[link(name = "advapi32")]
            unsafe extern "system" {
                fn GetKernelObjectSecurity(
                    handle: HANDLE,
                    info: u32,
                    descriptor: *mut c_void,
                    length: u32,
                    needed: *mut u32,
                ) -> i32;
                fn ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    descriptor: *const c_void,
                    revision: u32,
                    info: u32,
                    text: *mut *mut u16,
                    length: *mut u32,
                ) -> i32;
            }
            let mut needed = 0;
            unsafe {
                GetKernelObjectSecurity(stage.dir.0, 5, std::ptr::null_mut(), 0, &mut needed);
            }
            let mut descriptor = vec![0u64; (needed as usize).div_ceil(8)];
            assert_ne!(
                unsafe {
                    GetKernelObjectSecurity(
                        stage.dir.0,
                        5,
                        descriptor.as_mut_ptr() as _,
                        (descriptor.len() * 8) as u32,
                        &mut needed,
                    )
                },
                0
            );
            let mut text = std::ptr::null_mut();
            let mut length = 0;
            assert_ne!(
                unsafe {
                    ConvertSecurityDescriptorToStringSecurityDescriptorW(
                        descriptor.as_ptr() as _,
                        1,
                        5,
                        &mut text,
                        &mut length,
                    )
                },
                0
            );
            let sddl = unsafe {
                String::from_utf16_lossy(std::slice::from_raw_parts(text, length as usize - 1))
            };
            unsafe {
                LocalFree(text as _);
            }
            assert!(sddl.contains("O:BA") && sddl.contains("D:P"), "{sddl}");
            assert!(sddl.contains(";;;SY)") && sddl.contains(";;;BA)"), "{sddl}");
            assert_eq!(sddl.matches("(A;").count(), 2, "{sddl}");
        }

        #[test]
        fn handle_transfer_ignores_renamed_source_and_junction_decoy() {
            if !elevated() {
                return;
            }
            let f = Fixture::new();
            let (path, source, parent) = f.source();
            let source_id = identity(HANDLE(source.as_raw_handle())).unwrap();
            let decoy_dir = f.0.join("protected-decoy");
            fs::create_dir(&decoy_dir).unwrap();
            fs::write(decoy_dir.join("payload"), b"DO NOT TOUCH").unwrap();
            let old_parent = f.0.join("renamed-source");
            let journal = RefCell::new(Vec::new());
            let result = fixture_recycle(
                HANDLE(source.as_raw_handle()),
                HANDLE(parent.as_raw_handle()),
                &path,
                &f.0,
                &AtomicBool::new(false),
                &|_| Ok(()),
                &|entry| {
                    journal.borrow_mut().push(entry.map(str::to_owned));
                    Ok(())
                },
                &|staged, handle| {
                    assert_ne!(staged, path);
                    assert_eq!(identity(handle)?, source_id);
                    fs::rename(path.parent().unwrap(), &old_parent).map_err(|e| e.to_string())?;
                    let status = std::process::Command::new("C:\\Windows\\System32\\cmd.exe")
                        .args(["/D", "/C", "mklink", "/J"])
                        .arg(path.parent().unwrap())
                        .arg(&decoy_dir)
                        .output()
                        .map_err(|e| e.to_string())?;
                    assert!(status.status.success());
                    fs::rename(staged, f.0.join("fixture-recycled")).map_err(|e| e.to_string())
                },
            );
            assert!(result.is_ok(), "{result:?}");
            assert_eq!(fs::read(&path).unwrap(), b"DO NOT TOUCH");
            assert_eq!(
                fs::read(f.0.join("fixture-recycled")).unwrap(),
                b"verified bytes"
            );
            fs::remove_dir(path.parent().unwrap()).unwrap();
            assert!(journal.borrow()[0].as_ref().unwrap().contains("staging="));
            assert!(journal
                .borrow()
                .last()
                .unwrap()
                .as_ref()
                .unwrap()
                .starts_with("recycled: "));
        }

        #[test]
        fn failed_recycle_restores_by_handle_without_overwrite_or_keeps_recovery() {
            if !elevated() {
                return;
            }
            for recreate in [false, true] {
                let f = Fixture::new();
                let (path, source, parent) = f.source();
                let pending = RefCell::new(None::<String>);
                let result = fixture_recycle(
                    HANDLE(source.as_raw_handle()),
                    HANDLE(parent.as_raw_handle()),
                    &path,
                    &f.0,
                    &AtomicBool::new(false),
                    &|_| Ok(()),
                    &|detail| {
                        *pending.borrow_mut() = detail.map(str::to_owned);
                        Ok(())
                    },
                    &|_, _| {
                        if recreate {
                            fs::write(&path, b"NEW OBJECT").unwrap();
                        }
                        Err("fixture shell failure".into())
                    },
                );
                assert!(result.is_err());
                if recreate {
                    assert_eq!(fs::read(&path).unwrap(), b"NEW OBJECT");
                    let recovery = volume_path(HANDLE(source.as_raw_handle())).unwrap();
                    assert_ne!(recovery, path);
                    assert_eq!(fs::read(&recovery).unwrap(), b"verified bytes");
                    assert!(pending
                        .borrow()
                        .as_ref()
                        .unwrap()
                        .contains(&recovery.display().to_string()));
                    assert!(pending
                        .borrow()
                        .as_ref()
                        .unwrap()
                        .contains("fixture shell failure"));
                } else {
                    assert_eq!(handle_path(HANDLE(source.as_raw_handle())).unwrap(), path);
                    assert_eq!(fs::read(&path).unwrap(), b"verified bytes");
                    assert!(pending.borrow().is_none());
                }
            }
        }

        #[test]
        fn pending_flush_failure_and_cancel_never_transfer_source() {
            if !elevated() {
                return;
            }
            let f = Fixture::new();
            let (path, source, parent) = f.source();
            let called = Cell::new(false);
            for cancel in [false, true] {
                let result = fixture_recycle(
                    HANDLE(source.as_raw_handle()),
                    HANDLE(parent.as_raw_handle()),
                    &path,
                    &f.0,
                    &AtomicBool::new(cancel),
                    &|_| Ok(()),
                    &|_| Err("fixture flush failure".into()),
                    &|_, _| {
                        called.set(true);
                        Ok(())
                    },
                );
                assert!(result.is_err());
                assert_eq!(handle_path(HANDLE(source.as_raw_handle())).unwrap(), path);
                assert!(!called.get());
            }
        }

        #[test]
        fn handle_rename_transfers_only_opened_identity_and_never_overwrites() {
            let f = Fixture::new();
            let (path, source, parent) = f.source();
            let before = identity(HANDLE(source.as_raw_handle())).unwrap();
            fs::rename(&path, path.with_file_name("selected-renamed")).unwrap();
            fs::write(&path, b"DECOY").unwrap();
            let destination = f.0.join("owned-transfer-test");
            fs::create_dir(&destination).unwrap();
            let destination_handle = OpenOptions::new()
                .access_mode(STAGE_ACCESS)
                .share_mode(STAGE_SHARE)
                .custom_flags(0x02200000)
                .open(&destination)
                .unwrap();
            rename(
                HANDLE(source.as_raw_handle()),
                HANDLE(destination_handle.as_raw_handle()),
                std::ffi::OsStr::new("x"),
            )
            .unwrap();
            assert_eq!(identity(HANDLE(source.as_raw_handle())).unwrap(), before);
            assert!(fs::rename(&destination, f.0.join("redirected-destination")).is_err());
            assert_eq!(fs::read(&path).unwrap(), b"DECOY");
            assert_eq!(fs::read(destination.join("x")).unwrap(), b"verified bytes");
            assert!(rename(
                HANDLE(source.as_raw_handle()),
                HANDLE(parent.as_raw_handle()),
                path.file_name().unwrap()
            )
            .is_err());
            assert_eq!(fs::read(&path).unwrap(), b"DECOY");
            assert_eq!(fs::read(destination.join("x")).unwrap(), b"verified bytes");
            rename(
                HANDLE(source.as_raw_handle()),
                HANDLE(parent.as_raw_handle()),
                std::ffi::OsStr::new("z"),
            )
            .unwrap();
            assert_eq!(
                fs::read(path.with_file_name("z")).unwrap(),
                b"verified bytes"
            );
            assert_eq!(fs::read(&path).unwrap(), b"DECOY");
        }

        #[test]
        fn confirmed_mapping_is_durable_and_keeps_original_identity() {
            let f = Fixture::new();
            let (path, source, _) = f.source();
            let destination = f.0.join("owned-bin");
            fs::create_dir(&destination).unwrap();
            let bin = destination.join("payload");
            fs::rename(&path, &bin).unwrap();
            let entries = RefCell::new(Vec::new());
            let handle = HANDLE(source.as_raw_handle());
            finish_recycle(handle, &path, "recovery_pending: fixture", &|entry| {
                entries.borrow_mut().push(entry.unwrap().to_owned());
                Ok(())
            })
            .unwrap();
            let journal = entries.borrow();
            assert_eq!(journal.len(), 2);
            assert!(journal[0].starts_with("recovery_pending:"));
            assert!(journal[1].starts_with("recycled: original="));
            assert!(journal[1].contains(&path.display().to_string()));
            assert!(journal[1].contains(&volume_path(handle).unwrap().display().to_string()));
            assert!(journal[1].contains("file_id="));
            assert_eq!(fs::read(&bin).unwrap(), b"verified bytes");
        }

        #[test]
        fn late_stream_preserves_payload_and_mapping_without_verified_success() {
            let f = Fixture::new();
            let (path, source, _) = f.source();
            let destination = f.0.join("owned-bin");
            fs::create_dir(&destination).unwrap();
            let bin = destination.join("payload");
            fs::rename(&path, &bin).unwrap();
            let stream = format!("{}:late", bin.display());
            fs::write(&stream, b"UNIQUE LATE DATA").unwrap();
            let entries = RefCell::new(Vec::new());
            let result = finish_recycle(
                HANDLE(source.as_raw_handle()),
                &path,
                "recovery_pending: fixture",
                &|entry| {
                    entries.borrow_mut().push(entry.unwrap().to_owned());
                    Ok(())
                },
            );
            assert_eq!(result.unwrap_err(), "named_stream_refused");
            let journal = entries.borrow();
            assert_eq!(journal.len(), 1);
            assert!(journal[0].contains("recycle_location="));
            assert!(!journal.iter().any(|entry| entry.starts_with("recycled:")));
            assert_eq!(fs::read(&bin).unwrap(), b"verified bytes");
            assert_eq!(fs::read(&stream).unwrap(), b"UNIQUE LATE DATA");
        }

        #[test]
        fn stream_enumeration_errors_fail_closed() {
            let f = Fixture::new();
            let (path, source, _) = f.source();
            assert!(
                default_stream_only(&f.0.join("missing"), HANDLE(source.as_raw_handle())).is_err()
            );
            assert!(default_stream_only(&path, HANDLE::default()).is_err());
        }
    }

    mod shell {
        use super::*;
        use std::sync::{atomic::AtomicU32, Arc, Mutex};
        use windows::core::{IUnknown, IUnknown_Vtbl, Interface, GUID, HRESULT, HSTRING, PCWSTR};
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
            COINIT_APARTMENTTHREADED,
        };
        use windows::Win32::UI::Shell::{
            FileOperation, IFileOperation, IFileOperationProgressSink,
            IFileOperationProgressSink_Vtbl, IShellItem, SHCreateItemFromParsingName,
            FOFX_ADDUNDORECORD, FOFX_EARLYFAILURE, FOFX_RECYCLEONDELETE, FOF_ALLOWUNDO,
            FOF_FILESONLY, FOF_NORECURSEREPARSE, FOF_NORECURSION, FOF_NO_CONNECTED_ELEMENTS,
            FOF_NO_UI, SIGDN_FILESYSPATH, TSF_DELETE_RECYCLE_IF_POSSIBLE,
        };
        const OK: HRESULT = HRESULT(0);
        const ABORT: HRESULT = HRESULT(0x80004004u32 as i32);
        struct State {
            path: PathBuf,
            id: (u64, [u8; 16]),
            prechecked: bool,
            recycled: bool,
            error: Option<String>,
        }
        #[repr(C)]
        struct Sink {
            vtable: *const IFileOperationProgressSink_Vtbl,
            refs: AtomicU32,
            state: Arc<Mutex<State>>,
        }
        unsafe extern "system" fn query(
            this: *mut c_void,
            iid: *const GUID,
            out: *mut *mut c_void,
        ) -> HRESULT {
            if iid.is_null() || out.is_null() {
                return HRESULT(0x80004003u32 as i32);
            }
            unsafe {
                *out = std::ptr::null_mut();
                if *iid == IUnknown::IID || *iid == IFileOperationProgressSink::IID {
                    *out = this;
                    add_ref(this);
                    OK
                } else {
                    HRESULT(0x80004002u32 as i32)
                }
            }
        }
        unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
            unsafe {
                (&*(this as *const Sink))
                    .refs
                    .fetch_add(1, Ordering::Relaxed)
                    + 1
            }
        }
        unsafe extern "system" fn release(this: *mut c_void) -> u32 {
            let remaining = unsafe {
                (&*(this as *const Sink))
                    .refs
                    .fetch_sub(1, Ordering::Release)
                    - 1
            };
            if remaining == 0 {
                std::sync::atomic::fence(Ordering::Acquire);
                unsafe {
                    drop(Box::from_raw(this as *mut Sink));
                }
            }
            remaining
        }
        unsafe extern "system" fn idle(_: *mut c_void) -> HRESULT {
            OK
        }
        unsafe extern "system" fn finish(_: *mut c_void, _: HRESULT) -> HRESULT {
            OK
        }
        unsafe extern "system" fn progress(_: *mut c_void, _: u32, _: u32) -> HRESULT {
            OK
        }
        macro_rules! veto {
            ($($name:ident($($arg:ident: $kind:ty),*);)*) => {$ (
                unsafe extern "system" fn $name(_: *mut c_void, $($arg: $kind),*) -> HRESULT { ABORT }
            )*};
        }
        veto! {
            pre_rename(_flags: u32, _item: *mut c_void, _name: PCWSTR);
            post_rename(_flags: u32, _item: *mut c_void, _name: PCWSTR, _result: HRESULT, _new: *mut c_void);
            pre_move(_flags: u32, _item: *mut c_void, _parent: *mut c_void, _name: PCWSTR);
            post_move(_flags: u32, _item: *mut c_void, _parent: *mut c_void, _name: PCWSTR, _result: HRESULT, _new: *mut c_void);
            pre_copy(_flags: u32, _item: *mut c_void, _parent: *mut c_void, _name: PCWSTR);
            post_copy(_flags: u32, _item: *mut c_void, _parent: *mut c_void, _name: PCWSTR, _result: HRESULT, _new: *mut c_void);
            pre_new(_flags: u32, _parent: *mut c_void, _name: PCWSTR);
            post_new(_flags: u32, _parent: *mut c_void, _name: PCWSTR, _template: PCWSTR, _attrs: u32, _result: HRESULT, _new: *mut c_void);
        }
        unsafe fn item_path(raw: *mut c_void) -> Result<PathBuf, String> {
            let item =
                unsafe { IShellItem::from_raw_borrowed(&raw) }.ok_or("missing_shell_item")?;
            let path =
                unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }.map_err(|e| e.to_string())?;
            let text = unsafe { path.to_string() };
            unsafe {
                CoTaskMemFree(Some(path.as_ptr() as _));
            }
            let text = text.map_err(|e| e.to_string())?;
            Ok(PathBuf::from(text))
        }
        unsafe extern "system" fn pre_delete(
            this: *mut c_void,
            flags: u32,
            item: *mut c_void,
        ) -> HRESULT {
            let sink = unsafe { &*(this as *const Sink) };
            let Ok(mut state) = sink.state.lock() else {
                return ABORT;
            };
            if state.prechecked || flags & TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32 == 0 {
                state.error = Some("permanent_or_extra_delete_refused".into());
                return ABORT;
            }
            let check = unsafe { item_path(item) }.and_then(|path| {
                if !path
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&state.path.to_string_lossy())
                {
                    return Err("unexpected_shell_item".into());
                }
                let file = OpenOptions::new()
                    .read(true)
                    .share_mode(5)
                    .custom_flags(0x00200000)
                    .open(&path)
                    .map_err(|e| e.to_string())?;
                let handle = HANDLE(file.as_raw_handle());
                if identity(handle)? != state.id
                    || !file.metadata().map_err(|e| e.to_string())?.is_file()
                {
                    return Err("staged_identity_changed".into());
                }
                use std::os::windows::fs::MetadataExt;
                if file
                    .metadata()
                    .map_err(|e| e.to_string())?
                    .file_attributes()
                    & 0x400
                    != 0
                {
                    return Err("staged_reparse_refused".into());
                }
                plain_leaf(handle)?;
                default_stream_only(&path, handle)
            });
            match check {
                Ok(()) => {
                    state.prechecked = true;
                    OK
                }
                Err(error) => {
                    state.error = Some(error);
                    ABORT
                }
            }
        }
        unsafe extern "system" fn post_delete(
            this: *mut c_void,
            _: u32,
            _: *mut c_void,
            result: HRESULT,
            new: *mut c_void,
        ) -> HRESULT {
            let sink = unsafe { &*(this as *const Sink) };
            let Ok(mut state) = sink.state.lock() else {
                return ABORT;
            };
            if result == OK && state.prechecked && !new.is_null() {
                let confirmed = unsafe { item_path(new) }.and_then(|path| {
                    let file = OpenOptions::new()
                        .read(true)
                        .share_mode(5)
                        .custom_flags(0x00200000)
                        .open(path)
                        .map_err(|e| e.to_string())?;
                    if identity(HANDLE(file.as_raw_handle()))? != state.id {
                        return Err("recycled_identity_changed".into());
                    }
                    Ok(())
                });
                if let Err(error) = confirmed {
                    state.error = Some(error);
                    return ABORT;
                }
                state.recycled = true;
                OK
            } else {
                state.error = Some(format!("recycle_not_confirmed: {result:?}"));
                ABORT
            }
        }
        #[cfg(test)]
        #[test]
        fn sink_vetoes_permanent_unconfirmed_and_extra_deletions_without_io() {
            let state = Arc::new(Mutex::new(State {
                path: PathBuf::new(),
                id: (0, [0; 16]),
                prechecked: false,
                recycled: false,
                error: None,
            }));
            let raw = Box::into_raw(Box::new(Sink {
                vtable: &VTABLE,
                refs: AtomicU32::new(1),
                state: state.clone(),
            })) as *mut c_void;
            unsafe {
                let mut queried = std::ptr::null_mut();
                assert_eq!(
                    query(raw, &IFileOperationProgressSink::IID, &mut queried),
                    OK
                );
                assert_eq!(release(queried), 1);
                assert_eq!(pre_delete(raw, 0, std::ptr::null_mut()), ABORT);
                assert_eq!(
                    pre_delete(
                        raw,
                        TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32,
                        std::ptr::null_mut()
                    ),
                    ABORT
                );
                assert_eq!(
                    post_delete(raw, 0, std::ptr::null_mut(), OK, std::ptr::null_mut()),
                    ABORT
                );
                state.lock().unwrap().prechecked = true;
                assert_eq!(
                    pre_delete(
                        raw,
                        TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32,
                        std::ptr::null_mut()
                    ),
                    ABORT
                );
                assert!(!state.lock().unwrap().recycled);
                assert_eq!(release(raw), 0);
            }
        }

        static VTABLE: IFileOperationProgressSink_Vtbl = IFileOperationProgressSink_Vtbl {
            base__: IUnknown_Vtbl {
                QueryInterface: query,
                AddRef: add_ref,
                Release: release,
            },
            StartOperations: idle,
            FinishOperations: finish,
            PreRenameItem: pre_rename,
            PostRenameItem: post_rename,
            PreMoveItem: pre_move,
            PostMoveItem: post_move,
            PreCopyItem: pre_copy,
            PostCopyItem: post_copy,
            PreDeleteItem: pre_delete,
            PostDeleteItem: post_delete,
            PreNewItem: pre_new,
            PostNewItem: post_new,
            UpdateProgress: progress,
            ResetTimer: idle,
            PauseTimer: idle,
            ResumeTimer: idle,
        };
        struct Apartment;
        impl Drop for Apartment {
            fn drop(&mut self) {
                unsafe {
                    CoUninitialize();
                }
            }
        }
        pub(super) fn recycle(path: &Path, handle: HANDLE) -> Result<(), String> {
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() }
                .map_err(|e| e.to_string())?;
            let _apartment = Apartment;
            let state = Arc::new(Mutex::new(State {
                path: path.into(),
                id: identity(handle)?,
                prechecked: false,
                recycled: false,
                error: None,
            }));
            let sink = Box::new(Sink {
                vtable: &VTABLE,
                refs: AtomicU32::new(1),
                state: state.clone(),
            });
            let sink = unsafe { IFileOperationProgressSink::from_raw(Box::into_raw(sink) as _) };
            unsafe {
                let operation: IFileOperation =
                    CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)
                        .map_err(|e| e.to_string())?;
                operation
                    .SetOperationFlags(
                        FOFX_RECYCLEONDELETE
                            | FOF_ALLOWUNDO
                            | FOFX_ADDUNDORECORD
                            | FOFX_EARLYFAILURE
                            | FOF_NO_UI
                            | FOF_FILESONLY
                            | FOF_NORECURSION
                            | FOF_NORECURSEREPARSE
                            | FOF_NO_CONNECTED_ELEMENTS,
                    )
                    .map_err(|e| e.to_string())?;
                let item: IShellItem =
                    SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None)
                        .map_err(|e| e.to_string())?;
                operation
                    .DeleteItem(&item, &sink)
                    .map_err(|e| e.to_string())?;
                let performed = operation.PerformOperations();
                let aborted = operation.GetAnyOperationsAborted();
                let state = state.lock().map_err(|_| "recycle_state_poisoned")?;
                if state.recycled {
                    return Ok(());
                }
                if let Some(error) = &state.error {
                    return Err(error.clone());
                }
                performed.map_err(|e| e.to_string())?;
                if aborted.map_err(|e| e.to_string())?.as_bool() {
                    return Err("recycle_aborted".into());
                }
                Err("recycle_not_confirmed".into())
            }
        }
    }
}

#[cfg(all(windows, test))]
pub(crate) use native::fixture_recycle;
#[cfg(windows)]
pub(crate) use native::{default_stream_only, handle_path, recycle_verified};
