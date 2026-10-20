//! Загрузка по HTTPS через WinHTTP — встроенный в Windows клиент: учитывает системный прокси
//! и сертификаты, не требует сторонних библиотек. Переадресации (GitHub отдаёт файлы релизов
//! с другого сервера) WinHTTP выполняет сам.

use std::fmt;

use windows::Win32::Networking::WinHttp::{
    INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ADDREQ_FLAG_ADD, WINHTTP_FLAG_SECURE,
    WINHTTP_QUERY_CONTENT_LENGTH, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpAddRequestHeaders,
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData,
    WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
};
use windows::core::{HSTRING, PCWSTR, w};

/// Сколько ждать соединения и каждого ответа сервера, мс.
const TIMEOUT_MS: i32 = 30_000;

/// Сколько читать за раз.
const CHUNK_SIZE: usize = 64 * 1024;

/// Почему не удалось загрузить.
#[derive(Debug)]
pub enum HttpError {
    /// Адрес не `https://сервер/путь`.
    InvalidUrl(String),
    /// Ошибка Windows: нет сети, сервер недоступен, проблема с сертификатом.
    Network(windows::core::Error),
    /// Сервер ответил кодом, отличным от 200.
    Status(u32),
    /// Загрузку остановили.
    Cancelled,
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            HttpError::InvalidUrl(url) => write!(f, "{url}"),
            HttpError::Network(error) => write!(f, "{}", error.message()),
            HttpError::Status(code) => write!(f, "HTTP {code}"),
            HttpError::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl From<windows::core::Error> for HttpError {
    fn from(error: windows::core::Error) -> Self {
        HttpError::Network(error)
    }
}

/// Дескриптор WinHTTP, закрывается при выходе из области видимости.
struct Handle(*mut core::ffi::c_void);

impl Handle {
    fn new(raw: *mut core::ffi::c_void) -> Result<Self, HttpError> {
        if raw.is_null() { Err(windows::core::Error::from_thread().into()) } else { Ok(Self(raw)) }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

/// Загружает `url` (только https) целиком в память.
///
/// `headers` — дополнительные заголовки запроса, по одному в строке.
/// `on_progress(загружено, всего)` вызывается после каждой порции; `всего` — `None`,
/// если сервер не сообщил размер. Если он вернёт `false`, загрузка останавливается.
pub fn get(
    url: &str,
    user_agent: &str,
    headers: &[&str],
    mut on_progress: impl FnMut(u64, Option<u64>) -> bool,
) -> Result<Vec<u8>, HttpError> {
    let (host, path) = split_url(url).ok_or_else(|| HttpError::InvalidUrl(url.to_owned()))?;

    unsafe {
        let session =
            Handle::new(WinHttpOpen(&HSTRING::from(user_agent), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, None, None, 0))?;
        WinHttpSetTimeouts(session.0, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS)?;
        let connection = Handle::new(WinHttpConnect(session.0, &HSTRING::from(host), INTERNET_DEFAULT_HTTPS_PORT, 0))?;
        let request = Handle::new(WinHttpOpenRequest(
            connection.0,
            w!("GET"),
            &HSTRING::from(path),
            None,
            None,
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ))?;
        for header in headers {
            let wide: Vec<u16> = header.encode_utf16().collect();
            WinHttpAddRequestHeaders(request.0, &wide, WINHTTP_ADDREQ_FLAG_ADD)?;
        }

        WinHttpSendRequest(request.0, None, None, 0, 0, 0)?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut())?;

        let status = query_number(&request, WINHTTP_QUERY_STATUS_CODE).unwrap_or(0);
        if status != 200 {
            return Err(HttpError::Status(status));
        }
        let total = query_number(&request, WINHTTP_QUERY_CONTENT_LENGTH).map(u64::from);

        let mut body = Vec::with_capacity(total.unwrap_or(0) as usize);
        let mut chunk = vec![0u8; CHUNK_SIZE];
        loop {
            let mut read = 0;
            WinHttpReadData(request.0, chunk.as_mut_ptr().cast(), chunk.len() as u32, &mut read)?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read as usize]);
            if !on_progress(body.len() as u64, total) {
                return Err(HttpError::Cancelled);
            }
        }
        Ok(body)
    }
}

/// Числовой заголовок ответа: код ответа, размер.
fn query_number(request: &Handle, info: u32) -> Option<u32> {
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    unsafe {
        WinHttpQueryHeaders(
            request.0,
            info | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut value as *mut u32).cast()),
            &mut size,
            std::ptr::null_mut(),
        )
        .ok()?;
    }
    Some(value)
}

/// `https://api.github.com/repos/x?y` → `("api.github.com", "/repos/x?y")`.
fn split_url(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = match rest.find('/') {
        Some(slash) => rest.split_at(slash),
        None => (rest, "/"),
    };
    (!host.is_empty()).then_some((host, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_https_urls_only() {
        assert_eq!(split_url("https://api.github.com/repos/a/b"), Some(("api.github.com", "/repos/a/b")));
        assert_eq!(split_url("https://example.com"), Some(("example.com", "/")));
        assert_eq!(split_url("http://example.com/a"), None);
        assert_eq!(split_url("https:///a"), None);
    }
}
