//! The Web tab through the router: the sites, Hosting and the way to the internet, providers,
//! certificate settings, the local CA and onion addresses.
//!
//! The mock's sites: `shop` (home network and internet, at shop.example.com), `monokulo-pay`
//! (home network and Tor; its reachability check finds CGNAT), `moneropay` (home network; its
//! checks pass), `wallet-relay` (offline and locked). Porkbun is connected, with example.com.

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::http::StatusCode;
use common::{Answer, App, Browser};
use nodo_ui_core::mock::MockBackend;

const HOSTING: &str = "/apps/moneropay/hosting";

/// A row of the list of sites, from its link to the next one.
fn site_row<'a>(page: &'a Answer, site: &str) -> &'a str {
    let marker = format!(r#"href="/apps/{site}/hosting""#);
    let start = page.body.find(&marker).unwrap_or_else(|| panic!("{site} is not listed in:\n{}", page.body));
    let rest = &page.body[start..];
    rest[1..].find("<a class=\"row\"").map_or(rest, |end| &rest[..end])
}

async fn confirm(browser: &mut Browser<'_>, title: &str) -> Answer {
    let question = browser.get("/confirm").await;
    question.assert_has(title);
    let id = Browser::field(&question.body, "id");
    browser.post_then("/confirm", &[("id", &id)]).await
}

#[tokio::test]
async fn the_list_says_who_can_reach_each_site_and_all_can_be_taken_offline() {
    let app = App::new();
    let mut browser = app.login().await;
    let web = browser.get("/web").await;
    web.assert_has("<h1>Sites online</h1>").assert_has("To put another app online");
    assert!(site_row(&web, "shop").contains("https://shop.example.com") && site_row(&web, "shop").contains("Home and internet"));
    assert!(site_row(&web, "monokulo-pay").contains("Home network and Tor"));
    assert!(site_row(&web, "moneropay").contains("Home network"));
    web.assert_lacks(r#"href="/apps/wallet-relay/hosting""#).assert_lacks("ports 80 and 443 are closed");
    for link in ["/web/providers", "/web/domain", "/web/certificates", "/web/trust", "/system/onion"] {
        web.assert_has(&format!(r#"href="{link}""#));
    }

    // Asked first; confirmed, every site that was online is offline.
    assert_eq!(browser.post("/web/offline", &[]).await.location(), "/confirm");
    let offline = confirm(&mut browser, "Take all sites offline?").await;
    offline.assert_has("No site is online yet.").assert_has("Nothing is on the internet, so ports 80 and 443 are closed on this device.");
    offline.assert_lacks("Take all offline");
    // With none online there is nothing to ask.
    assert_eq!(browser.post("/web/offline", &[]).await.location(), "/web");
    assert_eq!(browser.get("/confirm").await.location(), "/");
}

#[tokio::test]
async fn a_site_goes_offline_onto_the_home_network_and_onto_tor_without_being_asked() {
    let app = App::new();
    let mut browser = app.login().await;
    browser.get("/apps/moneropay").await.assert_has(r#"href="/apps/moneropay/hosting""#);
    let page = browser.get(HOSTING).await;
    page.assert_has("Choose who can open moneropay. Any combination works.").assert_has(r#"name="local" value="on" checked"#);
    page.assert_lacks(r#"name="public" value="on" checked"#).assert_lacks(r#"name="sub""#).assert_has(">Save<");

    // Nothing changed.
    browser.post_then(HOSTING, &[("do", "save"), ("ticks", "1"), ("local", "on")]).await.assert_has("There are no changes to save.");
    // Offline: no question, and the list of sites is where that leaves the owner.
    let saved = browser.post(HOSTING, &[("do", "save"), ("ticks", "1")]).await;
    assert_eq!(saved.location(), "/web");
    browser.get("/web").await.assert_has("Saved.").assert_lacks(r#"href="/apps/moneropay/hosting""#);
    // Home network, then Tor as well.
    browser.post(HOSTING, &[("do", "save"), ("ticks", "1"), ("local", "on")]).await;
    assert!(site_row(&browser.get("/web").await, "moneropay").contains("Home network"));
    browser.post(HOSTING, &[("do", "save"), ("ticks", "1"), ("local", "on"), ("onion", "on")]).await;
    assert!(site_row(&browser.get("/web").await, "moneropay").contains("Home network and Tor"));

    // An app without a web page has no Hosting page.
    let none = browser.get("/apps/tor/hosting").await;
    assert_eq!(none.location(), "/apps/tor");
    browser.get("/apps/tor").await.assert_has("tor has no web page to put online.");
}

/// Ticks the internet for moneropay at donate.example.com and saves, with fields a page never
/// sends. Returns where the check led.
async fn check_and_save(browser: &mut Browser<'_>) -> String {
    let forged = [("host", "evil.example.org"), ("plan", "evil.example.org"), ("was_public", "1"), ("has_provider", "0"), ("return_to", "shop")];
    let mut fields = vec![("do", "save"), ("ticks", "1"), ("local", "on"), ("public", "on"), ("sub", "donate"), ("domain", "example.com")];
    fields.extend(forged);
    let started = browser.post(HOSTING, &fields).await;
    assert_eq!(started.location(), "/working", "{}", started.body);
    browser.wait_for_job().await.location().to_owned()
}

#[tokio::test]
async fn the_internet_comes_on_only_after_the_check_and_the_address_typed() {
    let app = App::new();
    let mut browser = app.login().await;

    // Ticking the internet shows what it needs; with htmx the form alone comes back.
    let mut htmx = browser.clone();
    htmx.htmx = true;
    let form = htmx.post(HOSTING, &[("ticks", "1"), ("local", "on"), ("public", "on")]).await;
    assert!(form.body.starts_with("<form"), "{}", form.body);
    form.assert_has(r#"name="sub" value="moneropay""#).assert_has(r#"<option value="example.com" selected>"#).assert_has("Check and save");
    form.assert_has("Visitors will be able to see your home's internet address.").assert_has("Provider   Porkbun");
    // The address in its two parts.
    let renamed = browser.post_then(HOSTING, &[("ticks", "1"), ("local", "on"), ("public", "on"), ("sub", "donate"), ("domain", "example.com")]).await;
    renamed.assert_has(r#"name="sub" value="donate""#).assert_has("https://donate.example.com");
    // A domain that is none of the owner's is not taken.
    browser.post_then(HOSTING, &[("ticks", "1"), ("public", "on"), ("sub", "donate"), ("domain", "evil.example.org")]).await.assert_has("https://donate.example.com");

    // Save applies the other choices, saves the names and runs the check, as a job. Nothing is
    // public yet, whatever else the browser sent.
    assert_eq!(check_and_save(&mut browser).await, "/apps/moneropay/hosting/confirm");
    assert!(site_row(&browser.get("/web").await, "moneropay").contains("Home network"), "not on the internet before it is confirmed");
    let page = browser.get("/apps/moneropay/hosting/confirm").await;
    page.assert_has("Ready to put moneropay on the internet").assert_has("https://donate.example.com").assert_has("Your home's internet address becomes public.");
    page.assert_has("Type donate.example.com to confirm").assert_lacks("evil.example.org");
    // Cancel comes before the button that confirms, and has the focus.
    let cancel = page.body.find(r#"<a class="btn" href="/apps/moneropay/hosting" autofocus>Cancel</a>"#).expect("Cancel");
    assert!(cancel < page.body.find("Put moneropay online</button>").expect("the confirming button"));

    // The address must be typed exactly; a host sent beside it changes nothing.
    for typed in ["", "donate.example.co", "evil.example.org"] {
        let refused = browser.post("/apps/moneropay/hosting/confirm", &[("typed", typed), ("host", typed), ("plan", typed)]).await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{typed}");
        refused.assert_has("That isn't the address. Type donate.example.com exactly.");
    }
    assert!(site_row(&browser.get("/web").await, "moneropay").contains("Home network"));

    // Typed: it goes public at the address of the confirmation kept on the Nodo, and its
    // certificate is followed.
    let confirmed = browser.post("/apps/moneropay/hosting/confirm", &[("typed", "donate.example.com"), ("host", "evil.example.org")]).await;
    assert_eq!(confirmed.location(), "/working");
    assert_eq!(browser.wait_for_job().await.location(), "/apps/moneropay/hosting/certificate");
    let certificate = browser.get("/apps/moneropay/hosting/certificate").await;
    certificate.assert_has("moneropay is online").assert_has("https://donate.example.com").assert_lacks("evil.example.org");
    let row = browser.get("/web").await;
    assert!(site_row(&row, "moneropay").contains("https://donate.example.com") && site_row(&row, "moneropay").contains("Home and internet"));
    // The confirmation is used up.
    assert_eq!(browser.get("/apps/moneropay/hosting/confirm").await.location(), HOSTING);

    // While it is public its address is fixed and no name can be added; one posted anyway is
    // refused in the flow's words.
    let public = browser.get(HOSTING).await;
    public.assert_has(r#"name="public" value="on" checked"#).assert_has("To change the address, take the site off the internet first.").assert_lacks(r#"name="alias""#);
    let refused = browser.post(HOSTING, &[("do", "add-alias"), ("alias", "www.example.com"), ("ticks", "1"), ("local", "on"), ("public", "on")]).await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    refused.assert_has("Take the site off the internet before adding another name.");

    // Off the internet again: no question.
    assert_eq!(browser.post(HOSTING, &[("do", "save"), ("ticks", "1"), ("local", "on")]).await.location(), "/web");
    assert!(site_row(&browser.get("/web").await, "moneropay").contains("Home network"));
}

#[tokio::test]
async fn following_a_certificate_does_not_keep_a_session_alive() {
    let app = App::new();
    let mut browser = app.login().await;
    assert_eq!(check_and_save(&mut browser).await, "/apps/moneropay/hosting/confirm");
    browser.post("/apps/moneropay/hosting/confirm", &[("typed", "donate.example.com")]).await;
    browser.wait_for_job().await;
    // The page asks for the certificate's state every two seconds until it is there.
    let page = browser.get("/apps/moneropay/hosting/certificate").await;
    page.assert_has(r#"hx-get="/apps/moneropay/hosting/certificate/status" hx-trigger="every 2s""#).assert_has("Getting its secure connection…");
    let mut htmx = browser.clone();
    htmx.htmx = true;
    app.clock.advance(Duration::from_secs(14 * 60));
    let status = htmx.get("/apps/moneropay/hosting/certificate/status").await;
    assert!(status.body.starts_with("<section"), "{}", status.body);
    app.clock.advance(Duration::from_secs(2 * 60));
    assert_eq!(browser.get("/").await.location(), "/login", "16 minutes without the owner");
    // A site whose certificate is in place is not asked about again.
    let mut browser = app.login().await;
    browser.get("/apps/shop/hosting/certificate").await.assert_has("shop is online").assert_lacks("hx-trigger");
}

#[tokio::test]
async fn a_failed_step_on_the_way_to_public_stops_there() {
    let app = App::new();
    // The site's protections cannot be applied; they come before its address is published.
    app.backends.prepare(|mock| mock.knobs.fail_public_access = true);
    let mut browser = app.login().await;
    assert_eq!(check_and_save(&mut browser).await, "/apps/moneropay/hosting/confirm");
    assert_eq!(browser.post("/apps/moneropay/hosting/confirm", &[("typed", "donate.example.com")]).await.location(), "/working");
    // Back at the confirmation with the reason, and not public.
    assert_eq!(browser.wait_for_job().await.location(), "/apps/moneropay/hosting/confirm");
    browser.get("/apps/moneropay/hosting/confirm").await.assert_has("Could not apply the site's protections.");
    let web = browser.get("/web").await;
    assert!(site_row(&web, "moneropay").contains("Home network") && !site_row(&web, "moneropay").contains("internet"));
    assert_eq!(browser.get("/apps/moneropay/hosting/certificate").await.location(), HOSTING, "no certificate to follow");
}

#[tokio::test]
async fn a_check_that_fails_shows_what_to_do_and_never_leads_to_the_confirmation_by_itself() {
    let app = App::new();
    // The shop is not public yet here. The mock's first check of it finds its ports closed.
    app.backends.prepare(|mock| {
        let shop = mock.web.sites.iter_mut().find(|site| site.package == "shop").expect("the mock has the shop");
        shop.exposure.public = false;
        shop.public_aliases.clear();
    });
    let mut browser = app.login().await;
    let save = [("do", "save"), ("ticks", "1"), ("local", "on"), ("public", "on"), ("sub", "shop"), ("domain", "example.com")];
    assert_eq!(browser.post("/apps/shop/hosting", &save).await.location(), "/working");
    assert_eq!(browser.wait_for_job().await.location(), "/apps/shop/hosting/check");
    let check = browser.get("/apps/shop/hosting/check").await;
    check.assert_has("The internet can't reach your Nodo yet").assert_has("Port 80 is not reachable from outside").assert_has("Timed out");
    check.assert_has("Your router needs to send web visitors to the Nodo.").assert_has("to 192.168.1.50:443").assert_has("Use Tor instead");
    check.assert_lacks("Continue anyway");
    browser.get("/apps/shop/hosting/check/routers").await.assert_has("<pre class=\"text\">");
    // A failed check cannot be passed over, by button or by request.
    assert_eq!(browser.post("/apps/shop/hosting/check/continue", &[]).await.location(), "/apps/shop/hosting/check");
    assert_eq!(browser.get("/apps/shop/hosting/confirm").await.location(), "/apps/shop/hosting");
    // A check is not run by a GET.
    assert_eq!(browser.get("/apps/shop/hosting/check/again").await.location(), "/apps/shop/hosting/check");

    // Checked again, the ports are open: the confirmation.
    assert_eq!(browser.post("/apps/shop/hosting/check/again", &[]).await.location(), "/working");
    assert_eq!(browser.wait_for_job().await.location(), "/apps/shop/hosting/confirm");
    browser.get("/apps/shop/hosting/confirm").await.assert_has("Ready to put shop on the internet");

    // Or Tor instead, which needs none of it.
    let mut other = app.login().await;
    other.post("/apps/shop/hosting", &save).await;
    other.wait_for_job().await;
    assert_eq!(other.post("/apps/shop/hosting/check/tor", &[]).await.location(), "/web");
    let web = other.get("/web").await;
    assert!(site_row(&web, "shop").contains("Home network and Tor"), "{}", site_row(&web, "shop"));
}

#[tokio::test]
async fn a_shared_internet_address_is_explained_with_the_ways_on() {
    let app = App::new();
    let mut browser = app.login().await;
    let save = [("do", "save"), ("ticks", "1"), ("local", "on"), ("public", "on"), ("sub", "tips"), ("domain", "example.com")];
    assert_eq!(browser.post("/apps/monokulo-pay/hosting", &save).await.location(), "/working");
    assert_eq!(browser.wait_for_job().await.location(), "/apps/monokulo-pay/hosting/cgnat");
    let page = browser.get("/apps/monokulo-pay/hosting/cgnat").await;
    page.assert_has("The internet can't reach your Nodo").assert_has("shares one address between many homes").assert_has("100.72.14.9").assert_has("100.64.0.0/10");
    for way in ["Use Tor instead", "Ask your provider for a public address", "Use IPv6, if your provider has it", "Use a relay that forwards visitors"] {
        page.assert_has(way);
    }
    // A way that is explained is a text to read, which leads back here.
    assert_eq!(browser.post("/apps/monokulo-pay/hosting/cgnat/ipv6", &[]).await.location(), "/document");
    browser.get("/document").await.assert_has(r#"href="/apps/monokulo-pay/hosting/cgnat""#);
    // Tor is turned on, and nothing went public.
    assert_eq!(browser.post("/apps/monokulo-pay/hosting/cgnat/tor", &[]).await.location(), "/web");
    let web = browser.get("/web").await;
    assert!(site_row(&web, "monokulo-pay").contains("Tor") && !site_row(&web, "monokulo-pay").contains("internet"));
}

#[tokio::test]
async fn other_names_are_checked_as_they_are_added() {
    let app = App::new();
    let mut browser = app.login().await;
    let draft = [("ticks", "1"), ("local", "on"), ("public", "on"), ("sub", "donate"), ("domain", "example.com")];
    browser.post(HOSTING, &draft).await;
    browser.get(HOSTING).await.assert_has("Nothing else yet: only donate.example.com.").assert_has(r#"name="alias""#);
    let add = |alias: &'static str, mode: &'static str| {
        let mut fields = draft.to_vec();
        fields.extend([("do", "add-alias"), ("alias", alias), ("mode", mode)]);
        fields
    };
    // A name another site answers at, and one under nobody's domain: refused beside the box.
    for taken in ["shop.example.com", "pay.elsewhere.org"] {
        let refused = browser.post(HOSTING, &add(taken, "serve")).await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{taken}");
        refused.assert_has(r#"<p class="problem" id="alias-problem">"#);
    }
    // Said once.
    browser.get(HOSTING).await.assert_lacks("alias-problem\">");
    // Two that are fine, one of each kind.
    browser.post_then(HOSTING, &add("tips.example.com", "serve")).await.assert_has("tips.example.com").assert_has("shows the same site");
    let both = browser.post_then(HOSTING, &add("give.example.com", "redirect")).await;
    both.assert_has("give.example.com").assert_has("redirects to donate.example.com");
    // Removed.
    let mut fields = draft.to_vec();
    fields.push(("do", "remove-alias:tips.example.com"));
    browser.post_then(HOSTING, &fields).await.assert_lacks("tips.example.com").assert_has("give.example.com");

    // They are saved with the address and shown on the confirmation.
    let mut fields = draft.to_vec();
    fields.push(("do", "save"));
    assert_eq!(browser.post(HOSTING, &fields).await.location(), "/working");
    assert_eq!(browser.wait_for_job().await.location(), "/apps/moneropay/hosting/confirm");
    browser.get("/apps/moneropay/hosting/confirm").await.assert_has("and https://give.example.com").assert_has("redirects to donate.example.com");
}

#[tokio::test]
async fn a_provider_is_added_step_by_step_and_removed_after_a_question() {
    let app = App::new();
    let mut browser = app.login().await;
    let providers = "/web/providers";
    let list = browser.get(providers).await;
    list.assert_has("<h1>Domain providers</h1>").assert_has("Porkbun").assert_has("example.com").assert_has("Add a provider");

    // Choosing the company.
    assert_eq!(browser.post(providers, &[("do", "add")]).await.location(), providers);
    let pick = browser.get(providers).await;
    pick.assert_has("Where did you buy your web address?").assert_has("Cloudflare").assert_has("Can't be connected: no way to change records");
    let unsupported = browser.post(providers, &[("choose", "Hover")]).await;
    assert_eq!(unsupported.status, StatusCode::UNPROCESSABLE_ENTITY);
    unsupported.assert_has("Hover can't be connected").assert_has("Where did you buy your web address?");

    // The key: the boxes the company needs, always empty.
    let key = browser.post(providers, &[("choose", "GoDaddy")]).await;
    assert_eq!(key.status, StatusCode::OK);
    key.assert_has("Connect GoDaddy").assert_has(r#"<input type="text" id="domain" name="domain""#).assert_has(r#"<input type="password" id="api_key" name="api_key" value="""#);
    key.assert_has(r#"<input type="password" id="secret_key" name="secret_key" value="""#);
    browser.post(providers, &[("do", "help")]).await.assert_has("<pre class=\"text\">");

    // What is missing is said, with its box marked, and nothing is sent.
    let no_domain = browser.post(providers, &[("do", "connect"), ("api_key", "key-one"), ("secret_key", "key-two")]).await;
    assert_eq!(no_domain.status, StatusCode::UNPROCESSABLE_ENTITY);
    no_domain.assert_has("GoDaddy can't list your domains, so type the web address you bought").assert_has(r#"name="domain" value="" autocomplete="off" autocapitalize="none" spellcheck="false" placeholder="example.com" aria-invalid="true""#);
    let no_key = browser.post(providers, &[("do", "connect"), ("domain", "myhome.org"), ("api_key", "key-one")]).await;
    assert_eq!(no_key.status, StatusCode::UNPROCESSABLE_ENTITY);
    no_key.assert_has("Paste the key from GoDaddy.").assert_has(r#"name="secret_key" value="" autocomplete="off" autocapitalize="none" spellcheck="false" aria-invalid="true""#);
    no_key.assert_has(r#"name="domain" value="myhome.org""#).assert_lacks("key-one");
    // Refused by the helper: its words above the boxes, which are empty again.
    let refused = browser.post(providers, &[("do", "connect"), ("domain", "nodot"), ("api_key", "key-one"), ("secret_key", "key-two")]).await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    refused.assert_has("GoDaddy can't list your domains, so type the domain to use").assert_lacks("key-one").assert_lacks("key-two");

    // Connected: the domains its key controls.
    let done = browser.post(providers, &[("do", "connect"), ("domain", "myhome.org"), ("api_key", "key-one"), ("secret_key", "key-two")]).await;
    assert_eq!(done.status, StatusCode::OK);
    done.assert_has("GoDaddy is connected").assert_has("Apps can now use these web addresses:").assert_has("myhome.org").assert_lacks("key-one");
    // Done leads to the list, whatever a browser says about where to return.
    assert_eq!(browser.post(providers, &[("do", "finish"), ("return_to", "shop")]).await.location(), providers);
    let list = browser.get(providers).await;
    list.assert_has("<h2>GoDaddy</h2>").assert_has("myhome.org").assert_has("<h2>Porkbun</h2>");

    // Looking again, and removing after the question.
    browser.post_then(providers, &[("refresh", "porkbun")]).await.assert_has("Porkbun lists");
    assert_eq!(browser.post(providers, &[("remove", "godaddy")]).await.location(), "/confirm");
    let removed = confirm(&mut browser, "Remove GoDaddy?").await;
    removed.assert_has("The provider was removed.").assert_lacks("<h2>GoDaddy</h2>");
    // Back from choosing the company leads to the list.
    browser.post(providers, &[("do", "add")]).await;
    browser.post_then(providers, &[("do", "back")]).await.assert_has("<h1>Domain providers</h1>");
}

#[tokio::test]
async fn hosting_sends_the_owner_to_add_a_provider_and_takes_them_back_with_it() {
    let app = App::new();
    app.backends.prepare(|mock| {
        mock.connected.clear();
        mock.web.domain.clear();
        mock.web.provider.clear();
    });
    let mut browser = app.login().await;
    // The internet ticked with no provider: no address to choose, nothing to save, one way on.
    let page = browser.post_then(HOSTING, &[("ticks", "1"), ("local", "on"), ("public", "on")]).await;
    page.assert_has("To use a web address you own, first add the company you bought it from.").assert_has("You'll come back here when it's added.");
    page.assert_lacks(r#"name="sub""#).assert_lacks(r#"value="save""#);
    // Saving all the same does nothing but say why.
    browser.post_then(HOSTING, &[("do", "save"), ("ticks", "1"), ("local", "on"), ("public", "on")]).await.assert_has("Add a domain provider first");

    assert_eq!(browser.post(HOSTING, &[("do", "add-provider"), ("ticks", "1"), ("local", "on"), ("public", "on")]).await.location(), "/web/providers");
    browser.get("/web/providers").await.assert_has("Where did you buy your web address?");
    browser.post("/web/providers", &[("choose", "Porkbun")]).await.assert_has("Connect Porkbun").assert_lacks(r#"name="domain""#);
    let done = browser.post("/web/providers", &[("do", "connect"), ("api_key", "pk1_abc"), ("secret_key", "sk1_def")]).await;
    done.assert_has("Porkbun is connected").assert_has("example.org");
    // Done: back on the Hosting page, the internet ticked and the provider's first domain chosen.
    assert_eq!(browser.post("/web/providers", &[("do", "finish")]).await.location(), HOSTING);
    let back = browser.get(HOSTING).await;
    back.assert_has(r#"name="public" value="on" checked"#).assert_has(r#"<option value="example.org" selected>"#).assert_has("Check and save");
    assert_eq!(browser.get("/web/providers").await.status, StatusCode::OK);
    browser.get("/web/providers").await.assert_has("<h1>Domain providers</h1>");
}

#[tokio::test]
async fn a_locked_device_is_unlocked_before_a_key_is_stored() {
    let app = App::new();
    app.backends.stay_locked.store(true, Ordering::SeqCst);
    let mut browser = app.login().await;
    browser.post("/web/providers", &[("do", "add")]).await;
    browser.post("/web/providers", &[("choose", "Porkbun")]).await.assert_has("This Nodo is locked.");
    let sent = browser.post("/web/providers", &[("do", "connect"), ("api_key", "pk1_abc"), ("secret_key", "sk1_def")]).await;
    assert_eq!(sent.location(), "/");
    browser.get("/").await.assert_has("Connecting Porkbun stores its key locked with the device passphrase.").assert_has(r#"action="/unlock""#);
}

#[tokio::test]
async fn certificate_settings_are_saved_refused_above_the_fields_and_local_names_are_tested() {
    let app = App::new();
    app.backends.prepare(|mock| {
        mock.web.acme.dns01 = true;
        mock.web.acme.dns01_fields = vec!["api_key".into(), "secret_api_key".into()];
        mock.web.acme.dns01_stored = false;
    });
    let mut browser = app.login().await;
    let domain = "/web/domain";
    let page = browser.get(domain).await;
    page.assert_has("Email for expiry notices").assert_has("Use the staging server").assert_has("site.local, with this device's own CA").assert_has("Under your domain");
    page.assert_has(r#"<label for="dns01_first">api key</label>"#).assert_has(r#"<label for="dns01_second">secret api key</label>"#);

    // Refused by the helper: its words above the fields, what was typed kept, secrets not.
    let refused = browser.post(domain, &[("do", "save"), ("email", "not-an-address"), ("naming", "local"), ("dns01_first", "first-secret"), ("dns01_second", "second-secret")]).await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    refused.assert_has("✕ that email address is not valid").assert_has(r#"name="email" value="not-an-address""#).assert_lacks("first-secret");
    assert!(refused.body.find("that email address is not valid").expect("said") < refused.body.find("name=\"email\"").expect("the field"));

    // Saved: the flow's words, and the page shows what is stored.
    let saved = browser.post_then(domain, &[("do", "save"), ("email", "me@example.org"), ("staging", "on"), ("naming", "domain"), ("local_domain", "home.example.com"), ("dns01_first", "first-secret"), ("dns01_second", "second-secret")]).await;
    saved.assert_has("Certificate settings saved and applied.").assert_has(r#"name="email" value="me@example.org""#).assert_has(r#"name="staging" value="on" checked"#);
    saved.assert_has(r#"name="naming" value="domain" checked"#).assert_has(r#"name="local_domain" value="home.example.com""#);
    saved.assert_has(r#"placeholder="stored, type to replace""#).assert_lacks("first-secret").assert_lacks("second-secret");

    // The test of the local names, with its results on the page.
    let tested = browser.post(domain, &[("do", "check"), ("email", "me@example.org"), ("staging", "on"), ("naming", "domain"), ("local_domain", "home.example.com")]).await;
    assert_eq!(tested.status, StatusCode::OK);
    tested.assert_has("Local names on this network").assert_has("shop.home.example.com").assert_has("Resolves to");
}

#[tokio::test]
async fn certificates_and_the_local_ca_are_shown_and_no_file_is_served_from_here() {
    let app = App::new();
    let mut browser = app.login().await;
    let certificates = browser.get("/web/certificates").await;
    certificates.assert_has("<h1>Certificates</h1>").assert_has("shop.example.com").assert_has("✓ Valid, 61 days left").assert_has("moneropay.local");
    let trust = browser.get("/web/trust").await;
    trust.assert_has("<h1>Local CA</h1>").assert_has("SHA-256 fingerprint").assert_has("3F:9A:21:C4:7E:05:B8:D2:61:4A:93:0C:EE:17:5B:28");
    trust.assert_has("Install the root only if the page shows this same fingerprint.");
    // The root itself is offered by Caddy's page on the home network; this one only links there.
    trust.assert_has(r#"href="http://nodo.local""#).assert_lacks(".crt").assert_lacks("download");
    for path in ["/web/trust/root.crt", "/web/trust/download", "/web/ca"] {
        assert_eq!(browser.get(path).await.status, StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn onion_addresses_are_switched_and_what_cannot_be_is_said() {
    let app = App::new();
    let mut browser = app.login().await;
    let status = MockBackend::new().onion_status();
    let onion = "/system/onion";
    let page = browser.get(onion).await;
    page.assert_has("<h1>Onion addresses</h1>").assert_has("● Tor running").assert_has("restricted RPC, port 18081").assert_has("About onion addresses");
    page.assert_has("single address");
    let pressed = |body: &str, package: &str| {
        let at = body.find(&format!(r#"name="package" value="{package}""#)).expect("the site's switch");
        body[at..].contains(r#"aria-pressed="true""#) && body[at..].find(r#"aria-pressed="true""#) < body[at..].find(r#"aria-pressed="false""#).or(Some(usize::MAX))
    };
    let site = |package: &str| status.sites.iter().find(|site| site.id.package == package).map(|site| site.id.site.clone()).expect("the mock has it");
    assert!(pressed(&page.body, "shop") && !pressed(&page.body, "moneropay"));

    // A site's switch, by its id.
    let off = browser.post_then("/system/onion/site", &[("package", "shop"), ("site", &site("shop"))]).await;
    off.assert_has("Onion exposure changed.");
    assert!(!pressed(&off.body, "shop"));
    // Refused, in the flow's words: a site that works from one address, and a locked one.
    browser.post_then("/system/onion/site", &[("package", "moneropay"), ("site", &site("moneropay"))]).await.assert_has("moneropay works from a single address. Turn its other address off under Web first.");
    // The node's own address.
    let node = browser.post_then("/system/onion/node", &[]).await;
    node.assert_has("Onion exposure changed.");
    // The settings of the app that provides the node.
    // The settings of the app that provides the node: its form, or the remark that it has none
    // (the mock's cuprated has none).
    let settings = browser.post_then("/system/onion/node-settings", &[]).await;
    assert!(settings.body.contains("cuprated has no settings to configure.") || settings.body.contains("<h1>Settings</h1>"), "{}", settings.body);
}

#[tokio::test]
async fn a_locked_site_gets_no_onion_address_until_the_device_is_unlocked() {
    let app = App::new();
    app.backends.stay_locked.store(true, Ordering::SeqCst);
    let mut browser = app.login().await;
    browser.get("/system/onion").await.assert_has("◆ locked");
    let refused = browser.post_then("/system/onion/site", &[("package", "wallet-relay"), ("site", "web")]).await;
    refused.assert_has("wallet-relay is locked. Unlock the device first.");
}
