//! Optional update check (off by default). It asks GitHub for the latest published release and NOTHING else:
//!
//! * One `GET` of [`LATEST_RELEASE_URL`] with the fixed `User-Agent` [`USER_AGENT`] (GitHub's API requires one). The
//!   request has no other header, no cookie, no version, no operating system and no identifier ([`Request`] has no
//!   field for any of them).
//! * Only the `tag_name` of the answer is read, and only if it is exactly `v<major>.<minor>.<patch>`.
//! * It never downloads or runs anything. The page that "see what is new" opens is built HERE from the validated
//!   version ([`release_url`]); no URL from the answer is ever used.
//! * Every failure is a value, never a panic, and the caller ignores it.
//!
//! The network sits behind the [`Transport`] trait so that no test touches it; the real one is [`winhttp::WinHttp`].

pub mod winhttp;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// The only address the check ever requests (`/releases/latest` already excludes drafts and prereleases).
pub const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/wandoth1/tesela/releases/latest";
/// The only header sent: a fixed name, without version.
pub const USER_AGENT: &str = "Tesela";
/// Prefix of the release page opened by "see what is new".
pub const RELEASE_PAGE_PREFIX: &str = "https://github.com/wandoth1/tesela/releases/tag/";
/// At most one request every 24 hours.
pub const MIN_INTERVAL_SECS: u64 = 24 * 60 * 60;
/// Largest answer that is read (a release with a long description is tens of KB).
pub const MAX_BODY_BYTES: usize = 1 << 20;

/// A version `major.minor.patch` (what a release tag `vX.Y.Z` means). Ordered like semver without prerelease parts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    /// Strictly `v<major>.<minor>.<patch>`: digits only, no leading zeros, exactly three parts, nothing around.
    /// Anything else (`1.2.3`, `v1.2`, `v1.2.3-rc1`, ` v1.2.3`, `v01.2.3`...) is `None`.
    pub fn parse_tag(tag: &str) -> Option<Version> {
        Version::parse_numbers(tag.strip_prefix('v')?)
    }

    /// The version of THIS build (`CARGO_PKG_VERSION`, shared by the whole workspace). `None` if it has a
    /// prerelease or build suffix: then nothing is compared and no notice is shown.
    pub fn current() -> Option<Version> {
        Version::parse_numbers(env!("CARGO_PKG_VERSION"))
    }

    fn parse_numbers(s: &str) -> Option<Version> {
        let mut parts = s.split('.');
        let (a, b, c) = (parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() {
            return None;
        }
        Some(Version {
            major: number(a)?,
            minor: number(b)?,
            patch: number(c)?,
        })
    }

    /// The release tag: `v0.1.5`.
    pub fn tag(&self) -> String {
        self.to_string()
    }
}

/// A decimal number of at most 9 digits with no leading zeros ("0" is fine).
fn number(s: &str) -> Option<u32> {
    let ok = !s.is_empty()
        && s.len() <= 9
        && s.bytes().all(|b| b.is_ascii_digit())
        && (s == "0" || !s.starts_with('0'));
    if ok { s.parse().ok() } else { None }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Saved as its tag (`"v0.1.5"`).
impl Serialize for Version {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Version, D::Error> {
        let s = String::deserialize(d)?;
        Version::parse_tag(&s).ok_or_else(|| serde::de::Error::custom("not a vX.Y.Z version tag"))
    }
}

/// For a saved `Option<Version>` field: anything that is not a valid tag reads as `None` instead of making the whole
/// configuration file unreadable (use with `#[serde(default, deserialize_with = ...)]`).
pub fn deserialize_lenient<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Version>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Any {
        Text(String),
        Other(serde::de::IgnoredAny),
    }
    Ok(match Any::deserialize(d)? {
        Any::Text(s) => Version::parse_tag(&s),
        Any::Other(_) => None,
    })
}

/// The page of a release, built from a validated version. The ONLY URL the check ever asks the system to open.
pub fn release_url(v: Version) -> String {
    format!("{RELEASE_PAGE_PREFIX}{v}")
}

/// Is `latest` newer than `current`?
pub fn is_newer(latest: Version, current: Version) -> bool {
    latest > current
}

/// Should a request be made now? Only if the check is on, and at most once every 24 hours (`last` is the Unix time of
/// the last attempt, successful or not). A date in the future (the clock went back) counts as due, so a wrong clock
/// cannot silence the check for years.
pub fn is_due(enabled: bool, last: Option<u64>, now: u64) -> bool {
    enabled && last.is_none_or(|l| l > now || now - l >= MIN_INTERVAL_SECS)
}

/// The version to tell the user about, if any: the check is on, `latest` is newer than `current`, and it was not
/// dismissed. (Dismissing a version does not hide a later one.)
pub fn notice_for(
    enabled: bool,
    latest: Option<Version>,
    current: Option<Version>,
    dismissed: Option<Version>,
) -> Option<Version> {
    let (latest, current) = (latest?, current?);
    (enabled && is_newer(latest, current) && Some(latest) != dismissed).then_some(latest)
}

/// What is requested: a URL and a `User-Agent`. There is deliberately no field for anything else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub url: String,
    pub user_agent: String,
}

impl Request {
    /// The one request of the check.
    pub fn latest_release() -> Request {
        Request {
            url: LATEST_RELEASE_URL.to_owned(),
            user_agent: USER_AGENT.to_owned(),
        }
    }
}

/// Why a request produced no body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportError {
    /// No network, proxy, TLS, timeout...
    Unavailable,
    /// The server answered with something other than 200 (rate limit, 404...).
    Status(u16),
    /// The answer is larger than [`MAX_BODY_BYTES`].
    TooLarge,
    /// The request cannot be made (not an `https` URL).
    BadRequest,
}

/// How a request gets made. The real one is [`winhttp::WinHttp`]; tests inject theirs.
pub trait Transport: Send + Sync {
    /// Makes the request and returns the body of a 200 answer.
    fn get(&self, request: &Request) -> Result<Vec<u8>, TransportError>;
}

/// Why a check produced no version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckError {
    Transport(TransportError),
    /// The body is not JSON with a text `tag_name`.
    BadJson,
    /// `tag_name` is not exactly `vX.Y.Z`.
    BadTag,
}

/// Reads the `tag_name` of a `releases/latest` answer. Nothing else in the body is looked at.
pub fn parse_latest(body: &[u8]) -> Result<Version, CheckError> {
    #[derive(Deserialize)]
    struct Latest {
        tag_name: String,
    }
    let latest: Latest = serde_json::from_slice(body).map_err(|_| CheckError::BadJson)?;
    Version::parse_tag(&latest.tag_name).ok_or(CheckError::BadTag)
}

/// Makes the request through `transport` and returns the latest published version.
pub fn fetch_latest(transport: &dyn Transport) -> Result<Version, CheckError> {
    let body = transport
        .get(&Request::latest_release())
        .map_err(CheckError::Transport)?;
    parse_latest(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(major: u32, minor: u32, patch: u32) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    #[test]
    fn the_tag_format_is_strict() {
        assert_eq!(Version::parse_tag("v0.1.5"), Some(v(0, 1, 5)));
        assert_eq!(Version::parse_tag("v10.200.3000"), Some(v(10, 200, 3000)));
        assert_eq!(Version::parse_tag("v0.0.0"), Some(v(0, 0, 0)));
        for bad in [
            "",
            "v",
            "0.1.5",
            "V0.1.5",
            "v0.1",
            "v0.1.5.2",
            "v0.1.5-rc1",
            "v0.1.5+build",
            " v0.1.5",
            "v0.1.5 ",
            "v0.1.5\n",
            "v01.1.5",
            "v0.01.5",
            "v0.1.05",
            "v-1.1.5",
            "v+1.1.5",
            "v0..5",
            "v0.1.x",
            "v0.1.4294967296",
            "v1234567890.0.0",
            "v٣.1.5",
            "v0.1.5/../x",
            "https://example.com/v0.1.5",
        ] {
            assert_eq!(Version::parse_tag(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert!(is_newer(v(0, 1, 5), v(0, 1, 4)));
        assert!(is_newer(v(0, 10, 0), v(0, 9, 9)));
        assert!(is_newer(v(1, 0, 0), v(0, 99, 99)));
        assert!(!is_newer(v(0, 1, 4), v(0, 1, 4)));
        assert!(!is_newer(v(0, 1, 3), v(0, 1, 4)));
        assert_eq!(v(0, 1, 5).tag(), "v0.1.5");
        assert_eq!(
            Version::parse_tag(&v(3, 14, 15).tag()),
            Some(v(3, 14, 15)),
            "the tag reads back"
        );
    }

    #[test]
    fn this_build_has_a_current_version() {
        let current = Version::current().expect("the workspace version is major.minor.patch");
        assert_eq!(current.tag(), format!("v{}", env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn the_release_page_is_built_from_the_version_and_belongs_to_the_repository() {
        assert_eq!(
            release_url(v(0, 1, 5)),
            "https://github.com/wandoth1/tesela/releases/tag/v0.1.5"
        );
        // The hard-coded addresses agree with the repository the workspace declares.
        let repo = env!("CARGO_PKG_REPOSITORY");
        assert!(release_url(v(0, 1, 5)).starts_with(&format!("{repo}/releases/tag/")));
        assert_eq!(
            LATEST_RELEASE_URL,
            repo.replace("https://github.com/", "https://api.github.com/repos/")
                + "/releases/latest"
        );
    }

    #[test]
    fn at_most_one_request_every_24_hours_and_only_if_enabled() {
        let day = MIN_INTERVAL_SECS;
        assert!(!is_due(false, None, 1_000_000), "off: never");
        assert!(
            !is_due(false, Some(0), 10 * day),
            "off: never, even long after"
        );
        assert!(is_due(true, None, 1_000_000), "never checked");
        assert!(!is_due(true, Some(1_000_000), 1_000_000), "just checked");
        assert!(!is_due(true, Some(1_000_000), 1_000_000 + day - 1));
        assert!(is_due(true, Some(1_000_000), 1_000_000 + day));
        assert!(
            is_due(true, Some(5_000_000), 1_000_000),
            "date in the future"
        );
    }

    #[test]
    fn the_notice_needs_a_newer_version_that_was_not_dismissed_and_the_check_on() {
        let cur = Some(v(0, 1, 4));
        let new = Some(v(0, 1, 5));
        assert_eq!(notice_for(true, new, cur, None), new);
        assert_eq!(notice_for(false, new, cur, None), None, "off: no notice");
        assert_eq!(notice_for(true, cur, cur, None), None, "same version");
        assert_eq!(notice_for(true, Some(v(0, 1, 3)), cur, None), None, "older");
        assert_eq!(notice_for(true, None, cur, None), None, "never learnt");
        assert_eq!(
            notice_for(true, new, None, None),
            None,
            "unknown own version"
        );
        assert_eq!(notice_for(true, new, cur, new), None, "dismissed");
        // A dismissed version does not hide a later one.
        assert_eq!(
            notice_for(true, Some(v(0, 1, 6)), cur, new),
            Some(v(0, 1, 6))
        );
    }

    #[test]
    fn only_the_tag_name_is_read() {
        let body = br#"{"url":"https://evil.example/x","html_url":"https://evil.example/y","tag_name":"v0.1.5","body":"hola","assets":[{"browser_download_url":"https://evil.example/z.exe"}]}"#;
        assert_eq!(parse_latest(body), Ok(v(0, 1, 5)));
        assert_eq!(parse_latest(br#"{"tag_name":"v0.1.5"}"#), Ok(v(0, 1, 5)));
        assert_eq!(
            parse_latest(br#"{"tag_name":"v0.1.5-rc1"}"#),
            Err(CheckError::BadTag)
        );
        assert_eq!(
            parse_latest(br#"{"tag_name":"0.1.5"}"#),
            Err(CheckError::BadTag)
        );
        assert_eq!(parse_latest(br#"{"tag_name":""}"#), Err(CheckError::BadTag));
        for bad in [
            &b""[..],
            b"not json",
            b"{",
            b"[]",
            b"{}",
            br#"{"tag":"v0.1.5"}"#,
            br#"{"tag_name":5}"#,
            br#"{"tag_name":null}"#,
            &[0xff, 0xfe, 0x00][..],
        ] {
            assert_eq!(parse_latest(bad), Err(CheckError::BadJson), "{bad:?}");
        }
    }

    /// A transport that never touches the network: it records the requests and answers with what it was given.
    struct Fake {
        answer: Result<Vec<u8>, TransportError>,
        requests: std::sync::Mutex<Vec<Request>>,
    }

    impl Transport for Fake {
        fn get(&self, request: &Request) -> Result<Vec<u8>, TransportError> {
            self.requests.lock().unwrap().push(request.clone());
            self.answer.clone()
        }
    }

    fn fake(answer: Result<Vec<u8>, TransportError>) -> Fake {
        Fake {
            answer,
            requests: Default::default(),
        }
    }

    #[test]
    fn the_request_is_exactly_the_expected_url_and_user_agent() {
        let t = fake(Ok(br#"{"tag_name":"v0.2.0"}"#.to_vec()));
        assert_eq!(fetch_latest(&t), Ok(v(0, 2, 0)));
        let sent = t.requests.lock().unwrap();
        assert_eq!(
            *sent,
            vec![Request {
                url: "https://api.github.com/repos/wandoth1/tesela/releases/latest".into(),
                user_agent: "Tesela".into(),
            }]
        );
        // A fixed name: no version, no system, no identifier.
        assert!(!sent[0].user_agent.contains(char::is_numeric));
        assert!(!sent[0].user_agent.contains('/'));
        assert!(!sent[0].url.contains('?'));
    }

    #[test]
    fn every_transport_failure_is_a_value() {
        for e in [
            TransportError::Unavailable,
            TransportError::Status(403),
            TransportError::Status(404),
            TransportError::TooLarge,
            TransportError::BadRequest,
        ] {
            assert_eq!(fetch_latest(&fake(Err(e))), Err(CheckError::Transport(e)));
        }
        assert_eq!(
            fetch_latest(&fake(Ok(b"<html>captive portal</html>".to_vec()))),
            Err(CheckError::BadJson)
        );
    }

    #[test]
    fn a_saved_version_is_its_tag_and_a_bad_one_reads_as_none() {
        #[derive(Serialize, Deserialize, PartialEq, Debug, Default)]
        struct Saved {
            #[serde(default, deserialize_with = "deserialize_lenient")]
            seen: Option<Version>,
        }
        let json = |s: &str| serde_json::from_str::<Saved>(s).unwrap();
        assert_eq!(json(r#"{"seen":"v0.1.5"}"#).seen, Some(v(0, 1, 5)));
        assert_eq!(json(r#"{"seen":"nonsense"}"#).seen, None);
        assert_eq!(json(r#"{"seen":7}"#).seen, None);
        assert_eq!(json(r#"{"seen":null}"#).seen, None);
        assert_eq!(json(r#"{}"#).seen, None);
        let text = serde_json::to_string(&Saved {
            seen: Some(v(0, 1, 5)),
        })
        .unwrap();
        assert_eq!(text, r#"{"seen":"v0.1.5"}"#);
        assert_eq!(json(&text).seen, Some(v(0, 1, 5)));
        // The strict impl rejects a bad tag (only the lenient field helper forgives it).
        assert!(serde_json::from_str::<Version>("\"v1.2\"").is_err());
    }
}
