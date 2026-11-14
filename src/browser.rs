pub(crate) fn web_url(value: &str) -> Result<reqwest::Url, &'static str> {
    let invalid = "HTTPまたはHTTPSのリンクを指定してください。";
    if value.len() > 8192 || value.trim() != value || value.chars().any(char::is_control) {
        return Err(invalid);
    }
    let url = reqwest::Url::parse(value).map_err(|_| invalid)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid);
    }
    Ok(url)
}

#[cfg(windows)]
pub(crate) fn open_web_url(value: &str) -> Result<(), &'static str> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    let url = web_url(value)?;
    let operation = "open".encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let url = url
        .as_str()
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
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
        return Err("既定のブラウザーでリンクを開けませんでした。");
    }
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn open_web_url(value: &str) -> Result<(), &'static str> {
    web_url(value)?;
    Err("既定のブラウザーで開く操作はWindowsで利用できます。")
}

#[cfg(test)]
mod tests {
    #[test]
    fn browser_links_accept_web_urls_without_credentials_or_control_characters() {
        assert!(super::web_url("https://example.test/path?q=1#part").is_ok());
        for value in [
            "file:///notes",
            "javascript:void(0)",
            "https://user:pw@example.test/",
            "http://example.test/\n",
            "http://bad\0.example/",
            "#fragment",
        ] {
            assert!(super::web_url(value).is_err());
        }
    }
}
