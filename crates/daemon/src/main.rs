fn main() {
    #[cfg(target_os = "linux")]
    if std::env::args_os()
        .nth(1)
        .is_some_and(|argument| argument == linux::CAPTURE_WORKER_ARGUMENT)
    {
        linux::run_capture_worker();
    }
    if let Err(message) = daemon::serve_forever() {
        eprintln!("error: {message}");
        std::process::exit(1);
    }
}
