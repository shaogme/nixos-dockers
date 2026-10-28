use dev_env_cli::Application;
use std::{env, process::exit};

fn main() {
    let exit_code = match Application::run(env::args_os()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("dev-env: {error}");
            error.exit_code()
        }
    };
    exit(exit_code);
}
