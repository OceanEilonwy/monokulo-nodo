//! Turns a package manifest and its approved grants into system configuration.
//!
//! Manifests come from user-added feeds and are untrusted, so [`ServiceSpec::new`] validates
//! every value, and the generators accept only that validated type. Everything here is pure: no
//! file is read or written, and the installer decides where the output goes.

mod nft;
mod runit;
mod shell;
mod spec;
pub mod validate;

pub use nft::nft_fragment;
pub use runit::{control_t, log_run, run_script};
pub use shell::sh_quote;
pub use spec::{
    mapper_name, service_user, volume_mount, Directory, LanGrant, Layout, Profile, ResolvedGrants,
    ServiceSpec, VOLUME_ROOT,
};
pub use validate::GenError;

use nodo_manifest::Manifest;

/// Every generated file for one service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifacts {
    pub user: String,
    pub run: String,
    pub log_run: String,
    pub control_t: String,
    pub nft: String,
    /// Directories the installer must create before the service first starts.
    pub directories: Vec<Directory>,
}

pub fn generate(
    manifest: &Manifest,
    profile: Profile,
    grants: &ResolvedGrants,
) -> Result<Artifacts, GenError> {
    let spec = ServiceSpec::new(manifest, profile, grants)?;
    Ok(Artifacts {
        user: spec.user().to_owned(),
        run: run_script(&spec),
        log_run: log_run(&spec),
        control_t: control_t(&spec),
        nft: nft_fragment(&spec),
        directories: spec.directories(),
    })
}
