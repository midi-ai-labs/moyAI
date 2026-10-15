//! A Windows sandbox helper whose authority is granted once per application launch.

#[cfg(windows)]
mod bootstrap;
#[cfg(windows)]
mod client;
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
        #[arg(long, conflicts_with = "service", required_unless_present = "service")]
        elevated_bootstrap: bool,
        #[arg(long)]
        service: bool,
        #[arg(long)]
        session: String,
        #[arg(long)]
        parent_pid: u32,
        #[arg(long)]
        parent_created: u64,
        #[arg(long)]
        user: String,
    }
    let args = Args::parse();
    validate_session(&args.session)?;
    if args.elevated_bootstrap {
        bootstrap::run_bootstrap(
            &args.session,
            args.parent_pid,
            args.parent_created,
            &args.user,
        )
    } else {
        server::run_service(
            args.session,
            args.parent_pid,
            args.parent_created,
            args.user,
        )
    }
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
fn pipe_name(session: &str) -> String {
    format!(r"\\.\pipe\moyai-sandbox-{session}")
}
