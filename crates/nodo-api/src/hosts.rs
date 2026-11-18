//! A site's public names: the primary host the owner confirmed, and the other names it also answers
//! at. The rules for those other names live here, so the helper and the TUI's test backend refuse
//! exactly the same things with the same words.

use serde::{Deserialize, Serialize};

/// What a name other than the primary does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AliasMode {
    /// The same site, served at this name too.
    Serve,
    /// A permanent redirect (308) to the primary host, keeping the path and query.
    Redirect,
}

impl AliasMode {
    pub fn label(self) -> &'static str {
        match self {
            AliasMode::Serve => "serve",
            AliasMode::Redirect => "redirect",
        }
    }
}

/// Another public name for a site, such as the bare `example.com` beside `www.example.com`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PublicAlias {
    pub host: String,
    pub mode: AliasMode,
}

/// Each name is one Caddy route, one DNS record and one certificate, so a site keeps to a few.
pub const MAX_ALIASES: usize = 16;

/// A DNS name of at least two labels. Lowercase only, so the typed confirmation compares exactly
/// with what was stored.
pub fn valid_public_host(host: &str) -> bool {
    let labels: Vec<&str> = host.split('.').collect();
    host.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
        && !labels.last().is_some_and(|tld| tld.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `host` is `domain` or a name under it, matched whole labels at a time.
pub fn host_under(host: &str, domain: &str) -> bool {
    host == domain || host.strip_suffix(domain).is_some_and(|rest| rest.ends_with('.'))
}

/// Checks the names a site would also answer at.
///
/// `owned` are the domains of the connected providers. An alias must be under one, because nothing
/// else can publish its DNS record or answer its DNS-01 certificate challenge. `taken` are the
/// names other sites already use, as `(host, site)`, primaries and aliases both.
pub fn check_aliases(
    primary: &str,
    aliases: &[PublicAlias],
    owned: &[String],
    taken: &[(String, String)],
) -> Result<(), String> {
    if aliases.len() > MAX_ALIASES {
        return Err(format!("a site can answer at {MAX_ALIASES} other names at most"));
    }
    for (index, alias) in aliases.iter().enumerate() {
        let host = alias.host.as_str();
        if !valid_public_host(host) {
            return Err(format!(
                "{} is not a full lowercase domain name, such as shop.example.com",
                host.escape_debug()
            ));
        }
        if host == primary {
            return Err(format!("{host} is already the site's main address"));
        }
        if aliases[..index].iter().any(|earlier| earlier.host == host) {
            return Err(format!("{host} is listed twice"));
        }
        if let Some((_, site)) = taken.iter().find(|(name, _)| name == host) {
            return Err(format!("{host} is already an address of {site}"));
        }
        if !owned.iter().any(|domain| host_under(host, domain)) {
            return Err(format!(
                "{host} is not under a domain of a connected provider, so its record and certificate \
                 can't be set up. Connect the provider that holds it first"
            ));
        }
    }
    if let Some((_, site)) = taken.iter().find(|(name, _)| name == primary) {
        return Err(format!("{primary} is already an address of {site}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alias(host: &str, mode: AliasMode) -> PublicAlias {
        PublicAlias { host: host.to_owned(), mode }
    }

    fn owned() -> Vec<String> {
        vec!["monokulo.com".to_owned(), "hollingworth.xyz".to_owned()]
    }

    #[test]
    fn an_apex_redirect_and_a_name_under_a_second_domain_are_accepted() {
        let aliases = [alias("monokulo.com", AliasMode::Redirect), alias("pay.hollingworth.xyz", AliasMode::Serve)];
        assert_eq!(check_aliases("www.monokulo.com", &aliases, &owned(), &[]), Ok(()));
        assert_eq!(check_aliases("www.monokulo.com", &[], &[], &[]), Ok(()));
    }

    #[test]
    fn bad_names_duplicates_and_the_primary_are_refused() {
        let check = |aliases: &[PublicAlias]| check_aliases("www.monokulo.com", aliases, &owned(), &[]);
        for bad in ["", "Monokulo.com", "monokulo", "a..monokulo.com", "-a.monokulo.com", "1.2.3.4"] {
            assert!(check(&[alias(bad, AliasMode::Serve)]).is_err(), "{bad}");
        }
        let primary = check(&[alias("www.monokulo.com", AliasMode::Redirect)]).unwrap_err();
        assert!(primary.contains("already the site's main address"), "{primary}");
        let twice = check(&[alias("monokulo.com", AliasMode::Redirect), alias("monokulo.com", AliasMode::Serve)]).unwrap_err();
        assert!(twice.contains("listed twice"), "{twice}");
        let many: Vec<PublicAlias> = (0..=MAX_ALIASES).map(|i| alias(&format!("a{i}.monokulo.com"), AliasMode::Serve)).collect();
        assert!(check(&many).is_err());
    }

    #[test]
    fn a_name_outside_the_connected_domains_is_refused_with_the_reason() {
        let error = check_aliases("www.monokulo.com", &[alias("monokulo.net", AliasMode::Redirect)], &owned(), &[]).unwrap_err();
        assert!(error.contains("not under a domain of a connected provider"), "{error}");
        // Whole labels only: notmonokulo.com is not under monokulo.com.
        assert!(check_aliases("www.monokulo.com", &[alias("notmonokulo.com", AliasMode::Serve)], &owned(), &[]).is_err());
    }

    #[test]
    fn names_of_other_sites_are_refused_both_ways() {
        let taken = [("monokulo.com".to_owned(), "shop/web".to_owned())];
        let error = check_aliases("www.monokulo.com", &[alias("monokulo.com", AliasMode::Redirect)], &owned(), &taken).unwrap_err();
        assert_eq!(error, "monokulo.com is already an address of shop/web");
        assert!(check_aliases("monokulo.com", &[], &owned(), &taken).is_err(), "a primary another site answers at");
    }

    #[test]
    fn modes_read_and_write_as_lowercase_words() {
        assert_eq!(AliasMode::Redirect.label(), "redirect");
        assert_eq!(AliasMode::Serve.label(), "serve");
    }
}
