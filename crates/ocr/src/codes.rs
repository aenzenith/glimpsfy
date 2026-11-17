//! Which decoded payloads are offered as links. Only `http` and `https` URLs are: a QR code can
//! carry `file:`, `javascript:`, `smb:` or an app's custom scheme, and opening one of those from
//! a screenshot would let any picture on screen launch something on the Mac.

/// The payload as a link to open, when it is an `http` or `https` URL with a host; `None`
/// otherwise. Surrounding whitespace is ignored, and nothing else is changed.
pub fn link_for(payload: &str) -> Option<String> {
    let text = payload.trim();
    if text.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let (scheme, rest) = text.split_once("://")?;
    if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")) {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // User information ("https://bank.com@evil.example") hides the real host from a reader.
    if authority.contains('@') {
        return None;
    }
    let host = authority.rsplit_once(':').map_or(authority, |(host, port)| {
        if port.chars().all(|c| c.is_ascii_digit()) {
            host
        } else {
            authority
        }
    });
    if host.is_empty() || host.starts_with('.') {
        return None;
    }
    Some(text.to_string())
}
