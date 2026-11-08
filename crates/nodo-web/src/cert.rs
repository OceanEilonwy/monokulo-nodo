//! Just enough X.509 to report a certificate on the Web screens: who issued it, when it is valid,
//! and which names it covers. The input is Caddy's own storage, so this reads and never verifies.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertInfo {
    /// Unix seconds.
    pub not_before: u64,
    pub not_after: u64,
    pub issuer_cn: Option<String>,
    pub issuer_org: Option<String>,
    pub dns_names: Vec<String>,
}

impl CertInfo {
    /// The issuer as people know it, such as "Let's Encrypt (R11)".
    pub fn issuer(&self) -> String {
        match (&self.issuer_org, &self.issuer_cn) {
            (Some(org), Some(cn)) if org != cn => format!("{org} ({cn})"),
            (Some(name), _) | (None, Some(name)) => name.clone(),
            (None, None) => "unknown issuer".to_owned(),
        }
    }

    /// Exact names and one level of wildcard, as TLS clients match them.
    pub fn covers(&self, host: &str) -> bool {
        self.dns_names.iter().any(|name| {
            name.eq_ignore_ascii_case(host)
                || name.strip_prefix("*.").is_some_and(|base| {
                    host.split_once('.').is_some_and(|(_, rest)| rest.eq_ignore_ascii_case(base))
                })
        })
    }
}

/// The first certificate of a PEM chain, which is the leaf in what an ACME server returns.
pub fn parse_pem(pem: &str) -> Option<CertInfo> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    let start = pem.find(BEGIN)? + BEGIN.len();
    let end = start + pem[start..].find(END)?;
    let body: String = pem[start..end].chars().filter(|c| !c.is_whitespace()).collect();
    parse_der(&STANDARD.decode(body).ok()?)
}

pub fn parse_der(der: &[u8]) -> Option<CertInfo> {
    let (tag, certificate, _) = tlv(der)?;
    expect(tag, SEQUENCE)?;
    let (tag, tbs, _) = tlv(certificate)?;
    expect(tag, SEQUENCE)?;
    // The version is optional and tagged; the serial number follows it.
    let (mut tag, _, mut next) = tlv(tbs)?;
    if tag == CONTEXT_0 {
        (tag, _, next) = tlv(next)?;
    }
    expect(tag, INTEGER)?;
    let (_, _, after_alg) = tlv(next)?;
    let (tag, issuer, after_issuer) = tlv(after_alg)?;
    expect(tag, SEQUENCE)?;
    let (tag, validity, after_validity) = tlv(after_issuer)?;
    expect(tag, SEQUENCE)?;
    let (tag_a, not_before, after_nb) = tlv(validity)?;
    let (tag_b, not_after, _) = tlv(after_nb)?;
    let (_, _, after_subject) = tlv(after_validity)?;
    let (_, _, mut optional) = tlv(after_subject)?;
    let mut dns_names = Vec::new();
    while !optional.is_empty() {
        let (tag, content, next) = tlv(optional)?;
        if tag == CONTEXT_3 {
            dns_names = subject_alt_names(content)?;
        }
        optional = next;
    }
    Some(CertInfo {
        not_before: time(tag_a, not_before)?,
        not_after: time(tag_b, not_after)?,
        issuer_cn: name_attribute(issuer, OID_CN),
        issuer_org: name_attribute(issuer, OID_ORG),
        dns_names,
    })
}

const INTEGER: u8 = 0x02;
const OCTET_STRING: u8 = 0x04;
const OID: u8 = 0x06;
const SEQUENCE: u8 = 0x30;
const SET: u8 = 0x31;
const UTC_TIME: u8 = 0x17;
const GENERALIZED_TIME: u8 = 0x18;
const CONTEXT_0: u8 = 0xa0;
const CONTEXT_3: u8 = 0xa3;
const DNS_NAME: u8 = 0x82;
const OID_CN: &[u8] = &[0x55, 0x04, 0x03];
const OID_ORG: &[u8] = &[0x55, 0x04, 0x0a];
const OID_SAN: &[u8] = &[0x55, 0x1d, 0x11];

fn expect(tag: u8, want: u8) -> Option<()> {
    (tag == want).then_some(())
}

/// One DER element: its tag, its content and what follows it. Only single-byte tags occur here.
fn tlv(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 4 || rest.len() < count {
            return None;
        }
        let len = rest[..count].iter().fold(0usize, |acc, b| (acc << 8) | usize::from(*b));
        (len, &rest[count..])
    };
    (rest.len() >= len).then(|| (tag, &rest[..len], &rest[len..]))
}

fn name_attribute(name: &[u8], oid: &[u8]) -> Option<String> {
    let mut sets = name;
    let mut found = None;
    while !sets.is_empty() {
        let (tag, set, next) = tlv(sets)?;
        sets = next;
        if tag != SET {
            continue;
        }
        let (_, pair, _) = tlv(set)?;
        let (tag, id, value) = tlv(pair)?;
        if tag == OID && id == oid {
            let (_, text, _) = tlv(value)?;
            // The most specific attribute comes last in a distinguished name.
            found = Some(String::from_utf8_lossy(text).into_owned());
        }
    }
    found
}

fn subject_alt_names(explicit: &[u8]) -> Option<Vec<String>> {
    let (_, extensions, _) = tlv(explicit)?;
    let mut rest = extensions;
    while !rest.is_empty() {
        let (_, extension, next) = tlv(rest)?;
        rest = next;
        let (tag, id, mut fields) = tlv(extension)?;
        if tag != OID || id != OID_SAN {
            continue;
        }
        // A critical flag may sit before the value.
        let (mut tag, mut value, after) = tlv(fields)?;
        if tag != OCTET_STRING {
            fields = after;
            (tag, value, _) = tlv(fields)?;
        }
        expect(tag, OCTET_STRING)?;
        let (_, general_names, _) = tlv(value)?;
        let mut names = Vec::new();
        let mut items = general_names;
        while !items.is_empty() {
            let (tag, name, next) = tlv(items)?;
            if tag == DNS_NAME {
                names.push(String::from_utf8_lossy(name).to_ascii_lowercase());
            }
            items = next;
        }
        return Some(names);
    }
    Some(Vec::new())
}

fn time(tag: u8, text: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(text).ok()?.strip_suffix('Z')?;
    let digits = |range: std::ops::Range<usize>| text.get(range)?.parse::<u64>().ok();
    let (year, rest) = match tag {
        // RFC 5280: two-digit years 50 to 99 are 1950 to 1999.
        UTC_TIME if text.len() == 12 => {
            let yy = digits(0..2)?;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, 2)
        }
        GENERALIZED_TIME if text.len() == 14 => (digits(0..4)?, 4),
        _ => return None,
    };
    let field = |i: usize| digits(rest + 2 * i..rest + 2 * i + 2);
    let (month, day, hour, minute, second) = (field(0)?, field(1)?, field(2)?, field(3)?, field(4)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    Some(days_from_civil(year, month, day)? * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Days since 1970-01-01 in the proleptic Gregorian calendar (Howard Hinnant's algorithm).
fn days_from_civil(year: u64, month: u64, day: u64) -> Option<u64> {
    let y = i64::try_from(year).ok()? - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::try_from(month).ok()?;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::try_from(day).ok()? - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    u64::try_from(era * 146_097 + doe - 719_468).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 90
    ///  -subj "/O=Test Org/CN=Test Issuer R1" -addext "subjectAltName=DNS:shop.example.com,DNS:*.home.example.com"`
    /// with fixed dates 2026-11-01 00:00:00 to 2027-01-30 00:00:00.
    const PEM: &str = include_str!("../tests/fixtures/cert.pem");

    #[test]
    fn reads_dates_issuer_and_names() {
        let cert = parse_pem(PEM).unwrap();
        assert_eq!(cert.not_before, 1_793_491_200);
        assert_eq!(cert.not_after, 1_801_267_200);
        assert_eq!(cert.issuer_org.as_deref(), Some("Test Org"));
        assert_eq!(cert.issuer_cn.as_deref(), Some("Test Issuer R1"));
        assert_eq!(cert.issuer(), "Test Org (Test Issuer R1)");
        assert_eq!(cert.dns_names, vec!["shop.example.com", "*.home.example.com"]);
        assert!(cert.covers("shop.example.com"));
        assert!(cert.covers("pay.home.example.com"));
        assert!(!cert.covers("a.b.home.example.com"));
        assert!(!cert.covers("example.com"));
    }

    #[test]
    fn damaged_input_is_none_never_a_panic() {
        assert_eq!(parse_pem("nothing here"), None);
        let der = STANDARD
            .decode(
                PEM.lines()
                    .filter(|l| !l.starts_with("-----"))
                    .collect::<String>(),
            )
            .unwrap();
        for cut in 0..der.len() {
            let _ = parse_der(&der[..cut]);
        }
        let mut flipped = der.clone();
        for i in 0..flipped.len() {
            flipped[i] ^= 0xff;
            let _ = parse_der(&flipped);
            flipped[i] ^= 0xff;
        }
    }

    #[test]
    fn times_follow_rfc_5280() {
        assert_eq!(time(UTC_TIME, b"700101000000Z"), Some(0));
        assert_eq!(time(UTC_TIME, b"491231235959Z"), Some(2_524_607_999));
        assert_eq!(time(GENERALIZED_TIME, b"20500101000000Z"), Some(2_524_608_000));
        assert_eq!(time(UTC_TIME, b"701301000000Z"), None);
        assert_eq!(time(UTC_TIME, b"700101000000"), None);
    }
}
