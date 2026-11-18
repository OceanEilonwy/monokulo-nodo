#!/usr/bin/env python3
"""The real nodo-tui against the real nodo-helper in the dev VM, through a pseudo-terminal.

Runs as root inside a fresh VM, but every TUI session runs as tui-tester, an ordinary user in the
nodo group, so the helper's access rules are the ones a person meets. Each journey opens its own
session, presses keys and waits for screen text with a timeout. A failed wait prints the last
screen, and the remaining journeys still run. tests/tui-e2e.sh copies this directory into the VM
and starts it.

Journeys follow the four tabs (Home, Apps, Web, System): first boot and its USB restore, Home,
sources, installing and settings under Apps, Hosting, domain providers and onion addresses under
Web, Updates, Undo an update, the Nodo's own update, Network, Device logs and Backups under System,
and the error states. At the end, no passphrase may appear in anything the TUI drew or in any log
on the device.
"""

import os
import re
import subprocess
import sys
import time
import traceback

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ptydrive import ScreenTimeout, Tui  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
WORK = "/tmp/tui-e2e"
USER = "tui-tester"
PASSPHRASE = "correct horse battery"
WRONG_PASSPHRASE = "wrong horse battery"
BACKUP_PASSPHRASE = "backup staple horse lantern"
# The fake Cloudflare API in tests/acme/fake-dns.py accepts only this token.
CF_TOKEN = "cfut_0123456789abcdefghijABCDEFGHIJ0123456789"
# Strings that must never be drawn or logged. The short first try is typed on the setup screen.
SECRETS = [PASSPHRASE, WRONG_PASSPHRASE, BACKUP_PASSPHRASE, "horse", CF_TOKEN]
FEED = "/srv/tui-feed"

passed = 0
failed = 0
only = set(sys.argv[1:])


def say(text):
    print(text, flush=True)


def ok(name):
    global passed
    passed += 1
    say(f"PASS {name}")


def bad(name, detail=""):
    global failed
    failed += 1
    say(f"FAIL {name}")
    for line in detail.rstrip().splitlines():
        say(f"    {line}")


def sh(command, check=True, timeout=120):
    """Runs a root shell command in the VM and returns its output."""
    result = subprocess.run(["sh", "-c", command], capture_output=True, text=True, timeout=timeout)
    if check and result.returncode != 0:
        raise AssertionError(f"command failed ({result.returncode}): {command}\n{result.stdout}{result.stderr}")
    return result.stdout + result.stderr


def ctl(*args, check=True):
    quoted = " ".join("'" + a.replace("'", "'\\''") + "'" for a in args)
    return sh(f"nodo-tui --ctl {quoted}", check=check)


def wait_until(what, test, timeout=30):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if test():
            return
        time.sleep(0.5)
    raise AssertionError(f"timed out waiting for {what}")


class Session(Tui):
    """One nodo-tui session as the unprivileged tester. Every byte it draws goes to raw.log."""

    raw_log = None

    def __init__(self, rows=32, cols=100):
        super().__init__(
            ["chpst", "-u", f"{USER}:nodo", "nodo-tui"],
            rows=rows,
            cols=cols,
            env={"HOME": f"/home/{USER}", "USER": USER},
            log=Session.raw_log,
        )

    def expect(self, name, *needles, timeout=20, absent=()):
        self.wait_for(*needles, timeout=timeout, absent=absent)
        ok(name)

    def quit(self):
        self.send("ctrl-c")
        self.wait_exit()


class Journey:
    """Runs a journey's steps in order. The first failure ends the journey, not the run."""

    def __init__(self, name):
        self.name = name

    def __enter__(self):
        say(f"# {self.name}")
        return self

    def __exit__(self, kind, error, tb):
        if error is None:
            return False
        if isinstance(error, ScreenTimeout):
            bad(f"{self.name}: a screen did not appear", str(error))
        elif isinstance(error, AssertionError):
            bad(f"{self.name}: {error}".splitlines()[0], str(error))
        else:
            bad(f"{self.name}: crashed", "".join(traceback.format_exception(kind, error, tb)))
        return True


def wanted(name):
    return not only or name in only or ("device" in only and name in DEVICE)


def check(name, condition, detail=""):
    if condition:
        ok(name)
    else:
        raise AssertionError(f"{name}\n{detail}")


# Setup, as root

def prepare():
    os.makedirs(WORK, exist_ok=True)
    Session.raw_log = open(f"{WORK}/raw.log", "wb")
    sh(f"id {USER} >/dev/null 2>&1 || useradd -m {USER}")
    sh(f"usermod -aG nodo {USER}")
    sh("[ -L /var/service/nodo-helper ] || ln -s /etc/sv/nodo-helper /var/service/")
    # The helper reads the nodo group when it starts, so it restarts after the change.
    wait_until("the helper service", lambda: "run:" in sh("sv status nodo-helper", check=False), 30)
    sh("sv -w 30 restart nodo-helper")
    wait_until("the helper socket", lambda: sh("nodo-tui --ctl services", check=False).find("Services") >= 0, 30)
    # On a VM already past first boot, the restart locked it, and later journeys expect it open.
    if "encryption: Locked" in ctl("overview"):
        sh(f"printf '%s\\n' '{PASSPHRASE}' | nodo-tui --ctl unlock")
    if not os.path.exists(FEED):
        sh(f"sh {HERE}/feed.sh init && sh {HERE}/feed.sh pkg 1.0")
    # cuprated must be up for Home to show its height.
    wait_until("cuprated's RPC", lambda: ":46A1 00000000:0000 0A" in sh("cat /proc/net/tcp"), 180)


def restart_helper():
    sh("sv -w 30 restart nodo-helper")
    wait_until("the helper socket", lambda: "Services" in sh("nodo-tui --ctl services", check=False), 30)


# Driving the screens. The TUI has four tabs since 5cd7557: Home, Apps, Web and System. Breadcrumbs
# are drawn with wide gaps ("Apps  ›  tui-shop"), so they are matched with crumbs().

def crumbs(*parts):
    """A breadcrumb trail such as Apps › tui-shop › Settings, however wide its gaps are drawn."""
    return re.compile(r"\s+›\s+".join(re.escape(p) for p in parts))


def select(tui, name, limit=30):
    """Moves a list's ▸ marker down onto the row that starts with `name`."""
    row = re.compile(r"▸\s*(\[.\]\s+)?" + re.escape(name) + r"(\s|$)", re.M)
    for _ in range(limit):
        if tui.contains(row):
            return
        tui.send("down")
    raise AssertionError(f"no row {name} to select\n{tui.text()}")


def go_tab(tui, number):
    """Back out of whatever page is open, then to tab `number`: pages under a tab take their own
    keys, so the digits only switch tabs from a tab's first page."""
    for _ in range(4):
        tui.send("esc", pause=0.1)
    tui.send(str(number))


def open_system_row(tui, row, crumb):
    """System › `row`, from any page."""
    go_tab(tui, 4)
    tui.wait_for("This Nodo", timeout=20)
    select(tui, row, limit=8)
    tui.send("enter")
    tui.wait_for(crumbs("System", crumb), timeout=30)


def open_app(tui, name):
    """Apps › `name`, the app's own page."""
    go_tab(tui, 2)
    tui.wait_for("Installed", timeout=20)
    select(tui, name)
    tui.send("enter")
    tui.wait_for(crumbs("Apps", name), timeout=20)


def open_app_row(tui, name, row):
    """One row of an app's page, such as Settings or Hosting."""
    open_app(tui, name)
    select(tui, row, limit=6)
    tui.send("enter")
    tui.wait_for(crumbs("Apps", name, row), timeout=30)


def open_sources(tui):
    go_tab(tui, 2)
    tui.wait_for("Installed", timeout=20)
    tui.send("g")
    tui.wait_for("Sources ›", timeout=20)
    tui.click_text("Sources ›")
    tui.wait_for(crumbs("Get apps", "Sources"), timeout=20)


def select_field(tui, label, labels):
    """Moves the selection of a settings form onto `label`. `labels` are the form's, in order."""
    longest_first = sorted(labels, key=len, reverse=True)

    def selected():
        screen = tui.text()
        return next((l for l in longest_first if re.search(r"▸ " + re.escape(l) + r"(\s|$)", screen, re.M)), None)

    for _ in range(2 * len(labels) + 2):
        now = selected()
        if now == label:
            return
        if now is None:
            raise AssertionError(f"no field of the form is selected\n{tui.text()}")
        tui.send("down" if labels.index(label) > labels.index(now) else "up", pause=0.08)
    raise AssertionError(f"could not select {label}\n{tui.text()}")


SAVED = re.compile(r"\bSaved\b")


# Journeys

def first_boot():
    with Journey("first boot: passphrase setup"), Session() as tui:
        tui.expect("first boot opens the passphrase setup", "Set up this device", "Device passphrase",
                   "There is no default passphrase")
        tui.type("short", enter=True)
        tui.expect("a short passphrase is refused with its rule", "Use at least 8 characters.")
        tui.send("ctrl-u")
        tui.type(PASSPHRASE, enter=True)
        tui.expect("the passphrase is asked for again", "Again", re.compile(r"Esc\s+retype the first"))
        tui.type(WRONG_PASSPHRASE, enter=True)
        tui.expect("two different passphrases are refused", "The two passphrases differ. Type them again.")
        tui.type(PASSPHRASE)
        tui.expect("the passphrase shows as one bullet per character", "•" * len(PASSPHRASE))
        tui.send("enter")
        tui.wait_for("Again")
        tui.type(PASSPHRASE, enter=True)
        tui.expect("setting it moves on to SSH keys", "SSH keys", "Device passphrase set. Keep it somewhere safe.",
                   "monokulo-test", timeout=60)
        tui.send("esc")
        tui.expect("leaving the keys screen opens Home with the node running", "Home",
                   re.compile(r"cuprated\s+Running"), timeout=30)
        tui.quit()
    with Journey("first boot: the helper side"):
        check("the device key exists, root only",
              sh("stat -c '%a %U' /var/lib/nodo/keys/device.key").strip() == "600 root")
        check("the helper reports encryption as unlocked", "encryption: Unlocked" in ctl("overview"))


def home():
    with Journey("Home"), Session() as tui:
        # Height and peers left Home with the four-tab redesign; it lists the apps and their state.
        tui.expect("Home lists cuprated as running", re.compile(r"cuprated\s+Running · up "), timeout=30)
        tui.expect("Home warns that the health monitor is not reporting", "The health monitor is not reporting")
        tui.expect("Home's health line shows the host and the system disk", "Host monokulo-nodo", "System disk")
        tui.send("?")
        tui.expect("? opens the help", "Help", "Choices that could harm the device start on the safe answer.")
        tui.send("esc")
        tui.wait_gone("Choices that could harm")
        tui.send("4")
        tui.wait_for("This Nodo", timeout=20)
        select(tui, "About this Nodo", limit=8)
        tui.send("enter")
        tui.expect("About this Nodo shows the host and the uptime", "About this Nodo", "monokulo-nodo", "Uptime",
                   timeout=20)
        check("uptime reads as plain English", not re.search(r"\b1 minutes\b|\b1 hours\b", tui.text()), tui.text())
        tui.send("esc")
        tui.quit()


def feeds_add():
    with Journey("Feeds: add with key confirmation"), Session() as tui:
        probe = ctl("probe-feed", FEED)
        raw = re.search(r'fingerprint: "([^"]+)"', probe).group(1)
        open_sources(tui)
        tui.expect("Sources lists where apps come from, without the test feed yet", "Add a source",
                   "Where the Nodo finds apps", absent=("tui-feed",))
        tui.send("a")
        tui.expect("a opens Add a source", "Add a source", "Paste the address the publisher gave you.")
        tui.type(FEED, enter=True)
        tui.expect("the feed is looked up and its key shown", re.compile(r"✓\s+tui-feed\s+/srv/tui-feed"),
                   "It offers 1 app.", "Its key", "Check this matches what the publisher gave you", timeout=30)
        shown = re.search(r"Its key\s+((?:[0-9a-f]{8}\s+){3}[0-9a-f]{8})", tui.text())
        check("the key shown is the feed's real fingerprint",
              shown and re.sub(r"\s", "", shown.group(1)) == re.sub(r"[\s:]", "", raw).lower(), f"{shown} vs {raw}")
        tui.expect("Cancel is the default choice", re.compile(r"▸ Cancel\s+Add source"))
        tui.send("enter")
        tui.expect("Enter on the default cancels", crumbs("Get apps", "Sources"), "Add a source",
                   absent=("It offers 1 app.",))
        check("cancelling added nothing", "tui-feed" not in ctl("feeds"))
        tui.send("a")
        tui.type(FEED, enter=True)
        tui.wait_for("It offers 1 app.", timeout=30)
        tui.send("right", "enter")
        tui.expect("trusting the key adds the feed", "Added tui-feed and pinned its key.",
                   re.compile(r"tui-feed\s+\[x\] On"), timeout=30)
        tui.quit()
    with Journey("Feeds: the helper side"):
        check("the helper pinned the confirmed key", f'fingerprint: "{raw}"' in ctl("feeds"))


def install_with_passphrase(tui, name):
    """From an app's review, approves the install and types the device passphrase if asked."""
    tui.send("right", "enter", pause=0.5)
    if "Its locked storage is made now" in tui.text():
        tui.type(PASSPHRASE)
        tui.wait_for("•" * len(PASSPHRASE))
        tui.send("enter", pause=0)


def packages_install():
    with Journey("Packages: search and install with permission review"), Session() as tui:
        tui.send("2")
        tui.wait_for("Installed", timeout=20)
        tui.send("g")
        tui.expect("Get apps opens with the search focused", crumbs("Apps", "Get apps"), "Search", "Type to search.")
        tui.type("shop")
        tui.expect("search finds the test package in its feed", re.compile(r"tui-shop\s+From the feed tui-feed"),
                   timeout=20)
        tui.send("down", "enter")
        tui.expect("Enter opens the package detail", crumbs("Get apps", "tui-shop"), "from tui-feed", "Install",
                   timeout=20)
        check("a feed package is not marked as a failed signature", "signature not verified" not in tui.text(),
              tui.text())
        tui.send("i")
        tui.expect("i shows the permission review", "Install tui-shop?", "It comes from tui-feed, a source you added",
                   "Connects to the internet", "Serves the site 'shop'", "Keep data in encrypted storage", timeout=60)
        tui.expect("the review starts on Cancel", re.compile(r"▸ Cancel\s+Install"))
        tui.send("esc")
        tui.wait_gone("Install tui-shop?")
        check("leaving the review installed nothing", "tui-shop" not in sh("xbps-query -l"))
        tui.send("i")
        tui.wait_for("Install tui-shop?", timeout=60)
        tui.send("right", "enter", pause=0.5)
        tui.expect("its encrypted storage asks for the device passphrase", "Its locked storage is made now")
        tui.type(PASSPHRASE)
        tui.expect("the passphrase shows as bullets", "•" * len(PASSPHRASE))
        tui.send("enter")
        tui.expect("approving shows the install while the helper works", "Installing tui-shop", timeout=30)
        tui.expect("the install finishes", "tui-shop is installed", timeout=240)
        tui.quit()
    with Journey("Packages: the helper side"):
        check("xbps installed the approved version", sh("xbps-query -p pkgver tui-shop").strip() == "tui-shop-1.0_1")
        check("its storage is an open LUKS volume",
              sh("mountpoint -q /run/nodo/volumes/tui-shop && cryptsetup status nodo-tui-shop", check=False).find(
                  "is active") >= 0)
        wait_until("tui-shop to run", lambda: sh("sv status tui-shop", check=False).startswith("run:"), 30)
        ok("the installed package runs as a service")


SHOP_LABELS = ["Worker threads", "Greeting"]


def package_settings():
    with Journey("Packages: settings"), Session() as tui:
        open_app_row(tui, "tui-shop", "Settings")
        tui.expect("Settings opens the package's form from its Lua schema", "Worker threads", "Greeting",
                   "Requests handled at once.", "No changes yet", timeout=20)
        select_field(tui, "Worker threads", SHOP_LABELS)
        tui.send("enter", "ctrl-u")
        tui.type("99", enter=True)
        tui.expect("an out of range value is refused in plain words", "Must be from 1 to 16.")
        tui.send("ctrl-s")
        tui.expect("saving with the refused value changes nothing",
                   "Fix the marked field before saving. Nothing was changed.")
        select_field(tui, "Worker threads", SHOP_LABELS)
        tui.send("enter", "ctrl-u")
        tui.type("4", enter=True)
        tui.wait_gone("Must be from 1 to 16.")
        tui.expect("a valid value is a pending change", "1 change, not saved yet")
        select_field(tui, "Greeting", SHOP_LABELS)
        tui.send("enter", "ctrl-u")
        tui.type("this greeting is far too long", enter=True)
        tui.send("ctrl-s")
        tui.expect("the package's own validator refuses through the helper", "must be 12 characters or fewer",
                   timeout=30)
        check("the refused save left the stored settings alone", "workers" not in sh(
            "cat /var/lib/nodo/settings/tui-shop.toml 2>/dev/null || true"))
        select_field(tui, "Greeting", SHOP_LABELS)
        tui.send("enter", "ctrl-u")
        tui.type("hi there", enter=True)
        tui.send("ctrl-s")
        tui.expect("a valid save restarts the package and passes its health check",
                   "Saved. tui-shop restarted and passed its health check.", timeout=90)
        tui.quit()
    with Journey("Packages: settings on the device"):
        stored = sh("cat /var/lib/nodo/settings/tui-shop.toml")
        check("the helper stored the saved values", "literal = \"4\"" in stored and "literal = \"hi there\"" in stored,
              stored)


def open_updates(tui):
    open_system_row(tui, "Updates", "Updates")
    tui.wait_for("Check now", timeout=30)


def updates():
    with Journey("Updates: check, hold and auto-update"):
        # Published once: a rebuilt 1.1 would not match the copy the helper already fetched.
        if not os.path.exists(f"{FEED}/tui-shop-1.1_1.x86_64.xbps"):
            sh(f"sh {HERE}/feed.sh pkg 1.1")
        # A rerun starts from 1.1 not held.
        ctl("hold", "tui-shop", "1.1_1", "0", check=False)
    with Journey("Updates: check, hold and auto-update"), Session() as tui:
        open_updates(tui)
        tui.send("r")
        tui.expect("checking finds the new version, ticked", re.compile(r"\[x\]\s+tui-shop\s+1\.0_1\s+→\s+1\.1_1"),
                   timeout=90)
        tui.send("h")
        # Held, it is no longer ticked.
        tui.expect("h holds that version",
                   re.compile(r"\[ \]\s+tui-shop\s+1\.0_1\s+→\s+1\.1_1\s+(held back|you skipped this version)"), timeout=30)
        tui.send("a")
        tui.expect("a opens automatic updates, off for the package", crumbs("Updates", "Automatic"),
                   "Update these apps by themselves", re.compile(r"tui-shop\s+\[ \] Off"), timeout=20)
        select(tui, "tui-shop", limit=8)
        tui.send("space")
        tui.expect("Space picks the package", re.compile(r"tui-shop\s+\[x\] On"))
        tui.send("enter")
        tui.expect("saving turns automatic updates on for it", "Auto-update choices saved.", timeout=30)
        tui.quit()
    with Journey("Updates: the helper side"):
        status = ctl("update-status")
        check("the helper holds the version", "held: true" in status, status)
        check("auto-update is on for the package only", '"tui-shop"' in ctl("auto-get"), ctl("auto-get"))
        check("nothing was updated while it is held", sh("xbps-query -p pkgver tui-shop").strip() == "tui-shop-1.0_1")


def feed_key_changed():
    with Journey("Feeds: key changed"):
        sh(f"sh {HERE}/feed.sh resign")
    with Journey("Feeds: key changed"), Session() as tui:
        open_sources(tui)
        select(tui, "tui-feed", limit=8)
        tui.send("r")
        tui.expect("looking again shows the feed's key changed", "Its key changed. Nothing new installs from it",
                   timeout=30)
        tui.send("enter")
        tui.expect("Enter on it explains the change and shows both keys", "tui-feed is now signed with a different key",
                   "Key you trust", "New key", timeout=20)
        tui.expect("keeping the old key is the default", "▸ Keep the old key")
        old = re.search(r'pinned: "([^"]+)"', ctl("feed-key", "tui-feed")).group(1)
        tui.send("down", "enter")
        tui.type("tui-fee", enter=True)
        tui.expect("a wrong feed name is refused", "That isn't the source's name. Type tui-feed exactly.")
        tui.send("ctrl-u")
        tui.type("tui-feed", enter=True)
        tui.expect("the typed name trusts the new key", "tui-feed now uses the new key.", timeout=30)
        tui.quit()
    with Journey("Feeds: key changed, helper side"):
        feeds = ctl("feeds")
        check("the new key is pinned", "key_changed: false" in feeds and old not in feeds, feeds)


def site_flags():
    """The shop site's exposure as the helper reports it: (local, onion, public)."""
    out = ctl("sites")
    block = out[out.find('site: "shop"'):]
    flags = re.findall(r"(local|onion|public): (true|false)", block)[:3]
    return tuple(value == "true" for _, value in flags)


def web():
    pebble = False
    with Journey("Web: hosting"):
        # The network comes first: a session reads the device once when it starts.
        pebble = "pebble" in sh(f"sh {HERE}/web-net.sh up {CF_TOKEN}", timeout=300)
        ctl("expose", "tui-shop", "shop", "0", "0", "0")  # a rerun starts from off too
        for provider in re.findall(r'\bid: "([^"]+)"', ctl("domain-providers")):
            ctl("remove-provider", provider)
    try:
        with Journey("Web: hosting"), Session() as tui:
            tui.send("3")
            tui.expect("the Web tab says no site is online yet", "Sites online", "No site is online yet.",
                       "Domain providers ›", timeout=20)
            open_app_row(tui, "tui-shop", "Hosting")
            tui.expect("Hosting offers the three ways in, all off", "Choose who can open tui-shop",
                       re.compile(r"\[ \]\s+Home network"), re.compile(r"\[ \]\s+Tor"), re.compile(r"\[ \]\s+Internet"))
            tui.send("space")
            tui.expect("Space ticks the home network", re.compile(r"\[x\]\s+Home network"))
            tui.send("enter")
            tui.expect("saving applies it", "Saved.", timeout=60, absent=("Choose who can open tui-shop",))
            check("the helper has the site on the local network only", site_flags() == (True, False, False),
                  ctl("sites"))
            go_tab(tui, 3)
            tui.expect("the Web tab lists the site on the home network",
                       re.compile(r"tui-shop\s+https://\S+\s+Home network"), timeout=20)
            tui.send("enter")
            tui.wait_for(crumbs("Apps", "tui-shop", "Hosting"), timeout=20)
            select(tui, "[ ]  Internet", limit=6)
            tui.send("space")
            tui.expect("the internet first needs a domain provider", "first add the company you bought it from",
                       "Add a provider")
            tui.send("up", "enter")
            tui.expect("saving without a provider says what to do",
                       "Add a domain provider first, so the internet can use your web address.")
            check("nothing went public", site_flags()[2] is False, ctl("sites"))
            go_tab(tui, 3)
            tui.wait_for("Sites online", timeout=20)
            tui.send("d")
            tui.expect("d opens the certificates and domains", crumbs("Web", "Domains"), "Email for expiry notices",
                       "Use the staging server", timeout=20)
            tui.type("not an email", enter=True)
            tui.expect("a bad email is refused in plain words", "not valid", timeout=30)
            tui.send("ctrl-u")
            tui.type("tui@example.com", enter=True)
            tui.expect("a good email is saved", "Certificate settings saved and applied.", absent=("not valid",),
                       timeout=30)
            check("the helper stored the email", "tui@example.com" in ctl("acme"))
            tui.send("esc")
            tui.wait_for("Sites online", timeout=20)
            tui.send("p")
            tui.expect("p opens the domain providers, none yet", crumbs("Web", "Domain providers"),
                       "The companies that hold your web addresses", timeout=20)
            tui.send("a")
            tui.expect("adding asks where the address was bought", "Where did you buy your web address?", "Cloudflare",
                       timeout=30)
            select(tui, "Cloudflare", limit=15)
            tui.send("enter")
            tui.wait_for("Connect Cloudflare", timeout=10)
            tui.type(CF_TOKEN)
            check("the API token shows as bullets", CF_TOKEN not in tui.text() and "•" * 10 in tui.text(), tui.text())
            tui.send("enter")
            tui.expect("Cloudflare connects and lists the domains its key controls", "Cloudflare is connected",
                       "example.com", timeout=60)
            tui.send("enter")
            tui.wait_for("Add a provider", timeout=20)
            tui.send("esc")
            # The internet, now with a provider: the check passes what it can and shows what it
            # could not verify, such as the ports, for the owner to decide.
            open_app_row(tui, "tui-shop", "Hosting")
            select(tui, "[ ]  Internet", limit=6)
            tui.send("space")
            tui.expect("the internet offers the provider's domain", "Provider", "Cloudflare", "example.com")
            tui.send("enter")
            tui.wait_for(re.compile(r"Ready to put tui-shop on the internet|The internet can't reach|"
                                    r"Some checks couldn't be confirmed"), timeout=120)
            if not pebble:
                # Without tests/acme-prepare.sh there is no DNS server on the pretend internet, so
                # the temporary record cannot be seen and the check stops the owner there.
                tui.expect("without a DNS server the check says the record failed, and offers Tor",
                           "✓  Domain and API key work", "✕  Temporary DNS record failed", "Use Tor instead")
                check("nothing went public", site_flags()[2] is False, ctl("sites"))
                tui.send("esc")
            else:
                if "Ready to put tui-shop" not in tui.text():
                    # Nothing failed, but the ports cannot be verified from inside the VM.
                    tui.expect("the reachability check passes what it can and says what it could not confirm",
                               "Some checks couldn't be confirmed", "✓  Domain and API key work",
                               "✓  Temporary DNS record works", "Continue anyway", absent=("✕",))
                    tui.send("c")
                tui.expect("going public asks first, on the safe answer", "Ready to put tui-shop on the internet",
                           re.compile(r"https://shop\.example\.com"), "Your home's internet address becomes public.",
                           "Cancel is selected for the keyboard.", timeout=60)
                check("nothing is public before the confirm", site_flags()[2] is False, ctl("sites"))
                tui.send("tab", "enter")
                tui.expect("going public shows the secure connection being set up", "tui-shop is online",
                           "https://shop.example.com", timeout=90)
                tui.expect("the certificate comes from the test ACME server", re.compile(r"✓\s+tui-shop is online"),
                           timeout=180, absent=("The secure connection isn't ready",))
                check("the helper has the site public", site_flags()[2] is True, ctl("sites"))
                tui.send("enter")
                go_tab(tui, 3)
                tui.expect("the Web tab lists the site as public", re.compile(r"tui-shop\s+https://shop\.example\.com"),
                           timeout=20)
            go_tab(tui, 3)
            tui.wait_for("Sites online", timeout=20)
            tui.send("o")
            tui.expect("taking every site offline asks first, on the safe answer", "Take all sites offline?",
                       re.compile(r"▸Cancel"))
            tui.send("right", "enter")
            tui.expect("every site goes offline", "No site is online yet.", timeout=60)
            check("the helper has the site off everywhere", site_flags() == (False, False, False), ctl("sites"))
            tui.quit()
    finally:
        sh(f"sh {HERE}/web-net.sh down", check=False)


def logs():
    marker = f"tui-e2e marker {int(time.time())}"
    with Journey("Logs"), Session() as tui:
        open_system_row(tui, "Device logs", "Device logs")
        tui.expect("Device logs lists the log sources, following", "Problems", "cuprated", "nodo-helper",
                   "● Following live", timeout=20)
        sh(f"logger -t tui-e2e '{marker}' 2>/dev/null; echo '{marker}' >> /var/log/cuprated/current")
        # The device logs open on problems only, so a plain line shows once e turns that off. The
        # line was written after the page opened, so finding it shows the page follows the log.
        tui.expect("problems come first, so the plain new line is not shown", absent=(marker,))
        tui.send("e", "/")
        tui.type("e2e marker")
        tui.send("enter")
        tui.expect("with every line shown, search finds the newest line", marker, timeout=20)
        tui.send("e")
        tui.expect("problems only hides other lines", absent=(marker,))
        tui.send("f")
        tui.expect("f pauses following", "○ Paused")
        tui.quit()


def backups():
    name = ""
    with Journey("Backups"), Session() as tui:
        tui.wait_for("Home", timeout=20)
        tui.send("b")
        tui.expect("b opens the backups, empty at first", crumbs("System", "Backups"), "Backups on this device",
                   "No backups yet. Press n to make one.", timeout=20)
        tui.send("n")
        tui.wait_for("Device passphrase", "Backup passphrase")
        tui.type(PASSPHRASE, enter=True)
        tui.type(BACKUP_PASSPHRASE, enter=True)
        tui.type(BACKUP_PASSPHRASE, enter=True)
        tui.expect("a backup is made with the device key", re.compile(r"nodo-.*\.nodobak"), timeout=120,
                   absent=("passphrase did not",))
        name = re.search(r"(nodo-\S*\.nodobak)", tui.text()).group(1)
        tui.send("esc")
        tui.wait_for("Backups on this device", timeout=20)
        tui.send("enter")
        tui.type("not the backup passphrase", enter=True)
        tui.expect("a wrong backup passphrase is refused", "passphrase did not open", timeout=60)
        tui.send("ctrl-u")
        tui.type(BACKUP_PASSPHRASE, enter=True)
        tui.expect("inspecting lists what a restore would replace", "It will replace", "tui-shop", timeout=60)
        tui.send("esc")
        tui.quit()
    with Journey("Backups: the helper side"):
        check("the backup is root only", sh(f"stat -c '%a %U' /var/lib/nodo/backups/{name}").strip() == "600 root")


def locked():
    # As a reboot would: the volume closes and the helper forgets the device key. The package and
    # its logger, which keeps its log inside the volume open, stop for the close and are then
    # wanted up again, as at boot, so runit keeps trying them until the unlock opens the volume.
    with Journey("Device locked: unlock"):
        sh("sv -w 30 stop tui-shop tui-shop/log; umount /run/nodo/volumes/tui-shop && cryptsetup close nodo-tui-shop")
        restart_helper()
        sh("sv start tui-shop/log tui-shop >/dev/null 2>&1 || true")
        unlock_journey()
    with Journey("Device locked: the helper side"):
        check("the helper reports the device unlocked", "encryption: Unlocked" in ctl("overview"))
        check("the volume is open again", sh("mountpoint /run/nodo/volumes/tui-shop", check=False).find("is a mountpoint") >= 0)
        wait_until("tui-shop to run again", lambda: sh("sv status tui-shop", check=False).startswith("run:"), 30)
        ok("the waiting package started after the unlock")


def unlock_journey():
    with Session() as tui:
        tui.expect("Home says the device is locked", "Your Nodo is locked",
                   re.compile(r"tui-shop\s+Locked until you unlock"), timeout=30)
        tui.send("u")
        tui.expect("u opens the unlock screen listing what waits", "Unlock your Nodo",
                   re.compile(r"tui-shop start after you unlock|tui-shop starts? after you unlock"), timeout=10)
        tui.type(WRONG_PASSPHRASE, enter=True)
        tui.expect("a wrong passphrase is refused in plain words",
                   "That passphrase did not unlock this device. Try again.", timeout=60)
        tui.pump(3)  # the first retry delay
        tui.send("ctrl-u")
        tui.type(PASSPHRASE, enter=True)
        tui.expect("the right passphrase unlocks", re.compile(r"Unlocked\."), timeout=60)
        tui.quit()


def helper_down():
    with Journey("Helper down"):
        sh("sv -w 30 stop nodo-helper")
        try:
            with Session() as tui:
                status = tui.wait_exit(10)
                check("without the helper the TUI explains and exits", status != 0 and
                      "cannot reach the helper" in tui.text() and "nodo group" in tui.text(), tui.text())
        finally:
            sh("sv -w 30 start nodo-helper")
            wait_until("the helper socket", lambda: "Services" in sh("nodo-tui --ctl services", check=False), 30)
    with Journey("Helper restarts while the TUI is open"), Session() as tui:
        open_sources(tui)
        tui.wait_for("tui-feed", timeout=20)
        sh("sv -w 30 stop nodo-helper")
        tui.send("r")
        tui.expect("a failed request is shown, not hidden", re.compile(r"(?i)could not|cannot reach|not running|refused"),
                   timeout=20)
        sh("sv -w 30 start nodo-helper")
        wait_until("the helper socket", lambda: "Services" in sh("nodo-tui --ctl services", check=False), 30)
        tui.send("r")
        tui.expect("the TUI recovers once the helper is back", re.compile(r"tui-feed\s+\[x\] On"), timeout=30,
                   absent=("cannot reach",))
        tui.quit()
        sh(f"printf '%s\\n' '{PASSPHRASE}' | nodo-tui --ctl unlock")


def no_secrets_anywhere():
    with Journey("No passphrase on screen or in logs"):
        Session.raw_log.flush()
        drawn = open(f"{WORK}/raw.log", "rb").read().decode("utf-8", "replace")
        for secret in SECRETS:
            check(f"the TUI never drew '{secret[:6]}…'", secret not in drawn)
        found = sh("grep -rIl -e 'horse' -e 'staple horse' /var/log /run/nodo /var/lib/nodo /tmp/tui-pkgs 2>/dev/null "
                   "| grep -v '^/var/lib/nodo/backups/' || true").strip()
        check("no log or state file holds a passphrase", found == "", found)
        # The DNS token is meant to be in Caddy's and nodo-ddns's private configs, never in a log.
        logged = sh(f"grep -rIl -e '{CF_TOKEN}' /var/log 2>/dev/null || true").strip()
        check("no log holds the DNS provider token", logged == "", logged)



def restore_entry():
    """First boot offers Ctrl-R. Run before first_boot, while the device is not set up."""
    with Journey("first boot: restore from a USB backup"), Session() as tui:
        tui.expect("first boot offers the USB restore", "Ctrl-R restore from a USB backup")
        tui.send("ctrl-r")
        tui.expect("Ctrl-R opens the restore on its drive step", crumbs("Restore from a USB backup", "Drive"), "USB drives",
                   "▸ Choose the drive", timeout=30)
        tui.pump(2)
        screen = tui.text()
        if "No USB drive is plugged in" in screen:
            tui.send("enter")
            tui.expect("choosing with no drive says what to do", "No USB drive is plugged in. Plug one in and press g.")
        else:
            # tests/tui-e2e.sh plugs in a blank stick, which has no filesystem to read.
            check("a plugged-in drive is listed with why it cannot be used", "▲" in screen, screen)
            tui.send("enter")
            tui.expect("a drive that cannot be used is refused in plain words", "cannot be used:")
        check("the system disk is never offered", not re.search(r"\bvda\d*\b", tui.text()), tui.text())
        tui.send("g")
        tui.expect("g looks for drives again", "USB drives", timeout=30, absent=("Working…",))
        tui.send("esc")
        tui.expect("Esc goes back to the passphrase setup", "Set up this device", "Device passphrase")
        tui.quit()
    with Journey("first boot: restore left nothing behind"):
        check("the device is still not set up", "encryption: NotSetUp" in ctl("overview"), ctl("overview"))


TORRC = "/var/lib/nodo-web/torrc"
NODE_HOSTNAME = "/var/lib/tor/nodo/node-rpc/hostname"


def tor_running():
    return sh("sv status tor", check=False).startswith("run:")


def onion():
    # cuprated always serves restricted RPC on 127.0.0.1:18081, so the node needs no setting first.
    # Tor comes from the pinned mirror (tests/tui-e2e.sh installs it). The VM is offline, but Tor
    # makes the onion address's keys and hostname file at start without the network.
    with Journey("Onion: node and site on and off"), Session() as tui:
        check("Tor is installed", os.path.isdir("/etc/sv/tor"))
        tui.send("3")
        tui.wait_for("Sites online", timeout=20)
        tui.send("n")
        tui.expect("n opens the onion screen with the node and the site", crumbs("Web", "Onion addresses"), "cuprated",
                   "tui-shop", timeout=30)
        tui.expect("the node can go on Tor with no setting first", "restricted RPC, port 18081", timeout=30,
                   absent=("no restricted RPC port",))
        tui.send("space")
        # Working… is the TUI busy.
        tui.expect("Space turns the node's onion address on", re.compile(r"\[x\] cuprated"), timeout=90,
                   absent=("Working…",))
        stored = sh("cat /var/lib/nodo/node-onion.toml")
        check("the helper stored the choice", "enabled = true" in stored, stored)
        torrc = sh(f"cat {TORRC}")
        lines = torrc.splitlines()
        check("the torrc is the helper's and runs Tor with no SOCKS port",
              lines[1:4] == ["User tor", "DataDirectory /var/lib/tor", "SocksPort 0"], torrc)
        check("the torrc names the node's onion service",
              "HiddenServiceDir /var/lib/tor/nodo/node-rpc" in lines and "HiddenServiceVersion 3" in lines, torrc)
        ports = [line for line in lines if line.startswith("HiddenServicePort") and "18081" in line]
        check("Tor forwards only to the restricted RPC on loopback",
              ports == ["HiddenServicePort 18081 127.0.0.1:18081"] and "18089" not in torrc, torrc)
        wait_until("Tor to make the node's onion address", lambda: sh(f"cat {NODE_HOSTNAME}", check=False).strip()
                   .endswith(".onion"), 60)
        host = sh(f"cat {NODE_HOSTNAME}").strip()
        check("Tor made a real onion address offline", re.fullmatch(r"[a-z2-7]{56}\.onion", host) is not None, host)
        check("Tor runs as its own user", tor_running() and sh("pgrep -u tor -x tor", check=False).strip() != "")
        tui.expect("the screen shows the address Tor made", f"{host}:18081", timeout=90)
        tui.send("space")
        tui.expect("Space turns it off again", re.compile(r"\[ \] cuprated"), timeout=90, absent=("Working…",))
        check("Tor no longer serves the node", "node-rpc" not in sh(f"cat {TORRC}"))
        before = site_flags()
        if not before[1]:
            # The site was not on Tor either, so nothing is left for Tor to publish.
            check("with every onion address off, Tor is stopped and lists no service",
                  not tor_running() and "HiddenService" not in sh(f"cat {TORRC}"), sh("sv status tor", check=False))
        mark, back = ("[ ]", "[x]") if before[1] else ("[x]", "[ ]")
        tui.send("down", "space")
        tui.expect("Space flips the site's onion exposure", f"{mark} tui-shop", timeout=90, absent=("Working…",))
        after = site_flags()
        check("only the onion flag changed", after[1] != before[1] and after[0] == before[0] and after[2] == before[2],
              ctl("sites"))
        if after[1]:
            check("Tor serves the site's onion address", tor_running() and
                  "HiddenServiceDir /var/lib/tor/nodo/tui-shop.shop" in sh(f"cat {TORRC}"), sh(f"cat {TORRC}"))
        tui.send("space")
        tui.expect("Space flips it back", f"{back} tui-shop", timeout=90, absent=("Working…",))
        check("the helper has the site as before", site_flags() == before, ctl("sites"))
        if not before[1]:
            check("turning the last onion address off stops Tor again",
                  not tor_running() and "HiddenService" not in sh(f"cat {TORRC}"), sh("sv status tor", check=False))
        tui.send("esc")
        tui.expect("Esc goes back to the sites", "Sites online")
        tui.quit()


WIFI_SSID = "NodoTest"


def network():
    # tests/tui-e2e.sh runs tests/wifi-prepare.sh, which builds mac80211_hwsim for the VM kernel.
    # wifi-ap.sh then makes two virtual radios and an access point in a network namespace, whose
    # passphrase is the device passphrase, so the final check also covers it. The VM has no
    # Bluetooth controller (Void's kernel has no virtual one the helper could pair with), so that
    # panel only says so.
    if sh("modinfo -n mac80211_hwsim", check=False).strip().startswith("modinfo"):
        with Journey("Network: no radios"), Session() as tui:
            tui.send("w")
            tui.expect("the Network screen says this VM has no Wi-Fi", crumbs("System", "Network"), "No Wi-Fi hardware was found.",
                       "No Bluetooth controller was found.", timeout=30)
            tui.quit()
        return
    try:
        with Journey("Network: Wi-Fi"):
            sh(f"sh {HERE}/wifi-ap.sh radios")
            # The helper blocks a radio the owner left off when it starts, so it starts again now
            # that there are radios. The access point then unblocks its own.
            restart_helper()
            out = sh(f"printf '%s\\n' '{PASSPHRASE}' | sh {HERE}/wifi-ap.sh up {WIFI_SSID}", check=False)
            check("the virtual access point is up", "ready" in out.splitlines(), out)
        with Journey("Network: Wi-Fi"), Session() as tui:
            tui.send("w")
            tui.expect("Wi-Fi is off until the owner turns it on", crumbs("System", "Network"), "Off. Press o to turn it on.",
                       "No Bluetooth controller was found.", timeout=30)
            check("the helper blocked the radio", "1" in sh("cat /sys/class/rfkill/*/soft"))
            tui.send("o")
            tui.expect("o turns Wi-Fi on", "Not joined to any network", timeout=60, absent=("Turning Wi-Fi on",))
            tui.send("s")
            tui.expect("s finds the access point with its security and signal",
                       re.compile(WIFI_SSID + r"\s+WPA2\s+\S+\s+-\d+ dBm"), timeout=60)
            tui.send("enter")
            tui.expect("Enter asks for the passphrase", f"Join {WIFI_SSID}", "Passphrase")
            tui.type(WRONG_PASSPHRASE)
            tui.expect("the passphrase shows as bullets", "•" * len(WRONG_PASSPHRASE))
            tui.send("enter")
            tui.expect("a wrong passphrase is refused and nothing is saved",
                       f"{WIFI_SSID} refused the passphrase. Nothing was saved.", timeout=90)
            check("the helper saved no network", not os.path.exists("/var/lib/nodo/wifi/networks.toml"))
            tui.send("enter")
            tui.wait_for(f"Join {WIFI_SSID}")
            tui.type(PASSPHRASE, enter=True)
            tui.expect("the right passphrase joins and shows the address from DHCP",
                       re.compile(rf"Joined {WIFI_SSID}\. Its address is 192\.168\.77\.5\d\."), timeout=120)
            tui.expect("the network is saved and joined", "saved · joined", "● Joined " + WIFI_SSID)
            check("the device has the address on its wireless interface",
                  "inet 192.168.77.5" in sh("ip -4 addr show wlan0"))
            routes = sh("ip route")
            wired = [int(m) for m in re.findall(r"dev eth0 .*metric (\d+)", routes)]
            wifi = [int(m) for m in re.findall(r"dev wlan0 .*metric (\d+)", routes)]
            check("wired keeps the lower route metric", wired and wifi and min(wifi) > max(wired), routes)
            tui.send("f")
            tui.expect("forgetting asks first", f"Forget {WIFI_SSID}?")
            tui.send("right", "enter")
            tui.expect("forgetting leaves the network", f"Forgot {WIFI_SSID}.", "Not joined to any network",
                       timeout=60)
            tui.send("o")
            tui.expect("o turns Wi-Fi off again", "Off. Press o to turn it on.", timeout=60,
                       absent=("Turning Wi-Fi off",))
            tui.send("esc")
            tui.expect("Esc goes back to Home", re.compile(r"Your Nodo|Everything is running"))
            tui.quit()
        with Journey("Network: Wi-Fi, helper side"):
            check("wpa_supplicant stopped", sh("pgrep -x wpa_supplicant", check=False).strip() == "")
            check("the radio is blocked again", "0" not in sh("cat /sys/class/rfkill/*/soft").split())
            check("no Wi-Fi file holds the passphrase", "horse" not in sh("cat /var/lib/nodo/wifi/*", check=False))
            check("the files are root-only",
                  sh("stat -c '%a %U' /var/lib/nodo/wifi /var/lib/nodo/wifi/wpa_supplicant.conf").split()
                  == ["700", "root", "600", "root"])
    finally:
        sh(f"sh {HERE}/wifi-ap.sh down", check=False)


def rollbacks():
    with Journey("Rollbacks: put the previous version back"):
        # The updates journey left 1.1 held. Released and applied, the package keeps 1.0 to go back to.
        ctl("hold", "tui-shop", "1.1_1", "0")
        ctl("check-updates")
        applied = ctl("apply-update", "tui-shop", "1.1_1")
        check("the update applied", sh("xbps-query -p pkgver tui-shop").strip() == "tui-shop-1.1_1", applied)
    with Journey("Rollbacks: put the previous version back"), Session() as tui:
        tui.wait_for("Home", timeout=20)
        tui.send("r")
        tui.expect("r opens Undo an update with the kept version", crumbs("Updates", "Undo an update"),
                   re.compile(r"tui-shop\s+1\.1_1\s+→ 1\.0_1"), "can roll back", timeout=30)
        select(tui, "tui-shop", limit=6)
        tui.send("enter")
        tui.expect("rolling back asks first, on Cancel", "Roll tui-shop back?", re.compile(r"▸Cancel\s+Roll back"))
        tui.send("right", "enter")
        tui.expect("the old version comes back healthy", "tui-shop is back on 1.0_1.", timeout=240)
        tui.expect("one generation only, nothing more to roll back", "Nothing to roll back.", timeout=30)
        tui.quit()
    with Journey("Rollbacks: the helper side"):
        check("xbps has the previous version", sh("xbps-query -p pkgver tui-shop").strip() == "tui-shop-1.0_1")
        # The update made 1.1 the installed version, so it is a candidate again only after a check.
        status = ctl("check-updates")
        check("the version rolled back from is held", re.search(r'to: "1\.1_1",[^}]*held: true', status, re.S), status)
        wait_until("tui-shop to run", lambda: sh("sv status tui-shop", check=False).startswith("run:"), 60)
        ok("the package runs on the previous version")


def pkgver(name):
    return sh(f"xbps-query -p pkgver {name}", check=False).strip()


GROWING = re.compile(r"tui-shop\s+1\.0_1\s+→\s+1\.2_1\s+needs the device passphrase")


def storage_growth():
    # 1.2 asks for 320 MiB of encrypted storage instead of the 256 MiB tui-shop's volume has, and
    # LUKS needs the key to resize it, so the update needs the device passphrase.
    volume = "/var/lib/nodo/volumes/tui-shop.luks"
    size_before = 0
    with Journey("Updates: growing encrypted storage asks for the device passphrase"):
        sh(f"sh {HERE}/feed.sh pkg 1.2 grow")
        ctl("check-updates")
        size_before = int(sh(f"stat -c %s {volume}").strip())
    with Journey("Updates: growing encrypted storage asks for the device passphrase"), Session() as tui:
        open_updates(tui)
        tui.expect("the update says it needs the device passphrase", GROWING, timeout=60)
        if not re.search(r"\[x\]\s+tui-shop", tui.text()):
            select(tui, "[ ]  tui-shop", limit=6)
            tui.send("space")
        tui.send("enter")
        tui.expect("Enter asks for the passphrase first, saying what grows", "Some apps need more locked storage",
                   re.compile(r"tui-shop: \d+ MB to 320 MB of encrypted storage"), "Growing locked storage needs the device passphrase")
        check("nothing changed before the passphrase", pkgver("tui-shop") == "tui-shop-1.0_1")
        tui.send("esc")
        tui.expect("Esc cancels the whole batch", GROWING, absent=("Some apps need more locked storage",))
        tui.send("enter")
        tui.wait_for("Some apps need more locked storage")
        tui.type(WRONG_PASSPHRASE, enter=True)
        tui.expect("a wrong passphrase is refused in plain words", re.compile(r"(?i)did not unlock|passphrase"),
                   "✕", timeout=90)
        status = ctl("update-status")
        check("a wrong passphrase changed nothing and held nothing",
              pkgver("tui-shop") == "tui-shop-1.0_1" and int(sh(f"stat -c %s {volume}").strip()) == size_before
              and not re.search(r'to: "1\.2_1",[^}]*held: true', status, re.S), status)
        tui.send("esc")
        tui.wait_for(GROWING, timeout=20)
        if not re.search(r"\[x\]\s+tui-shop", tui.text()):
            select(tui, "[ ]  tui-shop", limit=6)
            tui.send("space")
        tui.send("enter")
        tui.wait_for("Some apps need more locked storage")
        tui.type(PASSPHRASE, enter=True)
        tui.expect("the right passphrase grows the storage and updates", "1 update is installed",
                   re.compile(r"✓\s+tui-shop 1\.2_1"), timeout=240)
        tui.quit()
    with Journey("Updates: growing encrypted storage, helper side"):
        check("the package is on 1.2", pkgver("tui-shop") == "tui-shop-1.2_1")
        size = int(sh(f"stat -c %s {volume}").strip())
        check("the volume grew in place to 320 MiB", size == 320 << 20, f"{size_before} -> {size}")
        check("the volume is still open", "is a mountpoint" in sh("mountpoint /run/nodo/volumes/tui-shop", check=False))
        wait_until("tui-shop to run", lambda: sh("sv status tui-shop", check=False).startswith("run:"), 60)
        ok("the package runs on the larger volume")


def system_update():
    # tests/tui-e2e.sh puts the test snapshots in the VM with tests/snapshot-prepare.sh. The device
    # moves to test-a, and the channel offers test-b, which updates nodo-snaptest.
    shop = pkgver("tui-shop")
    out = sh(f"sh {HERE}/sysupdate-net.sh up", check=False, timeout=600)
    try:
        with Journey("Updates: the Nodo's own update"):
            check("the test update server serves the snapshots", "ready" in out.splitlines(), out)
            # Checked before the session, so the card is there when the page opens.
            ctl("system-check")
            ctl("check-updates")
        with Journey("Updates: the Nodo's own update"), Session() as tui:
            open_updates(tui)
            tui.expect("the next system snapshot is a card of its own, with its own button",
                       re.compile(r"The Nodo\s+test-a\s+→\s+test-b"), "Update the Nodo", timeout=60)
            tui.expect("the system line says automatic system updates only notify", "System: snapshot test-a",
                       "Automatic system updates are off")
            tui.send("n")
            tui.expect("updating applies the snapshot and says what changed", "1 update is installed",
                       re.compile(r"system base test-b\s+Applied, \d+ packages? changed"), timeout=300)
            tui.quit()
        with Journey("Updates: the Nodo's own update, helper side"):
            status = re.sub(r"\s", "", ctl("system-status"))
            check("the device is on test-b with nothing more to offer",
                  re.search(r'current_snapshot:Some\("test-b",?\)', status) and "available:None" in status, status)
            check("the update is recorded as the owner's, not automatic", "automatic:false,}" in status, status)
            check("nodo-snaptest moved to 1.1", pkgver("nodo-snaptest") == "nodo-snaptest-1.1_1")
            check("the packages were left alone", pkgver("tui-shop") == shop, shop)
    finally:
        sh(f"sh {HERE}/sysupdate-net.sh down", check=False)



JOURNEYS = [
    ("restore", restore_entry),
    ("first-boot", first_boot),
    ("home", home),
    ("feeds", feeds_add),
    ("install", packages_install),
    ("settings", package_settings),
    ("updates", updates),
    ("rollbacks", rollbacks),
    ("grow", storage_growth),
    ("system", system_update),
    ("feed-key", feed_key_changed),
    ("web", web),
    ("onion", onion),
    ("network", network),
    ("logs", logs),
    ("backups", backups),
    ("locked", locked),
    ("helper-down", helper_down),
    ("secrets", no_secrets_anywhere),
]

# The journeys for flows that broke on a real device are in device_flows.py, which uses the helpers
# above. Run as a script this file is __main__, so it is registered under its own name first, or
# that import would load a second copy of it with its own counters. They run before the final
# secrets check, so it covers what they typed too.
sys.modules.setdefault("journeys", sys.modules[__name__])
import device_flows  # noqa: E402

JOURNEYS[-1:-1] = device_flows.JOURNEYS
DEVICE = {name for name, _ in device_flows.JOURNEYS}


def main():
    started = time.monotonic()
    try:
        prepare()
    except Exception:
        bad("setup", traceback.format_exc())
        return 1
    waiting = []
    for name, run in JOURNEYS:
        if wanted(name):
            before = failed
            run()
            if failed > before and name in device_flows.NEEDS_FIXES:
                waiting.append(name)
    say(f"{passed} passed, {failed} failed in {time.monotonic() - started:.0f}s")
    # Still failures: this only says which of them are known bugs waiting for a fix.
    for name in waiting:
        say(f"  {name} failed on a known bug: {device_flows.NEEDS_FIXES[name]}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
