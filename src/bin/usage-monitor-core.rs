fn main() {
    #[cfg(windows)]
    if usage_monitor::updater::dispatch_worker() { return; }
    if let Err(error) = usage_monitor::bridge::run() {
        eprintln!("Usage Monitor core: {error}");
        std::process::exit(1);
    }
}
