use dev_env_provider::ProviderWorker;
use std::process::exit;

fn main() {
    exit(ProviderWorker::run());
}
