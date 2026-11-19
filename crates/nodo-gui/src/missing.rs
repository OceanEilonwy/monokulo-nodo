//! What the console's design asks for and no helper call does yet.
//!
//! Each is a method here, so the screens are built and tested against it, and the day the helper
//! can do it only this file changes. On the device every one answers [`NOT_YET`] (the
//! [`NotYet`] implementation); the screenshots and tests use [`Sample`], which answers like the
//! design so the screens it leads to can be drawn.

#![deny(clippy::let_underscore_must_use, unused_must_use)]

/// What every missing call says on the device.
pub const NOT_YET: &str = "not available yet";

/// A one-time code that lets someone set a new web password, and where to use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetCode {
    pub code: String,
    /// The link the QR code holds, with the code in it.
    pub link: String,
    /// Where to type the code instead.
    pub page: String,
}

/// One step of the network check the helper does not make yet: router, names, internet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub passed: bool,
    pub detail: String,
    pub value: String,
}

/// The network check's steps past the device's own link and address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkChecks {
    pub router: Checked,
    pub names: Checked,
    pub internet: Checked,
    /// The device asks a public name server, not the router's.
    pub public_dns: bool,
}

pub trait Missing {
    /// Removes the web admin's password and makes a one-time code to set a new one.
    fn reset_web_password(&mut self) -> Result<ResetCode, String>;
    /// Removes every SSH key. The helper removes keys one at a time and refuses the last one
    /// ("this is the only key that can log in"), so it cannot be done with the calls there are.
    fn reset_ssh_access(&mut self) -> Result<(), String>;
    /// Restarts the device (nodo-power only shuts down).
    fn restart_device(&mut self) -> Result<(), String>;
    /// Sets a new screen PIN from the screen (the helper takes it only from SSH, with the device
    /// passphrase).
    fn change_screen_pin(&mut self) -> Result<(), String>;
    /// Asks the router, a name and the internet.
    fn network_checks(&mut self) -> Result<NetworkChecks, String>;
    /// Uses a public name server instead of the router's, or goes back to the router's.
    fn use_public_dns(&mut self, on: bool) -> Result<(), String>;
    /// Asks the router for an address again.
    fn renew_address(&mut self) -> Result<(), String>;
    /// The ports a service listens on, worded, such as "443 https · 18083 API".
    fn service_ports(&self, service: &str) -> Option<String>;
}

/// The device today: none of it can be done.
pub struct NotYet;

impl Missing for NotYet {
    fn reset_web_password(&mut self) -> Result<ResetCode, String> {
        Err(NOT_YET.to_owned())
    }
    fn reset_ssh_access(&mut self) -> Result<(), String> {
        Err(NOT_YET.to_owned())
    }
    fn restart_device(&mut self) -> Result<(), String> {
        Err(NOT_YET.to_owned())
    }
    fn change_screen_pin(&mut self) -> Result<(), String> {
        Err(NOT_YET.to_owned())
    }
    fn network_checks(&mut self) -> Result<NetworkChecks, String> {
        Err(NOT_YET.to_owned())
    }
    fn use_public_dns(&mut self, _on: bool) -> Result<(), String> {
        Err(NOT_YET.to_owned())
    }
    fn renew_address(&mut self) -> Result<(), String> {
        Err(NOT_YET.to_owned())
    }
    fn service_ports(&self, _service: &str) -> Option<String> {
        None
    }
}

/// Answers as the design does: for the screenshots of the screens these calls lead to, and tests.
#[derive(Default)]
pub struct Sample {
    pub public_dns: bool,
}

impl Missing for Sample {
    fn reset_web_password(&mut self) -> Result<ResetCode, String> {
        Ok(ResetCode {
            code: "K7QF-29XM".to_owned(),
            link: "https://192.168.1.155/reset#K7QF-29XM".to_owned(),
            page: "https://192.168.1.155/reset".to_owned(),
        })
    }
    fn reset_ssh_access(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn restart_device(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn change_screen_pin(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn network_checks(&mut self) -> Result<NetworkChecks, String> {
        let ok = |detail: &str, value: &str| Checked { passed: true, detail: detail.to_owned(), value: value.to_owned() };
        Ok(NetworkChecks {
            router: ok("Answers", "192.168.1.1"),
            names: if self.public_dns {
                ok("Public name server", "1.1.1.1")
            } else {
                Checked { passed: false, detail: "Router doesn't answer".to_owned(), value: "192.168.1.1".to_owned() }
            },
            internet: if self.public_dns { ok("Reachable", "") } else { Checked { passed: false, detail: String::new(), value: String::new() } },
            public_dns: self.public_dns,
        })
    }
    fn use_public_dns(&mut self, on: bool) -> Result<(), String> {
        self.public_dns = on;
        Ok(())
    }
    fn renew_address(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn service_ports(&self, service: &str) -> Option<String> {
        (service == "moneropay").then(|| "443 https · 5000 API".to_owned())
    }
}
