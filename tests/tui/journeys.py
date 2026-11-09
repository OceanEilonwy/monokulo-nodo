#!/usr/bin/env python3
"""The real nodo-tui against the real nodo-helper in the dev VM, through a pseudo-terminal.

Runs as root inside a fresh VM, but every TUI session runs as tui-tester, an ordinary user in the
nodo group, so the helper's access rules are the ones a person meets. Each journey opens its own
session, presses keys and waits for screen text with a timeout. A failed wait prints the last
screen, and the remaining journeys still run. tests/tui-e2e.sh copies this directory into the VM
and starts it.

Journeys follow the design canvas, docs/design/tui/canvas.json: first boot and its USB restore,
Home, Packages, Feeds, Updates and Rollbacks, Web and Onion, Logs, Backups, and the error states. At the end, no passphrase may appear
in anything the TUI drew or in any log on the device.
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
    return not only or name in only


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


# Journeys

def first_boot():
    with Journey("first boot: passphrase setup"), Session() as tui:
        tui.expect("first boot opens the passphrase setup", "Set up this device", "Device passphrase",
                   "There is no default passphrase")
        tui.type("short", enter=True)
        tui.expect("a short passphrase is refused with its rule", "Use at least 8 characters.")
        tui.send("ctrl-u")
        tui.type(PASSPHRASE, enter=True)
        tui.expect("the passphrase is asked for again", "Again", "Esc retype the first")
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
        tui.expect("leaving the keys screen opens Home", "Node", "Services", "monero-daemon")
        tui.quit()
    with Journey("first boot: the helper side"):
        check("the device key exists, root only",
              sh("stat -c '%a %U' /var/lib/nodo/keys/device.key").strip() == "600 root")
        check("the helper reports encryption as unlocked", "encryption: Unlocked" in ctl("overview"))


def home():
    with Journey("Home"), Session() as tui:
        tui.expect("Home shows the node's height and peers", "Height", re.compile(r"\b1 \(synced\)"),
                   "0 out, 0 in", timeout=30)
        tui.expect("Home lists cuprated as running", re.compile(r"cuprated\s+running\s+monero-daemon"))
        tui.expect("Home warns that the health monitor is not reporting", "Needs attention",
                   "The health monitor is not reporting")
        tui.expect("the device panel shows the host and uptime", "monokulo-nodo", "Uptime")
        check("uptime reads as plain English", not re.search(r"\b1 minutes\b|\b1 hours\b", tui.text()), tui.text())
        tui.send("?")
        tui.expect("? opens the help", "Help", "Choices that could harm the device start on the safe answer.")
        tui.send("esc")
        tui.quit()


def feeds_add():
    with Journey("Feeds: add with key confirmation"), Session() as tui:
        probe = ctl("probe-feed", FEED)
        raw = re.search(r'fingerprint: "([^"]+)"', probe).group(1)
        tui.send("tab", "tab", "tab")
        tui.expect("the Feeds tab is empty at first and says what to do", "Your feeds",
                   "No feeds yet. Press a to add one.")
        tui.send("a")
        tui.expect("a opens Add a feed", "Add a feed", "Paste the address the publisher gave you.")
        tui.type(FEED, enter=True)
        tui.expect("the feed is looked up and its key shown", 'Found feed "tui-feed", 1 package, signed',
                   "Check this key", "Trust this key", timeout=30)
        shown = re.search(r"((?:[0-9a-f]{2}:){15}[0-9a-f]{2})", tui.text())
        check("the key shown is the feed's real fingerprint",
              shown and shown.group(1).replace(":", "") == raw.replace(":", "").lower(), f"{shown} vs {raw}")
        tui.expect("Cancel is the default choice", re.compile(r"▸Cancel\s+Trust this key"))
        tui.send("enter")
        tui.expect("Enter on the default cancels", "Your feeds")
        check("cancelling added nothing", "tui-feed" not in ctl("feeds"))
        tui.send("a")
        tui.type(FEED, enter=True)
        tui.wait_for("Trust this key", timeout=30)
        tui.send("right", "enter")
        tui.expect("trusting the key adds the feed", "Added tui-feed and pinned its key.", "pinned", timeout=30)
        tui.quit()
    with Journey("Feeds: the helper side"):
        check("the helper pinned the confirmed key", f'fingerprint: "{raw}"' in ctl("feeds"))


def packages_install():
    with Journey("Packages: search and install with permission review"), Session() as tui:
        tui.send("tab")
        tui.expect("the Packages tab opens with the search focused", "Search", "type to filter")
        tui.type("shop")
        tui.expect("search finds the test package in its feed", "/ shop", re.compile(r"○ tui-shop\s+tui-feed"))
        tui.send("down", "enter")
        tui.expect("Enter opens the package detail", "Packages › tui-shop", "i install", timeout=20)
        check("a feed package is not marked as a failed signature", "signature not verified" not in tui.text(),
              tui.text())
        tui.send("i")
        tui.expect("i shows the permission review", "Install tui-shop 1.0_1", "Third-party package",
                   "Connects to the internet", "Serves the site 'shop'", timeout=60)
        tui.expect("the review starts on Cancel", re.compile(r"▸Cancel\s+Install and configure"))
        tui.send("esc")
        tui.wait_gone("Third-party package")
        check("leaving the review installed nothing", "tui-shop" not in sh("xbps-query -l"))
        tui.send("i")
        tui.wait_for("Install and configure", timeout=60)
        tui.send("right", "enter")
        tui.expect("its encrypted storage asks for the device passphrase", "Device passphrase",
                   "Its encrypted storage is created now and opens with this passphrase.")
        tui.type(PASSPHRASE)
        tui.expect("the passphrase shows as bullets", "•" * len(PASSPHRASE))
        tui.send("enter")
        tui.expect("approving runs every install step", "Installing tui-shop 1.0_1", "Verify feed signature",
                   "matches the pinned key", "no root scripts, paths allowed", timeout=30)
        tui.expect("the install finishes healthy", "Installed and healthy.", "↵ configure", timeout=120)
        tui.quit()
    with Journey("Packages: the helper side"):
        check("xbps installed the approved version", sh("xbps-query -p pkgver tui-shop").strip() == "tui-shop-1.0_1")
        check("its storage is an open LUKS volume",
              sh("mountpoint -q /run/nodo/volumes/tui-shop && cryptsetup status nodo-tui-shop", check=False).find(
                  "is active") >= 0)
        wait_until("tui-shop to run", lambda: sh("sv status tui-shop", check=False).startswith("run:"), 30)
        ok("the installed package runs as a service")


def package_settings():
    with Journey("Packages: settings"), Session() as tui:
        tui.send("tab")
        tui.type("shop")
        tui.send("down", "enter")
        tui.wait_for("s settings", timeout=20)
        tui.send("s")
        tui.expect("s opens the package's settings form from its Lua schema", "tui-shop › Settings",
                   "Worker threads", "Greeting", "Requests handled at once.", timeout=20)
        tui.send("enter", "ctrl-u")
        tui.type("99", enter=True)
        tui.expect("an out of range value is refused in plain words", "Must be from 1 to 16.")
        tui.send("ctrl-s")
        tui.expect("saving with the refused value changes nothing", "Fix the marked field before saving. Nothing was changed.")
        tui.send("enter", "ctrl-u")
        tui.type("4", enter=True)
        tui.wait_gone("Must be from 1 to 16.")
        tui.expect("a valid value is a pending change", "1 unsaved change")
        tui.send("down", "enter", "ctrl-u")
        tui.type("this greeting is far too long", enter=True)
        tui.send("ctrl-s")
        tui.expect("the package's own validator refuses through the helper", "must be 12 characters or fewer",
                   timeout=30)
        check("the refused save left the stored settings alone", "workers" not in sh(
            "cat /var/lib/nodo/settings/tui-shop.toml 2>/dev/null || true"))
        tui.send("enter", "ctrl-u")
        tui.type("hi there", enter=True)
        tui.send("ctrl-s")
        tui.expect("a valid save restarts the package and passes its health check", "No unsaved changes",
                   timeout=60, absent=("● 1 unsaved change",))
        tui.quit()
    with Journey("Packages: settings on the device"):
        stored = sh("cat /var/lib/nodo/settings/tui-shop.toml")
        check("the helper stored the saved values", "literal = \"4\"" in stored and "literal = \"hi there\"" in stored,
              stored)


def updates():
    sh(f"sh {HERE}/feed.sh pkg 1.1")
    with Journey("Updates: check, hold and auto-update"), Session() as tui:
        tui.send("tab", "tab", "tab", "tab")
        tui.expect("the Updates tab opens", "How updates apply", "a auto-update")
        tui.send("r")
        tui.expect("checking finds the new version", re.compile(r"tui-shop.*1\.0_1.*1\.1_1"), timeout=60)
        tui.send("h")
        tui.expect("h holds that version", re.compile(r"(?i)held|on hold"), timeout=30)
        tui.send("a")
        tui.expect("a opens the auto-update screen, off by default", "Updates › Auto-update", "Auto-update is off",
                   re.compile(r"\[ \] tui-shop"))
        tui.send("down", "down", "space")
        tui.expect("Space picks the package", re.compile(r"\[x\] tui-shop"))
        tui.send("enter")
        tui.expect("saving turns auto-update on for it", re.compile(r"Auto-update on for (tui-shop|1 package)\b"),
                   timeout=30)
        tui.quit()
    with Journey("Updates: the helper side"):
        status = ctl("update-status")
        check("the helper holds the version", "held: true" in status, status)
        check("auto-update is on for the package only", 'enabled: [\n        "tui-shop",' in ctl("auto-get")
              or '"tui-shop"' in ctl("auto-get"))
        check("nothing was updated while it is held", sh("xbps-query -p pkgver tui-shop").strip() == "tui-shop-1.0_1")


def feed_key_changed():
    sh(f"sh {HERE}/feed.sh resign")
    with Journey("Feeds: key changed"), Session() as tui:
        tui.send("tab", "tab", "tab")
        tui.wait_for("Your feeds")
        tui.send("r")
        tui.expect("a refresh shows the feed's key changed", re.compile(r"(?i)key changed|changed"), timeout=30)
        tui.send("enter")
        tui.expect("Enter on it explains the change and shows both keys", "This feed's signing key changed", "Now signing", timeout=20)
        tui.expect("keeping the old key is the default", "▸Keep the old key and stay blocked (safe default)")
        old = re.search(r'pinned: "([^"]+)"', ctl("feed-key", "tui-feed")).group(1)
        tui.send("down", "enter")
        tui.type("tui-fee", enter=True)
        tui.expect("a wrong feed name is refused", "That is not the feed name. Type tui-feed exactly.")
        tui.send("ctrl-u")
        tui.type("tui-feed", enter=True)
        tui.expect("the typed name trusts the new key", "Your feeds", timeout=30, absent=("That is not the feed name",))
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
    # The network comes first: a session reads the device once when it starts.
    pebble = "pebble" in sh(f"sh {HERE}/web-net.sh up {CF_TOKEN}")
    ctl("expose", "tui-shop", "shop", "0", "0", "0")  # a rerun starts from off too
    with Journey("Web: exposure"), Session() as tui:
        tui.send("tab", "tab")
        tui.expect("the Web tab lists the package's site, off", "Sites", re.compile(r"tui-shop.*Off"),
                   "80 and 443 are closed on this device")
        tui.send("enter")
        tui.expect("Enter opens the site's exposure", "Exposure", "Local network", "Tor onion", "Public internet",
                   "No unsaved changes")
        tui.send("space")
        tui.expect("Space turns local on as a draft", "● 1 unsaved change", "Turning on")
        tui.send("enter")
        tui.expect("saving applies it and returns to the sites", re.compile(r"tui-shop\s+●.*Online"), timeout=60)
        check("the helper has the site on the local network only", site_flags() == (True, False, False), ctl("sites"))
        tui.send("enter", "down", "down", "space", "enter")
        tui.pump(2)
        if "public_host: None" in ctl("sites"):
            tui.expect("going public first asks for a public address", "Choose a public address for this site first.")
            tui.type("shop.example.com", enter=True)
            tui.expect("the address is stored", "https://shop.example.com", timeout=30)
            tui.send("enter")
        if "provider: None" in ctl("web-status"):
            tui.expect("without a provider the reachability check fails in plain words", "Reachability check",
                       "Domain or API key did not work", timeout=60)
        else:
            tui.wait_for("Reachability check", timeout=60)
        check("nothing went public", site_flags()[2] is False, ctl("sites"))
        tui.send("esc", "esc")
        tui.wait_for("Sites")
        tui.send("d")
        tui.expect("d opens the Domains screen", "Web › Domains", "Email for expiry notices", "Use the staging server")
        tui.type("not an email", enter=True)
        tui.expect("a bad email is refused in plain words", "not valid", timeout=30)
        tui.send("ctrl-u")
        tui.type("tui@example.com", enter=True)
        tui.expect("a good email is saved", absent=("not valid",), timeout=30)
        check("the helper stored the email", "tui@example.com" in ctl("acme"))
        tui.send("down", "down")  # onto the .local choice, which takes letters as keys
        tui.send("p")
        tui.expect("p opens the DNS providers", "Where is your DNS hosted?", "Cloudflare")
        while "▸ Cloudflare" not in tui.text() and "▸Cloudflare" not in tui.text():
            tui.send("down")
            if "Squarespace" in tui.text() and "▸ Squarespace" in tui.text():
                raise AssertionError("Cloudflare is not in the provider list\n" + tui.text())
        tui.send("tab")
        tui.send("ctrl-u")
        tui.type("example.com")
        tui.send("tab")
        tui.send("ctrl-u")
        tui.type(CF_TOKEN)
        check("the API token shows as bullets", CF_TOKEN not in tui.text() and "•" * 10 in tui.text(), tui.text())
        tui.send("enter")
        tui.expect("the provider is saved, its token sealed", re.compile(r"(?i)cloudflare"), timeout=60,
                   absent=("Where is your DNS hosted?",))
        tui.send("esc")
        tui.wait_for("Sites", timeout=20)
        # Public, now with a provider: the check passes what it can and shows the ports as unknown.
        tui.send("enter", "down", "down", "space", "enter")
        tui.wait_for(re.compile(r"Go public|Reachability check"), timeout=90)
        if "Reachability check" in tui.text():
            # Something could not be verified, such as the ports. The user sees it and decides.
            tui.expect("the reachability check passes the provider", "Domain and API key work")
            tui.send("c")
        tui.expect("going public asks to type the address, on the safe answer", "Type the address to confirm",
                   "This puts tui-shop on the public internet", "https://shop.example.com",
                   "shop.example.com  A  93.184.216.34", re.compile(r"▸Cancel\s+Go public"))
        tui.type("shop.example.co", enter=True)
        tui.expect("a mistyped address is refused", "Type shop.example.com exactly to confirm.")
        tui.send("backspace")
        tui.type("om", enter=True)
        tui.send("enter")
        tui.expect("going public opens the Certificate screen", "Certificate", "shop.example.com is configured on this device",
                   timeout=60)
        if pebble:
            tui.expect("the certificate comes from the test ACME server", "✓ Certificate in place",
                       "now has a trusted certificate", timeout=180)
        else:
            tui.expect("the certificate shows as being obtained, never as done", "◆ Getting a certificate",
                       "Asking the test ACME server to prove control of the name", absent=("✓ Certificate in place",))
        check("the helper has the site public", site_flags()[2] is True, ctl("sites"))
        tui.send("esc")
        tui.wait_for("Sites")
        tui.send("o")
        tui.expect("taking every site offline asks first, on the safe answer", "Take all sites offline?",
                   re.compile(r"▸Cancel"))
        tui.send("right", "enter")
        tui.expect("every site goes offline", re.compile(r"tui-shop.*Off"), timeout=60)
        check("the helper has the site off everywhere", site_flags() == (False, False, False), ctl("sites"))
        tui.quit()
    sh(f"sh {HERE}/web-net.sh down", check=False)


def logs():
    marker = f"tui-e2e marker {int(time.time())}"
    with Journey("Logs"), Session() as tui:
        tui.send("tab", "tab", "tab", "tab", "tab")
        tui.expect("the Logs tab lists log sources", "Service", "cuprated", "nodo-helper", "Following live")
        sh(f"logger -t tui-e2e '{marker}' 2>/dev/null; echo '{marker}' >> /var/log/cuprated/current")
        tui.expect("following shows the newest line", marker, timeout=20)
        tui.send("/")
        tui.type("marker")
        tui.expect("find filters the lines", marker, "/ marker")
        tui.send("enter", "e")
        tui.expect("errors only hides other lines", absent=(marker,))
        tui.send("e", "f")
        tui.expect("f pauses following", "Paused")
        tui.quit()


def backups():
    with Journey("Backups"), Session() as tui:
        tui.wait_for("Services", timeout=20)
        tui.send("b")
        tui.expect("b opens the backups, empty at first", "Backups on this device", "No backups yet. Press n to make one.",
                   timeout=20)
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
        tui.expect("inspecting lists what a restore would replace", "tui-shop", timeout=60)
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
        tui.expect("Home says the device is locked", "This device is locked", re.compile(r"tui-shop\s+locked"),
                   timeout=30)
        tui.send("u")
        tui.expect("u opens the unlock screen listing what waits", "Unlock this device", "tui-shop", timeout=10)
        tui.type(WRONG_PASSPHRASE, enter=True)
        tui.expect("a wrong passphrase is refused in plain words", re.compile(r"(?i)did not|wrong|incorrect"), timeout=60)
        tui.pump(3)  # the first retry delay
        tui.send("ctrl-u")
        tui.type(PASSPHRASE, enter=True)
        tui.expect("the right passphrase unlocks", re.compile(r"(?i)unlocked"), timeout=60)
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
        tui.wait_for("Services", timeout=20)
        sh("sv -w 30 stop nodo-helper")
        tui.send("tab", "tab", "tab", "r")
        tui.expect("a failed request is shown, not hidden", re.compile(r"(?i)could not|cannot reach|not running|refused"),
                   timeout=20)
        sh("sv -w 30 start nodo-helper")
        wait_until("the helper socket", lambda: "Services" in sh("nodo-tui --ctl services", check=False), 30)
        tui.send("r")
        tui.expect("the TUI recovers once the helper is back", "tui-feed", timeout=30)
        tui.quit()
        printf_unlock = f"printf '%s\\n' '{PASSPHRASE}' | nodo-tui --ctl unlock"
        sh(printf_unlock)


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
        tui.expect("Ctrl-R opens the restore on its drive step", "Restore from a USB backup › Drive", "USB drives",
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
        tui.send("tab", "tab")
        tui.wait_for("Sites", timeout=20)
        tui.send("n")
        tui.expect("n opens the onion screen with the node and the site", "Web › Onion addresses", "cuprated",
                   "tui-shop › shop", timeout=30)
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
        tui.expect("Esc goes back to the sites", "Sites")
        tui.quit()


def rollbacks():
    # The updates journey left 1.1 held. Released and applied, the package keeps 1.0 to go back to.
    ctl("hold", "tui-shop", "1.1_1", "0")
    ctl("check-updates")
    applied = ctl("apply-update", "tui-shop", "1.1_1")
    with Journey("Rollbacks: put the previous version back"), Session() as tui:
        check("the update applied", sh("xbps-query -p pkgver tui-shop").strip() == "tui-shop-1.1_1", applied)
        tui.wait_for("Services", timeout=20)
        tui.send("r")
        tui.expect("r opens the rollbacks with the kept version", "Home › Rollbacks",
                   re.compile(r"tui-shop\s+1\.1_1\s+→ 1\.0_1"), "can roll back", timeout=30)
        tui.send("enter")
        tui.expect("rolling back asks first, on Cancel", "Roll tui-shop back?", re.compile(r"▸Cancel\s+Roll back"))
        tui.send("right", "enter")
        tui.expect("the old version comes back healthy", "✓ tui-shop is back on 1.0_1.", timeout=240)
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


def storage_growth():
    # 1.2 asks for 320 MiB of encrypted storage instead of the 256 MiB tui-shop's volume has, and
    # LUKS needs the key to resize it, so the update needs the device passphrase.
    volume = "/var/lib/nodo/volumes/tui-shop.luks"
    sh(f"sh {HERE}/feed.sh pkg 1.2 grow")
    ctl("check-updates")
    size_before = int(sh(f"stat -c %s {volume}").strip())
    with Journey("Updates: growing encrypted storage asks for the device passphrase"), Session() as tui:
        tui.send("tab", "tab", "tab", "tab")
        tui.expect("the update says it needs the device passphrase", re.compile(r"tui-shop\s+1\.0_1 → 1\.2_1"),
                   "◆ Needs the device passphrase", timeout=60)
        tui.send("enter")
        tui.expect("Enter asks for the passphrase first, saying what grows", "Encrypted storage grows",
                   re.compile(r"tui-shop: \d+ MB to 320 MB of encrypted storage"), "Device passphrase", "Esc cancel")
        check("nothing changed before the passphrase", pkgver("tui-shop") == "tui-shop-1.0_1")
        tui.send("esc")
        tui.expect("Esc cancels the whole batch", "How updates apply", absent=("Encrypted storage grows",))
        tui.send("enter")
        tui.wait_for("Encrypted storage grows")
        tui.type(WRONG_PASSPHRASE, enter=True)
        tui.expect("a wrong passphrase is refused in plain words", "did not unlock", timeout=90)
        status = ctl("update-status")
        check("a wrong passphrase changed nothing and held nothing",
              pkgver("tui-shop") == "tui-shop-1.0_1" and int(sh(f"stat -c %s {volume}").strip()) == size_before
              and not re.search(r'to: "1\.2_1",[^}]*held: true', status, re.S), status)
        tui.send("esc")
        tui.wait_for(re.compile(r"tui-shop\s+1\.0_1 → 1\.2_1"), timeout=20)
        tui.send("enter")
        tui.wait_for("Encrypted storage grows")
        tui.type(PASSPHRASE, enter=True)
        tui.expect("the right passphrase grows the storage and updates", "✓ tui-shop", timeout=240,
                   absent=("did not unlock",))
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
        with Journey("Updates: the system base row"):
            check("the test update server serves the snapshots", "ready" in out.splitlines(), out)
            # Checked before the session, so the row is there, and ticked, when the tab opens.
            ctl("system-check")
            ctl("check-updates")
        with Journey("Updates: the system base row"), Session() as tui:
            tui.send("tab", "tab", "tab", "tab")
            tui.expect("the next system snapshot is a row of its own, from a tested snapshot",
                       re.compile(r"\[x\] system base\s+test-a → test-b\s+tested snapshot"), timeout=60)
            # The line wraps at this width, so only its start is matched.
            tui.expect("the system line says automatic system updates only notify", "System: snapshot test-a",
                       "Automatic system updates are off")
            tui.send("enter")
            tui.expect("updating applies the snapshot and says what changed", "✓ system base",
                       re.compile(r"Applied, \d+ packages? changed"), timeout=300)
            tui.quit()
        with Journey("Updates: the system base, helper side"):
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
    ("logs", logs),
    ("backups", backups),
    ("locked", locked),
    ("helper-down", helper_down),
    ("secrets", no_secrets_anywhere),
]


def main():
    started = time.monotonic()
    try:
        prepare()
    except Exception:
        bad("setup", traceback.format_exc())
        return 1
    for name, run in JOURNEYS:
        if wanted(name):
            run()
    say(f"{passed} passed, {failed} failed in {time.monotonic() - started:.0f}s")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
