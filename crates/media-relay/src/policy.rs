use crate::error::RelayError;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use url::Url;

/// Forbidden upstream request header names that must never be forwarded to CDNs.
pub const FORBIDDEN_UPSTREAM_HEADERS: &[&str] = &[
    "cookie",
    "set-cookie",
    "authorization",
    "proxy-authorization",
];

/// Check if an HTTP header name is forbidden when forwarding requests upstream.
pub fn is_forbidden_upstream_header(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    FORBIDDEN_UPSTREAM_HEADERS.iter().any(|&f| f == lower)
}

/// Strip any forbidden headers (cookies, auth) from a header map.
pub fn sanitize_upstream_headers(headers: &mut http::HeaderMap) {
    for forbidden in FORBIDDEN_UPSTREAM_HEADERS {
        headers.remove(*forbidden);
    }
}

/// Check if an IPv4 address belongs to loopback, private, CGNAT, link-local, multicast, or reserved ranges.
pub fn is_blocked_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    // 0.0.0.0/8 (unspecified / current network)
    if octets[0] == 0 {
        return true;
    }
    // 127.0.0.0/8 (loopback)
    if octets[0] == 127 {
        return true;
    }
    // 10.0.0.0/8 (RFC 1918 private)
    if octets[0] == 10 {
        return true;
    }
    // 172.16.0.0/12 (RFC 1918 private)
    if octets[0] == 172 && (16..=31).contains(&octets[1]) {
        return true;
    }
    // 192.168.0.0/16 (RFC 1918 private)
    if octets[0] == 192 && octets[1] == 168 {
        return true;
    }
    // 100.64.0.0/10 (CGNAT / Shared Address Space RFC 6598)
    if octets[0] == 100 && (64..=127).contains(&octets[1]) {
        return true;
    }
    // 169.254.0.0/16 (Link-local RFC 3927)
    if octets[0] == 169 && octets[1] == 254 {
        return true;
    }
    // 192.0.0.0/24 (IETF Protocol Assignments RFC 6890)
    if octets[0] == 192 && octets[1] == 0 && octets[2] == 0 {
        return true;
    }
    // 192.0.2.0/24 (Documentation / TEST-NET-1 RFC 5737)
    if octets[0] == 192 && octets[1] == 0 && octets[2] == 2 {
        return true;
    }
    // 198.18.0.0/15 (Benchmarking RFC 2544)
    if octets[0] == 198 && (18..=19).contains(&octets[1]) {
        return true;
    }
    // 198.51.100.0/24 (Documentation / TEST-NET-2 RFC 5737)
    if octets[0] == 198 && octets[1] == 51 && octets[2] == 100 {
        return true;
    }
    // 203.0.113.0/24 (Documentation / TEST-NET-3 RFC 5737)
    if octets[0] == 203 && octets[1] == 0 && octets[2] == 113 {
        return true;
    }
    // 224.0.0.0/4 (Multicast RFC 5771) and 240.0.0.0/4 (Reserved / Broadcast)
    if ip.is_multicast() || octets[0] >= 224 {
        return true;
    }
    // 255.255.255.255 (Broadcast)
    if ip.is_broadcast() {
        return true;
    }
    false
}

/// Check if an IPv6 address belongs to loopback, unspecified, unique-local, link-local, multicast, or reserved ranges.
pub fn is_blocked_ipv6(ip: Ipv6Addr) -> bool {
    // Unspecified ::
    if ip.is_unspecified() {
        return true;
    }
    // Loopback ::1
    if ip.is_loopback() {
        return true;
    }
    // Multicast ff00::/8
    if ip.is_multicast() {
        return true;
    }
    let segments = ip.segments();
    // Unique local fc00::/7
    if (segments[0] & 0xfe00) == 0xfc00 {
        return true;
    }
    // Link-local unicast fe80::/10
    if (segments[0] & 0xffc0) == 0xfe80 {
        return true;
    }
    // Documentation 2001:db8::/32
    if segments[0] == 0x2001 && segments[1] == 0xdb8 {
        return true;
    }
    // IPv4-mapped IPv6 ::ffff:x.x.x.x
    if segments[0] == 0
        && segments[1] == 0
        && segments[2] == 0
        && segments[3] == 0
        && segments[4] == 0
        && segments[5] == 0xffff
    {
        let octets = ip.octets();
        let v4 = Ipv4Addr::new(octets[12], octets[13], octets[14], octets[15]);
        return is_blocked_ipv4(v4);
    }
    // IPv4-compatible IPv6 ::x.x.x.x (deprecated)
    if segments[0] == 0
        && segments[1] == 0
        && segments[2] == 0
        && segments[3] == 0
        && segments[4] == 0
        && segments[5] == 0
    {
        let octets = ip.octets();
        let v4 = Ipv4Addr::new(octets[12], octets[13], octets[14], octets[15]);
        return is_blocked_ipv4(v4);
    }
    false
}

/// Check if an IP address is blocked under the private/loopback/reserved policy.
pub fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_ipv4(v4),
        IpAddr::V6(v6) => is_blocked_ipv6(v6),
    }
}

/// Destination policy controlling which upstream hosts, schemes, and redirect targets are permitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinationPolicy {
    /// Allowed domain suffixes (e.g. `["music.126.net", "126.net", "163.com"]`). If empty, any host is allowed.
    pub allowed_host_suffixes: Vec<String>,
    /// Whether plain HTTP is allowed.
    pub allow_http: bool,
    /// Whether secure HTTPS is allowed.
    pub allow_https: bool,
    /// Whether private, loopback, link-local, and reserved IP addresses are blocked.
    pub block_private_ips: bool,
    /// Maximum number of redirects to follow manually.
    pub max_redirects: usize,
}

impl DestinationPolicy {
    /// NetEase Cloud Music CDN default policy:
    /// - HTTPS and HTTP enabled
    /// - Suffixes `["music.126.net", "126.net", "163.com"]`
    /// - Private/loopback IP blocking enabled
    /// - Max redirects: 3
    pub fn netease_default() -> Self {
        Self {
            allowed_host_suffixes: vec![
                "music.126.net".to_string(),
                "126.net".to_string(),
                "163.com".to_string(),
            ],
            allow_http: true,
            allow_https: true,
            block_private_ips: true,
            max_redirects: 3,
        }
    }

    /// Policy for local integration tests and fixture servers:
    /// - HTTPS and HTTP enabled
    /// - Permitted host list (e.g. `["127.0.0.1", "localhost"]`)
    /// - Private IP blocking disabled for test harness
    /// - Max redirects: 3
    pub fn local_fixture_for_test(allowed_hosts: Vec<String>) -> Self {
        Self {
            allowed_host_suffixes: allowed_hosts,
            allow_http: true,
            allow_https: true,
            block_private_ips: false,
            max_redirects: 3,
        }
    }

    /// Validate a raw URL string against destination policy constraints.
    pub fn validate_url(&self, raw_url: &str) -> Result<Url, RelayError> {
        let url = Url::parse(raw_url).map_err(|e| {
            RelayError::InvalidRequest(format!("failed to parse URL '{raw_url}': {e}"))
        })?;

        // 1. Scheme check
        match url.scheme() {
            "http" => {
                if !self.allow_http {
                    return Err(RelayError::PolicyViolation(format!(
                        "HTTP scheme is disallowed by policy for URL: {raw_url}"
                    )));
                }
            }
            "https" => {
                if !self.allow_https {
                    return Err(RelayError::PolicyViolation(format!(
                        "HTTPS scheme is disallowed by policy for URL: {raw_url}"
                    )));
                }
            }
            other => {
                return Err(RelayError::PolicyViolation(format!(
                    "unsupported scheme '{other}'; only http and https are allowed"
                )));
            }
        }

        // 2. Reject credentials in URL
        if !url.username().is_empty() || url.password().is_some() {
            return Err(RelayError::PolicyViolation(
                "credentials (username/password) embedded in URL are strictly forbidden".to_string(),
            ));
        }

        // 3. Host presence check
        let host = url.host().ok_or_else(|| {
            RelayError::PolicyViolation(format!("missing host in URL: {raw_url}"))
        })?;

        let host_str = match &host {
            url::Host::Domain(d) => *d,
            url::Host::Ipv4(_) | url::Host::Ipv6(_) => url.host_str().unwrap_or_default(),
        };

        // 4. Private / loopback IP checks
        if self.block_private_ips {
            let host_lower = host_str.to_ascii_lowercase();
            if host_lower == "localhost"
                || host_lower.ends_with(".localhost")
                || host_lower == "local"
                || host_lower.ends_with(".local")
            {
                return Err(RelayError::PolicyViolation(format!(
                    "local/private host '{host_str}' is blocked by policy"
                )));
            }

            match host {
                url::Host::Ipv4(v4) => {
                    if is_blocked_ipv4(v4) {
                        return Err(RelayError::PolicyViolation(format!(
                            "private or reserved IPv4 address '{v4}' is blocked by policy"
                        )));
                    }
                }
                url::Host::Ipv6(v6) => {
                    if is_blocked_ipv6(v6) {
                        return Err(RelayError::PolicyViolation(format!(
                            "private or reserved IPv6 address '{v6}' is blocked by policy"
                        )));
                    }
                }
                url::Host::Domain(domain) => {
                    // Check if domain parses as a literal numeric IP
                    if let Ok(ip) = domain.parse::<IpAddr>() {
                        if is_blocked_ip(ip) {
                            return Err(RelayError::PolicyViolation(format!(
                                "private or reserved IP address '{ip}' in domain is blocked by policy"
                            )));
                        }
                    }
                }
            }
        }

        // 5. Host suffix check
        if !self.allowed_host_suffixes.is_empty() {
            let host_lower = host_str.to_ascii_lowercase();
            let matched = self.allowed_host_suffixes.iter().any(|suffix| {
                let s_lower = suffix.to_ascii_lowercase();
                host_lower == s_lower || host_lower.ends_with(&format!(".{s_lower}"))
            });

            if !matched {
                return Err(RelayError::PolicyViolation(format!(
                    "host '{host_str}' does not match allowed host suffixes: {:?}",
                    self.allowed_host_suffixes
                )));
            }
        }

        Ok(url)
    }
}
