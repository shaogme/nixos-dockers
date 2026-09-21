use crate::error::PosixError;
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdMapEntry {
    pub container_start: u64,
    pub parent_start: u64,
    pub length: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamespaceMap {
    entries: Vec<IdMapEntry>,
}

impl NamespaceMap {
    pub fn parse(contents: &str) -> Result<Self, PosixError> {
        let mut entries = Vec::new();
        for (line_number, line) in contents.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() != 3 {
                return Err(PosixError::namespace(format!(
                    "namespace map line {} must contain exactly three decimal fields",
                    line_number + 1
                )));
            }
            let container_start = parse_u64(fields[0], line_number)?;
            let parent_start = parse_u64(fields[1], line_number)?;
            let length = parse_u64(fields[2], line_number)?;
            if length == 0
                || container_start.checked_add(length).is_none()
                || parent_start.checked_add(length).is_none()
            {
                return Err(PosixError::namespace(format!(
                    "namespace map line {} contains an overflowing or empty interval",
                    line_number + 1
                )));
            }
            entries.push(IdMapEntry {
                container_start,
                parent_start,
                length,
            });
        }
        if entries.is_empty() {
            return Err(PosixError::namespace(
                "namespace map does not contain an interval".to_owned(),
            ));
        }
        Ok(Self { entries })
    }

    pub fn entries(&self) -> &[IdMapEntry] {
        &self.entries
    }

    pub fn map_parent_id(&self, parent_id: u32) -> Result<u32, PosixError> {
        let parent_id = u64::from(parent_id);
        let entry = self.entries.iter().find(|entry| {
            entry.parent_start <= parent_id
                && parent_id < entry.parent_start.saturating_add(entry.length)
        });
        let Some(entry) = entry else {
            return Err(PosixError::namespace(format!(
                "parent ID {parent_id} is not covered by the namespace map"
            )));
        };
        let mapped = entry
            .container_start
            .checked_add(parent_id - entry.parent_start)
            .ok_or_else(|| PosixError::namespace("mapped namespace ID overflows u64".to_owned()))?;
        u32::try_from(mapped).map_err(|_| {
            PosixError::namespace(format!("mapped namespace ID {mapped} is outside u32 range"))
        })
    }
}

fn parse_u64(value: &str, line_number: usize) -> Result<u64, PosixError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(PosixError::namespace(format!(
            "namespace map line {} contains a non-decimal field",
            line_number + 1
        )));
    }
    value.parse::<u64>().map_err(|_| {
        PosixError::namespace(format!(
            "namespace map line {} contains a non-decimal field",
            line_number + 1
        ))
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MountInfoEntry {
    pub mount_id: u64,
    pub mount_point: PathBuf,
}

pub fn parse_mountinfo(contents: &str) -> Result<Vec<MountInfoEntry>, PosixError> {
    let mut entries = Vec::new();
    for (line_number, line) in contents.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let (mount_fields, filesystem_fields) = line.split_once(" - ").ok_or_else(|| {
            PosixError::namespace(format!(
                "mountinfo line {} has no filesystem separator",
                line_number + 1
            ))
        })?;
        let fields = mount_fields.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 6 || filesystem_fields.split_whitespace().count() < 1 {
            return Err(PosixError::namespace(format!(
                "mountinfo line {} is malformed",
                line_number + 1
            )));
        }
        let mount_id = fields[0].parse::<u64>().map_err(|_| {
            PosixError::namespace(format!(
                "mountinfo line {} contains an invalid mount ID",
                line_number + 1
            ))
        })?;
        let mount_point = decode_mountinfo_path(fields[4]).map_err(|message| {
            PosixError::namespace(format!("mountinfo line {}: {message}", line_number + 1))
        })?;
        if !mount_point.is_absolute() {
            return Err(PosixError::namespace(format!(
                "mountinfo line {} has a non-absolute mount point",
                line_number + 1
            )));
        }
        entries.push(MountInfoEntry {
            mount_id,
            mount_point,
        });
    }
    Ok(entries)
}

fn decode_mountinfo_path(value: &str) -> Result<PathBuf, String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 3 >= bytes.len()
            || !bytes[index + 1].is_ascii_digit()
            || !bytes[index + 2].is_ascii_digit()
            || !bytes[index + 3].is_ascii_digit()
        {
            return Err("invalid octal escape".to_owned());
        }
        let octal = &value[index + 1..index + 4];
        let byte = u8::from_str_radix(octal, 8).map_err(|_| "invalid octal escape".to_owned())?;
        decoded.push(byte);
        index += 4;
    }
    Ok(PathBuf::from(std::ffi::OsString::from_vec(decoded)))
}

pub fn normalize_workspace_path(path: &Path) -> Result<PathBuf, PosixError> {
    if !path.is_absolute() {
        return Err(PosixError::namespace(
            "workspace path must be absolute".to_owned(),
        ));
    }
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::CurDir => {}
            Component::Normal(value) => normalized.push(value),
            Component::ParentDir => {
                return Err(PosixError::namespace(
                    "workspace path may not contain '..'".to_owned(),
                ))
            }
            Component::Prefix(_) => {
                return Err(PosixError::namespace(
                    "workspace path has an unsupported prefix".to_owned(),
                ))
            }
        }
    }
    Ok(normalized)
}

pub fn reject_symlink_components(path: &Path) -> Result<(), PosixError> {
    let mut current = PathBuf::from("/");
    for component in path.components() {
        let Component::Normal(value) = component else {
            continue;
        };
        current.push(value);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(PosixError::namespace(format!(
                    "workspace path contains symlink component {}",
                    current.display()
                )))
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(PosixError::namespace(format!(
                    "cannot inspect workspace path component {}: {error}",
                    current.display()
                )))
            }
        }
    }
    Ok(())
}

pub fn read_namespace_map(path: &Path) -> Result<NamespaceMap, PosixError> {
    let contents = fs::read_to_string(path).map_err(|source| {
        PosixError::namespace(format!("cannot read {}: {source}", path.display()))
    })?;
    NamespaceMap::parse(&contents)
}

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::ffi::OsStringExt;

pub fn unshare_user_and_mount_namespaces(uid: u32, gid: u32) -> Result<(), PosixError> {
    let res = unsafe { libc::unshare(libc::CLONE_NEWUSER) };
    if res != 0 {
        return Err(PosixError::io(std::io::Error::last_os_error()));
    }

    let _ = fs::write("/proc/self/setgroups", "deny");
    fs::write("/proc/self/uid_map", format!("0 {uid} 1\n")).map_err(PosixError::io)?;
    fs::write("/proc/self/gid_map", format!("0 {gid} 1\n")).map_err(PosixError::io)?;

    let res = unsafe { libc::unshare(libc::CLONE_NEWNS | libc::CLONE_NEWCGROUP) };
    if res != 0 {
        return Err(PosixError::io(std::io::Error::last_os_error()));
    }

    let slash = CString::new("/").map_err(|_| PosixError::invalid("invalid cstring"))?;
    let res = unsafe {
        libc::mount(
            std::ptr::null(),
            slash.as_ptr(),
            std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        )
    };
    if res != 0 {
        return Err(PosixError::io(std::io::Error::last_os_error()));
    }
    Ok(())
}

pub fn mount_cgroup2(target: &Path) -> Result<(), PosixError> {
    if !target.exists() {
        let _ = fs::create_dir_all(target);
    }
    let cgroup2 = CString::new("cgroup2").map_err(|_| PosixError::invalid("invalid cstring"))?;
    let target_cstr = CString::new(target.as_os_str().as_bytes())
        .map_err(|_| PosixError::invalid("invalid target path"))?;
    let res = unsafe {
        libc::mount(
            cgroup2.as_ptr(),
            target_cstr.as_ptr(),
            cgroup2.as_ptr(),
            0,
            std::ptr::null(),
        )
    };
    if res != 0 {
        return Err(PosixError::io(std::io::Error::last_os_error()));
    }
    Ok(())
}

pub fn bind_mount(source: &Path, target: &Path) -> Result<(), PosixError> {
    if !target.exists() {
        let _ = fs::create_dir_all(target);
    }
    let source_cstr = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| PosixError::invalid("invalid source path"))?;
    let target_cstr = CString::new(target.as_os_str().as_bytes())
        .map_err(|_| PosixError::invalid("invalid target path"))?;
    let res = unsafe {
        libc::mount(
            source_cstr.as_ptr(),
            target_cstr.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND,
            std::ptr::null(),
        )
    };
    if res != 0 {
        return Err(PosixError::io(std::io::Error::last_os_error()));
    }
    Ok(())
}
