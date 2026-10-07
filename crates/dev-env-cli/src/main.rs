use std::env;

fn main() {
    let exit_code = match dev_env_cli::run(env::args_os()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("dev-env: {error}");
            error.exit_code()
        }
    };
    std::process::exit(exit_code);
}
