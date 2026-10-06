/// Configure smol's process-wide executor before it is initialized.
///
/// `smol` reads `SMOL_THREADS` only during the first use of its global
/// executor. `available_parallelism` observes the process's CPU affinity and
/// cgroup quota, so examples use the CPU limit assigned to the container
/// instead of the host's CPU count. An explicit environment setting remains
/// authoritative.
pub fn configure() {
    if std::env::var_os("SMOL_THREADS").is_some() {
        return;
    }

    let threads = std::thread::available_parallelism()
        .map(|parallelism| parallelism.get())
        .unwrap_or(1);

    // This runs at process startup, before smol starts any worker threads.
    // `SMOL_THREADS` is intentionally configured once and is never changed
    // after the global executor has been initialized.
    unsafe {
        std::env::set_var("SMOL_THREADS", threads.to_string());
    }
}
