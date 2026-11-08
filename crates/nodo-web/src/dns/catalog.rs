//! The provider list behind the Connect a domain screen.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "reason")]
pub enum ProviderStatus {
    Implemented,
    /// The vendor has an API, but its documentation could not be verified.
    NotImplemented(&'static str),
    /// The vendor has no API, so the architecture rules it out.
    NoApi,
}

/// The "HTTPS without open ports" column: whether `nodo-caddy` can answer ACME DNS-01 challenges here.
/// Every bundled module is built and its config accepted by `caddy validate` in the VM tier, but none
/// has issued a certificate in a test, because that needs a real provider account and ACME server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "note")]
pub enum Dns01 {
    /// Uses the same credentials as dynamic DNS.
    Bundled,
    /// The module is bundled, but needs different credentials.
    BundledSeparateCredentials(&'static str),
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProviderInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub status: ProviderStatus,
    /// What the user should know before relying on this provider.
    pub caveat: Option<&'static str>,
    pub docs_url: Option<&'static str>,
    pub dns01: Dns01,
}

pub const CATALOG: &[ProviderInfo] = &[
    ProviderInfo {
        id: "porkbun",
        name: "Porkbun",
        status: ProviderStatus::Implemented,
        caveat: Some("API access must be switched on for the domain in your Porkbun account."),
        docs_url: Some("https://porkbun.com/llms/dns"),
        dns01: Dns01::Bundled,
    },
    ProviderInfo {
        id: "cloudflare",
        name: "Cloudflare",
        status: ProviderStatus::Implemented,
        caveat: None,
        docs_url: Some("https://developers.cloudflare.com/api/resources/dns/subresources/records/"),
        dns01: Dns01::Bundled,
    },
    ProviderInfo {
        id: "godaddy",
        name: "GoDaddy",
        status: ProviderStatus::Implemented,
        caveat: Some("Needs a Personal Access Token, and GoDaddy can refuse some accounts."),
        docs_url: Some("https://developer.godaddy.com/openapi/domains-v3.json"),
        dns01: Dns01::BundledSeparateCredentials(
            "The caddy-dns module uses an API key and secret, not a Personal Access Token.",
        ),
    },
    ProviderInfo {
        id: "namecheap",
        name: "Namecheap",
        status: ProviderStatus::NotImplemented(
            "Namecheap's API documentation still refuses automated reading (HTTP 403 on every method page, \
             checked 2026-11-08), and no other official copy was found. setHosts replaces every host record, \
             so it cannot be written from secondhand descriptions without risking the user's records.",
        ),
        caveat: Some("API access also needs 20 domains, a $50 balance, or $50 spent in two years."),
        docs_url: Some("https://www.namecheap.com/support/api/methods/domains-dns/set-hosts/"),
        dns01: Dns01::Unavailable,
    },
    ProviderInfo {
        id: "ionos",
        name: "IONOS",
        status: ProviderStatus::NotImplemented(
            "The IONOS Hosting DNS reference still renders empty, and its OpenAPI file redirects to a login \
             (checked 2026-11-08). IONOS's help pages link only there. The one readable spec is a 2022 copy \
             in an unverified GitHub organisation, which is not enough to send credentials to.",
        ),
        caveat: None,
        docs_url: Some("https://developer.hosting.ionos.com/docs/dns"),
        dns01: Dns01::Unavailable,
    },
    ProviderInfo {
        id: "gandi",
        name: "Gandi",
        status: ProviderStatus::Implemented,
        caveat: None,
        docs_url: Some("https://api.gandi.net/docs/livedns/"),
        dns01: Dns01::Bundled,
    },
    ProviderInfo {
        id: "namecom",
        name: "Name.com",
        status: ProviderStatus::Implemented,
        caveat: None,
        docs_url: Some("https://docs.name.com/api/v1/reference/dns/list-records.md"),
        dns01: Dns01::Bundled,
    },
    ProviderInfo {
        id: "alibaba",
        name: "Alibaba Cloud",
        status: ProviderStatus::Implemented,
        caveat: None,
        docs_url: Some("https://www.alibabacloud.com/help/en/dns/api-alidns-2015-01-09-overview"),
        dns01: Dns01::Bundled,
    },
    ProviderInfo {
        id: "hostinger",
        name: "Hostinger",
        status: ProviderStatus::Implemented,
        caveat: Some("Dynamic DNS only. Certificates still need ports 80 and 443 open."),
        docs_url: Some("https://docs.hostinger.com/api-reference/endpoints/dns/zone.md"),
        dns01: Dns01::Unavailable,
    },
    ProviderInfo { id: "hover", name: "Hover", status: ProviderStatus::NoApi, caveat: None, docs_url: None, dns01: Dns01::Unavailable },
    ProviderInfo { id: "squarespace", name: "Squarespace", status: ProviderStatus::NoApi, caveat: None, docs_url: None, dns01: Dns01::Unavailable },
];

pub fn lookup(id: &str) -> Option<&'static ProviderInfo> {
    CATALOG.iter().find(|provider| provider.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unimplemented_providers_say_why() {
        for id in ["namecheap", "ionos"] {
            assert!(matches!(lookup(id).unwrap().status, ProviderStatus::NotImplemented(reason) if !reason.is_empty()));
        }
        assert_eq!(lookup("hover").unwrap().status, ProviderStatus::NoApi);
        assert!(lookup("nope").is_none());
    }

    #[test]
    fn dns01_matches_the_bundled_modules() {
        let bundled: Vec<_> = CATALOG.iter().filter(|p| p.dns01 != Dns01::Unavailable).map(|p| p.id).collect();
        assert_eq!(bundled, ["porkbun", "cloudflare", "godaddy", "gandi", "namecom", "alibaba"]);
        assert!(matches!(lookup("godaddy").unwrap().dns01, Dns01::BundledSeparateCredentials(_)));
    }

    #[test]
    fn implemented_providers_cite_their_docs() {
        for provider in CATALOG.iter().filter(|p| p.status == ProviderStatus::Implemented) {
            assert!(provider.docs_url.is_some(), "{}", provider.id);
        }
    }
}
