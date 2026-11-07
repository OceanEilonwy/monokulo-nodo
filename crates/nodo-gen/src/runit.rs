use std::fmt::Write;

use crate::shell::sh_quote;
use crate::spec::{volume_mount, ServiceSpec};

/// The runit `run` script. `packages/cuprated-service/files/cuprated/run` is the reference.
pub fn run_script(spec: &ServiceSpec) -> String {
    let mut out = String::from(
        "#!/bin/sh\n\
         exec 2>&1\n\
         # With a console as stdin, a service can run a command prompt and flood the log.\n\
         exec </dev/null\n\n",
    );
    if spec.encrypted {
        encrypted_preamble(spec, &mut out);
    } else {
        data_preamble(spec, &mut out);
    }
    out.push('\n');
    let user = sh_quote(&spec.user);
    if let Some(dir) = &spec.own_socket_dir {
        // Traversable so the proxy can reach sockets, which carry their own permissions.
        let _ = writeln!(
            out,
            "install -d -o {user} -g {user} -m 755 {}\n",
            sh_quote(dir)
        );
    }

    let _ = writeln!(out, "exec chpst -u {user}:{user} bwrap \\");
    let mut line = |text: &str| {
        out.push('\t');
        out.push_str(text);
        out.push_str(" \\\n");
    };
    line("--ro-bind /usr /usr");
    line("--symlink usr/bin /bin");
    line("--symlink usr/lib /lib");
    line("--symlink usr/lib /lib64");
    let layout = &spec.layout;
    if let Some(app_dir) = &layout.app_dir {
        let q = sh_quote(app_dir);
        line(&format!("--ro-bind {q} {q}"));
    }
    if spec.bind_config {
        let q = sh_quote(&layout.config_dir);
        line(&format!("--ro-bind {q} {q}"));
    }
    if spec.internet {
        line("--ro-bind-try /etc/resolv.conf /etc/resolv.conf");
        line("--ro-bind-try /etc/ssl /etc/ssl");
    }
    if let Some(dir) = &spec.own_socket_dir {
        let q = sh_quote(dir);
        line(&format!("--bind {q} {q}"));
    }
    for dir in &spec.dep_socket_dirs {
        let q = sh_quote(dir);
        line(&format!("--ro-bind {q} {q}"));
    }
    line(&format!("--bind \"$data\" {}", sh_quote(&layout.data_dir)));
    line("--proc /proc");
    line("--dev /dev");
    line("--tmpfs /tmp");
    line(&format!("--setenv HOME {}", sh_quote(&layout.data_dir)));
    line("--unshare-pid --unshare-ipc --unshare-uts --unshare-cgroup-try");
    line("--new-session --die-with-parent");

    let mut command = sh_quote(&spec.program);
    for arg in &spec.args {
        command.push(' ');
        command.push_str(&sh_quote(arg));
    }
    let _ = writeln!(out, "\t{command}");
    out
}

/// A board can move data to another drive through `conf`, written by root at install time.
fn data_preamble(spec: &ServiceSpec, out: &mut String) {
    out.push_str(
        "# A board can keep data on a separate drive. Never fall back to the system disk.\n",
    );
    let _ = writeln!(out, "data={}", sh_quote(&spec.layout.data_dir));
    out.push_str("[ -r ./conf ] && . ./conf\n");
    if let Some(mount) = &spec.data_mount {
        let _ = writeln!(out, "DATA_MOUNT={}", sh_quote(mount));
    }
    let user = sh_quote(&spec.user);
    let _ = write!(
        out,
        "if [ -n \"${{DATA_MOUNT:-}}\" ]; then\n\
         \tif ! mountpoint -q \"$DATA_MOUNT\"; then\n\
         \t\techo \"refusing to start: $DATA_MOUNT is not mounted\"\n\
         \t\tsleep 30\n\
         \t\texit 1\n\
         \tfi\n\
         \tdata=$DATA_MOUNT/{id}\n\
         \tinstall -d -o {user} -g {user} -m 750 \"$data\"\n\
         fi\n",
        id = spec.id,
    );
}

/// `conf` is deliberately not read here, so nothing can point the service at a plaintext
/// directory. The mountpoint check also stops it writing into the empty directory underneath.
fn encrypted_preamble(spec: &ServiceSpec, out: &mut String) {
    let mount = volume_mount(&spec.id).expect("id was validated by ServiceSpec::new");
    let _ = write!(
        out,
        "# Encrypted storage opens with the device passphrase. Wait here until it does, and\n\
         # never fall back to an unencrypted directory.\n\
         data={mount}\n\
         if ! mountpoint -q \"$data\"; then\n\
         \techo \"waiting: encrypted storage for {id} is locked\"\n\
         \tsleep 5\n\
         \texit 1\n\
         fi\n",
        mount = sh_quote(&mount),
        id = spec.id,
    );
}

/// The runit `log/run` script.
pub fn log_run(spec: &ServiceSpec) -> String {
    let user = sh_quote(&spec.user);
    format!(
        "#!/bin/sh\nexec chpst -u {user}:{user} svlogd -tt {}\n",
        sh_quote(&spec.layout.log_dir)
    )
}

/// The runit `control/t` script.
pub fn control_t(spec: &ServiceSpec) -> String {
    format!(
        "#!/bin/sh\n\
         # bwrap does not forward signals, and killing it would SIGKILL the service through\n\
         # --die-with-parent. Signal the service directly so it can shut down cleanly.\n\
         exec pkill -TERM -u {} -x {}\n",
        sh_quote(&spec.user),
        sh_quote(&spec.comm),
    )
}
