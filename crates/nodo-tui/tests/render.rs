//! Every design screen, rendered: exact copy, fit at 100x32, colours, and graceful degradation.
//!
//! Text snapshots live in `tests/snapshots`. After an intended change, regenerate them with
//! `UPDATE_SNAPSHOTS=1 cargo test -p nodo-tui --test render` and review the diff.

mod common;

use std::path::PathBuf;

use common::{draw, screen_text, text};
use nodo_tui::app::ScreenId;
use nodo_tui::fixtures::fixture;
use nodo_tui::theme::Theme;
use ratatui::style::Color;

fn snapshot_path(id: ScreenId) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots").join(format!("{}.txt", id.slug()))
}

#[test]
fn every_screen_matches_its_snapshot() {
    let update = std::env::var_os("UPDATE_SNAPSHOTS").is_some();
    let mut mismatched = Vec::new();
    if update {
        std::fs::create_dir_all(snapshot_path(ScreenId::Main).parent().expect("has a parent"))
            .expect("create snapshot directory");
    }
    for id in ScreenId::ALL {
        let (app, _) = fixture(id);
        let actual = screen_text(&app);
        let path = snapshot_path(id);
        if update {
            std::fs::write(&path, &actual).expect("write snapshot");
        } else if std::fs::read_to_string(&path).ok().as_deref() != Some(actual.as_str()) {
            mismatched.push(id.slug());
        }
    }
    assert!(mismatched.is_empty(), "snapshots differ: {mismatched:?}");
}

#[test]
fn the_fixtures_cover_the_whole_design_on_the_right_screen() {
    assert_eq!(ScreenId::ALL.len(), 33, "32 design files plus the auto-update screen");
    for id in ScreenId::ALL {
        assert_eq!(fixture(id).0.screen.id(), id);
    }
}

#[test]
fn nothing_is_clipped_or_broken_at_100x32() {
    for id in ScreenId::ALL {
        let (app, _) = fixture(id);
        let (buffer, report) = draw(&app, 100, 32);
        assert!(report.fits(), "{}: {report:?}\n{}", id.slug(), text(&buffer));
    }
}

/// Design copy that must appear intact at 100x32. Sentences that wrap in a panel are checked by
/// words on the wrapped line, so each string here is one that fits on a single row.
fn required(id: ScreenId) -> &'static [&'static str] {
    match id {
        ScreenId::Main => &[
            "Role", "monero-daemon", "cuprated 0.1.0-preview", "▲ syncing 61%", "2,121,400 of 3,478,000", "32 out, 9 in",
            "412 GB of 1.9 TB", "0.4 GB of 7.6 GB", "6.1 GB of 31 GB", "52 °C", "3 days", "Enter to open",
            "● tor", "◆ wallet-relay", "✕ bell", "○ monerod", "storage needs unlocking",
            "restarted 3 times, press l for logs", "alternative node",
            "◆ This device is locked. Press u to unlock all waiting services.",
            "✕ bell keeps failing. Press l to read its log.",
            "↑↓ move", "↵ open", "s start/stop", "/ search", "Tab next tab", "? help", "w network", "r rollbacks", "q quit",
        ],
        ScreenId::Search => &[
            "/ pay", "All feeds · Any status · 3 of 41 results", "moneropay", "official", "Payment gateway for Monero",
            "● monokulo-pay", "monokulo", "Accept payments from your node", "○ paywall-relay", "community",
            "Pay-to-use relay access", "● installed   ○ available   ▲ update ready",
            "Feed names show where a package came from.",
            "Same name in two feeds? Both are shown, and you pick one.",
            "From the official feed", "Runs against your local node.", "2.7.0", "14 MB", "✓ daemon-rpc",
            "type to filter", "↑↓ move", "↵ details", "i install", "f filter feed", "Esc clear",
        ],
        ScreenId::Detail => &[
            "Packages › moneropay", "official feed · ✓ signature verified", "Version 2.7.0 · 14 MB · MIT",
            "✓ daemon-rpc  from cuprated", "✓ wallet-rpc  installed with this package", "moneropay.api_url",
            "Other packages can reference this.", "its own user, _moneropay", "Only its own data and declared paths",
            "Reach daemon RPC on this device", "5000, local network only", "None",
            "◆ Yes, needs the device passphrase after reboot",
            "i install", "v choose version", "c changelog", "Esc back",
        ],
        ScreenId::Review => &[
            "Install wallet-relay 0.4.2", "community feed", "Third-party package", "It is asking to",
            "● Use its own user and data folder", "Standard", "● Read monero-daemon.public_rpc_url", "Public setting",
            "● Reach daemon RPC on this device", "Local only", "▲ Accept connections on port 18120",
            "Reachable from your network", "▲ Connect out to any internet address", "Broad access",
            "◆ Keep data in encrypted storage", "Locks at reboot", "✓ Cannot read other packages' data or secrets",
            "✓ Cannot run setup scripts as root or replace system packages",
            "If an update asks for more than this, you will be asked again.",
            "▸Cancel", "Install and configure", "←→ choose", "↵ confirm", "d file list", "Esc cancel",
        ],
        ScreenId::Installing => &[
            "Installing moneropay 2.7.0", "Step 5 of 8", "✓ Download package", "14 MB", "✓ Verify feed signature", "official key",
            "✓ Check package contents", "no root scripts, paths allowed", "/opt/nodo/apps/moneropay",
            "▸ Create sandbox and firewall rules", "working", "○ Set up encrypted storage", "○ Configure settings",
            "○ Start and run health check", "created user _moneropay", "sandbox allows /var/lib/apps/moneropay",
            "firewall allows _moneropay to 127.0.0.1:18081", "checking rules before loading them",
            "A failed step removes what this install added.", "Esc cancel", "v full log",
        ],
        ScreenId::Unlock => &[
            "Unlock this device", "Restarted 4 minutes ago", "Device passphrase", "Passphrase", "••••••••••••",
            "✕ That passphrase did not unlock this device. Try again.", "Attempts are delayed after each failure.",
            "Enter once to unlock all of these", "◆ monokulo-pay", "◆ wallet-relay", "○ bell", "Encrypted storage",
            "Starts after wallet-relay", "▸ Each package's storage opens inside its own sandbox only.",
            "▸ Services start in dependency order, and progress appears here.",
            "A lost passphrase cannot be recovered. Restore from a backup to get the data back.",
            "↵ unlock", "Esc leave locked",
        ],
        ScreenId::Feeds => &[
            "Your feeds", "Name", "Packages", "Key", "Checked", "official", "built in", "2 hours ago", "monokulo", "pinned",
            "✕ old-tools", "changed", "blocked, press Enter", "▲ experiments", "unreachable since yesterday",
            "https://github.com/example/monokulo-feed", "SHA256:7f3a 91c2 0e4d b856 4c11 de90 aa72 3b6f",
            "12 October 2026", "monokulo-pay, bell", "a add", "r refresh", "e disable", "x remove", "↵ packages",
            "Removing a feed keeps installed packages, without updates.",
        ],
        ScreenId::AddFeed => &[
            "Feeds › Add a feed", "Step 2 of 2", "https://github.com/example/monero-tools-feed",
            "✓ Found feed \"monero-tools\", 9 packages, signed", "Check this key",
            "SHA256:c41e 8d07 2fa9 66b3 19e0 75cd 0b42 e8a1",
            "Compare it with the key the publisher shows on a site you already trust.",
            "This device will only accept this feed's packages if they are signed by this key.",
            "● Its packages appear in search and can offer updates.",
            "● You still review each package's permissions before it installs.",
            "✓ It cannot replace system packages or another feed's packages.",
            "✓ It cannot install anything without you.",
            "Packages run on your node, near your wallet data. Only add publishers you trust.",
            "▸Cancel", "Trust this key", "←→ choose", "↵ confirm", "p list packages", "Esc back",
        ],
        ScreenId::FeedKey => &[
            "Feeds › old-tools", "✕ Updates blocked", "This feed's signing key changed",
            "SHA256:08bd 42f1 a7c3 91de 66e0 d214 f8a9 03c7", "Now signing", "SHA256:e93a 17c0 5bd2 80f4 1a6c 39e7 42db a106",
            "hash-tools 1.3.0", "Keeps running. No updates.", "peer-map 0.9.1",
            "Check the publisher's announcement through a channel other than this feed.",
            "▸Keep the old key and stay blocked (safe default)",
            "Trust the new key  you must type the feed name to confirm", "Remove this feed",
            "↑↓ choose", "↵ confirm", "Esc decide later",
        ],
        ScreenId::Settings => &[
            "moneropay › Settings", "● 2 unsaved changes", "Connection", "$(monero-daemon.public_rpc_url)",
            "Resolves to http://127.0.0.1:18081", "API port", "5000", "‹ info ›", "▸Payment address", "48edfHu7V9Z84Y",
            "✕ Address is too short. A Monero", "••••••••••••", "secret, private to this package", "[x] on",
            "Where received funds are sent.", "Required. Provided by the package.", "Saving will", "Restart moneropay",
            "Run its health check", "Restore these values if it fails", "Others can read", "moneropay.api_url",
            "↑↓ field", "↵ edit", "$ use a reference", "ctrl-s save", "z undo", "Esc back",
        ],
        ScreenId::Reference => &[
            "moneropay › Settings › Daemon RPC", "Expects a URL", "Search public settings", "$ rpc",
            "▸monero-daemon.public_rpc_url", "http://127.0.0.1:18081", "monero-daemon.restricted_rpc_url",
            "http://127.0.0.1:18089", "wallet-rpc.url", "http://127.0.0.1:34512",
            "Not shown: 4 settings of the wrong type. 2 secrets, which need a grant.", "monero-daemon is a role.",
            "It stays correct if you switch between cuprated and monerod.",
            "● Make moneropay depend on monero-daemon, so it starts afterward.",
            "● Let moneropay's sandbox reach 127.0.0.1:18081.",
            "● Update and restart moneropay whenever the value changes.",
            "↑↓ move", "↵ use reference", "t type a value instead", "Esc cancel",
        ],
        ScreenId::ApplyFailed => &[
            "moneropay › Settings", "● Running on previous settings", "Your changes were not kept",
            "✓ Settings validated and saved", "✓ Service restarted", "✕ Health check failed after 30 seconds",
            "✓ Previous settings restored, service healthy", "listen tcp :18081: bind: address already in use",
            "API port 18081 is used by monero-daemon. Pick a different port.", "Your draft, not applied",
            "5000 → 18081", "info → debug", "↵ edit draft", "l full log", "x discard draft", "Esc back",
        ],
        ScreenId::Updates => &[
            "Updates (3)", "Auto-update on for 3 packages", "[x] moneropay", "2.7.0 → 2.7.1", "official", "Ready",
            "[x] system base", "2026.11 → 2026.12", "tested snapshot", "Restart needed", "[ ] wallet-relay", "0.4.2 → 0.5.0",
            "community", "▲ Asks for new permission", "Held back", "[ ] cuprated", "0.1.0 → 0.2.0", "You skipped this version",
            "wallet-relay asks for more", "▲ New permission: connect out to any internet address.",
            "Selecting this update will ask you to review and approve that change first.",
            "The node restarts last. Encrypted packages stay unlocked unless the system restarts.",
            "Space select", "↵ update selected", "c changelog", "r check now", "h hold version", "a auto-update",
        ],
        ScreenId::AutoUpdate => &[
            "Updates › Auto-update", "Next run tonight at 03:00", "Schedule", "Check frequency", "‹ Daily ›", "Install window",
            "03:00 to 05:00 device time", "Update automatically", "[x] moneropay", "official", "Last updated 4 days ago",
            "[x] cuprated", "Node restarts, dependents wait", "[x] monokulo-pay", "monokulo", "Last updated 12 days ago",
            "[ ] wallet-relay", "community", "▲ Runs new code without your review", "[ ] system base", "Off means notify only",
            "Always waits for you", "New permissions requested", "Feed signing key changed", "Update fails install checks",
            "A version you put on hold", "System restart needed", "Device locked or disk low", "If an update fails",
            "Previous version and data return", "That version is held", "Home shows what happened",
            "↑↓ move", "Space toggle", "w change window", "h history", "↵ save", "Esc back",
        ],
        ScreenId::UpdateFailed => &[
            "Updates › Results", "1 updated, 1 rolled back", "✓ system base", "2026.12", "Applied, restart when ready",
            "✕ moneropay", "still on 2.7.0", "2.7.1 rolled back", "moneropay 2.7.1 failed its health check",
            "Version 2.7.0 and its data were restored. moneropay is running.",
            "database migration 14: table \"invoices\" already exists",
            "This looks like a package problem. Your settings were not the cause.", "What you can do",
            "▸Keep 2.7.0 and hold this version", "Try the update again", "Copy a report for the publisher  no keys or secrets included", "Read the full log",
            "↑↓ choose", "↵ confirm", "Esc back to updates",
        ],
        ScreenId::Logs => &[
            "● Following live", "Service", "cuprated", "▸bell ✕", "moneropay", "tor", "wallet-relay", "System", "Updates",
            "14:02:11 INFO  starting version 1.2.0", "14:02:11 INFO  reading /etc/bell/config.toml",
            "14:02:17 ERROR connection refused: wallet-relay is locked", "14:02:17 INFO  exiting with status 1",
            "── restarted by supervisor, attempt 3 ──", "Suggestion", "bell depends on wallet-relay, which is locked.",
            "Press u to unlock the device. bell will start automatically.",
            "↑↓ service", "/ find", "e errors only", "f follow", "y copy with secrets removed",
        ],
        ScreenId::Rejected => &[
            "Install fast-sync-helper 1.0.0", "community feed", "Not installed",
            "This package breaks the install rules, so nothing was changed on this device.", "Why it was rejected",
            "✕ Runs a setup script as root", "./INSTALL", "✕ Writes outside its own folder", "/etc/cron.d/fast-sync-helper",
            "✕ Tries to replace a system package", "glibc 2.42", "✓ Signature matches the feed's key",
            "A valid signature shows who published it, not that it is safe.", "There is no override for these rules.",
            "y copy report", "f open feed", "Esc back to search",
        ],
        ScreenId::Capability => &[
            "Install zmq-indexer 0.8.0", "▲ Needs a change first", "Your node cannot serve this package",
            "zmq-indexer needs ZMQ. Your current node, cuprated, does not provide it.", "What each node provides",
            "daemon-rpc", "zmq", "i2p", "Synced on this device", "▸Cancel and keep cuprated",
            "Check for a compatible version  none found in your feeds", "Install monerod and switch  about 300 GB more and a full sync",
            "Switching stops services that need a node until it finishes syncing.",
            "Both chains are kept, so you can switch back without syncing again.",
            "↑↓ choose", "↵ confirm", "Esc cancel",
        ],
        ScreenId::NoDrive => &[
            "✕ Node stopped", "The data drive is not available", "✓ NVMe drive detected", "nvme0n1, 1.9 TB", "✓ Partition found",
            "NODO_BLOCKCHAIN, xfs", "✕ Mount failed", "structure needs cleaning", "Also waiting",
            "moneropay, wallet-relay, bell. They start after the node returns.", "▸Retry mount",
            "Run a read-only drive check  changes nothing",
            "Repair the filesystem  can lose data, explains first and asks to confirm", "Show diagnostics",
            "The TUI never formats a drive without you typing its label.", "↑↓ choose", "↵ confirm", "l system log", "! shell",
        ],
        ScreenId::WebSites => &[
            "Home", "Packages", "Feeds", "Sites online", "Domain providers ›", "▸ shop", "https://shop.example.com",
            "Home and internet", "monokulo-pay", "Home network and Tor", "moneropay", "Home network",
            "To put another app online, open it in Packages.", "↵ open", "p providers", "↑↓ choose",
        ],
        ScreenId::WebExposure => &[
            "Packages  ›  shop  ›  Hosting", "Choose who can open shop. Any combination works.", "[x]  Home network",
            "Phones and computers on your Wi-Fi.   https://shop.local", "[x]  Tor",
            "A private link that opens in Tor Browser. It doesn't reveal your home.", "[x]  Internet",
            "Anyone, at a web address you own.", "Provider", "Porkbun", "Domain", "example.com", "Address",
            "Visitors will be able to see your home's internet address.",
            "With Tor on too, visitors could tell the two addresses are the same app.", "Show details ›", "Check and save",
            "Tab move", "Space tick", "↵ save", "Esc back",
        ],
        ScreenId::WebProviders => &[
            "Web  ›  Domain providers  ›  Add", "Where did you buy your web address?", "▸ Porkbun", "Cloudflare",
            "GoDaddy", "Namecheap", "Hover", "Can't be connected: no way to change records",
            "Somewhere else? It can't be connected yet, but Tor works without a web address.", "↑↓ choose", "↵ next", "Esc back",
        ],
        ScreenId::WebCheck => &[
            "Packages  ›  shop  ›  Hosting", "The internet can't reach your Nodo yet", "✓  Domain and API key work",
            "✕  Port 80 is not reachable from outside", "✕  Port 443 is not reachable from outside",
            "Your router needs to send web visitors to the Nodo. In its settings, forward:", "to 192.168.1.50:80",
            "to 192.168.1.50:443", "A private Tor link works without any of this.", "Show details ›", "Check again",
            "Use Tor instead", "r check again", "? router help", "Esc back",
        ],
        ScreenId::WebConfirm => &[
            "Packages  ›  shop  ›  Hosting", "Ready to put shop on the internet", "✓  Porkbun accepted the key",
            "✓  shop.example.com points to your home", "✓  The internet can reach your Nodo",
            "Anyone will be able to open https://shop.example.com",
            "Your home's internet address becomes public.", "Records of it can stay online after you stop.",
            "Anyone can connect this address to your home, including your Monero node.",
            "Public certificate logs list shop.example.com permanently.", "Cancel", "Put shop online",
            "▸ Cancel is selected for the keyboard.", "Tab choose", "↵ press", "Esc cancel",
        ],
        ScreenId::WebCgnat => &[
            "Packages  ›  shop  ›  Hosting", "The internet can't reach your Nodo",
            "Your internet provider shares one address between many homes",
            "A private Tor link still works on this connection.", "Show details ›", "Check again", "Use Tor instead",
            "o use Tor", "r check again", "Esc back",
        ],
        ScreenId::WebDomain => &[
            "Web › Domains", "✓ Certificates in place", "example.com", "Porkbun", "DNS-01 through Porkbun",
            "No open ports needed.", "shop.example.com", "✓ Valid, 61 days left",
            "Email for expiry notices", "me@example.com", "Use the staging server", "Untrusted certificates, for testing",
            "site.local, with this device's own CA", "Browsers warn until you trust it", "Under your domain: site.home.example.com",
            "Real certificates through DNS-01", "this device's LAN address, 192.168.1.60", "Press t to test this network.",
            "shop.home.example.com", "Hidden by this network", "rebinding exceptions", "↵ save", "Esc back",
        ],
        ScreenId::WebCertificate => &[
            "Packages  ›  shop  ›  Hosting", "shop is online", "https://shop.example.com",
            "The Nodo renews the secure connection and follows your home's address by itself.", "Done", "↵ done",
        ],
        ScreenId::WebTrust => &[
            "Web › Domains › Local CA", "✓ Served at http://nodo.local", "Open on a phone or computer here",
            "http://nodo.local", "macOS, Windows, Linux and Firefox. It needs no port and no app.",
            "3F:9A:21:C4:7E:05:B8:D2:61:4A:93:0C:EE:17:5B:28", "A6:D9:40:7F:12:C3:8B:65:E0:94:2D:71:BC:58:0A:F6",
            "Install the root only if the page shows this same fingerprint.", "What trusting it does",
            "r refresh", "Esc back",
        ],
        ScreenId::Setup => &["Set up this device", "Device passphrase", "There is no default passphrase"],
        ScreenId::Backup => &[
            "Home › Backups", "Backups on this device", "nodo-1762560000.nodobak", "nodo-1761955200.nodobak", "4.8 MB",
            "4.5 MB", "USB drive · SanDisk Ultra Fit, BACKUP", "nodo-1762473600.nodobak", "4.7 MB", "FAT32 · 30.1 GB free",
            "A backup holds", "✓ Package settings, secrets, certificates and Tor onion keys",
            "✓ The device key, so encrypted storage can be restored", "✓ The contents of unlocked encrypted storage",
            "✓ Feeds, pinned keys, approvals and the verified package files",
            "Never chain data. The node syncs it again.",
            "Encrypted with its own passphrase, at the same cost as the device key.",
            "↑↓ move", "Tab device or USB", "n new backup", "u back up to USB", "r restore", "Esc back",
        ],
        ScreenId::Onion => &[
            "Web › Onion addresses", "● Tor running", "▸[x] cuprated", "restricted RPC, port 18081",
            "nod…xyz.onion:18081", "[x] shop", "also local, public", "http://abc…xyz.onion", "[ ] moneropay",
            "single address", "◆ locked", "About onion addresses",
            "● The node's address serves only its restricted RPC, which wallets use. Never the full RPC.",
            "↑↓ move", "Space turn on or off", "s node settings", "g look again", "Esc back",
        ],
        ScreenId::Restore => &[
            "Set up this device › Restore from a USB backup › Drive", "USB drives", "▸sda1",
            "SanDisk Ultra Fit, BACKUP", "32.0 GB", "Generic Flash Disk", "▲ it has no filesystem", "▸ Choose the drive",
            "○ Confirm and restore", "Restoring sets up this device",
            "● The system disk and the blockchain drive are never listed or written.",
            "↑↓ move", "↵ choose", "g look again", "Esc back to setup",
        ],
        ScreenId::Rollbacks => &[
            "Home › Rollbacks", "Roll back a package", "▸moneropay", "2.7.0_1", "→ 2.6.1_1", "can roll back",
            "wallet-relay", "▲ its data is in layout 2", "Updates that were rolled back",
            "▲ bell 1.2.0_1 → 1.3.0_1 stayed on 1.2.0_1: bell did not pass its health check", "2026-11-07", "System",
            "The system runs the version it was updated to. No fallback happened.",
            "↑↓ move", "↵ roll back", "l log", "g look again", "Esc back",
        ],
        ScreenId::Network => &[
            "Home › Network", "● Wired connected", "[x] on", "● Joined home-5G", "-48 dBm", "192.168.1.57", "wlan0",
            "▸home-5G", "WPA2/WPA3", "▂▄▆█", "saved · joined", "Cafe Libre", "open", "neighbour", "WPA3",
            "office-staff", "enterprise", "travel-hotspot", "saved, not in range",
            "Not discoverable, pairs only when you ask", "D8:3A:DD:41:07:2C", "K380 Keyboard", "keyboard",
            "paired · connected", "WH-1000XM4", "headphones", "Kitchen Speaker", "speaker", "found",
            "Wired stays the main connection. Wi-Fi carries traffic only while the wired link is down.",
            "Wi-Fi keys are stored root-only on this device. For WPA2 only a derived key is kept.",
            "Bluetooth is never discoverable. Only pairings you start here can happen.",
            "←→ Wi-Fi/Bluetooth", "↑↓ move", "↵ join", "s scan", "o on/off", "f forget", "g look again", "Esc back",
        ],
    }
}

#[test]
fn every_screen_shows_the_design_copy_intact() {
    let mut missing = Vec::new();
    for id in ScreenId::ALL {
        let (app, _) = fixture(id);
        let screen = screen_text(&app);
        for needle in required(id) {
            if !screen.contains(needle) {
                missing.push(format!("{}: {needle}", id.slug()));
            }
        }
    }
    assert!(missing.is_empty(), "missing copy:\n{}", missing.join("\n"));
}

#[test]
fn colours_follow_the_design() {
    let theme = Theme::truecolor();
    for id in ScreenId::ALL {
        let (app, _) = fixture(id);
        let (buffer, _) = draw(&app, 100, 32);
        for cell in buffer.content() {
            assert!(
                [theme.ground, theme.accent, theme.bar, theme.card, theme.field, theme.raised].contains(&cell.bg),
                "{}: unexpected background {:?}",
                id.slug(),
                cell.bg
            );
        }
    }
    let (app, _) = fixture(ScreenId::Main);
    let (buffer, _) = draw(&app, 100, 32);
    assert!(buffer.content().iter().any(|cell| cell.fg == Color::Rgb(0xF2, 0xA3, 0x3A)), "amber accent is used");
}

/// A focused row keeps a symbol, so selection is readable with colour removed.
#[test]
fn selection_and_state_survive_without_colour() {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 32)).unwrap();
    let (app, _) = fixture(ScreenId::Main);
    terminal
        .draw(|frame| {
            nodo_tui::view::render(frame, &app, &Theme::mono());
        })
        .unwrap();
    let screen = text(terminal.backend().buffer());
    for symbol in ["▸", "●", "◆", "✕", "○"] {
        assert!(screen.contains(symbol), "missing {symbol}");
    }
    assert!(terminal.backend().buffer().content().iter().all(|cell| cell.bg == Color::Reset || cell.bg == Color::Black));
}

#[test]
fn every_screen_survives_any_size_and_keeps_its_keys_on_screen() {
    for id in ScreenId::ALL {
        for (width, height) in [(1, 1), (10, 3), (59, 40), (200, 15), (60, 16), (80, 24), (72, 20), (100, 31), (160, 50)] {
            let (mut app, _) = fixture(id);
            app.size = (width, height);
            let (buffer, _) = draw(&app, width, height);
            let screen = text(&buffer);
            if width >= 60 && height >= 16 {
                // Hints can wrap, so look at the last rows together.
                let rows: Vec<&str> = screen.lines().filter(|line| !line.trim().is_empty()).collect();
                let last = rows[rows.len().saturating_sub(3)..].join("\n");
                assert!(
                    ["↑↓", "Esc", "↵", "Space", "Tab", "←→", "type"].iter().any(|key| last.contains(key)),
                    "{} at {width}x{height}: footer lost\n{screen}",
                    id.slug()
                );
            } else {
                assert!(screen.contains("small") || width < 20, "{}: {screen}", id.slug());
            }
        }
    }
}

#[test]
fn the_focused_row_stays_visible_on_a_small_terminal() {
    let (mut app, _) = fixture(ScreenId::WebProviders);
    if let nodo_tui::app::Screen::WebProviders(state) = &mut app.screen {
        state.selected = 10;
    }
    let screen = text(&draw(&app, 60, 16).0);
    assert!(screen.contains("▸ Squarespace"), "{screen}");
}

/// The confirm buttons must not scroll off when the terminal is short.
#[test]
fn dangerous_screens_keep_their_buttons_visible_when_small() {
    for id in [ScreenId::Review, ScreenId::AddFeed] {
        let (app, _) = fixture(id);
        let screen = text(&draw(&app, 80, 16).0);
        assert!(screen.contains("▸Cancel"), "{}:\n{screen}", id.slug());
    }
    let (app, _) = fixture(ScreenId::WebConfirm);
    let screen = text(&draw(&app, 80, 16).0);
    assert!(screen.contains("Cancel") && screen.contains("Put shop online"), "webconfirm:\n{screen}");
}

/// Real packages have labels longer than the design's, such as cuprated's "Restricted RPC on
/// the network". The value must still stand apart from its label.
#[test]
fn long_setting_labels_keep_a_gap_before_the_value() {
    let (mut app, _) = fixture(ScreenId::Settings);
    if let nodo_tui::app::Screen::Settings(state) = &mut app.screen {
        state.form.fields[1].label = "Restricted RPC on the network".to_owned();
    }
    let screen = text(&draw(&app, 100, 32).0);
    let row = screen.lines().find(|line| line.contains("Restricted RPC on the network")).expect("label shown");
    let after = row.split("Restricted RPC on the network").nth(1).unwrap_or_default();
    assert!(after.starts_with("  "), "no gap after the label:\n{screen}");
}

/// A long log shows its newest lines, which is where a failure is, and what "Following live" promises.
#[test]
fn a_long_log_shows_its_newest_lines() {
    let (mut app, _) = fixture(ScreenId::Logs);
    if let nodo_tui::app::Screen::Logs(state) = &mut app.screen {
        state.view.lines = (1..=200)
            .map(|n| nodo_tui::model::LogLine { kind: nodo_tui::model::LogKind::Info, text: format!("line number {n}") })
            .collect();
        state.view.suggestion.clear();
    }
    let screen = text(&draw(&app, 100, 32).0);
    assert!(screen.contains("line number 200"), "the newest line is hidden:\n{screen}");
    assert!(!screen.contains("line number 1 "), "the oldest line is shown instead:\n{screen}");
}

/// Packages from the helper often have no size or license. Their detail must not show empty
/// separators, and a feed package not downloaded yet is not a failed signature: the helper checks
/// it against the pinned key when it downloads it.
#[test]
fn a_feed_package_detail_reads_cleanly_before_download() {
    let (mut app, _) = fixture(ScreenId::Detail);
    if let nodo_tui::app::Screen::Detail(detail) = &mut app.screen {
        detail.installed = false;
        detail.signature_verified = false;
        detail.version = "1.0_1".to_owned();
        detail.size = String::new();
        detail.license = String::new();
    }
    let screen = text(&draw(&app, 100, 32).0);
    let row = screen.lines().find(|line| line.contains("Version 1.0_1")).expect("version shown");
    assert!(!row.contains('·'), "empty parts left separators: {row}");
    assert!(!screen.contains("signature not verified"), "{screen}");
    assert!(screen.contains("signature checked when it downloads"), "{screen}");
}

/// A fresh device has no feeds, and the helper does not record when a feed was added.
#[test]
fn feeds_read_cleanly_with_none_and_with_helper_data() {
    let (mut app, _) = fixture(ScreenId::Feeds);
    for feed in &mut app.data.feeds {
        feed.added = String::new();
    }
    let screen = text(&draw(&app, 100, 32).0);
    assert!(!screen.lines().any(|line| line.trim_matches(|c| c == '│' || c == ' ') == "Added"), "{screen}");
    app.data.feeds.clear();
    let screen = text(&draw(&app, 100, 32).0);
    assert!(screen.contains("No feeds yet. Press a to add one."), "{screen}");
}

#[test]
fn terminal_control_characters_in_data_never_reach_the_screen() {
    let (mut app, _) = fixture(ScreenId::Search);
    app.data.catalog[0].tagline = "evil\u{1b}[2Jtext".to_owned();
    let (buffer, _) = draw(&app, 100, 32);
    assert!(!text(&buffer).contains('\u{1b}'));
}

/// Plain HTTP feeds are allowed, because signatures protect every package, but the screens say
/// what HTTP leaves open, and only for HTTP.
#[test]
fn plain_http_feeds_say_what_signatures_do_not_cover() {
    use nodo_tui::app::{AddFeedState, Screen};
    let (mut app, _) = fixture(ScreenId::AddFeed);
    let (buffer, _) = draw(&app, 100, 32);
    assert!(!text(&buffer).contains("Plain HTTP"));
    if let Screen::AddFeed(AddFeedState::Confirm { probe, .. }) = &mut app.screen {
        probe.address = "http://192.168.1.5:8000/feed".to_owned();
    } else {
        panic!("the AddFeed fixture is the confirm step");
    }
    let (buffer, report) = draw(&app, 100, 32);
    let shown = text(&buffer);
    assert!(report.fits(), "{report:?}\n{shown}");
    for words in ["Plain HTTP", "nothing changed on the way can install", "hold back updates", "Prefer https://", "Trust this key"] {
        assert!(shown.contains(words), "{words}\n{shown}");
    }

    let (mut app, _) = fixture(ScreenId::Feeds);
    let (buffer, _) = draw(&app, 100, 32);
    assert!(!text(&buffer).contains("plain HTTP"));
    app.data.feeds[1].address = "http://192.168.1.5:8000/feed".to_owned();
    let (buffer, _) = draw(&app, 100, 32);
    assert!(text(&buffer).contains("plain HTTP: signed, but updates can be held back"), "{}", text(&buffer));
}
