use super::auth_store::{AuthStore, Remembered};
use super::*;
#[cfg(all(test, windows))]
mod tests;

impl DeviceNetworkService {
    pub(super) async fn shared_login(
        &self,
        connection: &Connection,
        generation: u64,
        query: u64,
        username: String,
        password: String,
    ) -> Result<(), RequestError> {
        if username.trim().is_empty() || password.is_empty() || password.len() > 1024 {
            return Err(RequestError::Local(
                "利用者名とパスワードを入力してください。",
            ));
        }
        self.flush_human_logouts(connection).await;
        let revision = self
            .inner
            .shared_work
            .0
            .lock()
            .unwrap()
            .auth_store
            .load()?
            .revision;
        let session: LoginSession = request(
            &connection.client,
            "login",
            None,
            Some(json!({"username":username,"password":password})),
            &[],
        )
        .await?;
        let install = if self.shared_current(connection, generation, query) {
            let mut runtime = self.inner.shared_work.0.lock().unwrap();
            runtime.require_current(generation, query).and_then(|()| {
                if AuthStore::supported() {
                    if let Some(remembered) =
                        Remembered::from_login(connection.auth_binding(), &session)
                    {
                        runtime.auth_store.login(revision, remembered)?;
                    } else {
                        // Old Hub compatibility: no insecure plaintext fallback.
                        runtime.auth_store.logout()?;
                    }
                }
                runtime.clear();
                runtime.binding = connection.binding.clone();
                runtime.hub_binding = connection.hub_binding.clone();
                runtime.token = Some(session.token.clone());
                runtime.remembered_refresh = AuthStore::supported()
                    .then(|| session.refresh_token.clone())
                    .flatten();
                runtime.view.principal = Some(session.principal.clone());
                runtime.view.expires_at_ms = Some(session.expires_at_ms);
                Ok(())
            })
        } else {
            Err(RequestError::Local("利用者または接続先が変わりました。"))
        };
        if install.is_err() {
            let _ = request::<Value>(
                &connection.client,
                "logout",
                Some(&session.token),
                Some(json!({"refresh_token":session.refresh_token})),
                &[],
            )
            .await;
        }
        install
    }

    pub(super) async fn shared_logout(&self, expected_generation: &str) -> SharedWorkProjection {
        let connection = self.shared_connection();
        let token = {
            let mut runtime = self.inner.shared_work.0.lock().unwrap();
            if expected_generation != runtime.generation.to_string() {
                drop(runtime);
                return self.shared_work_projection();
            }
            // Persist the no-restore state before asynchronous network revocation.
            if let Err(error) = runtime.auth_store.logout() {
                runtime.view.error = Some(error.message().into());
                drop(runtime);
                return self.shared_work_projection();
            }
            let token = runtime.token.clone();
            runtime.clear();
            token
        };
        if let Some(connection) = connection {
            self.flush_human_logouts(&connection).await;
            if let Some(token) = token {
                let _ = request::<Value>(
                    &connection.client,
                    "logout",
                    Some(&token),
                    Some(json!({})),
                    &[],
                )
                .await;
            }
        }
        self.shared_work_projection()
    }

    async fn flush_human_logouts(&self, connection: &Connection) {
        // One bounded revocation per refresh. Other-Hub records wait for that Hub.
        let old = self
            .inner
            .shared_work
            .0
            .lock()
            .unwrap()
            .auth_store
            .load()
            .ok()
            .and_then(|doc| {
                doc.pending_logouts
                    .into_iter()
                    .find(|r| r.binding == connection.auth_binding())
            });
        if let Some(old) = old {
            let result = request::<Value>(
                &connection.client,
                "logout",
                None,
                Some(json!({"refresh_token":old.refresh_token})),
                &[],
            )
            .await;
            if result.is_ok() || matches!(result, Err(RequestError::Http(401 | 403))) {
                let _ = self
                    .inner
                    .shared_work
                    .0
                    .lock()
                    .unwrap()
                    .auth_store
                    .acknowledge_logout(&old);
            }
        }
    }

    pub(super) async fn shared_authenticate(
        &self,
        connection: &Connection,
        generation: u64,
        query: u64,
        current_token: Option<&str>,
    ) -> Result<String, RequestError> {
        self.flush_human_logouts(connection).await;
        let (remembered, current_expiry, current_user) = {
            let runtime = self.inner.shared_work.0.lock().unwrap();
            runtime.require_current(generation, query)?;
            let doc = runtime.auth_store.load()?;
            let remembered = doc
                .active
                .filter(|r| r.binding == connection.auth_binding());
            if runtime.remembered_refresh.as_ref().is_some_and(|refresh| {
                remembered
                    .as_ref()
                    .is_none_or(|r| &r.refresh_token != refresh)
            }) {
                return Err(RequestError::Http(401));
            }
            (
                remembered,
                runtime.view.expires_at_ms,
                runtime.view.principal.as_ref().map(|p| p.user_id.clone()),
            )
        };
        if let Some(token) = current_token
            .filter(|_| current_expiry.is_some_and(|v| v > now_ms().saturating_add(60_000)))
        {
            return Ok(token.to_owned());
        }
        let Some(remembered) = remembered else {
            // An older Hub or non-Windows client can still use its in-memory session.
            return current_token
                .map(str::to_owned)
                .ok_or(RequestError::SignInRequired);
        };
        if current_user
            .as_ref()
            .is_some_and(|id| id != &remembered.principal.user_id)
        {
            return Err(RequestError::Http(401));
        }
        let session = if remembered.expires_at_ms > now_ms().saturating_add(60_000) {
            match request::<Session>(
                &connection.client,
                "session",
                Some(&remembered.token),
                None,
                &[],
            )
            .await
            {
                Ok(value) => Ok(LoginSession {
                    token: remembered.token.clone(),
                    principal: value.principal,
                    expires_at_ms: value.expires_at_ms,
                    refresh_token: None,
                }),
                Err(RequestError::Http(401)) => {
                    request(
                        &connection.client,
                        "refresh",
                        None,
                        Some(json!({"refresh_token":remembered.refresh_token})),
                        &[],
                    )
                    .await
                }
                Err(error) => Err(error),
            }
        } else {
            request(
                &connection.client,
                "refresh",
                None,
                Some(json!({"refresh_token":remembered.refresh_token})),
                &[],
            )
            .await
        };
        if !self.shared_current(connection, generation, query) {
            return Err(RequestError::Local("利用者または表示対象が変わりました。"));
        }
        let session = match session {
            Ok(session) => session,
            Err(error @ RequestError::Http(401 | 403)) => {
                self.inner
                    .shared_work
                    .0
                    .lock()
                    .unwrap()
                    .auth_store
                    .forget(&remembered)?;
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        let mut runtime = self.inner.shared_work.0.lock().unwrap();
        runtime.require_current(generation, query)?;
        runtime.auth_store.update(&remembered, &session)?;
        runtime.binding = connection.binding.clone();
        runtime.hub_binding = connection.hub_binding.clone();
        runtime.token = Some(session.token.clone());
        runtime.remembered_refresh = Some(remembered.refresh_token);
        runtime.view.principal = Some(session.principal);
        runtime.view.expires_at_ms = Some(session.expires_at_ms);
        Ok(session.token)
    }
}
