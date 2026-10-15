#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    if let Err(error) = moyai::sandbox_broker::helper_main() {
        eprintln!("{error}");
        std::process::exit(1);
    }
    #[cfg(not(windows))]
    {
        eprintln!("The moyAI sandbox helper requires Windows.");
        std::process::exit(1);
    }
}
