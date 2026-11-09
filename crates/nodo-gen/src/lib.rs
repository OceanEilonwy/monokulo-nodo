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
pub use runit::{control_t, log_run, run_script, CONFINE};
pub use shell::sh_quote;
pub use spec::{
    mapper_name, secrets_dir, service_user, volume_directories, volume_mount, Directory, LanGrant, GPU_GROUP,
    Layout, Profile, Resolution, ResolvedGrants, ServiceSpec, ENV_ROOT, SECRETS_ROOT, VOLUME_ROOT,
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
    /// Directories the installer must create before the service first starts. Those inside an
    /// encrypted volume can only be created while it is mounted.
    pub directories: Vec<Directory>,
    /// Where the service's files and settings live.
    pub layout: Layout,
}

pub fn generate(
    manifest: &Manifest,
    profile: Profile,
    grants: &ResolvedGrants,
) -> Result<Artifacts, GenError> {
    generate_resolved(manifest, profile, grants, &Resolution::default())
}

/// Like [`generate`], with what the package's resolved settings decide: the command line with
/// references substituted, and whether its settings hold sealed secrets.
pub fn generate_resolved(
    manifest: &Manifest,
    profile: Profile,
    grants: &ResolvedGrants,
    resolution: &Resolution,
) -> Result<Artifacts, GenError> {
    let spec = ServiceSpec::resolved(manifest, profile, grants, resolution)?;
    Ok(Artifacts {
        user: spec.user().to_owned(),
        run: run_script(&spec),
        log_run: log_run(&spec),
        control_t: control_t(&spec),
        nft: nft_fragment(&spec),
        directories: spec.directories(),
        layout: spec.layout().clone(),
    })
}
