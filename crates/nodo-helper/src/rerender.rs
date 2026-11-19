//! Service files of installed apps, written again by the generator a system update installed.
//!
//! An app's run scripts and firewall fragment are generated when it is installed, updated or its
//! settings change. A system update that replaces the helper also replaces the generator, such
//! as a firewall that now lets internet apps send UDP, but nothing would write the new files
//! until each app changed for its own reasons. So when the helper starts after such an update,
//! every installed app is re-rendered.
//!
//! The list of apps still to do is kept on disk, written before the update's record says the
//! helper restarted, so a crash or power cut part way loses nothing. An app whose storage or
//! sealed settings are locked cannot be planned until the device is unlocked, so it stays on the
//! list and is done at the unlock. Each app is its own transaction: an app whose new files fail
//! their health check is put back on its old files without undoing the others. Files that come
//! out the same are not written, and their app is not restarted.

use nodo_api::{ApiError, Response};
use nodo_gen::Profile;

use crate::catalog::Package;
use crate::helper::{lock, Helper};
use crate::settings::Plan;
use crate::system::{System, WriteOptions};

/// Root only: it names what is installed.
const PENDING_MODE: u32 = 0o600;

enum Outcome {
    Done,
    /// Waits for the device to be unlocked.
    Locked,
    /// Left on its old files. Not retried, since the same generator would fail the same way.
    Failed(String),
}

impl<S: System> Helper<S> {
    /// Marks every installed app as needing its service files written again. Called when the
    /// helper starts on the program a system update installed, before that is recorded.
    pub(crate) fn mark_installed_for_rerender(&self) -> Result<(), String> {
        let (packages, _) = self.packages().map_err(|e| e.to_string())?;
        let ids: Vec<String> = packages
            .iter()
            .filter(|p| p.profile == Profile::App && self.is_installed(p))
            .map(|p| p.id().to_owned())
            .collect();
        self.write_pending(&ids)
    }

    /// At start: re-renders the apps still waiting since a system update. Does nothing when none
    /// are, so it runs on every start and finishes a pass a restart interrupted.
    pub fn rerender_pending(&self) {
        let _guard = lock(&self.service_lock);
        if let Err(e) = self.rerender_pending_held() {
            eprintln!("nodo-helper: re-rendering apps after the system update: {e}");
        }
    }

    /// As [`Helper::rerender_pending`], with the service lock already held, such as at an unlock.
    pub(crate) fn rerender_pending_held(&self) -> Result<(), String> {
        let pending = self.read_pending()?;
        if pending.is_empty() {
            return Ok(());
        }
        let (packages, _) = self.packages().map_err(|e| e.to_string())?;
        let mut waiting = Vec::new();
        for id in pending {
            // An app removed since is simply done.
            let Some(package) = packages.iter().find(|p| p.id() == id) else {
                continue;
            };
            if package.profile != Profile::App || !self.is_installed(package) {
                continue;
            }
            match self.rerender_one(&packages, package) {
                Outcome::Done => {}
                Outcome::Locked => waiting.push(id),
                Outcome::Failed(reason) => {
                    eprintln!("nodo-helper: {id} keeps its old service files: {reason}");
                }
            }
        }
        self.write_pending(&waiting)
    }

    fn rerender_one(&self, all: &[Package], package: &Package) -> Outcome {
        match self.is_locked(package) {
            Ok(true) => return Outcome::Locked,
            Ok(false) => {}
            Err(e) => return Outcome::Failed(e.to_string()),
        }
        let plan = match package.manifest.settings {
            Some(_) => self.with_settings(all, |ctx| self.settings.plan(ctx, package, &[])),
            None => self
                .with_settings(all, |ctx| self.settings.service(ctx, package))
                .map(|service| Plan { files: Vec::new(), service }),
        };
        let unit = match plan.and_then(|plan| self.unit_for(package, plan)) {
            Ok(unit) => unit,
            Err(ApiError::Locked(_)) => return Outcome::Locked,
            Err(e) => return Outcome::Failed(e.to_string()),
        };
        match self.transact(unit, || Ok(Vec::new())) {
            Ok(Response::RolledBack(rollback)) => Outcome::Failed(rollback.reason),
            Ok(_) => Outcome::Done,
            Err(ApiError::Locked(_)) => Outcome::Locked,
            Err(e) => Outcome::Failed(e.to_string()),
        }
    }

    fn read_pending(&self) -> Result<Vec<String>, String> {
        let bytes = self
            .sys
            .read_file(&self.paths.rerender_pending)
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        Ok(String::from_utf8_lossy(&bytes)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect())
    }

    fn write_pending(&self, ids: &[String]) -> Result<(), String> {
        let path = &self.paths.rerender_pending;
        if ids.is_empty() {
            if self.sys.exists(path) {
                self.sys.remove_file(path).map_err(|e| e.to_string())?;
            }
            return Ok(());
        }
        let mut text = ids.join("\n");
        text.push('\n');
        self.sys
            .atomic_write(path, text.as_bytes(), WriteOptions { mode: PENDING_MODE, group: None })
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{helper, FakeSystem, SHOP};

    const VAULT: &str = "api_version = 1\nid = \"vault\"\nversion = \"1\"\nexec = [\"bin/vault\"]\n\n[storage]\nencrypted = true\n";

    /// Shop installed with service files an older generator wrote, and marked as after an update.
    fn updated() -> Helper<FakeSystem> {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        sys.put("/s/installed/shop", b"");
        sys.put("/nft/shop.nft", b"# written by the old generator\n");
        sys.put("/sv/shop/run", b"#!/bin/sh\n# old\n");
        sys.put("/s/rerender-pending", b"shop\n");
        helper(sys)
    }

    fn fragment(h: &Helper<FakeSystem>) -> String {
        String::from_utf8(h.sys.get("/nft/shop.nft").unwrap()).unwrap()
    }

    #[test]
    fn an_installed_app_gets_the_new_generators_files_and_the_firewall_is_reloaded() {
        let h = updated();
        h.rerender_pending();
        assert!(fragment(&h).contains("table inet monokulo"), "{}", fragment(&h));
        assert!(!fragment(&h).contains("old generator"));
        assert!(!String::from_utf8(h.sys.get("/sv/shop/run").unwrap()).unwrap().contains("# old"));
        assert!(h.sys.calls().contains(&"nft check /nft/shop.nft.pending".to_owned()));
        assert!(h.sys.calls().contains(&"nft load /nft/main.conf".to_owned()));
        assert!(h.sys.get("/s/rerender-pending").is_none(), "done, so nothing is left to do");
    }

    #[test]
    fn files_that_come_out_the_same_are_not_written_and_nothing_restarts() {
        let h = updated();
        h.rerender_pending();
        h.sys.put("/s/rerender-pending", b"shop\n");
        h.sys.clear_calls();
        h.rerender_pending();
        let calls = h.sys.calls();
        assert!(!calls.iter().any(|c| c.starts_with("nft load")), "{calls:?}");
        assert!(!calls.iter().any(|c| c.starts_with("sv ")), "{calls:?}");
    }

    #[test]
    fn with_nothing_pending_a_start_changes_nothing() {
        let h = updated();
        h.sys.remove_file(std::path::Path::new("/s/rerender-pending")).unwrap();
        h.sys.clear_calls();
        h.rerender_pending();
        assert_eq!(fragment(&h), "# written by the old generator\n");
        assert!(!h.sys.calls().iter().any(|c| c.starts_with("nft")));
    }

    #[test]
    fn a_locked_app_waits_for_the_unlock_and_the_others_go_ahead() {
        let h = updated();
        h.sys.add_manifest(VAULT);
        h.sys.put("/s/installed/vault", b"");
        h.sys.put("/nft/vault.nft", b"# old vault\n");
        h.sys.put("/s/rerender-pending", b"vault\nshop\n");
        h.rerender_pending();
        assert!(!fragment(&h).contains("old generator"));
        assert_eq!(h.sys.get("/nft/vault.nft").unwrap(), b"# old vault\n");
        assert_eq!(h.sys.get("/s/rerender-pending").unwrap(), b"vault\n");

        // Its storage opens with the device passphrase; the unlock finishes the pass.
        let mount = nodo_gen::volume_mount("vault").unwrap();
        h.sys.set_mounted(&mount);
        let _guard = lock(&h.service_lock);
        h.rerender_pending_held().unwrap();
        assert!(String::from_utf8(h.sys.get("/nft/vault.nft").unwrap()).unwrap().contains("_vault"));
        assert!(h.sys.get("/s/rerender-pending").is_none());
    }

    #[test]
    fn an_app_removed_since_is_dropped_from_the_list() {
        let h = updated();
        h.sys.put("/s/rerender-pending", b"gone\nshop\n");
        h.rerender_pending();
        assert!(h.sys.get("/s/rerender-pending").is_none());
    }

    #[test]
    fn an_app_whose_new_files_fail_keeps_its_old_ones_and_is_not_retried() {
        let h = updated();
        h.sys.fail("nft check");
        h.rerender_pending();
        assert_eq!(fragment(&h), "# written by the old generator\n");
        assert!(h.sys.get("/s/rerender-pending").is_none());
    }
}
