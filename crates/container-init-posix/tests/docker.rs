use container_init_posix::{PosixIdentity, PosixSystem};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

#[test]
fn exercises_the_live_container_account_and_filesystem() {
    assert_eq!(std::env::consts::OS, "linux");
    let system = PosixSystem::new();
    assert_eq!(system.current_ids().0, 0, "Docker fixture must run as root");

    let root = system.lookup_user_by_name("root").unwrap().unwrap();
    assert_eq!((root.uid, root.gid), (0, 0));

    let temp = TempDir::new().unwrap();
    let file = temp.path().join("container-init-posix");
    fs::write(&file, "docker\n").unwrap();
    system.chown(&file, 0, 0, true).unwrap();
    container_init_posix::set_mode(&file, 0o640).unwrap();
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o640
    );

    // Exercise the real container privilege boundary as a subprocess so the
    // test harness remains root for cleanup.
    let identity = PosixIdentity::new(65_534, 65_534, "nobody", "/nonexistent");
    let child = unsafe { libc::fork() };
    assert!(child >= 0, "fork should succeed");
    if child == 0 {
        let result = system.drop_privileges(&identity);
        let success = result.is_ok() && system.current_ids() == (65_534, 65_534);
        unsafe { libc::_exit(if success { 0 } else { 1 }) };
    }
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
    assert!(libc::WIFEXITED(status));
    assert_eq!(libc::WEXITSTATUS(status), 0);
}
