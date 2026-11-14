//! Provider selection and credentials as stored in the DDNS config.

use super::{alibaba, cloudflare, gandi, godaddy, hostinger, namecom, porkbun, DnsError, DnsProvider, Zone};
use crate::http::SharedHttp;
use crate::secret::Secret;
use serde::Deserialize;

/// Debug output of this type is safe to log, because every credential is a [`Secret`].
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "provider", rename_all = "lowercase", deny_unknown_fields)]
pub enum ProviderConfig {
    Porkbun { api_key: Secret, secret_key: Secret },
    Cloudflare { api_token: Secret },
    GoDaddy { personal_access_token: Secret },
    Gandi { personal_access_token: Secret },
    NameCom { username: String, api_token: Secret },
    Alibaba { access_key_id: Secret, access_key_secret: Secret },
    Hostinger { api_token: Secret },
}

/// Listing needs no zone, but providers are built for one. This placeholder is never contacted.
const LISTING_ZONE: &str = "example.com";

impl ProviderConfig {
    /// The catalog id, such as `cloudflare`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Porkbun { .. } => "porkbun",
            Self::Cloudflare { .. } => "cloudflare",
            Self::GoDaddy { .. } => "godaddy",
            Self::Gandi { .. } => "gandi",
            Self::NameCom { .. } => "namecom",
            Self::Alibaba { .. } => "alibaba",
            Self::Hostinger { .. } => "hostinger",
        }
    }

    /// The domains these credentials control, or `None` when the provider cannot list them.
    pub fn list_domains(self, http: SharedHttp) -> Result<Option<Vec<String>>, DnsError> {
        self.build(http, LISTING_ZONE)?.list_domains()
    }

    pub fn build(self, http: SharedHttp, domain: &str) -> Result<Box<dyn DnsProvider>, DnsError> {
        let zone = Zone::new(domain)?;
        Ok(match self {
            Self::Porkbun { api_key, secret_key } => Box::new(porkbun::Porkbun::new(http, zone, api_key, secret_key)),
            Self::Cloudflare { api_token } => Box::new(cloudflare::Cloudflare::new(http, zone, api_token)),
            Self::GoDaddy { personal_access_token } => {
                Box::new(godaddy::GoDaddy::new(http, zone, personal_access_token))
            }
            Self::Gandi { personal_access_token } => Box::new(gandi::Gandi::new(http, zone, personal_access_token)),
            Self::NameCom { username, api_token } => {
                Box::new(namecom::NameCom::new(http, zone, &username, api_token))
            }
            Self::Alibaba { access_key_id, access_key_secret } => {
                Box::new(alibaba::Alibaba::new(http, zone, access_key_id, access_key_secret))
            }
            Self::Hostinger { api_token } => Box::new(hostinger::Hostinger::new(http, zone, api_token)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake::FakeHttp;

    #[test]
    fn parses_and_never_prints_credentials() {
        let config: ProviderConfig =
            toml::from_str("provider = \"porkbun\"\napi_key = \"pk1_abc\"\nsecret_key = \"sk1_def\"\n").unwrap();
        let shown = format!("{config:?}");
        assert!(!shown.contains("pk1_abc") && !shown.contains("sk1_def"), "{shown}");
        assert!(config.build(FakeHttp::new(), "example.com").is_ok());
    }

    #[test]
    fn unknown_providers_and_fields_are_rejected() {
        assert!(toml::from_str::<ProviderConfig>("provider = \"namecheap\"\napi_key = \"x\"\n").is_err());
        assert!(toml::from_str::<ProviderConfig>("provider = \"cloudflare\"\napi_token = \"x\"\nz = 1\n").is_err());
    }

    #[test]
    fn a_hostile_domain_is_rejected() {
        let config: ProviderConfig = toml::from_str("provider = \"cloudflare\"\napi_token = \"x\"\n").unwrap();
        assert!(matches!(config.build(FakeHttp::new(), "a/b.com\n"), Err(DnsError::InvalidName(_))));
    }

    #[test]
    fn kinds_match_the_catalog_and_its_listing_flag() {
        use crate::dns::catalog::lookup;
        let forms = [
            "provider = \"porkbun\"\napi_key = \"a\"\nsecret_key = \"b\"\n",
            "provider = \"cloudflare\"\napi_token = \"a\"\n",
            "provider = \"godaddy\"\npersonal_access_token = \"a\"\n",
            "provider = \"gandi\"\npersonal_access_token = \"a\"\n",
            "provider = \"namecom\"\nusername = \"u\"\napi_token = \"a\"\n",
            "provider = \"alibaba\"\naccess_key_id = \"a\"\naccess_key_secret = \"b\"\n",
            "provider = \"hostinger\"\napi_token = \"a\"\n",
        ];
        for form in forms {
            let config: ProviderConfig = toml::from_str(form).unwrap();
            let info = lookup(config.kind()).unwrap();
            // A provider that cannot list answers None without contacting anyone.
            let http = FakeHttp::new();
            if !info.lists_domains {
                assert_eq!(config.list_domains(http.clone()).unwrap(), None, "{}", info.id);
                assert!(http.requests().is_empty());
            }
        }
    }

    #[test]
    fn every_implemented_catalog_entry_has_a_config_form() {
        use crate::dns::catalog::{ProviderStatus, CATALOG};
        for provider in CATALOG.iter().filter(|p| p.status == ProviderStatus::Implemented) {
            let text = format!("provider = \"{}\"\n", provider.id);
            let error = toml::from_str::<ProviderConfig>(&text).unwrap_err().to_string();
            assert!(error.contains("missing field"), "{}: {error}", provider.id);
        }
    }
}
