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
        current_token: Option<&str>,
    ) -> Result<String, RequestError> {
        {
            let runtime = self.inner.shared_work.0.lock().unwrap();
            runtime.require_current(generation, query)?;
            if let Some(token) = current_token.filter(|_| {
                runtime.binding == connection.binding
                    && runtime
                        .view
                        .expires_at_ms
                        .is_some_and(|time| time > now_ms().saturating_add(60_000))
            }) {
                return Ok(token.to_owned());
            }
        }
        // mTLS identifies this device. Old remembered human credentials are neither
        // restored nor replaced, and there is no password fallback.
        let session: LoginSession = request(
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
        })?;
        if session.token.is_empty()
            || session.token.len() > 4096
            || !super::super::stable_id(&session.principal.user_id)
            || session.expires_at_ms <= now_ms()
        {
            return Err(RequestError::Invalid);
        }
        if !self.shared_current(connection, generation, query) {
            return Err(RequestError::Local("端末または表示対象が変わりました。"));
        }
        let mut runtime = self.inner.shared_work.0.lock().unwrap();
        runtime.require_current(generation, query)?;
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
        runtime.view.principal = Some(session.principal);
        runtime.view.expires_at_ms = Some(session.expires_at_ms);
        Ok(session.token)
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
