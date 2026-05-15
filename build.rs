use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

const APP_ICON: &str = "logo/fabicon/moyai_app_icon.ico";
const WINDOW_ICON: &str = "logo/fabicon/android-chrome-512x512.png";

fn main() {
    #[cfg(feature = "tauri-desktop")]
    println!("cargo:rerun-if-changed=ui/desktop-web");
    println!("cargo:rerun-if-changed={APP_ICON}");
    println!("cargo:rerun-if-changed={WINDOW_ICON}");
    configure_windows_main_thread_stack();

    #[cfg(feature = "tauri-desktop")]
    tauri_build::build();

    if env::var("CARGO_CFG_WINDOWS").is_ok() && env::var_os("CARGO_FEATURE_TAURI_DESKTOP").is_none()
    {
        embed_windows_app_icon();
    }
}

fn configure_windows_main_thread_stack() {
    if env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }
    let stack_bytes = 16 * 1024 * 1024;
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        for bin in ["moyai", "moyai-desktop"] {
            println!("cargo:rustc-link-arg-bin={bin}=/STACK:{stack_bytes}");
        }
    } else {
        for bin in ["moyai", "moyai-desktop"] {
            println!("cargo:rustc-link-arg-bin={bin}=-Wl,--stack,{stack_bytes}");
        }
    }
}

fn embed_windows_app_icon() {
    let icon_path = env::current_dir()
        .expect("failed to resolve package directory")
        .join(APP_ICON);
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is not set"));
    let rc_path = out_dir.join("moyai_app_icon.rc");
    let resource_path = out_dir.join("moyai_app_icon.res");

    let rc = format!("1 ICON \"{}\"\n", escape_rc_path(&icon_path));
    fs::write(&rc_path, rc).expect("failed to write Windows app icon resource script");

    compile_windows_resource(&rc_path, &resource_path);

    for bin in ["moyai", "moyai-desktop"] {
        println!("cargo:rustc-link-arg-bin={bin}={}", resource_path.display());
    }
}

fn escape_rc_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "\\\\")
}

fn compile_windows_resource(rc_path: &Path, resource_path: &Path) {
    match compile_with_windres(rc_path, resource_path) {
        Ok(()) => return,
        Err(error)
            if error.kind() == io::ErrorKind::NotFound && env::var_os("WINDRES").is_none() => {}
        Err(error) => panic!("failed to run windres for Windows app icon resource: {error}"),
    }

    match compile_with_rc(rc_path, resource_path) {
        Ok(()) => {}
        Err(error) => {
            panic!("failed to compile Windows app icon resource with windres or rc.exe: {error}")
        }
    }
}

fn compile_with_windres(rc_path: &Path, resource_path: &Path) -> io::Result<()> {
    let windres = env::var_os("WINDRES")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("windres"));
    let status = Command::new(windres)
        .arg("--input-format=rc")
        .arg("--output-format=res")
        .arg(rc_path)
        .arg(resource_path)
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "windres exited with status {status}"
        )));
    }
    Ok(())
}

fn compile_with_rc(rc_path: &Path, resource_path: &Path) -> io::Result<()> {
    let rc = env::var_os("RC")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("rc.exe"));
    let status = Command::new(rc)
        .arg("/nologo")
        .arg(format!("/fo{}", resource_path.display()))
        .arg(rc_path)
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "rc.exe exited with status {status}"
        )));
    }
    Ok(())
}
