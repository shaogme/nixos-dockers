use container_init_cli::{run, run_handoff_helper};
use std::{env::args_os, ffi::OsStr, process};

fn main() {
    if args_os().nth(1).as_deref() == Some(OsStr::new("__container_init_handoff_helper")) {
        let exit_code = run_handoff_helper();
        if exit_code != 0 {
            process::exit(exit_code);
        }
        return;
    }
    let exit_code = match run(args_os()) {
        Ok(exit_code) => exit_code,
        Err(error) => {
            eprintln!("container-init: {error}");
            error.exit_code()
        }
    };
    if exit_code != 0 {
        process::exit(exit_code);
    }
}
