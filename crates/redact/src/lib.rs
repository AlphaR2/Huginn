//! RPC URLs often carry secrets (`?api-key=...`, `user:pass@`, a token in the path).
//! Anything we print, log or write must go through here so that only the host remains.

/// Reduce one URL to `scheme://host[:port]/…`. Text without `://` is returned unchanged.
pub fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Drop any `user:password@` part.
    let host = authority.rsplit('@').next().unwrap_or("");
    let had_more = rest.len() > authority.len();
    format!("{scheme}://{host}{}", if had_more { "/…" } else { "" })
}

/// Replace every URL inside free text (error messages, logs) with its redacted form.
/// ureq and friends put the full URL in their error strings, so call this on any
/// error before it is shown.
pub fn redact_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = find_scheme(rest) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | ')' | ']' | '}' | ','))
            .unwrap_or(tail.len());
        out.push_str(&redact_url(&tail[..end]));
        rest = &tail[end..];
    }
    out.push_str(rest);
    redact_key_values(&out)
}

/// Start index of the next `http://`, `https://`, `ws://` or `wss://`.
fn find_scheme(s: &str) -> Option<usize> {
    ["https://", "http://", "wss://", "ws://"]
        .iter()
        .filter_map(|p| s.find(p))
        .min()
}

/// Blank the value after `api-key=`, `apikey=`, `token=`, `secret=` and `private_key=`
/// when they show up outside a URL (for example in a copied query string).
fn redact_key_values(text: &str) -> String {
    const KEYS: [&str; 6] = ["api-key=", "api_key=", "apikey=", "token=", "secret=", "private_key="];
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let hit = KEYS
            .iter()
            .find(|k| lower[i..].starts_with(*k))
            .copied();
        if let Some(k) = hit {
            out.push_str(&text[i..i + k.len()]);
            out.push_str("[redacted]");
            i += k.len();
            while i < text.len() && !text[i..].starts_with(|c: char| c.is_whitespace() || matches!(c, '&' | '"' | '\'')) {
                i += text[i..].chars().next().map_or(1, char::len_utf8);
            }
        } else {
            let ch = text[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// A `Display` wrapper: `println!("{}", Host(&url))` shows only the host.
pub struct Host<'a>(pub &'a str);

impl std::fmt::Display for Host<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&redact_url(self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_keeps_only_host() {
        assert_eq!(redact_url("https://mainnet.helius-rpc.com/?api-key=SECRET"), "https://mainnet.helius-rpc.com/…");
        assert_eq!(redact_url("https://user:pw@rpc.example.com:8899/v1/TOKEN"), "https://rpc.example.com:8899/…");
        assert_eq!(redact_url("http://127.0.0.1:8899"), "http://127.0.0.1:8899");
        assert_eq!(redact_url("not a url"), "not a url");
    }

    #[test]
    fn text_scrubs_urls_in_errors() {
        let e = "getBlocks request failed: https://x.helius-rpc.com/?api-key=ABC123: status code 401, retry";
        let r = redact_text(e);
        assert!(!r.contains("ABC123"), "{r}");
        assert!(r.contains("https://x.helius-rpc.com/…"), "{r}");
        assert!(r.ends_with("status code 401, retry"), "{r}");
    }

    #[test]
    fn text_scrubs_bare_key_values() {
        let r = redact_text("query was api-key=ABC123&x=1 and Token=zzz");
        assert!(!r.contains("ABC123") && !r.contains("zzz"), "{r}");
        assert!(r.contains("x=1"), "{r}");
    }

    #[test]
    fn multiple_urls_and_unicode() {
        let r = redact_text("a https://a.io/k1 → b (http://b.io:1/k2?x=1) é");
        assert!(!r.contains("k1") && !r.contains("k2"), "{r}");
        assert!(r.contains("é"), "{r}");
    }
}
