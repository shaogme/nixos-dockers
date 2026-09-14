use std::process;

fn main() {
    let exit_code = match container_init_cli::run(std::env::args_os()) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("container-init: {error}");
            error.exit_code()
        }
    };
    if exit_code != 0 {
        process::exit(exit_code);
    }
}
