use super::*;
#[cfg(test)]
mod tests;

impl DeviceNetworkService {
    pub(super) async fn shared_open_management(
        &self,
        connection: &Connection,
        generation: u64,
        query: u64,
        token: &str,
    ) -> Result<(), RequestError> {
        {
            let runtime = self.inner.shared_work.0.lock().unwrap();
            runtime.require_current(generation, query)?;
            if !runtime
                .view
                .principal
                .as_ref()
                .is_some_and(|principal| principal.administrator)
            {
                return Err(RequestError::Local(
                    "Hubの管理者として許可されたPCから開いてください。",
                ));
            }
        }
        #[derive(Deserialize)]
        struct WebAccess {
            url: String,
        }
        let access: WebAccess = request(&connection.client, "admin/web-access", Some(token), Some(json!({})), &[])
            .await
            .map_err(|error| match error {
                RequestError::Http(403) => RequestError::Local("このPCにはHubの管理権限がありません。"),
                RequestError::Http(400 | 404 | 409 | 503) => RequestError::Local(
                    "このHubの遠隔管理画面を開けません。Hub設置PCで管理画面を開き、HTTPSの管理用URLとこのPCの管理権限を確認してください。",
                ),
                error => error,
            })?;
        validate_management_url(&access.url)?;
        if !self.shared_current(connection, generation, query) {
            return Err(RequestError::Local(
                "管理画面を開く前にHubまたは端末の設定が変わりました。",
            ));
        }
        tokio::task::spawn_blocking(move || open_management_browser(&access.url))
            .await
            .map_err(|_| RequestError::Local("Hubの管理画面を開けませんでした。"))??;
        if self.shared_current(connection, generation, query) {
            self.inner.shared_work.0.lock().unwrap().view.feedback =
                Some("Hubの管理画面を開きました。".into());
        }
        Ok(())
    }

    pub(super) async fn shared_authenticate(
        &self,
        connection: &Connection,
        generation: u64,
        query: u64,
    ) -> Result<String, RequestError> {
        {
            let runtime = self.inner.shared_work.0.lock().unwrap();
            runtime.require_current(generation, query)?;
        }
        let session = self.shared_device_session(connection).await?;
        self.inner
            .shared_work
            .0
            .lock()
            .unwrap()
            .require_current(generation, query)?;
        Ok(session.token)
    }

    /// Foreground, notifications and team tools share one credential owner.
    /// Renewal is independent of which conversation a concurrent UI query selects.
    pub(super) async fn shared_device_session(
        &self,
        connection: &Connection,
    ) -> Result<LoginSession, RequestError> {
        let _authentication = self.inner.shared_work.2.lock().await;
        if self
            .shared_connection()
            .is_none_or(|current| current.binding != connection.binding)
        {
            return Err(RequestError::Local("端末または接続先が変わりました。"));
        }
        let generation = {
            let mut runtime = self.inner.shared_work.0.lock().unwrap();
            if runtime.binding != connection.binding
                && (runtime.token.is_some() || !runtime.view.projects.is_empty())
            {
                runtime.clear();
            }
            if runtime.binding == connection.binding
                && let (Some(token), Some(principal), Some(expires_at_ms)) = (
                    &runtime.token,
                    &runtime.view.principal,
                    runtime.view.expires_at_ms,
                )
                && expires_at_ms > now_ms().saturating_add(60_000)
            {
                return Ok(LoginSession {
                    token: token.clone(),
                    principal: principal.clone(),
                    expires_at_ms,
                });
            }
            runtime.generation
        };
        // mTLS identifies this device. Old remembered human credentials are neither
        // restored nor replaced, and there is no password fallback.
        let session: Result<LoginSession, RequestError> = request(
            &connection.client,
            "device-session",
            None,
            Some(json!({})),
            &[],
        )
        .await
        .map_err(|error| match error {
            RequestError::Http(404 | 405) => RequestError::Local(
                "Hubを更新してください。このHubは端末の承認だけで利用する方式に対応していません。",
            ),
            error => error,
        });
        if self
            .shared_connection()
            .is_none_or(|current| current.binding != connection.binding)
        {
            return Err(RequestError::Local("端末または接続先が変わりました。"));
        }
        let session = match session {
            Ok(session) => session,
            Err(error) => {
                if matches!(error, RequestError::Http(401 | 403)) {
                    let mut runtime = self.inner.shared_work.0.lock().unwrap();
                    if runtime.generation == generation {
                        runtime.clear();
                        runtime.view.error = Some(error.message().into());
                    }
                }
                return Err(error);
            }
        };
        if session.token.is_empty()
            || session.token.len() > 4096
            || !super::super::stable_id(&session.principal.user_id)
            || session.expires_at_ms <= now_ms()
        {
            return Err(RequestError::Invalid);
        }
        let mut runtime = self.inner.shared_work.0.lock().unwrap();
        if runtime.generation != generation {
            return Err(RequestError::Local("端末または接続先が変わりました。"));
        }
        if runtime
            .view
            .principal
            .as_ref()
            .is_some_and(|principal| principal.user_id != session.principal.user_id)
        {
            runtime.clear();
            return Err(RequestError::Local(
                "このPCの利用登録が変更されました。プロジェクトを確認し直してください。",
            ));
        }
        runtime.binding = connection.binding.clone();
        runtime.hub_binding = connection.hub_binding.clone();
        runtime.token = Some(session.token.clone());
        runtime.view.principal = Some(session.principal.clone());
        runtime.view.expires_at_ms = Some(session.expires_at_ms);
        Ok(session)
    }
}

fn validate_management_url(value: &str) -> Result<(), RequestError> {
    let invalid = || RequestError::Local("Hubから安全な管理画面のURLを確認できませんでした。");
    if value.len() > 4096 {
        return Err(invalid());
    }
    let url = reqwest::Url::parse(value).map_err(|_| invalid())?;
    let valid_ticket = url
        .fragment()
        .and_then(|fragment| fragment.strip_prefix("access="))
        .is_some_and(|ticket| {
            ticket.len() == 64 && ticket.bytes().all(|byte| byte.is_ascii_hexdigit())
        });
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || !valid_ticket
    {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(windows)]
fn open_management_browser(url: &str) -> Result<(), RequestError> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    let operation = "open".encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let url = url.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    // Direct OS URL opening: no command shell, arguments, logging or webview ticket projection.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            url.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    };
    if result as isize <= 32 {
        return Err(RequestError::Local(
            "既定のブラウザーでHub管理画面を開けませんでした。",
        ));
    }
    Ok(())
}
#[cfg(not(windows))]
fn open_management_browser(_: &str) -> Result<(), RequestError> {
    Err(RequestError::Local(
        "この管理画面の起動入口はWindowsで利用できます。",
    ))
}
