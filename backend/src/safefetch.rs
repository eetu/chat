//! Guarded outbound page fetcher for the web search path.
//!
//! The backend runs on the same LAN as kanidm, vaultwarden and every
//! other service, so fetching a URL that ultimately came from a chat
//! message is an SSRF primitive unless it is fenced in. Everything here
//! exists to make that fetch boring:
//!
//! - only `http` / `https`, no credentials in the URL, only default ports;
//! - every resolved address is checked against the non-public ranges
//!   *before* connecting, and the connection is pinned to the address
//!   that passed, so a second DNS answer can't swap in a private IP
//!   (rebinding);
//! - redirects are followed by hand, at most [`MAX_REDIRECTS`] hops, each
//!   hop re-validated from scratch;
//! - the response must be HTML or plain text, and the body is streamed
//!   with a hard byte cap so a stream with no `content-length` can't
//!   exhaust memory.
//!
//! Page text comes out via `dom_smoothie` (a Readability port), so the
//! model sees article prose rather than nav chrome.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use dom_smoothie::{Config, Readability};
use futures_util::StreamExt;
use reqwest::header::{CONTENT_TYPE, LOCATION};
use url::Url;

/// Redirect hops followed before giving up. Three covers the usual
/// http→https→canonical-host chain without turning into a crawler.
const MAX_REDIRECTS: usize = 3;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Sent so operators reading their logs can see what this is and that
/// it isn't pretending to be a browser.
const USER_AGENT: &str = concat!(
    "chat-backend/",
    env!("CARGO_PKG_VERSION"),
    " (+self-hosted)"
);

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("unsupported url ({0})")]
    UnsupportedUrl(&'static str),
    #[error("refused to fetch a non-public address")]
    BlockedAddress,
    #[error("no address resolved for host")]
    Unresolved,
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("upstream returned status {0}")]
    Status(u16),
    #[error("unsupported content type ({0})")]
    ContentType(String),
    #[error("redirect chain too long")]
    TooManyRedirects,
    #[error("page had no extractable text")]
    Empty,
}

/// A fetched page reduced to readable text.
#[derive(Debug)]
pub struct Page {
    pub title: String,
    /// The URL that actually served the content — the end of the
    /// redirect chain, not necessarily what was asked for.
    pub url: String,
    pub text: String,
}

/// True when an address is on the public internet. Everything else —
/// loopback, RFC1918, link-local (which includes the 169.254.169.254
/// cloud metadata endpoint), CGNAT, multicast, reserved — is refused.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            // A v6 address can carry a v4 one. Judge it by the address
            // that packets actually reach, not by its notation.
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_public_v4(mapped);
            }
            if let Some(compat) = ipv4_compatible(v6) {
                return is_public_v4(compat);
            }
            is_public_v6(v6)
        }
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    let shared = o[0] == 100 && (o[1] & 0xc0) == 64; // 100.64/10 CGNAT
    let protocol_assignments = o[0] == 192 && o[1] == 0 && o[2] == 0; // 192.0.0/24
    let reserved = (o[0] & 0xf0) == 240; // 240/4, incl. 255.255.255.255
    let benchmarking = o[0] == 198 && (o[1] & 0xfe) == 18; // 198.18/15
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || shared
        || protocol_assignments
        || reserved
        || benchmarking)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    // 6to4 embeds an arbitrary v4 address in the prefix, so a 2002::
    // target can address RFC1918 space. The transition mechanism is
    // long dead; refusing it is cheaper than decoding it.
    let sixtofour = s[0] == 0x2002;
    let unique_local = (s[0] & 0xfe00) == 0xfc00; // fc00::/7
    let link_local = (s[0] & 0xffc0) == 0xfe80; // fe80::/10
    let documentation = s[0] == 0x2001 && s[1] == 0x0db8; // 2001:db8::/32
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || unique_local
        || link_local
        || sixtofour
        || documentation)
}

/// `::a.b.c.d` — the deprecated compatible form, still routable as v4 on
/// some stacks. `Ipv6Addr::to_ipv4` would also match `::1`, which is
/// already covered by `is_loopback`, so only the embedded-v4 shape is
/// treated as v4 here.
fn ipv4_compatible(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let s = ip.segments();
    if s[0..6] == [0, 0, 0, 0, 0, 0] && (s[6] != 0 || s[7] > 1) {
        let o = ip.octets();
        return Some(Ipv4Addr::new(o[12], o[13], o[14], o[15]));
    }
    None
}

/// Parse and vet a URL's shape. Rejects anything but plain http(s) on a
/// default port with no embedded credentials — an article never needs
/// more, while every extra degree of freedom is somewhere an internal
/// service could hide.
pub fn validate_url(raw: &str) -> Result<Url, FetchError> {
    let url = Url::parse(raw).map_err(|_| FetchError::UnsupportedUrl("unparseable"))?;
    match url.scheme() {
        "http" | "https" => {}
        _ => return Err(FetchError::UnsupportedUrl("scheme must be http or https")),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(FetchError::UnsupportedUrl("credentials in url"));
    }
    if url.host_str().is_none() {
        return Err(FetchError::UnsupportedUrl("no host"));
    }
    // `port()` is None when the port is the scheme default, so this
    // allows :80 / :443 and nothing else.
    if let Some(port) = url.port() {
        if port != 80 && port != 443 {
            return Err(FetchError::UnsupportedUrl("non-default port"));
        }
    }
    Ok(url)
}

/// Resolve the host and return the one address we're willing to talk to.
/// Every answer must be public: if a name resolves to a mix, the private
/// entry is the interesting one to an attacker, so the whole name is
/// refused rather than quietly using the public sibling.
async fn resolve_public_addr(url: &Url) -> Result<SocketAddr, FetchError> {
    let host = url
        .host_str()
        .ok_or(FetchError::UnsupportedUrl("no host"))?
        .to_string();
    let port = url.port_or_known_default().unwrap_or(80);
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| FetchError::Unresolved)?
        .collect();
    if addrs.is_empty() {
        return Err(FetchError::Unresolved);
    }
    if addrs.iter().any(|a| !is_public_ip(a.ip())) {
        return Err(FetchError::BlockedAddress);
    }
    Ok(addrs[0])
}

/// Fetch one page and reduce it to readable text.
///
/// `max_bytes` caps the response body; the cap is enforced while
/// streaming, so it holds even when the server lies about (or omits)
/// `content-length`.
pub async fn fetch_page(raw_url: &str, max_bytes: usize) -> Result<Page, FetchError> {
    let mut target = validate_url(raw_url)?;

    for _ in 0..=MAX_REDIRECTS {
        let addr = resolve_public_addr(&target).await?;
        let host = target
            .host_str()
            .ok_or(FetchError::UnsupportedUrl("no host"))?
            .to_string();

        // Pin the connection to the address that passed the check.
        // Without this, reqwest resolves again at connect time and a
        // hostile DNS answer could point the second lookup at a private
        // address. A fresh client per hop is the cost of that pinning;
        // at a handful of fetches per turn it doesn't matter.
        let client = reqwest::Client::builder()
            .resolve(&host, addr)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .user_agent(USER_AGENT)
            .build()?;

        let res = client.get(target.clone()).send().await?;
        let status = res.status();

        if status.is_redirection() {
            let location = res
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or(FetchError::UnsupportedUrl("redirect without location"))?;
            // Relative redirects are legal, so resolve against the
            // current URL, then re-run every check on the result.
            let next = target
                .join(location)
                .map_err(|_| FetchError::UnsupportedUrl("unparseable redirect"))?;
            target = validate_url(next.as_str())?;
            continue;
        }

        if !status.is_success() {
            return Err(FetchError::Status(status.as_u16()));
        }

        let content_type = res
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let is_html =
            content_type.contains("text/html") || content_type.contains("application/xhtml");
        let is_text = content_type.contains("text/plain");
        if !is_html && !is_text {
            return Err(FetchError::ContentType(content_type));
        }

        let mut body: Vec<u8> = Vec::new();
        let mut stream = res.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            let remaining = max_bytes.saturating_sub(body.len());
            if remaining == 0 {
                break;
            }
            let take = chunk.len().min(remaining);
            body.extend_from_slice(&chunk[..take]);
            if take < chunk.len() {
                // Cap reached mid-chunk. Truncated HTML still parses
                // into something useful, so keep what we have and stop
                // rather than failing the whole fetch.
                break;
            }
        }

        let raw = String::from_utf8_lossy(&body).to_string();
        let final_url = target.to_string();
        let (title, text) = if is_html {
            extract_article(&raw, &final_url)
        } else {
            (String::new(), raw)
        };
        let text = collapse_whitespace(&text);
        if text.is_empty() {
            return Err(FetchError::Empty);
        }
        return Ok(Page {
            title,
            url: final_url,
            text,
        });
    }

    Err(FetchError::TooManyRedirects)
}

/// Readability pass. A page that defeats the extractor (an app shell, a
/// paywall interstitial) yields empty text, which the caller treats as a
/// failed fetch and falls back to the search snippet.
fn extract_article(html: &str, url: &str) -> (String, String) {
    let config = Config {
        // Bounded so a pathological document can't spin the parser. The
        // byte cap already limits input size; this limits node count.
        max_elements_to_parse: 20_000,
        ..Default::default()
    };
    match Readability::new(html, Some(url), Some(config)) {
        Ok(mut readability) => match readability.parse() {
            Ok(article) => (article.title.to_string(), article.text_content.to_string()),
            Err(e) => {
                tracing::debug!("readability parse failed: {e}");
                (String::new(), String::new())
            }
        },
        Err(e) => {
            tracing::debug!("readability init failed: {e}");
            (String::new(), String::new())
        }
    }
}

/// Squeeze the extractor's output into something token-efficient:
/// trimmed lines, no runs of blank lines.
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = 0usize;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
            out.push('\n');
            continue;
        }
        blank_run = 0;
        out.push_str(trimmed);
        out.push('\n');
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn ip(s: &str) -> IpAddr {
        IpAddr::from_str(s).expect("parseable address")
    }

    #[test]
    fn public_addresses_are_allowed() {
        for s in ["1.1.1.1", "8.8.8.8", "93.184.216.34", "2606:4700::1111"] {
            assert!(is_public_ip(ip(s)), "{s} should be public");
        }
    }

    #[test]
    fn private_and_local_v4_are_blocked() {
        for s in [
            "127.0.0.1",
            "0.0.0.0",
            "10.0.0.1",
            "172.16.5.4",
            "192.168.1.155",   // the Mac mini — a real internal target
            "169.254.169.254", // cloud metadata
            "100.100.0.1",     // CGNAT
            "192.0.0.1",
            "198.18.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "240.0.0.1",
        ] {
            assert!(!is_public_ip(ip(s)), "{s} should be blocked");
        }
    }

    #[test]
    fn private_and_local_v6_are_blocked() {
        for s in [
            "::1",
            "::",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
        ] {
            assert!(!is_public_ip(ip(s)), "{s} should be blocked");
        }
    }

    #[test]
    fn v6_wrapped_v4_is_judged_as_v4() {
        // ::ffff:192.168.1.1 and ::192.168.1.1 both reach the LAN.
        assert!(!is_public_ip(ip("::ffff:192.168.1.1")));
        assert!(!is_public_ip(ip("::c0a8:101")));
        assert!(is_public_ip(ip("::ffff:1.1.1.1")));
        // 6to4 can embed RFC1918 space in the prefix.
        assert!(!is_public_ip(ip("2002:c0a8:0101::1")));
    }

    #[test]
    fn url_shapes_are_vetted() {
        assert!(validate_url("https://example.com/post").is_ok());
        assert!(validate_url("http://example.com:80/x").is_ok());
        assert!(validate_url("https://example.com:443/x").is_ok());

        for bad in [
            "file:///etc/passwd",
            "gopher://example.com",
            "ftp://example.com/x",
            "https://user:pass@example.com/x",
            "https://example.com:8080/x",
            "http://example.com:2375/containers/json",
            "not a url",
        ] {
            assert!(validate_url(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[tokio::test]
    async fn loopback_hosts_are_refused_before_connecting() {
        // Nothing is listening on these; the point is that the guard
        // rejects them rather than attempting a connection at all.
        for target in [
            "http://127.0.0.1/",
            "http://localhost/",
            "http://[::1]/",
            "http://169.254.169.254/latest/meta-data/",
        ] {
            let err = fetch_page(target, 1024).await.expect_err("must refuse");
            assert!(
                matches!(err, FetchError::BlockedAddress | FetchError::Unresolved),
                "{target} gave {err:?}"
            );
        }
    }

    #[test]
    fn article_text_is_extracted_from_chrome() {
        let html = r#"<html><head><title>Ignored</title></head><body>
            <nav><a href="/">home</a><a href="/about">about</a></nav>
            <article><h1>Frobnicator stabilised</h1>
            <p>The frobnicator API is now stable, after four years in nightly.
            It replaces the old widget interface entirely.</p>
            <p>Migration is mechanical for most callers.</p></article>
            <footer>copyright</footer></body></html>"#;
        let (_, text) = extract_article(html, "https://example.com/post");
        assert!(text.contains("frobnicator API is now stable"));
        assert!(!text.contains("copyright"));
    }

    #[test]
    fn whitespace_collapses_to_single_blank_lines() {
        assert_eq!(collapse_whitespace("  a  \n\n\n\n  b \n"), "a\n\nb");
        assert_eq!(collapse_whitespace("   \n  \n"), "");
    }
}
