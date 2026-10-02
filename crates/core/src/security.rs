//! Security primitives — ports of the reference `src/server/security/`:
//!
//! - `sanitize_thread_html` (`sanitize.ts`): allowlist HTML sanitizer for
//!   untrusted email/ticket content. Never allows scripts, event handlers,
//!   javascript: URLs, iframes, or dangerous inline CSS; forces
//!   rel="noopener noreferrer nofollow" target="_blank" on links; drops
//!   data:text/html image sources.
//! - `escape_html` (`sanitize.ts`): server-side text escaping.
//! - SSRF guard (`ssrfGuard.ts`): URL shape + literal-address checks
//!   (loopback, RFC1918, CGNAT 100.64/10, link-local 169.254/16, IPv6
//!   loopback/ULA/link-local, IPv4-mapped IPv6, unspecified, numeric
//!   encodings 0x7f.0.0.1 / 2130706433) + hostname checks (localhost,
//!   *.local, *.internal) + DNS resolution where EVERY resolved address
//!   must be public. Fail-closed.
//! - `redact_text` (`redaction.ts`): the 6-pattern AI prompt redaction.

use std::collections::{HashMap, HashSet};

use regex::Regex;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// HTML sanitizer (sanitize.ts parity)
// ---------------------------------------------------------------------------

const DANGEROUS_CSS: &str = r"(?i)(url\s*\(|@import|expression\s*\(|position\s*:\s*(fixed|absolute)|behavior\s*:|-moz-binding|javascript\s*:)";

/// The reference's allowed tag set.
fn allowed_tags() -> HashSet<&'static str> {
    [
        "a",
        "b",
        "i",
        "em",
        "strong",
        "u",
        "s",
        "strike",
        "code",
        "pre",
        "blockquote",
        "p",
        "br",
        "div",
        "span",
        "ul",
        "ol",
        "li",
        "table",
        "thead",
        "tbody",
        "tr",
        "td",
        "th",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "hr",
        "img",
        "font",
        "center",
        "small",
        "sub",
        "sup",
        "dl",
        "dt",
        "dd",
    ]
    .into_iter()
    .collect()
}

fn tag_attributes() -> HashMap<&'static str, HashSet<&'static str>> {
    let mut m = HashMap::new();
    // NOTE: rel/target on <a> are managed by ammonia itself (link_rel +
    // set_tag_attribute_value) — allowing them in the map trips ammonia's
    // internal assertions.
    m.insert("a", ["href", "name", "title"].into_iter().collect());
    m.insert(
        "img",
        ["src", "alt", "title", "width", "height", "style"]
            .into_iter()
            .collect(),
    );
    m.insert("span", ["style"].into_iter().collect());
    m.insert("div", ["style"].into_iter().collect());
    m.insert("p", ["style"].into_iter().collect());
    m.insert(
        "table",
        ["style", "border", "cellpadding", "cellspacing", "align"]
            .into_iter()
            .collect(),
    );
    m.insert(
        "td",
        ["style", "colspan", "rowspan", "align", "valign"]
            .into_iter()
            .collect(),
    );
    m.insert(
        "th",
        ["style", "colspan", "rowspan", "align", "valign"]
            .into_iter()
            .collect(),
    );
    m.insert("font", ["color", "face", "size"].into_iter().collect());
    m.insert("blockquote", ["style", "cite"].into_iter().collect());
    m
}

/// Per-attribute filter (ammonia): CSS scrubbing + link hardening +
/// data:text/html image removal. Runs for every attribute on every allowed
/// element.
/// Sanitize untrusted thread/ticket HTML (spec #86, #87).
pub fn sanitize_thread_html(dirty: &str) -> String {
    let dangerous_css = Regex::new(DANGEROUS_CSS).expect("static regex");
    let mut builder = ammonia::Builder::new();
    builder.tags(allowed_tags());
    builder.tag_attributes(tag_attributes());
    // Global schemes = reference allowedSchemes; data/cid pass the scheme
    // gate for img but are removed from every other URL attribute by the
    // filter below (allowedSchemesByTag parity).
    builder.url_schemes(
        ["http", "https", "mailto", "tel", "data", "cid"]
            .into_iter()
            .collect(),
    );
    // No protocol-relative URLs (allowProtocolRelative: false).
    builder.url_relative(ammonia::UrlRelative::PassThrough);
    // <a> hardening (simpleTransform parity): ammonia manages rel/target.
    builder.link_rel(Some("noopener noreferrer nofollow"));
    builder.set_tag_attribute_value("a", "target", "_blank");
    builder.attribute_filter(move |element, attribute, value| {
        // CSS scrubbing on every element that carries a style attribute:
        // drop the whole attribute on dangerous constructs; keep benign
        // email layout styling clipped to 2000 chars.
        if attribute == "style" {
            if value.is_empty() || dangerous_css.is_match(value) {
                return None;
            }
            let clipped: String = value.chars().take(2000).collect();
            return Some(std::borrow::Cow::Owned(clipped));
        }
        // img src that is data:text/html is dropped (exclusiveFilter
        // parity — the attribute goes; ammonia leaves an inert <img>).
        if element == "img" && attribute == "src" && value.trim().starts_with("data:text/html") {
            return None;
        }
        // data:/cid: schemes are img-only (allowedSchemesByTag parity).
        if attribute == "href" && element != "img" {
            let lower = value.trim().to_ascii_lowercase();
            if lower.starts_with("data:") || lower.starts_with("cid:") {
                return None;
            }
        }
        Some(std::borrow::Cow::Borrowed(value))
    });
    builder.clean(dirty).to_string()
}

/// Escape text for safe HTML contexts.
pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

// ---------------------------------------------------------------------------
// SSRF guard (ssrfGuard.ts parity)
// ---------------------------------------------------------------------------

/// The result of an SSRF check (reference SsrfCheckResult).
#[derive(Debug, Clone, serde::Serialize)]
pub struct SsrfCheckResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "resolvedAddresses")]
    pub resolved: Vec<String>,
}

impl SsrfCheckResult {
    fn refused(reason: impl Into<String>, host: Option<&str>) -> Self {
        Self {
            ok: false,
            reason: Some(reason.into()),
            host: host.map(|h| h.to_string()),
            resolved: Vec::new(),
        }
    }
}

fn is_ipv4_literal(host: &str) -> bool {
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 3
                && p.chars().all(|c| c.is_ascii_digit())
                && p.parse::<u16>().is_ok_and(|n| n <= 255)
        })
}

/// All IPv4 range checks. Returns a refusal reason or None when public.
fn ipv4_reason(ip: &str) -> Option<String> {
    let parts: Vec<u32> = ip
        .split('.')
        .map(|p| p.parse::<u32>().ok())
        .collect::<Option<Vec<_>>>()?;
    if parts.len() != 4 || parts.iter().any(|p| *p > 255) {
        return Some("malformed IPv4 literal".into());
    }
    let (a, b) = (parts[0], parts[1]);
    Some(match () {
        _ if a == 0 => "0.0.0.0/8 (this-network)".to_string(),
        _ if a == 10 => "10.0.0.0/8 (RFC1918 private)".to_string(),
        _ if a == 127 => "127.0.0.0/8 (loopback)".to_string(),
        _ if a == 169 && b == 254 => "169.254.0.0/16 (link-local / cloud metadata)".to_string(),
        _ if (172..=172).contains(&a) && (16..=31).contains(&b) => {
            "172.16.0.0/12 (RFC1918 private)".to_string()
        }
        _ if a == 192 && b == 168 => "192.168.0.0/16 (RFC1918 private)".to_string(),
        _ if a == 100 && (64..=127).contains(&b) => "100.64.0.0/10 (carrier-grade NAT)".to_string(),
        _ if a == 198 && (b == 18 || b == 19) => "198.18.0.0/15 (benchmarking)".to_string(),
        _ if a >= 224 => format!("{a}.0.0.0/4 (multicast/reserved)"),
        _ => return None,
    })
}

/// Expand an IPv6 literal (may be bracket-stripped) into 8 numeric groups.
fn expand_ipv6(ip: &str) -> Option<Vec<u32>> {
    let raw: String = ip
        .to_ascii_lowercase()
        .trim_matches(|c| c == '[' || c == ']')
        .to_string();
    if !raw.contains(':') {
        return None;
    }
    let halves: Vec<&str> = raw.split("::").collect();
    if halves.len() > 2 {
        return None;
    }
    #[derive(Debug)]
    enum Group {
        Num(u32),
        V4([u32; 4]),
    }
    let parse_groups = |s: &str| -> Option<Vec<Group>> {
        s.split(':')
            .filter(|g| !g.is_empty())
            .map(|g| {
                if g.contains('.') {
                    // IPv4-mapped suffix (e.g. ::ffff:127.0.0.1).
                    let o: Vec<u32> = g
                        .split('.')
                        .map(|p| p.parse::<u32>().ok())
                        .collect::<Option<Vec<_>>>()?;
                    if o.len() != 4 || o.iter().any(|x| *x > 255) {
                        return None;
                    }
                    Some(Group::V4([o[0], o[1], o[2], o[3]]))
                } else {
                    let n = u32::from_str_radix(g, 16).ok()?;
                    if n <= 0xffff {
                        Some(Group::Num(n))
                    } else {
                        None
                    }
                }
            })
            .collect()
    };

    let mut groups: Vec<u32> = Vec::new();
    let emit = |groups: &mut Vec<u32>, g: &Group| match g {
        Group::Num(n) => groups.push(*n),
        Group::V4(o) => {
            groups.push((o[0] << 8) | o[1]);
            groups.push((o[2] << 8) | o[3]);
        }
    };

    if halves.len() == 2 {
        let head = parse_groups(halves[0])?;
        let tail = parse_groups(halves[1])?;
        for g in &head {
            emit(&mut groups, g);
        }
        let tail_slots: usize = tail
            .iter()
            .map(|g| if matches!(g, Group::Num(_)) { 1 } else { 2 })
            .sum();
        if groups.len() + tail_slots > 8 {
            return None;
        }
        while groups.len() + tail_slots < 8 {
            groups.push(0);
        }
        for g in &tail {
            emit(&mut groups, g);
        }
    } else {
        let head = parse_groups(&raw)?;
        for g in &head {
            emit(&mut groups, g);
        }
    }
    if groups.len() == 8 {
        Some(groups)
    } else {
        None
    }
}

fn ipv6_reason(groups: &[u32]) -> Option<String> {
    if groups.len() != 8 {
        return Some("malformed IPv6 literal".into());
    }
    let g0 = groups[0];
    let g5 = groups[5];
    let g6 = groups[6];
    let g7 = groups[7];
    if groups.iter().all(|g| *g == 0) {
        return Some(":: (unspecified address)".into());
    }
    if groups[..7].iter().all(|g| *g == 0) && g7 == 1 {
        return Some("::1 (IPv6 loopback)".into());
    }
    // ::ffff:0:0/96 — IPv4-mapped: check the embedded v4 address.
    if groups[..5].iter().all(|g| *g == 0) && g5 == 0xffff {
        let v4 = format!("{}.{}.{}.{}", g6 >> 8, g6 & 0xff, g7 >> 8, g7 & 0xff);
        if let Some(reason) = ipv4_reason(&v4) {
            return Some(format!("::ffff:{v4} maps to {reason}"));
        }
    }
    if (g0 & 0xfe00) == 0xfc00 {
        return Some("fc00::/7 (IPv6 unique local)".into());
    }
    if (g0 & 0xffc0) == 0xfe80 {
        return Some("fe80::/10 (IPv6 link-local)".into());
    }
    if (g0 & 0xff00) == 0xff00 {
        return Some("ff00::/8 (IPv6 multicast)".into());
    }
    None
}

/// Public check for any literal IP (v4 or v6). None = public/not-an-IP.
pub fn literal_ip_reason(ip: &str) -> Option<String> {
    if is_ipv4_literal(ip) {
        return ipv4_reason(ip);
    }
    if ip.contains(':') {
        return match expand_ipv6(ip) {
            Some(groups) => ipv6_reason(&groups),
            None => Some("unparseable IPv6 literal".into()),
        };
    }
    // Numeric-encoding tricks: pure integer or hex forms resolve to loopback
    // ranges in some stacks — refuse anything all-digits/hex-dots.
    if !ip.is_empty() && ip.chars().all(|c| c.is_ascii_digit()) {
        return Some("numeric-encoded address form".into());
    }
    let hexish = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_hexdigit() || c == '.' || c == 'x')
    };
    if ip.to_ascii_lowercase().starts_with("0x") && hexish(ip) {
        return Some("numeric-encoded address form".into());
    }
    // Also catch 0x-prefixed mixed forms like 0x7f.0.0.1 (handled above) and
    // bare hex quads like 7f000001.
    if !ip.is_empty() && ip.chars().all(|c| c.is_ascii_hexdigit()) && ip.len() >= 7 {
        return Some("numeric-encoded address form".into());
    }
    None
}

/// Hostname-shaped checks (no DNS): localhost and internal naming.
fn hostname_reason(host: &str) -> Option<String> {
    let h = host.to_ascii_lowercase();
    let h = h.trim_matches(|c| c == '[' || c == ']');
    if h == "localhost" || h.ends_with(".localhost") {
        return Some(format!("\"{h}\" is a localhost name"));
    }
    if h.ends_with(".local") || h.ends_with(".internal") {
        return Some(format!("\"{h}\" looks like an internal name"));
    }
    None
}

/// Synchronous layer: URL shape + literal IP + hostname checks.
pub fn check_url_literal(raw_url: &str) -> SsrfCheckResult {
    let parsed = match url::Url::parse(raw_url) {
        Ok(u) => u,
        Err(e) => return SsrfCheckResult::refused(format!("invalid URL: {e}"), None),
    };
    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return SsrfCheckResult::refused(
                format!("protocol \"{other}:\" is not allowed (http/https only)"),
                None,
            )
        }
    }
    let host_raw = match parsed.host_str() {
        Some(h) if !h.is_empty() => h.to_string(),
        _ => return SsrfCheckResult::refused("URL has no host", None),
    };
    let host = host_raw.trim_matches(|c| c == '[' || c == ']').to_string();
    if let Some(reason) = literal_ip_reason(&host) {
        return SsrfCheckResult::refused(format!("refused {reason}"), Some(&host));
    }
    if let Some(reason) = hostname_reason(&host) {
        return SsrfCheckResult::refused(reason, Some(&host));
    }
    SsrfCheckResult {
        ok: true,
        reason: None,
        host: Some(host),
        resolved: Vec::new(),
    }
}

/// Full check including DNS resolution: every resolved address must be
/// public. Fail-closed on resolver errors (a name that cannot resolve is
/// refused, not allowed).
pub async fn check_url_resolved(raw_url: &str) -> SsrfCheckResult {
    let literal = check_url_literal(raw_url);
    if !literal.ok {
        return literal;
    }
    let host = literal.host.clone().unwrap_or_default();
    // Literal public IPs need no DNS pass.
    if is_ipv4_literal(&host) || host.contains(':') {
        return literal;
    }
    let host_for_dns = host.clone();
    let addresses = tokio::task::spawn_blocking(move || {
        use std::net::ToSocketAddrs;
        (host_for_dns.as_str(), 0u16)
            .to_socket_addrs()
            .ok()
            .map(|it| it.map(|a| a.ip().to_string()).collect::<Vec<String>>())
    })
    .await
    .unwrap_or(None);
    let Some(addresses) = addresses else {
        return SsrfCheckResult::refused(format!("host \"{host}\" does not resolve"), Some(&host));
    };
    if addresses.is_empty() {
        return SsrfCheckResult::refused(
            format!("host \"{host}\" resolved to no addresses"),
            Some(&host),
        );
    }
    for addr in &addresses {
        if let Some(reason) = literal_ip_reason(addr) {
            return SsrfCheckResult {
                ok: false,
                reason: Some(format!("host \"{host}\" resolves to {addr} ({reason})")),
                host: Some(host),
                resolved: addresses,
            };
        }
    }
    SsrfCheckResult {
        ok: true,
        reason: None,
        host: Some(host),
        resolved: addresses,
    }
}

/// Sync wrapper over the full check (create-time + fetch-time gate).
pub fn validate_ssrf_full(raw_url: &str) -> crate::error::Result<()> {
    let literal = check_url_literal(raw_url);
    if !literal.ok {
        return Err(crate::error::Error::Config(format!(
            "SSRF guard: {}",
            literal.reason.unwrap_or_else(|| "refused".into())
        )));
    }
    let host = literal.host.clone().unwrap_or_default();
    // Literal public IPs pass without DNS.
    if is_ipv4_literal(&host) || host.contains(':') {
        return Ok(());
    }
    use std::net::ToSocketAddrs;
    match (host.as_str(), 0u16).to_socket_addrs() {
        Err(_) => {
            return Err(crate::error::Error::Config(format!(
                "SSRF guard: host \"{host}\" does not resolve"
            )))
        }
        Ok(addrs) => {
            let list: Vec<std::net::SocketAddr> = addrs.collect();
            if list.is_empty() {
                return Err(crate::error::Error::Config(format!(
                    "SSRF guard: host \"{host}\" resolved to no addresses"
                )));
            }
            for a in &list {
                if let Some(reason) = literal_ip_reason(&a.ip().to_string()) {
                    return Err(crate::error::Error::Config(format!(
                        "SSRF guard: host \"{host}\" resolves to {} ({reason})",
                        a.ip()
                    )));
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// AI redaction (redaction.ts parity)
// ---------------------------------------------------------------------------

/// A redaction pattern (name, regex, replacement).
pub struct RedactionPattern {
    pub name: &'static str,
    pub regex: Regex,
    pub replacement: &'static str,
    /// Prefix words kept in the replacement for readability (cvv:/api_key:...).
    pub prefix: Option<Regex>,
}

/// The reference's 6 patterns.
pub fn redaction_patterns() -> Vec<RedactionPattern> {
    vec![
        RedactionPattern {
            name: "card_number",
            regex: Regex::new(r"\b(?:\d[ -]*?){13,16}\b").unwrap(),
            replacement: "[REDACTED-CARD]",
            prefix: None,
        },
        RedactionPattern {
            name: "cvv",
            regex: Regex::new(r"(?i)\b(?:cvv|cvc|security code)[\s:=-]*(\d{3,4})\b").unwrap(),
            replacement: "[REDACTED-CVV]",
            prefix: Some(Regex::new(r"(?i)^[\s]*(?:cvv|cvc|security code)?[\s:=-]*").unwrap()),
        },
        RedactionPattern {
            name: "api_key",
            regex: Regex::new(r"(?i)\b(?:api[_-]?key|token|secret|password|passwd|pwd)\b[\s:=-]*[A-Za-z0-9_\-.]{8,}").unwrap(),
            replacement: "[REDACTED-SECRET]",
            prefix: Some(Regex::new(r"(?i)^[\s]*(?:api[_-]?key|token|secret|password|passwd|pwd)?[\s:=-]*").unwrap()),
        },
        RedactionPattern {
            name: "bearer",
            regex: Regex::new(r"(?i)\bBearer\s+[A-Za-z0-9\-._~+/]+=*").unwrap(),
            replacement: "[REDACTED-TOKEN]",
            prefix: None,
        },
        RedactionPattern {
            name: "aws_key",
            regex: Regex::new(r"\bAKIA[0-9A-Z]{16}\b").unwrap(),
            replacement: "[REDACTED-AWS-KEY]",
            prefix: None,
        },
        RedactionPattern {
            name: "private_key_block",
            regex: Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----").unwrap(),
            replacement: "[REDACTED-PRIVATE-KEY]",
            prefix: None,
        },
    ]
}

/// Redact secrets from AI prompts (spec #127). Returns the scrubbed text and
/// per-pattern counts (reference RedactionResult).
pub fn redact_text(text: &str, enabled: bool) -> (String, Vec<Value>) {
    if !enabled {
        return (text.to_string(), Vec::new());
    }
    let mut out = text.to_string();
    let mut redactions: Vec<Value> = Vec::new();
    for p in redaction_patterns() {
        let matches: Vec<&str> = p.regex.find_iter(&out).map(|m| m.as_str()).collect();
        if !matches.is_empty() {
            let count = matches.len();
            let prefix = p.prefix.clone();
            let replacement = p.replacement;
            out = p
                .regex
                .replace_all(&out, |caps: &regex::Captures<'_>| {
                    let whole = caps.get(0).map(|m| m.as_str()).unwrap_or_default();
                    let keep = prefix
                        .as_ref()
                        .and_then(|pre| pre.find(whole).map(|m| m.as_str().to_string()))
                        .unwrap_or_default();
                    format!("{keep}{replacement}")
                })
                .to_string();
            redactions.push(json!({ "pattern": p.name, "count": count }));
        }
    }
    (out, redactions)
}

// ---------------------------------------------------------------------------
// Tests — the reference's security test matrices
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------- Sanitizer ----------------

    #[test]
    fn sanitizer_strips_script_tags() {
        let out = sanitize_thread_html("<script>alert('xss')</script><p>hello</p>");
        assert!(!out.contains("script"));
        assert!(!out.contains("alert"));
        assert!(out.contains("hello"));
    }

    #[test]
    fn sanitizer_strips_event_handlers() {
        let out = sanitize_thread_html("<p onclick=\"evil()\">text</p>");
        assert!(out.contains("text"));
        assert!(!out.contains("onclick"));
    }

    #[test]
    fn sanitizer_strips_javascript_urls() {
        let out = sanitize_thread_html("<a href=\"javascript:alert(1)\">click</a>");
        assert!(out.contains("click"));
        assert!(!out.contains("javascript"));
    }

    #[test]
    fn sanitizer_hardens_links() {
        let out = sanitize_thread_html("<a href=\"https://example.com\">x</a>");
        assert!(out.contains("rel=\"noopener noreferrer nofollow\""));
        assert!(out.contains("target=\"_blank\""));
    }

    #[test]
    fn sanitizer_keeps_benign_formatting() {
        let out = sanitize_thread_html(
            "<p style=\"color: red; text-align: center\">hi</p><b>bold</b><ul><li>item</li></ul><a href=\"https://good.example\">link</a>",
        );
        assert!(out.contains("color: red"));
        assert!(out.contains("<b>bold</b>"));
        assert!(out.contains("<li>item</li>"));
        assert!(out.contains("https://good.example"));
    }

    #[test]
    fn sanitizer_drops_dangerous_css() {
        let out = sanitize_thread_html("<div style=\"position: fixed; top: 0\">overlay</div>");
        assert!(!out.contains("position"), "{out}");
        let out = sanitize_thread_html(
            "<span style=\"background: url(https://track.example/x)\">x</span>",
        );
        assert!(!out.contains("url("), "{out}");
    }

    #[test]
    fn sanitizer_drops_data_text_html_images() {
        let out = sanitize_thread_html(
            "<img src=\"data:text/html,<script>alert(1)</script>\" alt=\"x\">",
        );
        assert!(!out.contains("data:text/html"), "{out}");
    }

    #[test]
    fn sanitizer_keeps_http_images() {
        let out = sanitize_thread_html("<img src=\"https://cdn.example/x.png\" alt=\"x\">");
        assert!(out.contains("https://cdn.example/x.png"));
    }

    #[test]
    fn sanitizer_strips_iframes() {
        let out = sanitize_thread_html("<iframe src=\"https://evil.example\"></iframe><p>ok</p>");
        assert!(!out.contains("iframe"));
        assert!(out.contains("ok"));
    }

    #[test]
    fn escape_html_escapes_all_five() {
        assert_eq!(
            escape_html(r#"<a href="x">&'</a>"#),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;&lt;/a&gt;"
        );
    }

    // ---------------- SSRF guard ----------------

    #[test]
    fn ssrf_blocks_loopback_and_private() {
        for url in [
            "http://127.0.0.1/x",
            "http://localhost/x",
            "http://10.0.0.1/x",
            "http://172.16.0.1/x",
            "http://172.31.255.255/x",
            "http://192.168.1.1/x",
            "http://169.254.169.254/latest/meta-data",
            "http://100.64.0.1/x",
            "http://0.0.0.0/x",
            "http://198.18.0.1/x",
            "http://224.0.0.1/x",
            "http://[::1]/x",
            "http://[fe80::1]/x",
            "http://[fc00::1]/x",
            "http://[::ffff:127.0.0.1]/x",
            "http://[::]/x",
            "http://2130706433/x",
            "http://0x7f.0.0.1/x",
            "http://service.internal/x",
            "http://printer.local/x",
            "http://app.localhost/x",
        ] {
            let r = check_url_literal(url);
            assert!(!r.ok, "should refuse {url}: {:?}", r.reason);
        }
    }

    #[test]
    fn ssrf_allows_public_literals() {
        for url in [
            "https://example.com/x",
            "http://8.8.8.8/x",
            "http://93.184.216.34/x",
            "https://api.helpscout.net/v2/users",
        ] {
            let r = check_url_literal(url);
            assert!(r.ok, "should allow {url}: {:?}", r.reason);
        }
    }

    #[test]
    fn ssrf_refuses_non_http_schemes() {
        for url in ["file:///etc/passwd", "ftp://example.com/x", "gopher://x"] {
            let r = check_url_literal(url);
            assert!(!r.ok, "{url}");
            assert!(r
                .reason
                .as_deref()
                .unwrap_or_default()
                .contains("http/https only"));
        }
    }

    #[test]
    fn ssrf_refuses_garbage() {
        assert!(!check_url_literal("not a url at all").ok);
        assert!(!check_url_literal("http://").ok);
    }

    #[test]
    fn ssrf_ipv4_mapped_ipv6_blocked() {
        let r = check_url_literal("http://[::ffff:10.0.0.1]/x");
        assert!(!r.ok);
        assert!(r.reason.unwrap().contains("RFC1918"));
    }

    #[test]
    fn ssrf_resolution_fail_closed() {
        // A name that cannot resolve is refused, not allowed.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let r = rt.block_on(check_url_resolved(
            "http://this-domain-does-not-exist-xyz.invalid/x",
        ));
        assert!(!r.ok);
        assert!(r.reason.unwrap().contains("does not resolve"));
    }

    #[test]
    fn ssrf_resolved_public_host_passes() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let r = rt.block_on(check_url_resolved("https://example.com/"));
        assert!(r.ok, "{r:?}");
        assert!(!r.resolved.is_empty());
    }

    #[test]
    fn ssrf_validate_full_matches_literal_for_public_ips() {
        assert!(validate_ssrf_full("http://93.184.216.34/x").is_ok());
        assert!(validate_ssrf_full("http://127.0.0.1/x").is_err());
    }

    #[test]
    fn ssrf_localhost_literal_needs_no_dns() {
        // Even without network, literal checks are synchronous + fail-closed.
        let r = check_url_literal("http://127.0.0.1:3000/api");
        assert!(!r.ok);
        assert!(r.reason.unwrap().contains("loopback"));
    }

    // ---------------- Redaction ----------------

    #[test]
    fn redaction_cards() {
        let (text, redactions) = redact_text("My card is 4111 1111 1111 1111 thanks", true);
        assert!(text.contains("[REDACTED-CARD]"), "{text}");
        assert!(!text.contains("4111"));
        assert_eq!(redactions[0]["pattern"], json!("card_number"));
    }

    #[test]
    fn redaction_cvv_keeps_prefix() {
        let (text, _) = redact_text("cvv: 1234", true);
        assert!(text.contains("cvv:"), "{text}");
        assert!(text.contains("[REDACTED-CVV]"), "{text}");
        assert!(!text.contains("1234"));
    }

    #[test]
    fn redaction_api_keys() {
        let (text, redactions) =
            redact_text("use api_key = sk_live_abcdefghijklmnop then retry", true);
        assert!(text.contains("[REDACTED-SECRET]"), "{text}");
        assert!(!text.contains("sk_live_abcdefghijklmnop"));
        assert!(redactions.iter().any(|r| r["pattern"] == json!("api_key")));
    }

    #[test]
    fn redaction_bearer_tokens() {
        let (text, _) = redact_text("Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.x.y", true);
        assert!(text.contains("[REDACTED-TOKEN]"), "{text}");
        assert!(!text.contains("eyJhbGci"));
    }

    #[test]
    fn redaction_aws_keys() {
        let (text, _) = redact_text("key AKIAIOSFODNN7EXAMPLE here", true);
        assert!(text.contains("[REDACTED-AWS-KEY]"), "{text}");
    }

    #[test]
    fn redaction_private_key_blocks() {
        let (text, _) = redact_text(
            "signing key:\n-----BEGIN RSA PRIVATE KEY-----\nMIIEpA\n-----END RSA PRIVATE KEY-----\nbye",
            true,
        );
        assert!(text.contains("[REDACTED-PRIVATE-KEY]"), "{text}");
        assert!(!text.contains("MIIEpA"));
        assert!(!text.contains("BEGIN RSA"));
    }

    #[test]
    fn redaction_disabled_passthrough() {
        let (text, redactions) = redact_text("card 4111 1111 1111 1111", false);
        assert!(text.contains("4111"));
        assert!(redactions.is_empty());
    }

    #[test]
    fn redaction_clean_text_untouched() {
        let (text, redactions) = redact_text("Hello, my printer does not work. Please help!", true);
        assert_eq!(text, "Hello, my printer does not work. Please help!");
        assert!(redactions.is_empty());
    }
}
