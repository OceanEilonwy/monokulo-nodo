//! Validation for every name that reaches generated config.
//!
//! Package ids, site names and suggested subdomains come from untrusted feeds. Caddy expands `{...}`
//! placeholders and Tor's config is line based, so the rule is a strict allow-list rather than escaping.

const MAX_LABEL: usize = 63;
const MAX_NAME: usize = 253;

/// Suffixes that can never hold a public certificate or a public DNS record.
const NON_PUBLIC_SUFFIXES: &[&str] =
    &["local", "onion", "localhost", "internal", "lan", "home.arpa", "test", "example", "invalid"];

/// Suffixes the internal CA may issue for.
const LOCAL_SUFFIXES: &[&str] = &["local", "internal", "lan", "home.arpa"];

const ONION_V3_LABEL_LEN: usize = 56;

pub fn is_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_LABEL
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Dotted lowercase ASCII labels. Internationalised names must already be in `xn--` form.
pub fn is_dns_name(value: &str) -> bool {
    value.len() <= MAX_NAME && value.split('.').all(is_label)
}

fn has_suffix(value: &str, suffix: &str) -> bool {
    value == suffix || value.strip_suffix(suffix).is_some_and(|rest| rest.ends_with('.'))
}

/// A name that can sit in public DNS: at least two labels, a non-numeric TLD (so never an IP address),
/// and not under a reserved suffix.
pub fn is_public_name(value: &str) -> bool {
    let tld = value.rsplit('.').next().unwrap_or_default();
    is_dns_name(value)
        && value.contains('.')
        && !tld.bytes().all(|b| b.is_ascii_digit())
        && !NON_PUBLIC_SUFFIXES.iter().any(|suffix| has_suffix(value, suffix))
}

pub fn is_local_name(value: &str) -> bool {
    is_dns_name(value) && value.contains('.') && LOCAL_SUFFIXES.iter().any(|suffix| has_suffix(value, suffix))
}

/// Names that multicast DNS resolves. Other local suffixes need the router's DNS.
pub fn has_mdns_suffix(value: &str) -> bool {
    is_local_name(value) && has_suffix(value, "local")
}

/// A v3 onion address, with no subdomain.
pub fn is_onion_name(value: &str) -> bool {
    value.strip_suffix(".onion").is_some_and(|label| {
        label.len() == ONION_V3_LABEL_LEN
            && label.bytes().all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
    })
}

/// A rejected value quoted safely for an error message, since the value may be hostile.
pub fn quoted(value: &str) -> String {
    let shown: String = value.chars().take(80).collect();
    shown.escape_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONION: &str = "abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrstuvwx.onion";

    #[test]
    fn labels_reject_anything_outside_the_allow_list() {
        for bad in ["", "-a", "a-", "A", "a b", "a.b", "a{b}", "a\nb", "é", "a/b", "a\"b", "*"] {
            assert!(!is_label(bad), "{bad:?}");
        }
        assert!(is_label("monokulo-pay"));
        assert!(!is_label(&"a".repeat(64)));
    }

    #[test]
    fn public_names_exclude_reserved_suffixes_and_addresses() {
        for bad in
            ["localhost", "shop.local", "x.onion", "a.home.arpa", "1.2.3.4", "com", "shop.example.com.", "*.example.com"]
        {
            assert!(!is_public_name(bad), "{bad:?}");
        }
        for good in ["example.com", "shop.example.com", "xn--bcher-kva.de", "notlocal.com"] {
            assert!(is_public_name(good), "{good:?}");
        }
    }

    #[test]
    fn local_names_need_a_local_suffix() {
        assert!(is_local_name("shop.local"));
        assert!(is_local_name("shop.home.arpa"));
        assert!(!is_local_name("shop.example.com"));
        assert!(!is_local_name("local"));
    }

    #[test]
    fn onion_names_are_exactly_v3() {
        assert!(is_onion_name(ONION));
        assert!(!is_onion_name("short.onion"));
        assert!(!is_onion_name(&format!("sub.{ONION}")));
        assert!(!is_onion_name(&ONION.replace('a', "1")));
    }

    #[test]
    fn quoted_neutralises_control_characters() {
        assert_eq!(quoted("a\nb"), "a\\nb");
    }
}
