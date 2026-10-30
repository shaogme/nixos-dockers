use std::process::exit;

fn main() {
    exit(dev_env_provider::ProviderWorker::run());
}
