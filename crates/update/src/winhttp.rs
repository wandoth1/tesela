//! The real transport: WinHTTP, the system HTTP stack (it honors the system proxy and adds nothing to the supply chain).
//!
//! What it sends is exactly the request line, `Host`, `User-Agent` and `Connection`: no `Accept`, no cookies (disabled),
//! no redirects (disabled: the answer of the address that was asked for, or nothing) and no credentials of the current
//! Windows user (automatic logon only with the highest security level, so none is sent on its own). It is blocking: the
//! caller runs it on its own thread, and every phase has a short timeout.
//!
//! It is the only module of Tesela that can reach the network, and it is only reached if the user turned the update
//! check on. Nothing here is exercised by the automated tests (they use a fake [`Transport`]); see `docs/` and the
//! test notes for the single manual request.
#![allow(unsafe_code)]

use crate::{MAX_BODY_BYTES, Request, Transport, TransportError};
use std::ffi::c_void;
use std::time::{Duration, Instant};
use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_AUTOLOGON_SECURITY_LEVEL_HIGH,
    WINHTTP_DISABLE_COOKIES, WINHTTP_DISABLE_REDIRECTS, WINHTTP_FLAG_SECURE,
    WINHTTP_OPTION_AUTOLOGON_POLICY, WINHTTP_OPTION_DISABLE_FEATURE, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetOption, WinHttpSetTimeouts,
};
use windows::core::{HSTRING, PCWSTR};

/// Ceiling of each phase (name resolution, connection, sending, receiving), in milliseconds.
const PHASE_TIMEOUT_MS: i32 = 5_000;
/// Ceiling of reading the body once the answer started.
const BODY_BUDGET: Duration = Duration::from_secs(10);

/// WinHTTP, through the system proxy settings.
#[derive(Clone, Copy, Debug, Default)]
pub struct WinHttp;

impl Transport for WinHttp {
    fn get(&self, request: &Request) -> Result<Vec<u8>, TransportError> {
        let (host, port, path) = split_https(&request.url).ok_or(TransportError::BadRequest)?;
        if request.user_agent.is_empty() {
            return Err(TransportError::BadRequest);
        }
        get(&request.user_agent, host, port, path)
    }
}

/// An open WinHTTP handle, closed when it goes out of scope (on every path, including errors).
struct Handle(*mut c_void);

impl Handle {
    /// `None` if the call that returned `raw` failed (it returns null).
    fn new(raw: *mut c_void) -> Option<Handle> {
        (!raw.is_null()).then_some(Handle(raw))
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a non-null handle returned by WinHTTP that nobody else closes (the type is not `Clone`).
        let _ = unsafe { WinHttpCloseHandle(self.0) };
    }
}

fn get(user_agent: &str, host: &str, port: u16, path: &str) -> Result<Vec<u8>, TransportError> {
    let unavailable = |_| TransportError::Unavailable;
    // SAFETY: every pointer argument is either null (allowed: no proxy list, no bypass list) or a valid wide string
    // that lives for the whole call.
    let session = Handle::new(unsafe {
        WinHttpOpen(
            &HSTRING::from(user_agent),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        )
    })
    .ok_or(TransportError::Unavailable)?;
    // SAFETY: `session` is a valid open handle.
    unsafe {
        WinHttpSetTimeouts(
            session.0,
            PHASE_TIMEOUT_MS,
            PHASE_TIMEOUT_MS,
            PHASE_TIMEOUT_MS,
            PHASE_TIMEOUT_MS,
        )
    }
    .map_err(unavailable)?;
    // SAFETY: `session` is valid and the host is a valid wide string for the whole call.
    let connection =
        Handle::new(unsafe { WinHttpConnect(session.0, &HSTRING::from(host), port, 0) })
            .ok_or(TransportError::Unavailable)?;
    // SAFETY: `connection` is valid; the verb and the path are valid wide strings; null version (HTTP/1.1), no
    // referrer and no list of accepted types (so no `Accept` header).
    let request = Handle::new(unsafe {
        WinHttpOpenRequest(
            connection.0,
            &HSTRING::from("GET"),
            &HSTRING::from(path),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        )
    })
    .ok_or(TransportError::Unavailable)?;

    let disable = WINHTTP_DISABLE_COOKIES | WINHTTP_DISABLE_REDIRECTS;
    let autologon = WINHTTP_AUTOLOGON_SECURITY_LEVEL_HIGH;
    // SAFETY: `request` is valid; the buffers are `u32` values (what these two options expect) alive for the call.
    unsafe {
        WinHttpSetOption(
            Some(request.0.cast_const()),
            WINHTTP_OPTION_DISABLE_FEATURE,
            Some(&disable.to_ne_bytes()),
        )
    }
    .map_err(unavailable)?;
    // SAFETY: as above.
    unsafe {
        WinHttpSetOption(
            Some(request.0.cast_const()),
            WINHTTP_OPTION_AUTOLOGON_POLICY,
            Some(&autologon.to_ne_bytes()),
        )
    }
    .map_err(unavailable)?;

    // SAFETY: `request` is valid; no extra headers and no body.
    unsafe { WinHttpSendRequest(request.0, None, None, 0, 0, 0) }.map_err(unavailable)?;
    // SAFETY: `request` is valid and was sent; the reserved argument is null.
    unsafe { WinHttpReceiveResponse(request.0, std::ptr::null_mut()) }.map_err(unavailable)?;

    let mut status: u32 = 0;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: `request` has its answer; with `WINHTTP_QUERY_FLAG_NUMBER` the buffer receives a `u32`, and `size` is
    // its length. No header name and no index are used (null).
    unsafe {
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&raw mut status).cast::<c_void>()),
            &mut size,
            std::ptr::null_mut(),
        )
    }
    .map_err(unavailable)?;
    if status != 200 {
        return Err(TransportError::Status(
            status.min(u32::from(u16::MAX)) as u16
        ));
    }

    let started = Instant::now();
    let mut body: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if started.elapsed() > BODY_BUDGET {
            return Err(TransportError::Unavailable);
        }
        let mut available: u32 = 0;
        // SAFETY: `request` has its answer; `available` is a valid `u32` out-parameter.
        unsafe { WinHttpQueryDataAvailable(request.0, &mut available) }.map_err(unavailable)?;
        if available == 0 {
            break;
        }
        let want = (available as usize).min(chunk.len());
        let mut read: u32 = 0;
        // SAFETY: `chunk` has room for `want` bytes (`want <= chunk.len()`) and `read` is a valid out-parameter.
        unsafe {
            WinHttpReadData(
                request.0,
                chunk.as_mut_ptr().cast::<c_void>(),
                want as u32,
                &mut read,
            )
        }
        .map_err(unavailable)?;
        if read == 0 {
            break;
        }
        if body.len() + read as usize > MAX_BODY_BYTES {
            return Err(TransportError::TooLarge);
        }
        body.extend_from_slice(&chunk[..read as usize]);
    }
    Ok(body)
}

/// Splits `https://host[:port]/path` into its parts. Only `https` is accepted, and the host must be a plain name
/// (letters, digits, `.` and `-`).
fn split_https(url: &str) -> Option<(&str, u16, &str)> {
    let rest = url.strip_prefix("https://")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok().filter(|p| *p != 0)?),
        None => (authority, 443),
    };
    let plain = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    (plain && !path.contains(char::is_whitespace)).then_some((host, port, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_address_of_the_check_splits_into_host_port_and_path() {
        assert_eq!(
            split_https(crate::LATEST_RELEASE_URL),
            Some((
                "api.github.com",
                443,
                "/repos/wandoth1/tesela/releases/latest"
            ))
        );
        assert_eq!(
            split_https("https://example.com"),
            Some(("example.com", 443, "/"))
        );
        assert_eq!(
            split_https("https://example.com:8443/a"),
            Some(("example.com", 8443, "/a"))
        );
    }

    #[test]
    fn only_plain_https_addresses_are_accepted() {
        for bad in [
            "http://api.github.com/x",
            "ftp://api.github.com/x",
            "api.github.com/x",
            "https://",
            "https:///x",
            "https://user@host/x",
            "https://host:0/x",
            "https://host:99999/x",
            "https://host:abc/x",
            "https://ho st/x",
            "https://host/a b",
        ] {
            assert_eq!(split_https(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_request_that_cannot_be_made_fails_before_touching_the_network() {
        // Not `https`, or no user agent: rejected before any WinHTTP call.
        let http = Request {
            url: "http://api.github.com/x".into(),
            user_agent: "Tesela".into(),
        };
        assert_eq!(WinHttp.get(&http), Err(TransportError::BadRequest));
        let nameless = Request {
            url: crate::LATEST_RELEASE_URL.into(),
            user_agent: String::new(),
        };
        assert_eq!(WinHttp.get(&nameless), Err(TransportError::BadRequest));
    }
}
