"""Journeys for the flows that broke for the owner on a real device, and that the unit tests missed
because they run against crates/nodo-tui/src/mock.rs instead of the helper.

journeys.py runs these after its own, with the same session, journey and check helpers. Each one
sets up what it needs through `nodo-tui --ctl` first (device passphrase, the test feed, the
package), so any of them can be run alone on a fresh VM:

    tests/tui-e2e.sh device          all of them
    tests/tui-e2e.sh forms-mouse     just one

The package they use is tui-forms, built by feed.sh from forms/settings.lua: thirty settings of
every kind, long enough that the form scrolls. It stands in for nodo-fixture-forms.

The assertions say what the owner should see. Journeys in NEEDS_FIXES fail on main until branch
tui-setup-fixes lands; that is expected, and their assertions are not to be relaxed to pass.
"""

import os
import re
import time

import journeys as j
from journeys import FEED, HERE, PASSPHRASE, Journey, Session, check, ctl, ok, sh, wait_until

FORMS = "tui-forms"
# A second fake provider's key, for fake-dns.py's Gandi LiveDNS. Never drawn or logged either.
GANDI_TOKEN = "gandi-pat-0123456789abcdef0123456789abcdef"
j.SECRETS.append(GANDI_TOKEN)

# The fields of forms/settings.lua, in order.
LABELS = [
    "Instance name", "Log level", "Worker threads", "SMTP port", "Mail server", "Contact email",
    "Mail delivery", "API token", "Admin password", "Public address", "Allow sign-ups", "Colour theme",
    "Largest upload (MiB)", "Keep logs for (days)", "Webhook target", "Webhook signing key", "Greeting",
    "Footer text", "Display currency", "Payout wallet", "Time zone", "Cache size (MiB)", "Backup hour",
    "Maintenance mode", "Language", "Support page", "Message of the day", "Requests per minute",
    "Debug token", "Admin notes",
]
# The settings these journeys change, put back to their defaults before each one.
TOUCHED = ["log_level", "smtp_host", "contact_email", "mail_mode", "theme", "currency", "maintenance",
           "language", "smtp_port", "workers"]

# What the owner sees once a save went through, whatever else the notice says.
SAVED = re.compile(r"\bSaved\b")
REFUSED_LOCALLY = "Fix the marked field before saving"


# Setup through the helper's own commands, as root

def unlock():
    sh(f"printf '%s\\n' '{PASSPHRASE}' | nodo-tui --ctl unlock")


def ready_device():
    """Set up and unlocked, with the test feed trusted and tui-forms published in it."""
    overview = ctl("overview")
    if "encryption: NotSetUp" in overview:
        sh(f"printf '%s\\n' '{PASSPHRASE}' | nodo-tui --ctl init")
    elif "encryption: Locked" in overview:
        unlock()
    if not os.path.exists(FEED):
        sh(f"sh {HERE}/feed.sh init && sh {HERE}/feed.sh pkg 1.0")
    if not forms_versions():
        sh(f"sh {HERE}/feed.sh forms 1.0")
    if "tui-feed" not in ctl("feeds"):
        probe = ctl("probe-feed", FEED)
        ctl("add-feed", FEED, re.search(r'fingerprint: "([^"]+)"', probe).group(1))
    ctl("refresh-feeds")


def forms_versions():
    """The tui-forms versions in the feed, oldest first, as (major, minor)."""
    found = []
    for name in os.listdir(FEED) if os.path.isdir(FEED) else []:
        m = re.fullmatch(r"tui-forms-(\d+)\.(\d+)_1\.x86_64\.xbps", name)
        if m:
            found.append((int(m.group(1)), int(m.group(2))))
    return sorted(found)


def installed_version():
    """tui-forms's installed version as xbps has it, such as "1.0_1", or None."""
    out = sh(f"xbps-query -p pkgver {FORMS}", check=False).strip()
    return out.removeprefix(f"{FORMS}-") if out.startswith(f"{FORMS}-") else None


def running():
    return sh(f"sv status {FORMS}", check=False).startswith("run:")


def remove_forms():
    if installed_version():
        ctl("remove-pkg", FORMS)
        wait_until("tui-forms to be removed", lambda: installed_version() is None, 60)


def ready_forms():
    """tui-forms installed, running, and with every setting these journeys touch at its default."""
    ready_device()
    if installed_version() is None:
        sh(f"printf '%s\\n' '{PASSPHRASE}' | nodo-tui --ctl approve-install-encrypted tui-feed {FORMS}", timeout=300)
    for name in TOUCHED:
        if not current(name).startswith("Default"):
            ctl("reset", FORMS, name)
    wait_until("tui-forms to run", running, 60)


def current(name):
    """A setting's stored value as the helper's form reports it, without spaces: "Default,",
    "Literal(Text("debug",),)" and so on."""
    form = ctl("form", FORMS)
    at = form.find(f'name: "{name}"')
    if at < 0:
        raise AssertionError(f"the helper's form has no setting {name}\n{form}")
    chunk = form[at:]
    chunk = chunk[chunk.find("current:") + len("current:"):chunk.find("default:")]
    return re.sub(r"\s", "", chunk)


# Driving the screens

def select_row(tui, name, limit=30):
    """Moves a list's ▸ marker down onto the row that starts with `name`."""
    row = re.compile(r"▸\s*" + re.escape(name) + r"\b")
    for _ in range(limit):
        if tui.contains(row):
            return
        tui.send("down")
    raise AssertionError(f"no row {name} to select\n{tui.text()}")


def open_get_apps_detail(tui, name):
    tui.send("2")
    tui.wait_for("Installed", timeout=20)
    tui.send("g")
    tui.wait_for("Get apps", "Search")
    tui.type(name)
    tui.wait_for(re.compile(re.escape(name)), timeout=20)
    tui.send("enter")
    tui.wait_for(re.compile(r"Get apps\s+›\s+" + re.escape(name)), timeout=30)


def open_settings(tui):
    tui.send("2")
    tui.wait_for("Installed", timeout=20)
    select_row(tui, FORMS)
    tui.send("enter")
    tui.wait_for(re.compile(r"Apps\s+›\s+" + FORMS), "Settings", timeout=20)
    select_row(tui, "Settings", limit=6)
    tui.send("enter")
    tui.wait_for(re.compile(r"Apps\s+›\s+tui-forms\s+›\s+Settings"), "Instance name", timeout=30)


def selected_field(tui):
    screen = tui.text()
    for label in LABELS:
        if re.search(r"▸ " + re.escape(label) + r"(\s|$)", screen, re.M):
            return label
    return None


def select_field(tui, label):
    for _ in range(2 * len(LABELS)):
        now = selected_field(tui)
        if now == label:
            return
        if now is None:
            raise AssertionError(f"no field of the form is selected\n{tui.text()}")
        tui.send("down" if LABELS.index(label) > LABELS.index(now) else "up", pause=0.08)
    raise AssertionError(f"could not select {label}\n{tui.text()}")


def expect_selected(tui, name, label, timeout=5):
    tui.expect(name, re.compile(r"▸ " + re.escape(label) + r"(\s|$)", re.M), timeout=timeout)


def save(tui, what):
    tui.send("ctrl-s")
    tui.expect(what, SAVED, timeout=90, absent=(REFUSED_LOCALLY,))


def install_up_to_passphrase(tui):
    """From the package detail to the moment the install is approved, typing the passphrase if
    asked. Returns as soon as the last key is sent."""
    tui.send("i")
    tui.expect("i shows what tui-forms asks for", "Install tui-forms?", re.compile(r"▸ Cancel\s+Install"), timeout=60)
    tui.send("right")
    tui.send("enter", pause=0.5)
    if "Its locked storage is made now" in tui.text():
        tui.type(PASSPHRASE)
        tui.wait_for("•" * len(PASSPHRASE))
        tui.send("enter", pause=0)


# Journeys

def forms_install():
    with Journey("Install: the page shows the install while the helper works"):
        ready_device()
        remove_forms()
    with Journey("Install: the page shows the install while the helper works"), Session() as tui:
        open_get_apps_detail(tui, FORMS)
        tui.expect("the detail page offers the install", "Install", timeout=10)
        install_up_to_passphrase(tui)
        # The helper installs in one request that answers when everything is done, a while later:
        # it formats the encrypted storage and waits for a healthy start. Until then the owner
        # must see the install going, not the review they already answered.
        tui.expect("the review gives way to the install page at once", "Installing tui-forms", timeout=5,
                   absent=("Install tui-forms?",))
        check("the install page shows before the install is over", "tui-forms is installed" not in tui.text(),
              tui.text())
        tui.expect("the install ends on success", "tui-forms is installed", timeout=240)
        tui.quit()
    with Journey("Install: the helper side"):
        check("xbps installed tui-forms", installed_version() is not None)
        wait_until("tui-forms to run", running, 60)
        ok("tui-forms runs as a service")


def forms_install_esc():
    with Journey("Install: Esc while it runs still reports how it ended"):
        ready_device()
        remove_forms()
    outcome = ""
    with Journey("Install: Esc while it runs still reports how it ended"), Session() as tui:
        open_get_apps_detail(tui, FORMS)
        install_up_to_passphrase(tui)
        tui.pump(1.5)
        # The service may already run, but the helper answers only after its health check (at
        # least five seconds up), so the screen cannot know the outcome yet.
        check("the install has not reported its outcome when Esc is pressed",
              not re.search(r"tui-forms (is installed|wasn't installed)", tui.text()),
              "the install finished before Esc: it was too quick to leave during it\n" + tui.text())
        tui.send("esc")
        tui.expect("the outcome is reported after leaving the page",
                   re.compile(r"tui-forms (is installed|wasn't installed)|Install cancelled"), timeout=240)
        outcome = tui.text()
        tui.quit()
    with Journey("Install: Esc while it runs, helper side"):
        if not outcome:
            raise AssertionError("no outcome was shown, so there is nothing to compare")
        if installed_version() is not None:
            check("an install that went through is reported as installed", "tui-forms is installed" in outcome,
                  outcome)
        else:
            check("an install that did not go through is not reported as installed",
                  "tui-forms is installed" not in outcome, outcome)


def forms_settings():
    with Journey("Settings: an empty optional number never blocks saving"):
        ready_forms()
        check("SMTP port starts empty, with no default", current("smtp_port").startswith("Default"))
    with Journey("Settings: an empty optional number never blocks saving"), Session() as tui:
        open_settings(tui)
        check("the empty optional number is not marked as wrong",
              "Must be a whole number" not in tui.text() and "✕" not in tui.line_with("SMTP port"), tui.text())
        select_field(tui, "Log level")
        tui.send("right")
        tui.expect("changing another field is a pending change", "1 change, not saved yet")
        check("Log level shows its next choice", "debug" in tui.line_with("Log level"), tui.text())
        save(tui, "the change saves while SMTP port is still empty")
        tui.quit()
    with Journey("Settings: an empty optional number, helper side"):
        check("the helper stored the new log level", "debug" in current("log_level"), current("log_level"))
        check("SMTP port is still unset", current("smtp_port").startswith("Default"), current("smtp_port"))

    with Journey("Settings: set an optional value, then clear it"), Session() as tui:
        open_settings(tui)
        select_field(tui, "Mail server")
        tui.send("enter")
        tui.wait_for("Keys: type a value")
        tui.type("mail.example.com", enter=True)
        save(tui, "an optional text value saves")
        wait_until("the helper to store it", lambda: "mail.example.com" in current("smtp_host"), 10)
        ok("the helper stored the optional value")
        tui.send("enter")
        tui.wait_for("Keys: type a value")
        tui.send("ctrl-u", "enter")
        tui.expect("emptying it is a pending change", "1 change, not saved yet")
        save(tui, "the emptied value saves")
        check("clearing put the setting back to unset, not an empty text", current("smtp_host").startswith("Default"),
              current("smtp_host"))
        tui.quit()

    with Journey("Settings: an optional value with a validator"), Session() as tui:
        open_settings(tui)
        select_field(tui, "Contact email")
        tui.send("enter")
        tui.wait_for("Keys: type a value")
        tui.type("not an address", enter=True)
        tui.send("ctrl-s")
        tui.expect("the package's own validator refuses through the helper", "must look like name@example.com",
                   timeout=60)
        check("the refused value was not stored", current("contact_email").startswith("Default"),
              current("contact_email"))
        select_field(tui, "Contact email")
        tui.send("enter")
        tui.wait_for("Keys: type a value")
        tui.send("ctrl-u")
        tui.type("owner@example.com", enter=True)
        save(tui, "a value the validator accepts saves")
        wait_until("the helper to store it", lambda: "owner@example.com" in current("contact_email"), 10)
        ok("the helper stored the accepted value")
        tui.send("enter")
        tui.wait_for("Keys: type a value")
        tui.send("ctrl-u", "enter")
        save(tui, "the validated value can be cleared again")
        check("the cleared email is unset", current("contact_email").startswith("Default"), current("contact_email"))
        tui.quit()

    with Journey("Settings: choices changed back and forth, then saved"), Session() as tui:
        open_settings(tui)
        select_field(tui, "Colour theme")
        # auto, light, dark, auto, light, dark: five steps along three choices.
        for _ in range(5):
            tui.send("right")
        check("Colour theme shows dark", "dark" in tui.line_with("Colour theme"), tui.text())
        select_field(tui, "Mail delivery")
        # off, smtp, sendmail, then back to smtp: mail by SMTP with SMTP port left empty.
        tui.send("right", "right", "left")
        check("Mail delivery shows smtp", "smtp" in tui.line_with("Mail delivery"), tui.text())
        tui.expect("both are pending changes", "2 changes, not saved yet")
        save(tui, "the choices save")
        tui.send("right")
        check("Mail delivery moved on to sendmail", "sendmail" in tui.line_with("Mail delivery"), tui.text())
        save(tui, "a second change to the same choice saves too")
        tui.send("esc")
        tui.wait_for("Hosting", timeout=10)
        tui.quit()
    with Journey("Settings: choices, helper side and reopened"):
        check("the helper stored the theme", "dark" in current("theme"), current("theme"))
        check("the helper stored the last mail choice", "sendmail" in current("mail_mode"), current("mail_mode"))
    with Journey("Settings: choices, helper side and reopened"), Session() as tui:
        open_settings(tui)
        select_field(tui, "Mail delivery")
        check("the reopened form shows the saved mail choice", "sendmail" in tui.line_with("Mail delivery"), tui.text())
        select_field(tui, "Colour theme")
        check("and the saved theme", "dark" in tui.line_with("Colour theme"), tui.text())
        tui.quit()


def click_label(tui, label):
    """Clicks a field's label on the form as it is drawn now. The label must be on screen."""
    tui.click_text(label)


def click_link_under(tui, label, link="+ reference"):
    """Clicks the first `link` drawn below `label`'s row: the link that belongs to that field."""
    where = tui.find(label)
    if where is None:
        raise AssertionError(f"{label} is not on screen\n{tui.text()}")
    tui.click_text(link, below=where[0])


def visible(tui, candidates):
    """The first of `candidates` whose label is on screen and is not the selected field."""
    now = selected_field(tui)
    for label in candidates:
        if label != now and tui.find(label) is not None:
            return label
    raise AssertionError(f"none of {candidates} is on screen\n{tui.text()}")


def forms_mouse():
    with Journey("Mouse: clicks on a long settings form"):
        ready_forms()
    with Journey("Mouse: clicks on a long settings form, at the top"), Session() as tui:
        open_settings(tui)
        click_label(tui, "Worker threads")
        expect_selected(tui, "a click on a label selects that field", "Worker threads")
        tui.expect("and starts editing it", "Keys: type a value")
        tui.send("esc")
        click_link_under(tui, "Instance name")
        tui.expect("a click on a field's reference link opens the picker for that field",
                   "Insert a reference", "into tui-forms · Instance name", timeout=20)
        tui.send("esc")
        expect_selected(tui, "leaving the picker returns to that field", "Instance name")
        tui.quit()
    with Journey("Mouse: clicks on a long settings form, scrolled to the middle"), Session() as tui:
        open_settings(tui)
        select_field(tui, "Keep logs for (days)")
        check("the form scrolled: the first field is out of view", tui.find("Instance name") is None, tui.text())
        label = visible(tui, ["Display currency", "Colour theme", "Mail delivery"])
        before = tui.line_with(label)
        click_label(tui, label)
        expect_selected(tui, "a click on a label of the scrolled form selects that field", label)
        check("and the click acted on that field: its choice moved on", tui.line_with(label) != before,
              f"{before!r} -> {tui.line_with(label)!r}")
        target = visible(tui, ["Webhook target", "Public address", "Footer text", "Greeting"])
        click_link_under(tui, target)
        tui.expect("a reference link of the scrolled form opens the picker for its own field",
                   "Insert a reference", f"into tui-forms · {target}", timeout=20)
        tui.send("esc")
        expect_selected(tui, "leaving the picker returns to the clicked field", target)
        # The wheel moves the selection, as the arrow keys do, and so scrolls the form further.
        row, column = tui.find(target)
        tui.wheel(row, column, down=True, times=6)
        label = visible(tui, LABELS)
        click_label(tui, label)
        expect_selected(tui, "after the wheel scrolled the form, a click still selects what it hit", label)
        tui.quit()
    with Journey("Mouse: clicks on a long settings form, scrolled to the end"), Session() as tui:
        open_settings(tui)
        select_field(tui, "Admin notes")
        check("the form scrolled to its end", tui.find("Instance name") is None and tui.find("Save") is not None,
              tui.text())
        # A toggle or a choice: either way the click changes it, so the change shows and saves.
        clicked = visible(tui, ["Maintenance mode", "Language"])
        before = tui.line_with(clicked)
        click_label(tui, clicked)
        expect_selected(tui, "a click near the end selects that field", clicked)
        check("and the click acted on that field", tui.line_with(clicked) != before,
              f"{before!r} -> {tui.line_with(clicked)!r}")
        # The click scrolled the form to the clicked field, so back to the end for the last rows.
        select_field(tui, "Admin notes")
        click_label(tui, "Debug token")
        expect_selected(tui, "a click on the field above the last selects it", "Debug token")
        tui.expect("and starts typing a new secret", "Keys: type a value")
        tui.send("esc")
        save_at = next((r for r, line in enumerate(tui.text().splitlines()) if "Cancel" in line and "Save" in line), None)
        check("the Save button is on screen", save_at is not None, tui.text())
        tui.click_text("Save", below=save_at)
        tui.expect("clicking Save saves what the clicks changed", SAVED, timeout=90, absent=(REFUSED_LOCALLY,))
        tui.quit()
        name = {"Maintenance mode": "maintenance", "Language": "language"}[clicked]
        check("the helper stored the clicked field", not current(name).startswith("Default"), current(name))


def open_updates(tui):
    tui.send("4")
    tui.wait_for("This Nodo", timeout=20)
    select_row(tui, "Updates", limit=6)
    tui.send("enter")
    tui.wait_for(re.compile(r"System\s+›\s+Updates"), "Check now", timeout=30)


CHECK_RESULT = re.compile(r"Up to date|\b\d+ updates? (are )?(available|ready|found)|\b(One|An) update")
CHECK_FAILED = re.compile(r"(?i)could ?n[o']t check|can ?n[o']t (check|reach)|unreachable|not reachable")


def check_now():
    with Journey("Check now: the result is shown"):
        ready_forms()
        newest = forms_versions()[-1]
        installed = installed_version()
        if installed != f"{newest[0]}.{newest[1]}_1":
            # A newer version waits from an earlier run. It is installed, so the first check
            # finds nothing new for tui-forms.
            ctl("check-updates")
            ctl("apply-update", FORMS, f"{newest[0]}.{newest[1]}_1")
    with Journey("Check now: the result is shown"), Session() as tui:
        open_updates(tui)
        tui.send("r")
        tui.expect("checking says what it found", CHECK_RESULT, timeout=90)
        status = ctl("update-status")
        if "candidates: []" in status:
            tui.expect("with nothing to update it says up to date", "Up to date")
        check("tui-forms is not offered before a new version", not re.search(r"\]\s+tui-forms\b", tui.text()),
              tui.text())
        tui.quit()
    with Journey("Check now: found updates are ticked"):
        major, minor = forms_versions()[-1]
        nxt = f"{major}.{minor + 1}"
        sh(f"sh {HERE}/feed.sh forms {nxt}")
    with Journey("Check now: found updates are ticked"), Session() as tui:
        open_updates(tui)
        tui.click_text("Check now")
        tui.wait_for(re.compile(r"tui-forms\s+\S+\s+→\s+" + re.escape(nxt) + r"_1"), timeout=90)
        tui.expect("the new version is listed and ticked, ready to update",
                   re.compile(r"\[x\]\s+tui-forms\s+\S+\s+→\s+" + re.escape(nxt) + r"_1"), timeout=5)
        tui.expect("the check counts what it found", re.compile(r"\b\d+ updates? (are )?(available|ready|found)|\b(One|An) update"),
                   timeout=10, absent=("Up to date",))
        tui.quit()
    away = FEED + ".away"
    try:
        with Journey("Check now: an unreachable feed is an error"):
            sh(f"rm -rf {away} && mv {FEED} {away}")
        with Journey("Check now: an unreachable feed is an error"), Session() as tui:
            open_updates(tui)
            tui.send("r")
            # Named, so an unrelated failure (the system snapshot check, say) cannot pass for it.
            tui.expect("a feed that cannot be read is reported, not hidden", CHECK_FAILED, "tui-feed", timeout=90,
                       absent=("Up to date",))
            tui.quit()
    finally:
        sh(f"[ -d {away} ] && rm -rf {FEED} && mv {away} {FEED}; nodo-tui --ctl refresh-feeds >/dev/null", check=False)


def provider_ids():
    return re.findall(r'\bid: "([^"]+)"', ctl("domain-providers"))


def connect_provider(tui, company, token, domains):
    tui.send("a")
    tui.wait_for("Where did you buy your web address?", timeout=30)
    select_row(tui, company, limit=15)
    tui.send("enter")
    tui.wait_for(f"Connect {company}", timeout=10)
    tui.type(token)
    check(f"the {company} key shows as bullets", token not in tui.text(), tui.text())
    tui.send("enter")
    tui.expect(f"{company} connects and lists every domain its key controls", f"{company} is connected", *domains,
               timeout=60)
    tui.send("enter")
    tui.wait_for("Add a provider", timeout=20)


def providers():
    up = ""
    try:
        with Journey("Domain providers: two providers, one with two domains"):
            ready_device()
            up = sh(f"sh {HERE}/web-net.sh up {j.CF_TOKEN} {GANDI_TOKEN}", timeout=300)
            for provider in provider_ids():
                ctl("remove-provider", provider)
        with Journey("Domain providers: two providers, one with two domains"), Session() as tui:
            tui.send("3")
            tui.wait_for("Domain providers", timeout=20)
            tui.send("p")
            tui.expect("p opens the domain providers, none yet", "The companies that hold your web addresses",
                       "None yet.", timeout=20)
            connect_provider(tui, "Cloudflare", j.CF_TOKEN, ["example.com", "example.net"])
            tui.expect("the list shows Cloudflare with both its domains",
                       re.compile(r"Cloudflare\s+example\.com\s+example\.net"))
            connect_provider(tui, "Gandi", GANDI_TOKEN, ["example.org"])
            tui.expect("the list shows both providers, each with its domains",
                       re.compile(r"Cloudflare\s+example\.com\s+example\.net"), re.compile(r"Gandi\s+example\.org"))
            tui.quit()
        with Journey("Domain providers: the helper side"):
            listed = ctl("domain-providers")
            check("the helper holds both providers", len(provider_ids()) == 2, listed)
            check("with all three domains", all(d in listed for d in ("example.com", "example.net", "example.org")),
                  listed)
        with Journey("Domain providers: looking again keeps every domain"), Session() as tui:
            # A provider read again must keep all of its domains, and the other provider its own.
            tui.send("3")
            tui.wait_for("Domain providers", timeout=20)
            tui.send("p")
            tui.wait_for("Add a provider", timeout=20)
            tui.send("r")
            tui.expect("looking again keeps every domain", re.compile(r"lists 2 domains"), timeout=60)
            tui.expect("both providers are still listed", re.compile(r"Cloudflare\s+example\.com\s+example\.net"),
                       re.compile(r"Gandi\s+example\.org"))
            tui.quit()
    finally:
        if up:
            sh(f"sh {HERE}/web-net.sh down", check=False)


JOURNEYS = [
    ("forms-install", forms_install),
    ("forms-install-esc", forms_install_esc),
    ("forms-settings", forms_settings),
    ("forms-mouse", forms_mouse),
    ("check-now", check_now),
    ("providers", providers),
]

# Journeys that fail on main until branch tui-setup-fixes lands, and what each waits for. A FAIL
# in them before then is the bug they guard against, not a broken test.
NEEDS_FIXES = {
    "forms-install": "the install page shows at once instead of the review sitting there (update.rs start_install)",
    "forms-install-esc": "leaving the install page reports its outcome as a notice (on_install_progress)",
    "forms-settings": "an empty optional int no longer blocks saving; clearing sends a reset",
    "forms-mouse": "clicks on a scrolled form resolve to the field drawn under the pointer",
    "check-now": "Check now reports its result and ticks what it found; an unreachable feed is an error",
}
# Still failing with tui-setup-fixes 22ab4d8 (its nodo-tui swapped into the VM, 2026-11-18):
# check-now's result text. The system snapshot check always fails in the offline VM, and Check
# now then says only "Could not check for updates: could not download the snapshot manifest…",
# even when it found an app update, and says nothing about the unreadable tui-feed.
# providers passes on main: two providers and a key with two domains already work through the
# helper. It stays as a guard for the providers work going on separately.
