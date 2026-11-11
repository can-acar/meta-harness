//! Ortak yardımcılar: /etc/passwd ve /etc/group okuma (NSS'siz, statik ikili için), komut çalıştırma.

use std::ffi::CString;
use std::process::{Command, ExitStatus};

pub type Res<T = ()> = Result<T, String>;

pub struct Passwd {
    pub uid: u32,
    pub gid: u32,
    pub home: String,
}

pub fn passwd(name: &str) -> Option<Passwd> {
    let s = std::fs::read_to_string("/etc/passwd").ok()?;
    s.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.len() >= 7 && f[0] == name).then(|| Passwd {
            uid: f[2].parse().unwrap_or(u32::MAX),
            gid: f[3].parse().unwrap_or(u32::MAX),
            home: f[5].into(),
        })
    })
}

pub fn user_by_uid(uid: u32) -> Option<String> {
    let s = std::fs::read_to_string("/etc/passwd").ok()?;
    s.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.len() >= 3 && f[2].parse() == Ok(uid)).then(|| f[0].to_string())
    })
}

/// (gid, üyeler)
pub fn group(name: &str) -> Option<(u32, Vec<String>)> {
    let s = std::fs::read_to_string("/etc/group").ok()?;
    s.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.len() >= 4 && f[0] == name).then(|| {
            let members = f[3].split(',').filter(|m| !m.is_empty()).map(String::from).collect();
            (f[2].parse().unwrap_or(u32::MAX), members)
        })
    })
}

pub fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

pub fn require_root() -> Res {
    if is_root() { Ok(()) } else { Err("root gerekli (sudo ile çalıştırın)".into()) }
}

pub fn run(args: &[&str]) -> Res<ExitStatus> {
    Command::new(args[0]).args(&args[1..]).status().map_err(|e| format!("{} çalıştırılamadı: {e}", args[0]))
}

pub fn run_ok(args: &[&str]) -> Res {
    let st = run(args)?;
    if st.success() { Ok(()) } else { Err(format!("komut başarısız: {}", args.join(" "))) }
}

pub fn chown(path: &str, uid: u32, gid: u32) -> Res {
    let c = CString::new(path).map_err(|e| e.to_string())?;
    if unsafe { libc::chown(c.as_ptr(), uid, gid) } == 0 {
        Ok(())
    } else {
        Err(format!("chown {path}: {}", std::io::Error::last_os_error()))
    }
}

pub fn chmod(path: &str, mode: u32) -> Res {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|e| format!("chmod {path}: {e}"))
}

pub fn write_owned(path: &str, content: &str, uid: u32, gid: u32, mode: u32) -> Res {
    std::fs::write(path, content).map_err(|e| format!("{path}: {e}"))?;
    chown(path, uid, gid)?;
    chmod(path, mode)
}

/// Kullanıcının çalıştırdığı komut adı (argv[0] tabanı).
pub fn prog_name() -> String {
    std::env::args()
        .next()
        .and_then(|a| std::path::Path::new(&a).file_name().map(|f| f.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "mh".into())
}
