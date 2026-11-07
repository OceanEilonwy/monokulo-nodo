use std::collections::BTreeSet;
use std::net::IpAddr;

use nodo_manifest::{Manifest, Outbound, Protocol, Scope};

use crate::validate::{self, GenError};

/// The kernel truncates a process name to this length, and `pkill -x` compares against that.
const COMM_LEN: usize = 15;

/// Where unlocked encrypted volumes are mounted on the host.
pub const VOLUME_ROOT: &str = "/run/nodo/volumes";

/// How much a package is trusted. It comes from where the manifest was found, never from the
/// manifest itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// A package from a user-added feed.
    App,
    /// A first-party package shipped by the signed base image, such as cuprated.
    System,
}

/// The user a service runs as.
pub fn service_user(id: &str) -> Result<String, GenError> {
    validate::package_id(id)?;
    Ok(format!("_{id}"))
}

/// Host path where a package's unlocked volume is mounted.
pub fn volume_mount(id: &str) -> Result<String, GenError> {
    validate::package_id(id)?;
    Ok(format!("{VOLUME_ROOT}/{id}"))
}

/// Device-mapper name for a package's volume.
pub fn mapper_name(id: &str) -> Result<String, GenError> {
    validate::package_id(id)?;
    Ok(format!("nodo-{id}"))
}

/// Host directories belonging to a package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub config_dir: String,
    pub data_dir: String,
    pub log_dir: String,
    /// The package's own read-only tree. System packages live in `/usr` instead.
    pub app_dir: Option<String>,
}

impl Layout {
    /// Apps get their own namespace. Their ids are attacker-chosen, and an app called `ssh` or
    /// `nodo` must not land on `/etc/ssh` or `/var/lib/nodo`.
    pub fn new(id: &str, profile: Profile) -> Result<Self, GenError> {
        validate::package_id(id)?;
        Ok(match profile {
            Profile::System => Layout {
                config_dir: format!("/etc/{id}"),
                data_dir: format!("/var/lib/{id}"),
                log_dir: format!("/var/log/{id}"),
                app_dir: None,
            },
            Profile::App => Layout {
                config_dir: format!("/etc/apps/{id}"),
                data_dir: format!("/var/lib/apps/{id}"),
                log_dir: format!("/var/log/apps/{id}"),
                app_dir: Some(format!("/opt/apps/{id}")),
            },
        })
    }
}

/// A directory the installer must create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directory {
    pub path: String,
    /// `None` means root.
    pub owner: Option<String>,
    pub mode: u32,
}

/// Access to a local network address, approved by the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanGrant {
    pub address: IpAddr,
    pub prefix: u8,
    pub port: u16,
    pub protocol: Protocol,
}

/// Access approved beyond the manifest, usually implied by a `$(package.setting)` reference.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedGrants {
    /// Allow every loopback port. Only for system packages, since loopback carries other
    /// services' unauthenticated RPC.
    pub unrestricted_loopback: bool,
    /// Loopback TCP ports of granted dependencies.
    pub loopback_tcp_ports: Vec<u16>,
    /// Addresses reachable despite the private range block.
    pub lan: Vec<LanGrant>,
    /// Dependencies whose `/run/apps/<id>` socket directory appears in this sandbox.
    pub socket_deps: Vec<String>,
    /// A mount that must exist, overriding anything in the service's `conf`.
    pub data_mount: Option<String>,
}

/// A manifest and its grants after validation. Only this type reaches the generators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSpec {
    pub(crate) id: String,
    pub(crate) user: String,
    pub(crate) layout: Layout,
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) comm: String,
    pub(crate) internet: bool,
    pub(crate) encrypted: bool,
    pub(crate) bind_config: bool,
    pub(crate) own_socket_dir: Option<String>,
    pub(crate) dep_socket_dirs: Vec<String>,
    pub(crate) data_mount: Option<String>,
    pub(crate) inbound: Vec<(u16, Protocol, Scope)>,
    pub(crate) unrestricted_loopback: bool,
    pub(crate) loopback_tcp: BTreeSet<u16>,
    pub(crate) loopback_udp: BTreeSet<u16>,
    pub(crate) lan: Vec<LanGrant>,
    pub(crate) has_listener: bool,
}

impl ServiceSpec {
    pub fn new(
        manifest: &Manifest,
        profile: Profile,
        grants: &ResolvedGrants,
    ) -> Result<Self, GenError> {
        let id = manifest.id.clone();
        let user = service_user(&id)?;
        let layout = Layout::new(&id, profile)?;

        let (first, args) = manifest.exec.split_first().ok_or(GenError::Exec("empty"))?;
        let program = program_path(first, profile, &layout)?;
        validate::exec_args(args)?;
        let comm = comm_pattern(&program);

        if grants.unrestricted_loopback && profile != Profile::System {
            return Err(GenError::Grant(
                "only system packages get unrestricted loopback",
            ));
        }
        if manifest.storage.encrypted && grants.data_mount.is_some() {
            return Err(GenError::Grant("encrypted storage cannot use a data mount"));
        }
        if let Some(mount) = &grants.data_mount {
            data_mount(mount)?;
        }

        let mut loopback_tcp = BTreeSet::new();
        let mut loopback_udp = BTreeSet::new();
        for port in &grants.loopback_tcp_ports {
            loopback_tcp.insert(validate::port(*port)?);
        }

        let mut inbound = Vec::new();
        for rule in &manifest.network.inbound {
            let port = validate::port(rule.port)?;
            inbound.push((port, rule.protocol, rule.scope));
            match rule.protocol {
                Protocol::Tcp => loopback_tcp.insert(port),
                Protocol::Udp => loopback_udp.insert(port),
            };
        }

        let mut own_socket_dir = None;
        let mut has_web_port = false;
        for web in &manifest.web {
            validate::web_name(&web.name)?;
            match parse_upstream(&web.upstream, &id)? {
                Upstream::Unix(dir) => own_socket_dir = Some(dir),
                Upstream::Port(port) => {
                    loopback_tcp.insert(port);
                    has_web_port = true;
                }
            }
        }

        let mut dep_socket_dirs = Vec::new();
        for dep in &grants.socket_deps {
            validate::package_id(dep).map_err(|_| GenError::Grant("invalid dependency id"))?;
            dep_socket_dirs.push(format!("/run/apps/{dep}"));
        }

        for grant in &grants.lan {
            validate_lan(grant)?;
        }

        Ok(ServiceSpec {
            id,
            user,
            bind_config: profile == Profile::System || manifest.settings.is_some(),
            layout,
            program,
            args: args.to_vec(),
            comm,
            internet: manifest.network.outbound == Outbound::Internet,
            encrypted: manifest.storage.encrypted,
            has_listener: !inbound.is_empty() || has_web_port,
            own_socket_dir,
            dep_socket_dirs,
            data_mount: grants.data_mount.clone(),
            inbound,
            unrestricted_loopback: grants.unrestricted_loopback,
            loopback_tcp,
            loopback_udp,
            lan: grants.lan.clone(),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn user(&self) -> &str {
        &self.user
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    pub fn directories(&self) -> Vec<Directory> {
        let owned = |path: &str| Directory {
            path: path.to_owned(),
            owner: Some(self.user.clone()),
            mode: 0o750,
        };
        let mut dirs = Vec::new();
        if self.bind_config {
            dirs.push(Directory {
                path: self.layout.config_dir.clone(),
                owner: None,
                mode: 0o755,
            });
        }
        // bwrap creates the mount target inside the sandbox, so an encrypted package has no
        // plaintext data directory on the host at all.
        if !self.encrypted {
            dirs.push(owned(&self.layout.data_dir));
        }
        dirs.push(owned(&self.layout.log_dir));
        dirs
    }
}

fn program_path(first: &str, profile: Profile, layout: &Layout) -> Result<String, GenError> {
    match (profile, &layout.app_dir) {
        (Profile::App, Some(app_dir)) => {
            validate::relative_path(first)
                .map_err(|_| GenError::Exec("must be a path inside the package directory"))?;
            Ok(format!("{app_dir}/{first}"))
        }
        _ => {
            validate::absolute_path(first)
                .map_err(|_| GenError::Exec("must be an absolute path"))?;
            if !first.starts_with("/usr/") {
                return Err(GenError::Exec(
                    "system packages must run a program from /usr",
                ));
            }
            Ok(first.to_owned())
        }
    }
}

/// The `pkill -x` pattern for a program. `-x` takes a regular expression, and validation leaves
/// `.` as the only metacharacter a file name can contain.
fn comm_pattern(program: &str) -> String {
    let name = program.rsplit('/').next().unwrap_or(program);
    let truncated = &name[..name.len().min(COMM_LEN)];
    truncated.replace('.', r"\.")
}

/// Limited to mount locations so a grant can never redirect service data onto system
/// directories or another package's unlocked volume.
fn data_mount(path: &str) -> Result<(), GenError> {
    validate::absolute_path(path)?;
    if path.starts_with("/mnt/") || path.starts_with("/media/") {
        Ok(())
    } else {
        Err(GenError::Grant("data mount must be under /mnt or /media"))
    }
}

enum Upstream {
    Unix(String),
    Port(u16),
}

fn parse_upstream(upstream: &str, id: &str) -> Result<Upstream, GenError> {
    if let Some(path) = upstream.strip_prefix("unix:") {
        let dir = format!("/run/apps/{id}");
        let file = path
            .strip_prefix(&dir)
            .and_then(|rest| rest.strip_prefix('/'))
            .ok_or(GenError::Web(
                "socket must be in the package's own /run/apps directory",
            ))?;
        let name = file
            .strip_suffix(".sock")
            .ok_or(GenError::Web("socket must end in .sock"))?;
        validate::web_name(name)?;
        return Ok(Upstream::Unix(dir));
    }
    let port = upstream
        .strip_prefix("127.0.0.1:")
        .filter(|p| p.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|p| p.parse::<u16>().ok())
        .ok_or(GenError::Web(
            "upstream must be a /run/apps socket or 127.0.0.1:<port>",
        ))?;
    Ok(Upstream::Port(
        validate::port(port).map_err(|_| GenError::Web("invalid port"))?,
    ))
}

fn validate_lan(grant: &LanGrant) -> Result<(), GenError> {
    validate::port(grant.port).map_err(|_| GenError::Grant("invalid port"))?;
    let private = match grant.address {
        IpAddr::V4(addr) => {
            if grant.prefix > 32 {
                return Err(GenError::Grant("prefix too long"));
            }
            let bits = addr.to_bits();
            let mask = u32::MAX
                .checked_shl(32 - u32::from(grant.prefix))
                .unwrap_or(0);
            if bits & !mask != 0 {
                return Err(GenError::Grant("address has host bits set"));
            }
            let inside =
                |net: u32, len: u8| grant.prefix >= len && bits >> (32 - len) == net >> (32 - len);
            inside(0x0a00_0000, 8)
                || inside(0xac10_0000, 12)
                || inside(0xc0a8_0000, 16)
                || inside(0xa9fe_0000, 16)
        }
        IpAddr::V6(addr) => {
            if grant.prefix > 128 {
                return Err(GenError::Grant("prefix too long"));
            }
            let bits = addr.to_bits();
            let mask = u128::MAX
                .checked_shl(128 - u32::from(grant.prefix))
                .unwrap_or(0);
            if bits & !mask != 0 {
                return Err(GenError::Grant("address has host bits set"));
            }
            let inside = |net: u128, len: u8| {
                grant.prefix >= len && bits >> (128 - len) == net >> (128 - len)
            };
            inside(0xfc00_u128 << 112, 7) || inside(0xfe80_u128 << 112, 10)
        }
    };
    if private {
        Ok(())
    } else {
        // A wide or public grant would silently switch off the private range block.
        Err(GenError::Grant(
            "a local grant must stay inside a private range",
        ))
    }
}
