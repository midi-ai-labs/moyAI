//! A locally installed Windows sandbox service with process-owned application families.

#[cfg(windows)]
mod client;
#[cfg(windows)]
mod installed;
#[cfg(windows)]
mod native;
#[cfg(windows)]
mod server;
#[cfg(windows)]
pub(crate) mod token;
#[cfg(windows)]
mod wire;

#[cfg(windows)]
pub(crate) use client::execute;
#[cfg(windows)]
pub use client::{LaunchGuard, inherited_endpoint, start};
#[cfg(windows)]
pub(crate) use native::with_impersonation;

#[cfg(not(windows))]
pub struct LaunchGuard;
#[cfg(not(windows))]
pub fn start() -> Result<LaunchGuard, String> {
    Ok(LaunchGuard)
}
#[cfg(not(windows))]
pub fn inherited_endpoint() -> Result<Option<String>, String> {
    Ok(None)
}

#[cfg(windows)]
pub fn helper_main() -> Result<(), String> {
    use clap::Parser;
    #[derive(Parser)]
    struct Args {
        #[arg(long, required = true)]
        service: bool,
    }
    let args = Args::parse();
    if !args.service {
        return Err("The installed sandbox helper requires --service".into());
    }
    server::run_service()
}

#[cfg(windows)]
pub(crate) fn validate_session(value: &str) -> Result<(), String> {
    let id = value
        .parse::<ulid::Ulid>()
        .map_err(|_| "Invalid sandbox launch identity")?;
    if id.to_string() != value {
        return Err("Sandbox launch identity is not canonical".into());
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) const SERVICE_NAME: &str = "MoyAISandbox";

#[cfg(windows)]
fn service_pipe_name() -> &'static str {
    r"\\.\pipe\moyai-sandbox"
}
