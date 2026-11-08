//! The last step inside a service sandbox, run by bwrap just before the service itself.
//!
//! Services share the host's network namespace, because their firewall rules match their user
//! on the host's interfaces. Abstract Unix sockets (`@name`, no file) belong to the network
//! namespace, so every service could reach every other service's abstract sockets, and no file
//! permission or bind mount stands in the way. This closes that, in two layers:
//!
//! - **Landlock scoping** (Linux 6.12, Landlock ABI 6). The service is put in its own Landlock
//!   domain with `LANDLOCK_SCOPE_ABSTRACT_UNIX_SOCKET`, so the kernel refuses to let it connect
//!   or send to an abstract socket created outside that domain: other services, host daemons
//!   and root. Its own abstract sockets keep working. `LANDLOCK_SCOPE_SIGNAL` is added too, so
//!   it cannot signal anything outside its domain.
//! - **seccomp**. A service granted no Unix socket gets a filter that refuses
//!   `socket(AF_UNIX, ...)`, which closes abstract sockets on any kernel. `socketpair` stays
//!   allowed, since an unnamed pair cannot reach anyone. The filter also refuses io_uring, which
//!   can open and connect sockets without the system calls a filter sees, and every system call
//!   of a foreign ABI (i386 or x32 on x86_64), whose numbers differ and include `socketcall`.
//!
//! seccomp alone cannot do the job for a service that does need Unix sockets: a filter sees
//! system call arguments only by value, and the address of `connect`/`bind`/`sendto` is a
//! pointer to memory it cannot read, so it cannot tell `@name` from `/run/apps/x.sock`. That is
//! what Landlock scoping is for. On an older kernel such a service is still confined by
//! everything else, and nodo-confine says once in its log that abstract sockets stay open.
//!
//! Everything here runs inside the sandbox as the service user, with no privileges. It sets
//! `no_new_privs` itself, so it works whether or not bwrap already did.

use std::io;

/// The domain argument of `socket(2)`.
const AF_UNIX: u32 = 1;

const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1 << 0;
const LANDLOCK_SCOPE_ABSTRACT_UNIX_SOCKET: u64 = 1 << 0;
const LANDLOCK_SCOPE_SIGNAL: u64 = 1 << 1;
/// The first Landlock ABI with scoping.
pub const LANDLOCK_SCOPE_ABI: i64 = 6;

/// `struct landlock_ruleset_attr` as of ABI 6.
#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}

// Classic BPF, as seccomp takes it.
/// BPF_LD | BPF_W | BPF_ABS.
const BPF_LD_W_ABS: u16 = 0x20;
/// BPF_JMP | BPF_JEQ | BPF_K, and the same for JGE and RET.
const BPF_JMP_JEQ_K: u16 = 0x15;
const BPF_JMP_JGE_K: u16 = 0x35;
const BPF_RET_K: u16 = 0x06;

const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;

/// Offsets in `struct seccomp_data`. Both supported architectures are little-endian, so the
/// low 32 bits of an argument come first.
const DATA_NR: u32 = 0;
const DATA_ARCH: u32 = 4;
const DATA_ARG0_LOW: u32 = 16;

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH_NATIVE: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH_NATIVE: u32 = 0xc000_00b7;
/// x32 system calls carry this bit in their number but report the x86_64 architecture.
#[cfg(target_arch = "x86_64")]
const X32_SYSCALL_BIT: Option<u32> = Some(0x4000_0000);
#[cfg(target_arch = "aarch64")]
const X32_SYSCALL_BIT: Option<u32> = None;

fn nr(value: libc::c_long) -> u32 {
    u32::try_from(value).expect("system call numbers are small")
}

/// One classic BPF instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Insn {
    pub code: u16,
    pub jt: u8,
    pub jf: u8,
    pub k: u32,
}

impl Insn {
    fn load(offset: u32) -> Self {
        Insn { code: BPF_LD_W_ABS, jt: 0, jf: 0, k: offset }
    }
    /// Falls through on a match and skips `skip` instructions otherwise.
    fn jeq(value: u32, skip: u8) -> Self {
        Insn { code: BPF_JMP_JEQ_K, jt: 0, jf: skip, k: value }
    }
    fn ret(value: u32) -> Self {
        Insn { code: BPF_RET_K, jt: 0, jf: 0, k: value }
    }
    fn errno(errno: i32) -> Self {
        Insn::ret(SECCOMP_RET_ERRNO | (errno as u32 & 0xffff))
    }
}

/// What the sandbox may do with Unix sockets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// The service was granted a Unix socket: its own web socket, or a dependency's.
    pub unix: bool,
}

/// The seccomp program for these options. Pure, so it can be checked without loading it.
pub fn filter(options: Options) -> Vec<Insn> {
    let mut prog = vec![
        Insn::load(DATA_ARCH),
        // The native ABI skips the refusal.
        Insn { code: BPF_JMP_JEQ_K, jt: 1, jf: 0, k: AUDIT_ARCH_NATIVE },
        Insn::errno(libc::ENOSYS),
        Insn::load(DATA_NR),
    ];
    if let Some(bit) = X32_SYSCALL_BIT {
        prog.push(Insn { code: BPF_JMP_JGE_K, jt: 0, jf: 1, k: bit });
        prog.push(Insn::errno(libc::ENOSYS));
    }
    for call in [libc::SYS_io_uring_setup, libc::SYS_io_uring_enter, libc::SYS_io_uring_register] {
        prog.push(Insn::jeq(nr(call), 1));
        prog.push(Insn::errno(libc::ENOSYS));
    }
    if !options.unix {
        prog.push(Insn::jeq(nr(libc::SYS_socket), 3));
        prog.push(Insn::load(DATA_ARG0_LOW));
        prog.push(Insn::jeq(AF_UNIX, 1));
        prog.push(Insn::errno(libc::EACCES));
    }
    prog.push(Insn::ret(SECCOMP_RET_ALLOW));
    prog
}

/// The Landlock ABI this kernel offers, or `None` when Landlock is missing or switched off.
pub fn landlock_abi() -> Option<i64> {
    // SAFETY: with a null attribute and this flag the call only reports the ABI version.
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    (abi > 0).then_some(abi)
}

/// What [`confine`] managed to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Applied {
    /// Abstract sockets outside the sandbox are refused by Landlock.
    pub scoped: bool,
    /// `socket(AF_UNIX)` is refused by seccomp.
    pub unix_denied: bool,
}

impl Applied {
    /// Whether another service's abstract sockets are out of reach.
    pub fn abstract_closed(&self) -> bool {
        self.scoped || self.unix_denied
    }
}

fn last_error(what: &str) -> io::Error {
    let error = io::Error::last_os_error();
    io::Error::new(error.kind(), format!("{what}: {error}"))
}

fn no_new_privs() -> io::Result<()> {
    // SAFETY: plain prctl with integer arguments.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(last_error("PR_SET_NO_NEW_PRIVS"));
    }
    Ok(())
}

fn landlock_scope() -> io::Result<bool> {
    match landlock_abi() {
        Some(abi) if abi >= LANDLOCK_SCOPE_ABI => {}
        _ => return Ok(false),
    }
    let attr = RulesetAttr {
        handled_access_fs: 0,
        handled_access_net: 0,
        scoped: LANDLOCK_SCOPE_ABSTRACT_UNIX_SOCKET | LANDLOCK_SCOPE_SIGNAL,
    };
    // SAFETY: attr is a valid ruleset attribute of the size given, living across the call.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &attr as *const RulesetAttr,
            std::mem::size_of::<RulesetAttr>(),
            0u32,
        )
    };
    if fd < 0 {
        return Err(last_error("landlock_create_ruleset"));
    }
    let fd = fd as libc::c_int;
    // SAFETY: fd is the ruleset just created; it is closed right after.
    let restricted = unsafe { libc::syscall(libc::SYS_landlock_restrict_self, fd, 0u32) };
    let error = (restricted != 0).then(|| last_error("landlock_restrict_self"));
    // SAFETY: fd is ours and not used again.
    unsafe { libc::close(fd) };
    match error {
        Some(error) => Err(error),
        None => Ok(true),
    }
}

fn load_filter(prog: &[Insn]) -> io::Result<()> {
    let mut raw: Vec<libc::sock_filter> = prog
        .iter()
        .map(|i| libc::sock_filter { code: i.code, jt: i.jt, jf: i.jf, k: i.k })
        .collect();
    let fprog = libc::sock_fprog {
        len: u16::try_from(raw.len()).expect("the program is short"),
        filter: raw.as_mut_ptr(),
    };
    // SAFETY: fprog points at `raw`, which outlives the call; the kernel copies the program.
    let loaded = unsafe {
        libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &fprog as *const libc::sock_fprog, 0, 0)
    };
    if loaded != 0 {
        return Err(last_error("PR_SET_SECCOMP"));
    }
    Ok(())
}

/// Confines the calling process and everything it starts afterwards. It cannot be undone.
/// Any failure to apply what the kernel supports is an error, so the caller never starts a
/// service less confined than intended without saying so.
pub fn confine(options: Options) -> io::Result<Applied> {
    no_new_privs()?;
    let scoped = landlock_scope()?;
    load_filter(&filter(options))?;
    Ok(Applied { scoped, unix_denied: !options.unix })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jumps_stay_inside(prog: &[Insn]) {
        for (index, insn) in prog.iter().enumerate() {
            if insn.code & 0x07 == 0x05 {
                let end = prog.len();
                assert!(index + 1 + (insn.jt as usize) < end, "jt out of range at {index}");
                assert!(index + 1 + (insn.jf as usize) < end, "jf out of range at {index}");
            }
        }
        assert_eq!(prog.last().unwrap().code, BPF_RET_K, "the program must end in a return");
    }

    /// Runs the program the way the kernel would, for one system call.
    fn run(prog: &[Insn], arch: u32, nr: u32, arg0: u32) -> u32 {
        let mut acc = 0u32;
        let mut pc = 0usize;
        loop {
            let insn = prog[pc];
            match insn.code {
                BPF_LD_W_ABS => {
                    acc = match insn.k {
                        DATA_NR => nr,
                        DATA_ARCH => arch,
                        DATA_ARG0_LOW => arg0,
                        other => panic!("unexpected load {other}"),
                    };
                    pc += 1;
                }
                BPF_JMP_JEQ_K => pc += 1 + if acc == insn.k { insn.jt } else { insn.jf } as usize,
                BPF_JMP_JGE_K => pc += 1 + if acc >= insn.k { insn.jt } else { insn.jf } as usize,
                BPF_RET_K => return insn.k,
                other => panic!("unexpected instruction {other:#x}"),
            }
        }
    }

    const EACCES: u32 = SECCOMP_RET_ERRNO | libc::EACCES as u32;
    const ENOSYS: u32 = SECCOMP_RET_ERRNO | libc::ENOSYS as u32;

    #[test]
    fn without_a_unix_grant_only_unix_sockets_are_refused() {
        let prog = filter(Options { unix: false });
        jumps_stay_inside(&prog);
        let socket = nr(libc::SYS_socket);
        assert_eq!(run(&prog, AUDIT_ARCH_NATIVE, socket, AF_UNIX), EACCES);
        assert_eq!(run(&prog, AUDIT_ARCH_NATIVE, socket, libc::AF_INET as u32), SECCOMP_RET_ALLOW);
        assert_eq!(run(&prog, AUDIT_ARCH_NATIVE, socket, libc::AF_INET6 as u32), SECCOMP_RET_ALLOW);
        assert_eq!(run(&prog, AUDIT_ARCH_NATIVE, nr(libc::SYS_socketpair), AF_UNIX), SECCOMP_RET_ALLOW);
        assert_eq!(run(&prog, AUDIT_ARCH_NATIVE, nr(libc::SYS_connect), AF_UNIX), SECCOMP_RET_ALLOW);
    }

    #[test]
    fn with_a_unix_grant_unix_sockets_are_allowed() {
        let prog = filter(Options { unix: true });
        jumps_stay_inside(&prog);
        assert_eq!(run(&prog, AUDIT_ARCH_NATIVE, nr(libc::SYS_socket), AF_UNIX), SECCOMP_RET_ALLOW);
    }

    #[test]
    fn io_uring_and_foreign_abis_are_refused_either_way() {
        for unix in [false, true] {
            let prog = filter(Options { unix });
            for call in [libc::SYS_io_uring_setup, libc::SYS_io_uring_enter, libc::SYS_io_uring_register] {
                assert_eq!(run(&prog, AUDIT_ARCH_NATIVE, nr(call), 0), ENOSYS);
            }
            // i386 socketcall(SYS_SOCKET) would otherwise slip past the socket check.
            assert_eq!(run(&prog, 0x4000_0003, 102, 1), ENOSYS);
            if let Some(bit) = X32_SYSCALL_BIT {
                assert_eq!(run(&prog, AUDIT_ARCH_NATIVE, bit | 41, AF_UNIX), ENOSYS);
            }
            assert_eq!(run(&prog, AUDIT_ARCH_NATIVE, nr(libc::SYS_read), 0), SECCOMP_RET_ALLOW);
        }
    }
}
