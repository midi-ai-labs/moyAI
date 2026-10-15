use std::collections::HashMap;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::tool::executable::WindowsExecutableIdentity;
use crate::tool::os_sandbox::WorkspaceWriteSandboxProfile;
use crate::tool::sandbox_process::{
    SandboxExecutionError, SandboxedProcessOutput, SandboxedProcessRequest,
};

pub(super) const VERSION: u32 = 1;
pub(super) const MAX_FRAME: usize = 64 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Run {
    pub profile: WorkspaceWriteSandboxProfile,
    pub executable: Utf8PathBuf,
    pub executable_identity: WindowsExecutableIdentity,
    pub argv: Vec<String>,
    pub cwd: Utf8PathBuf,
    pub environment: HashMap<String, String>,
    pub stdin: Vec<u8>,
    pub timeout_ms: Option<u64>,
    pub max_output_bytes: usize,
    pub hide_window: bool,
}

impl Run {
    pub fn from_request(
        profile: WorkspaceWriteSandboxProfile,
        request: &SandboxedProcessRequest,
    ) -> Self {
        Self {
            profile,
            executable: request.executable.path().to_owned(),
            executable_identity: request.executable.windows_identity().clone(),
            argv: request.argv.clone(),
            cwd: request.cwd.clone(),
            environment: request.environment.clone(),
            stdin: request.stdin.clone(),
            timeout_ms: request.timeout_ms,
            max_output_bytes: request.max_output_bytes,
            hide_window: request.hide_window,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.argv.is_empty()
            || self.argv[0] != self.executable.as_str()
            || !self.executable.is_absolute()
            || !self.cwd.is_absolute()
        {
            return Err(
                "Sandbox executable and working directory must be explicit absolute paths".into(),
            );
        }
        if self.argv.len() > 4096
            || self.environment.len() > 4096
            || self.profile.writable_roots.len() >= 128
            || self.profile.read_only_roots.len() > 4096
        {
            return Err("Sandbox request exceeds its entry limit".into());
        }
        if self.stdin.len() > MAX_FRAME / 8 || self.max_output_bytes > MAX_FRAME / 16 {
            return Err("Sandbox IPC supports at most 8 MiB of stdin and 4 MiB of captured output per stream".into());
        }
        if self.profile.effect_temp_directory.is_some() {
            return Err("Effect TEMP must be created by the sandbox execution owner".into());
        }
        if self.argv.iter().any(|value| value.contains('\0'))
            || self.cwd.as_str().contains('\0')
            || self.environment.iter().any(|(key, value)| {
                key.is_empty() || key.contains(['\0', '=']) || value.contains('\0')
            })
        {
            return Err(
                "Sandbox process arguments or environment contain invalid Windows values".into(),
            );
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub(super) enum Request {
    Hello { version: u32, session: String },
    Run { request: Box<Run> },
    Cancel {},
    Attach {},
    Detach {},
    Shutdown {},
    Acknowledge {},
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "event", deny_unknown_fields)]
pub(super) enum Response {
    Ready {
        version: u32,
        session: String,
        parent_pid: u32,
        parent_created: u64,
    },
    Started {
        pid: u32,
    },
    Finished {
        result: Result<SandboxedProcessOutput, SandboxExecutionError>,
    },
    Error {
        message: String,
    },
    Stopping,
    Attached,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::os_sandbox::{
        SandboxNetworkPolicy, SandboxPathSnapshot, WindowsSandboxObjectIdentity,
    };

    fn request() -> Run {
        Run {
            profile: WorkspaceWriteSandboxProfile {
                writable_roots: Vec::new(),
                read_only_roots: Vec::new(),
                effect_temp_base: None,
                effect_temp_directory: None,
                network: SandboxNetworkPolicy::AdvisoryOfflineEnvironment,
            },
            executable: r"C:\Windows\System32\cmd.exe".into(),
            executable_identity: WindowsExecutableIdentity::Legacy {
                volume_serial_number: 1,
                file_index: 2,
            },
            argv: vec![
                r"C:\Windows\System32\cmd.exe".into(),
                "/c".into(),
                "exit 0".into(),
            ],
            cwd: r"C:\work".into(),
            environment: HashMap::new(),
            stdin: Vec::new(),
            timeout_ms: None,
            max_output_bytes: 16384,
            hide_window: true,
        }
    }

    #[test]
    fn rejects_ambiguous_windows_launch_values_before_effect() {
        for (key, value) in [
            ("", "value"),
            ("KEY=extra", "value"),
            ("KEY", "value\0suffix"),
        ] {
            let mut run = request();
            run.environment.insert(key.into(), value.into());
            assert!(run.validate().is_err());
        }
        let mut run = request();
        run.argv[1].push('\0');
        assert!(run.validate().is_err());
        let mut run = request();
        run.argv[0] = "different.exe".into();
        assert!(run.validate().is_err());
        assert!(request().validate().is_ok());
    }

    #[test]
    fn caller_cannot_select_another_effects_temp_directory() {
        let mut run = request();
        run.profile.effect_temp_directory = Some(SandboxPathSnapshot {
            requested: r"C:\temp\other".into(),
            canonical: r"C:\temp\other".into(),
            content_sha256: None,
            identity: WindowsSandboxObjectIdentity::Legacy {
                volume_serial_number: 1,
                file_index: 3,
            },
        });
        assert!(run.validate().is_err());
    }

    #[test]
    fn launch_messages_reject_added_authority_and_unknown_operations() {
        assert!(
            serde_json::from_str::<Request>(r#"{"operation":"Shutdown","user":"S-1-5-18"}"#)
                .is_err()
        );
        assert!(serde_json::from_str::<Request>(r#"{"operation":"ExecuteAsSystem"}"#).is_err());
        let mut value = serde_json::to_value(request()).unwrap();
        value["token"] = serde_json::json!(1234);
        assert!(serde_json::from_value::<Run>(value).is_err());
    }
}
