fn main() {
    let ballast = vec![1u8; 8 << 20];
    std::hint::black_box(&ballast);
    if let Err(message) = daemon::serve_forever() {
        eprintln!("error: {message}");
        std::process::exit(1);
    }
}
