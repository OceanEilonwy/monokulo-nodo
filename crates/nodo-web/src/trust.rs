//! The page that lets LAN devices trust this device's local CA.
//!
//! `.local` sites get their certificates from Caddy's internal CA, which no browser trusts until its
//! root is installed. The helper writes the files here into [`TRUST_DIR`], and Caddy serves them in
//! plain HTTP at `http://nodo.local` on the local address only, so nobody needs a port or a
//! command line. Plain HTTP is the point: the page exists because nothing on it can be trusted yet.
//! That is why it shows the root's SHA-256 fingerprint, which the owner compares with the one the
//! TUI shows before trusting anything.

use base64::Engine;
use sha2::{Digest, Sha256};

/// The name the mDNS responder answers for the page, on the local address.
pub const TRUST_HOST: &str = "nodo.local";
/// Where the helper writes the page and the certificate files. Caddy may read it, nothing else.
pub const TRUST_DIR: &str = "/var/lib/nodo-web/trust";
pub const INDEX_FILE: &str = "index.html";
/// The root in PEM, which Linux and Firefox import.
pub const PEM_FILE: &str = "nodo-local-ca.pem";
/// The root in DER, which Android, iOS, macOS and Windows open from a download.
pub const DER_FILE: &str = "nodo-local-ca.crt";

/// The local CA's root certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootCert {
    /// One certificate block, with a trailing newline.
    pub pem: String,
    pub der: Vec<u8>,
}

impl RootCert {
    /// Takes the first certificate in a PEM text, as Caddy's `root.crt` and its admin API give it.
    pub fn from_pem(text: &str) -> Option<Self> {
        let body: String = text
            .lines()
            .map(str::trim)
            .skip_while(|l| *l != "-----BEGIN CERTIFICATE-----")
            .skip(1)
            .take_while(|l| *l != "-----END CERTIFICATE-----")
            .collect();
        let der = base64::engine::general_purpose::STANDARD.decode(body).ok()?;
        crate::cert::parse_der(&der)?;
        Some(Self::from_der(der))
    }

    fn from_der(der: Vec<u8>) -> Self {
        let encoded = base64::engine::general_purpose::STANDARD.encode(&der);
        let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
        for chunk in encoded.as_bytes().chunks(64) {
            pem.push_str(std::str::from_utf8(chunk).unwrap_or_default());
            pem.push('\n');
        }
        pem.push_str("-----END CERTIFICATE-----\n");
        Self { pem, der }
    }

    /// SHA-256 of the DER, as colon separated upper-case hex, the way browsers show it.
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.der)
    }
}

pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der).iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

/// The fingerprint in two lines of 16 bytes each, which fits a phone screen and the TUI.
pub fn fingerprint_lines(fingerprint: &str) -> Vec<String> {
    let parts: Vec<&str> = fingerprint.split(':').collect();
    parts.chunks(16).map(|chunk| chunk.join(":")).collect()
}

const STYLE: &str = "body{font-family:system-ui,sans-serif;max-width:42rem;margin:1.5rem auto;padding:0 1rem;\
line-height:1.45;color:#1d1d1f;background:#fff}code,.fp{font-family:ui-monospace,monospace}\
.fp{background:#f2f2f2;padding:.6rem;border-radius:.3rem;word-break:break-all}\
a.button{display:inline-block;margin:.2rem .4rem .2rem 0;padding:.5rem .8rem;border:1px solid #888;\
border-radius:.3rem;text-decoration:none;color:inherit}h2{margin-top:1.6rem;font-size:1.1rem}\
@media (prefers-color-scheme:dark){body{color:#eee;background:#161616}.fp{background:#2a2a2a}}";

/// The page. Without a root yet, it says so and asks for a reload instead of offering a download.
pub fn page(root: Option<&RootCert>) -> String {
    let head = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<title>Trust this device's local sites</title><style>{STYLE}</style></head><body>"
    );
    let Some(root) = root else {
        return format!(
            "{head}<h1>Local CA not ready</h1><p>This device is still creating the certificate authority for its \
local sites. Reload this page in a minute.</p></body></html>"
        );
    };
    let fp = fingerprint_lines(&root.fingerprint()).join("<br>");
    format!(
        "{head}<h1>Trust this device's local sites</h1>\
<p>Sites ending in <code>.local</code> on this network use certificates from this device's own certificate \
authority. Install its root once on each phone or computer, and those sites open without a warning.</p>\
<p><strong>Check before you install.</strong> This page is not encrypted, so compare this SHA-256 fingerprint \
with the one under <em>Web › Domains › Local CA</em> on the device's own screen. Install only if they are \
the same.</p><p class=\"fp\">{fp}</p>\
<p><a class=\"button\" href=\"/{DER_FILE}\">Download {DER_FILE}</a>\
<a class=\"button\" href=\"/{PEM_FILE}\">Download {PEM_FILE}</a></p>\
<p>The root only vouches for this device's <code>.local</code> sites. Remove it from a device to stop \
trusting them.</p>\
<h2>Android</h2><ol><li>Download {DER_FILE}.</li><li>Open Settings › Security › More security settings › \
Encryption and credentials › Install a certificate › CA certificate. The names differ a little between \
makers.</li><li>Choose the downloaded file and confirm.</li></ol>\
<h2>iPhone and iPad</h2><ol><li>Open this page in Safari and download {DER_FILE}. Allow the profile \
download.</li><li>Open Settings › General › VPN and Device Management, choose the downloaded profile and \
install it.</li><li>Open Settings › General › About › Certificate Trust Settings and switch the new root \
on.</li></ol>\
<h2>macOS</h2><ol><li>Download {DER_FILE} and open it. Keychain Access adds it to the login keychain.</li>\
<li>Open the certificate in Keychain Access, expand Trust and choose Always Trust for SSL.</li></ol>\
<h2>Windows</h2><ol><li>Download {DER_FILE} and open it, then choose Install Certificate.</li>\
<li>Choose Local Machine, then Place all certificates in the following store, and pick Trusted Root \
Certification Authorities.</li><li>Finish, and restart the browser.</li></ol>\
<h2>Linux</h2><ol><li>Download {PEM_FILE}.</li><li>Debian and Ubuntu: copy it to \
<code>/usr/local/share/ca-certificates/nodo-local-ca.crt</code> and run \
<code>sudo update-ca-certificates</code>. Fedora and Arch: run \
<code>sudo trust anchor nodo-local-ca.pem</code>.</li><li>Restart the browser.</li></ol>\
<h2>Firefox</h2><ol><li>Download {PEM_FILE}.</li><li>Open Settings › Privacy &amp; Security › Certificates \
› View Certificates › Authorities, and choose Import.</li><li>Pick the file and tick Trust this CA to \
identify websites.</li></ol><p>On Windows and macOS, recent Firefox versions also accept a root installed \
in the system as above.</p></body></html>"
    )
}

/// The files to serve, as `(name, content)`, for the root or for the page saying it is not ready.
pub fn files(root: Option<&RootCert>) -> Vec<(&'static str, Vec<u8>)> {
    let mut files = vec![(INDEX_FILE, page(root).into_bytes())];
    if let Some(root) = root {
        files.push((PEM_FILE, root.pem.clone().into_bytes()));
        files.push((DER_FILE, root.der.clone()));
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPENSSL_FINGERPRINT: &str =
        "26:C5:E4:9B:EE:1F:99:29:3C:4B:DA:57:D9:6A:14:D3:20:62:D4:A5:3F:88:25:F9:0D:3A:36:FC:32:A8:A5:1F";

    /// A self-signed P-256 root made with openssl, standing in for Caddy's.
    const ROOT: &str = "-----BEGIN CERTIFICATE-----
MIIBmjCCAT+gAwIBAgIUaglMC+Y+t1Rh8cXrpwRKm2bLIw0wCgYIKoZIzj0EAwIw
IjEgMB4GA1UEAwwXbm9kbyB0ZXN0IExvY2FsIFJvb3QgQ0EwHhcNMjYxMTA4MDgy
NzAyWhcNMzYxMTA1MDgyNzAyWjAiMSAwHgYDVQQDDBdub2RvIHRlc3QgTG9jYWwg
Um9vdCBDQTBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABG9/Uocs/r9qcoYq+ir9
Nt9SkrNr4Yzs2qIiktkE9PqPpYOgP+xR3bMke5V+ecyCt8OhBPZi1fQnscgAPcsn
RHejUzBRMB0GA1UdDgQWBBRRwg/vyvmSsT+1WM2wLm7RoCVrjjAfBgNVHSMEGDAW
gBRRwg/vyvmSsT+1WM2wLm7RoCVrjjAPBgNVHRMBAf8EBTADAQH/MAoGCCqGSM49
BAMCA0kAMEYCIQC8n4rw9YlR1KP8rIjyoZm3rVNZikAK6hqJcAwUqSY+4gIhAIRm
t8spEPep0JoaQYoixKS/E07rdkO5URfHv1rzD530
-----END CERTIFICATE-----
";

    #[test]
    fn the_first_certificate_is_taken_and_rewritten_canonically() {
        let messy = format!("junk\n{}\n{}", ROOT.replace('\n', "\r\n"), ROOT);
        let root = RootCert::from_pem(&messy).expect("parses");
        assert!(root.pem.starts_with("-----BEGIN CERTIFICATE-----\n") && root.pem.ends_with("-----END CERTIFICATE-----\n"));
        assert!(root.pem.lines().all(|l| l.len() <= 64));
        assert_eq!(RootCert::from_pem(&root.pem).as_ref(), Some(&root));
        assert!(RootCert::from_pem("-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n").is_none());
        assert!(RootCert::from_pem("").is_none());
    }

    #[test]
    fn the_fingerprint_is_sha256_of_the_der_in_two_lines() {
        let root = RootCert::from_pem(ROOT).unwrap();
        let fp = root.fingerprint();
        // As `openssl x509 -noout -fingerprint -sha256` printed it for this certificate.
        assert_eq!(fp, OPENSSL_FINGERPRINT);
        assert_eq!(fp.len(), 32 * 3 - 1);
        assert_eq!(fp, fingerprint(&root.der));
        let lines = fingerprint_lines(&fp);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines.join(":"), fp);
    }

    #[test]
    fn the_page_shows_the_fingerprint_both_downloads_and_every_platform() {
        let root = RootCert::from_pem(ROOT).unwrap();
        let html = page(Some(&root));
        for line in fingerprint_lines(&root.fingerprint()) {
            assert!(html.contains(&line));
        }
        for text in ["/nodo-local-ca.crt", "/nodo-local-ca.pem", "Android", "iPhone", "macOS", "Windows", "Linux", "Firefox"] {
            assert!(html.contains(text), "{text}");
        }
        assert!(!html.contains("http://") && !html.contains("https://"), "nothing is loaded from elsewhere");
        let files = files(Some(&root));
        assert_eq!(files.iter().map(|f| f.0).collect::<Vec<_>>(), [INDEX_FILE, PEM_FILE, DER_FILE]);
        assert_eq!(files[2].1, root.der);
    }

    #[test]
    fn without_a_root_the_page_offers_nothing_to_install() {
        let html = page(None);
        assert!(html.contains("not ready") && !html.contains(DER_FILE));
        assert_eq!(files(None).len(), 1);
    }
}
