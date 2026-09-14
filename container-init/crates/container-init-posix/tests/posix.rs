use container_init_posix::{
    is_writable, mode, parse_mountinfo, NamespaceMap, PosixIdentity, PosixLock, PosixSystem,
};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use tempfile::TempDir;

#[test]
fn parses_only_valid_octal_modes() {
    assert_eq!(mode("0755"), Ok(0o755));
    assert_eq!(mode("1777"), Ok(0o1777));
    assert!(mode("").is_err());
    assert!(mode("888").is_err());
    assert!(mode("10000").is_err());
}

#[test]
fn namespace_maps_use_parent_to_container_translation_and_fail_closed() {
    let map = NamespaceMap::parse("\n0 1000 1\n1 2000 3\n").unwrap();
    assert_eq!(map.map_parent_id(1000).unwrap(), 0);
    assert_eq!(map.map_parent_id(2000).unwrap(), 1);
    assert_eq!(map.map_parent_id(2002).unwrap(), 3);
    assert!(map.map_parent_id(2003).is_err());
    assert!(NamespaceMap::parse("0 0 nope").is_err());
    assert!(NamespaceMap::parse("18446744073709551615 0 2").is_err());
}

#[test]
fn mountinfo_parser_decodes_linux_escapes() {
    let mounts =
        parse_mountinfo("42 1 0:1 / /workspace\\040project rw - bind /source\\040dir rw\n")
            .unwrap();
    assert_eq!(mounts[0].mount_id, 42);
    assert_eq!(mounts[0].mount_point, Path::new("/workspace project"));
}

#[test]
fn isolated_account_database_is_read_and_reconciled_atomically() {
    let temp = TempDir::new().unwrap();
    let passwd = temp.path().join("passwd");
    let group = temp.path().join("group");
    fs::write(&passwd, "fixture:x:2000:2000::/home/fixture:/bin/sh\n").unwrap();
    fs::write(&group, "\n").unwrap();
    let system = PosixSystem::with_account_files(&passwd, &group);

    let user = system.lookup_user_by_name("fixture").unwrap().unwrap();
    assert_eq!(user.uid, 2000);
    assert_eq!(user.gid, 2000);
    assert_eq!(user.home, Path::new("/home/fixture"));
    assert_eq!(system.lookup_user_by_uid(2000).unwrap(), Some(user));

    let identity = PosixIdentity::new(2100, 2101, "fixture", "/home/fixture");
    let first = system.map_user(&identity).unwrap();
    assert_eq!(first.message(), "mapped POSIX passwd entry");
    let second = system.map_user(&identity).unwrap();
    assert_eq!(second.message(), "POSIX passwd entry already mapped");
    system
        .set_user_shell("fixture", Path::new("/usr/bin/dev-env-login-shell"))
        .unwrap();

    assert!(fs::read_to_string(&passwd)
        .unwrap()
        .contains("fixture:x:2100:2101::/home/fixture:/usr/bin/dev-env-login-shell"));
    assert!(fs::read_to_string(&group)
        .unwrap()
        .contains("fixture:x:2101:"));
}

#[test]
fn existing_groups_are_reconciled_for_every_matching_gid_without_duplicates() {
    let temp = TempDir::new().unwrap();
    let passwd = temp.path().join("passwd");
    let group = temp.path().join("group");
    fs::write(
        &passwd,
        "dev:x:2000:100::/home/dev:/bin/sh\nother:x:2001:2001::/home/other:/bin/sh\n",
    )
    .unwrap();
    fs::write(
        &group,
        "users:x:100:alice,dev,dev\nshared:x:100:bob\nother:x:2001:\n",
    )
    .unwrap();
    let system = PosixSystem::with_account_files(&passwd, &group);
    let identity = PosixIdentity::new(2000, 100, "dev", "/home/dev");

    system.map_user(&identity).unwrap();
    let contents = fs::read_to_string(&group).unwrap();
    assert!(contents.contains("users:x:100:alice,dev\n"));
    assert!(contents.contains("shared:x:100:bob,dev\n"));
    assert_eq!(contents.matches("users:x:100:alice,dev").count(), 1);
    system.map_user(&identity).unwrap();
    assert_eq!(fs::read_to_string(&group).unwrap(), contents);
}

#[test]
fn malformed_account_entries_are_not_overwritten() {
    let temp = TempDir::new().unwrap();
    let passwd = temp.path().join("passwd");
    let group = temp.path().join("group");
    fs::write(&passwd, "dev:x:2000:100::/home/dev\n").unwrap();
    fs::write(&group, "users:x:100:\n").unwrap();
    let system = PosixSystem::with_account_files(&passwd, &group);
    let identity = PosixIdentity::new(2000, 100, "dev", "/home/dev");
    assert!(system.map_user(&identity).is_err());
    assert_eq!(
        fs::read_to_string(&passwd).unwrap(),
        "dev:x:2000:100::/home/dev\n"
    );
}

#[test]
fn ownership_modes_and_locking_use_real_posix_primitives() {
    let temp = TempDir::new().unwrap();
    let file = temp.path().join("state");
    fs::write(&file, "state\n").unwrap();
    let system = PosixSystem::new();
    let (uid, gid) = system.current_ids();

    system.chown(&file, uid, gid, true).unwrap();
    container_init_posix::set_mode(&file, 0o640).unwrap();
    assert_eq!(fs::metadata(&file).unwrap().mode() & 0o777, 0o640);
    assert_eq!(fs::metadata(&file).unwrap().uid(), uid);
    assert_eq!(fs::metadata(&file).unwrap().gid(), gid);

    let lock_path = temp.path().join("bootstrap.lock");
    let lock = PosixLock::acquire(&lock_path).unwrap();
    let error = PosixLock::acquire(&lock_path).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    drop(lock);
    let _lock = PosixLock::acquire(&lock_path).unwrap();

    assert!(is_writable(temp.path()));
    assert!(is_writable(&temp.path().join("not-created-yet")));
}

#[test]
fn dropping_privileges_changes_uid_gid_and_is_irreversible() {
    if PosixSystem::new().current_ids().0 != 0 {
        return;
    }

    // The child is essential: a successful setuid cannot be undone by this
    // test process, and the Rust test harness still needs root to clean up.
    let child = unsafe { libc::fork() };
    assert!(child >= 0, "fork should succeed");
    if child == 0 {
        let system = PosixSystem::new();
        let identity = PosixIdentity::new(65_534, 65_534, "nobody", "/nonexistent");
        let result = system.drop_privileges(&identity);
        let success = result.is_ok() && system.current_ids() == (65_534, 65_534);
        unsafe { libc::_exit(if success { 0 } else { 1 }) };
    }

    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
    assert!(libc::WIFEXITED(status));
    assert_eq!(libc::WEXITSTATUS(status), 0);
}
