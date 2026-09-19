//! Windows-user protected human credentials. Never projected or put in config.
use super::{LoginSession, RequestError, WorkPrincipal};
use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

const LIMIT: u64 = 96 * 1024;
const ERROR: &str =
    "利用者のログイン情報を安全に保存・確認できません。保存先とWindowsの利用者を確認してください。";

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Remembered {
    pub binding: String,
    pub refresh_token: String,
    pub token: String,
    pub expires_at_ms: u64,
    pub principal: WorkPrincipal,
}
impl Remembered {
    pub fn from_login(binding: String, session: &LoginSession) -> Option<Self> {
        Some(Self {
            binding,
            refresh_token: session.refresh_token.clone()?,
            token: session.token.clone(),
            expires_at_ms: session.expires_at_ms,
            principal: session.principal.clone(),
        })
    }
    fn valid(&self) -> bool {
        let token = |v: &str| {
            v.len() == 64
                && v.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        !self.binding.is_empty()
            && self.binding.len() <= 2048
            && token(&self.token)
            && token(&self.refresh_token)
            && super::super::stable_id(&self.principal.user_id)
            && self.principal.display_name.len() <= 256
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Document {
    version: u32,
    pub revision: u64,
    pub active: Option<Remembered>,
    pub pending_logouts: Vec<Remembered>,
}
#[derive(Default)]
pub(super) struct AuthStore {
    path: Utf8PathBuf,
}
impl AuthStore {
    pub fn new(path: Utf8PathBuf) -> Self {
        Self { path }
    }
    pub fn supported() -> bool {
        cfg!(windows)
    }
    pub fn load(&self) -> Result<Document, RequestError> {
        if !Self::supported() || self.path.as_str().is_empty() {
            return Ok(Document {
                version: 1,
                ..Default::default()
            });
        }
        let file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Document {
                    version: 1,
                    ..Default::default()
                });
            }
            Err(_) => return Err(RequestError::Local(ERROR)),
        };
        let mut bytes = Vec::new();
        file.take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| RequestError::Local(ERROR))?;
        if bytes.len() as u64 > LIMIT {
            return Err(RequestError::Local(ERROR));
        }
        let mut clear = unprotect(&bytes)?;
        let parsed = serde_json::from_slice::<Document>(&clear);
        clear.fill(0);
        let doc = parsed.map_err(|_| RequestError::Local(ERROR))?;
        if doc.version != 1
            || doc.pending_logouts.len() > 32
            || doc.active.as_ref().is_some_and(|a| !a.valid())
            || doc.pending_logouts.iter().any(|a| !a.valid())
        {
            return Err(RequestError::Local(ERROR));
        }
        Ok(doc)
    }
    fn change<T>(
        &self,
        operation: impl FnOnce(&mut Document) -> Result<T, RequestError>,
    ) -> Result<T, RequestError> {
        let parent = self
            .path
            .parent()
            .filter(|p| !p.as_str().is_empty())
            .ok_or(RequestError::Local(ERROR))?;
        std::fs::create_dir_all(parent).map_err(|_| RequestError::Local(ERROR))?;
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.path.with_extension("lock"))
            .map_err(|_| RequestError::Local(ERROR))?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|_| RequestError::Local(ERROR))?;
        let mut doc = self.load()?;
        let result = operation(&mut doc)?;
        doc.revision = doc
            .revision
            .checked_add(1)
            .ok_or(RequestError::Local(ERROR))?;
        let mut clear = serde_json::to_vec(&doc).map_err(|_| RequestError::Local(ERROR))?;
        let encrypted = protect(&clear);
        clear.fill(0);
        let encrypted = encrypted?;
        if encrypted.len() as u64 > LIMIT {
            return Err(RequestError::Local(ERROR));
        }
        let mut temporary =
            tempfile::NamedTempFile::new_in(parent).map_err(|_| RequestError::Local(ERROR))?;
        temporary
            .write_all(&encrypted)
            .and_then(|_| temporary.as_file().sync_all())
            .map_err(|_| RequestError::Local(ERROR))?;
        temporary
            .persist(&self.path)
            .map_err(|_| RequestError::Local(ERROR))?;
        Ok(result)
    }
    pub fn login(&self, expected_revision: u64, login: Remembered) -> Result<(), RequestError> {
        if !login.valid() {
            return Err(RequestError::Invalid);
        }
        self.change(|doc| {
            if doc.revision != expected_revision || doc.pending_logouts.len() >= 31 {
                return Err(RequestError::Local(
                    "ログイン状態が変わりました。もう一度確認してください。",
                ));
            }
            if let Some(old) = doc.active.take() {
                doc.pending_logouts.push(old);
            }
            doc.active = Some(login);
            Ok(())
        })
    }
    pub fn update(
        &self,
        previous: &Remembered,
        session: &LoginSession,
    ) -> Result<(), RequestError> {
        self.change(|doc| {
            let active = doc
                .active
                .as_mut()
                .filter(|a| {
                    a.binding == previous.binding && a.refresh_token == previous.refresh_token
                })
                .ok_or(RequestError::Local(
                    "別の画面でログアウトまたは利用者の切替が行われました。",
                ))?;
            if active.principal.user_id != session.principal.user_id {
                return Err(RequestError::Invalid);
            }
            active.token = session.token.clone();
            active.expires_at_ms = session.expires_at_ms;
            active.principal = session.principal.clone();
            Ok(())
        })
    }
    pub fn logout(&self) -> Result<(), RequestError> {
        if !Self::supported() || self.path.as_str().is_empty() {
            return Ok(());
        }
        self.change(|doc| {
            if let Some(active) = doc.active.take() {
                if doc.pending_logouts.len() >= 32 {
                    return Err(RequestError::Local(ERROR));
                }
                doc.pending_logouts.push(active);
            }
            Ok(())
        })
    }
    pub fn acknowledge_logout(&self, old: &Remembered) -> Result<(), RequestError> {
        self.change(|doc| {
            doc.pending_logouts
                .retain(|r| r.binding != old.binding || r.refresh_token != old.refresh_token);
            Ok(())
        })
    }
    pub fn forget(&self, old: &Remembered) -> Result<(), RequestError> {
        self.change(|doc| {
            if doc
                .active
                .as_ref()
                .is_some_and(|r| r.binding == old.binding && r.refresh_token == old.refresh_token)
            {
                doc.active = None;
            }
            Ok(())
        })
    }
}

#[cfg(windows)]
fn crypt(bytes: &[u8], decrypt: bool) -> Result<Vec<u8>, RequestError> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };
    // Current-user DPAPI only: never CRYPTPROTECT_LOCAL_MACHINE. Optional entropy
    // separates this format from other application-protected secrets.
    let domain = b"moyAI remembered human login v1";
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes
            .len()
            .try_into()
            .map_err(|_| RequestError::Local(ERROR))?,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: domain.len() as u32,
        pbData: domain.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: borrowed input buffers remain alive; DPAPI owns output until LocalFree.
    unsafe {
        let result = if decrypt {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                &entropy,
                std::ptr::null_mut(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptProtectData(
                &input,
                std::ptr::null(),
                &entropy,
                std::ptr::null_mut(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if result == 0 {
            return Err(RequestError::Local(ERROR));
        }
        let copied = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        if decrypt {
            std::slice::from_raw_parts_mut(output.pbData, output.cbData as usize).fill(0);
        }
        LocalFree(output.pbData.cast());
        Ok(copied)
    }
}
#[cfg(windows)]
fn protect(bytes: &[u8]) -> Result<Vec<u8>, RequestError> {
    crypt(bytes, false)
}
#[cfg(windows)]
fn unprotect(bytes: &[u8]) -> Result<Vec<u8>, RequestError> {
    crypt(bytes, true)
}
#[cfg(not(windows))]
fn protect(_: &[u8]) -> Result<Vec<u8>, RequestError> {
    Err(RequestError::Local(ERROR))
}
#[cfg(not(windows))]
fn unprotect(_: &[u8]) -> Result<Vec<u8>, RequestError> {
    Err(RequestError::Local(ERROR))
}

#[cfg(all(test, windows))]
mod tests;
