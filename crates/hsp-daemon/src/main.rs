fn main() {
    if let Err(error) = hsp_daemon::serve_default() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
