use std::process;

fn main() {
    if std::env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new("__container_init_handoff_helper"))
    {
        let exit_code = container_init_cli::run_handoff_helper();
        if exit_code != 0 {
            process::exit(exit_code);
        }
        return;
    }
    let exit_code = match container_init_cli::run(std::env::args_os()) {
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
