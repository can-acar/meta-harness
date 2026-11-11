//! mh-kullanici (insan/ajan hesapları) ve mh-anahtar (ajanların ortak API anahtarı).

use crate::util::{chmod, chown, group, passwd, require_root, run_ok, write_owned, Res};
use std::io::{BufRead, Write};

const ROLLER: &str = "/etc/metahernes/roller";
const YARDIM: &str = "\
mh-kullanici insan <ad> [ssh-açık-anahtar-dosyası] [--yonetici]
mh-kullanici ajan  <ad> <rol>        rol: /etc/metahernes/roller altındakiler
mh-kullanici sil   <ad>
mh-kullanici liste";

fn gecerli_ad(ad: &str) -> Res {
    let ok = !ad.is_empty() && ad.len() <= 24 && ad.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') && !ad.starts_with('-');
    if ok { Ok(()) } else { Err(format!("geçersiz ad: {ad} (küçük harf, rakam, - ve _)")) }
}

fn onboarding(ev: &str, uid: u32, gid: u32) -> Res {
    let yol = format!("{ev}/.claude.json");
    if std::path::Path::new(&yol).exists() {
        return Ok(());
    }
    write_owned(&yol, r#"{"hasCompletedOnboarding":true,"theme":"dark","projects":{"/srv/proje":{"hasTrustDialogAccepted":true}}}"#, uid, gid, 0o600)
}

fn insan(ad: &str, anahtar: Option<&str>, yonetici: bool) -> Res {
    gecerli_ad(ad)?;
    if passwd(ad).is_none() {
        run_ok(&["useradd", "-m", "-s", "/bin/bash", "-G", "insanlar,proje", ad])?;
    }
    run_ok(&["usermod", "-p", "*", ad])?; // şifre yok ama hesap kilitli değil: yalnızca SSH anahtarı
    if yonetici {
        run_ok(&["usermod", "-aG", "yonetici", ad])?;
    }
    let p = passwd(ad).ok_or("kullanıcı oluşturulamadı")?;
    if let Some(k) = anahtar {
        let icerik = std::fs::read_to_string(k).map_err(|e| format!("{k}: {e}"))?;
        let ssh = format!("{}/.ssh", p.home);
        std::fs::create_dir_all(&ssh).map_err(|e| e.to_string())?;
        chown(&ssh, p.uid, p.gid)?;
        chmod(&ssh, 0o700)?;
        let ak = format!("{ssh}/authorized_keys");
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&ak).map_err(|e| e.to_string())?;
        writeln!(f, "{}", icerik.trim()).map_err(|e| e.to_string())?;
        chown(&ak, p.uid, p.gid)?;
        chmod(&ak, 0o600)?;
    }
    onboarding(&p.home, p.uid, p.gid)?;
    println!("insan eklendi: {ad}{}", if yonetici { " (yönetici)" } else { "" });
    Ok(())
}

fn ajan(ad: &str, rol: &str) -> Res {
    gecerli_ad(ad)?;
    gecerli_ad(rol)?;
    let rd = format!("{ROLLER}/{rol}");
    if !std::path::Path::new(&rd).is_dir() {
        let mevcut: Vec<String> = std::fs::read_dir(ROLLER).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
        return Err(format!("Rol yok: {rol} (mevcut: {})", mevcut.join(", ")));
    }
    let k = format!("ajan-{ad}");
    let ev = format!("/var/lib/ajanlar/{ad}");
    if passwd(&k).is_none() {
        std::fs::create_dir_all("/var/lib/ajanlar").map_err(|e| e.to_string())?;
        run_ok(&["useradd", "-r", "-m", "-d", &ev, "-s", "/bin/bash", "-G", "ajanlar", &k])?;
    }
    run_ok(&["usermod", "-p", "*", &k])?;
    // Rol, Linux izinlerini de belirler: proje-yazma=evet değilse /srv/proje salt okunur kalır
    let yazma = std::fs::read_to_string(format!("{rd}/proje-yazma")).map(|s| s.trim() == "evet").unwrap_or(false);
    if yazma {
        run_ok(&["usermod", "-aG", "proje", &k])?;
    } else {
        let _ = std::process::Command::new("gpasswd").args(["-d", &k, "proje"]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
    }
    let p = passwd(&k).ok_or("ajan oluşturulamadı")?;
    let cd = format!("{ev}/.claude");
    std::fs::create_dir_all(&cd).map_err(|e| e.to_string())?;
    chown(&cd, p.uid, p.gid)?;
    for f in ["CLAUDE.md", "settings.json"] {
        let icerik = std::fs::read_to_string(format!("{rd}/{f}")).map_err(|e| format!("{rd}/{f}: {e}"))?;
        write_owned(&format!("{cd}/{f}"), &icerik, p.uid, p.gid, 0o644)?;
    }
    write_owned(&format!("{ev}/.gitconfig"), &format!("[user]\n\tname = {k}\n\temail = {k}@metahernes.local\n"), p.uid, p.gid, 0o644)?;
    std::fs::create_dir_all("/etc/metahernes/ajanlar").map_err(|e| e.to_string())?;
    std::fs::write(format!("/etc/metahernes/ajanlar/{ad}.rol"), format!("{rol}\n")).map_err(|e| e.to_string())?;
    onboarding(&ev, p.uid, p.gid)?;
    println!("ajan eklendi: {ad} (rol: {rol}, kullanıcı: {k})");
    Ok(())
}

pub fn main(args: &[String]) -> Res {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["insan", ad, rest @ ..] => {
            require_root()?;
            let yonetici = rest.contains(&"--yonetici");
            let anahtar = rest.iter().find(|x| **x != "--yonetici").copied();
            insan(ad, anahtar, yonetici)
        }
        ["ajan", ad, rol] => {
            require_root()?;
            ajan(ad, rol)
        }
        ["sil", ad] => {
            require_root()?;
            gecerli_ad(ad)?;
            if passwd(&format!("ajan-{ad}")).is_some() {
                run_ok(&["userdel", "-r", &format!("ajan-{ad}")])?;
                let _ = std::fs::remove_file(format!("/etc/metahernes/ajanlar/{ad}.rol"));
            } else {
                run_ok(&["userdel", "-r", ad])?;
            }
            println!("silindi: {ad}");
            Ok(())
        }
        ["liste"] => {
            for (baslik, g) in [("İNSANLAR", "insanlar"), ("YÖNETİCİLER", "yonetici"), ("AJANLAR", "ajanlar")] {
                println!("{baslik}: {}", group(g).map(|g| g.1.join(", ")).unwrap_or_default());
            }
            Ok(())
        }
        _ => {
            println!("{YARDIM}");
            Ok(())
        }
    }
}

/// mh-anahtar [dosya]: ajanların ortak Anthropic API anahtarı (yalnızca 'ajanlar' grubu okuyabilir).
pub fn anahtar(args: &[String]) -> Res {
    require_root()?;
    let key = match args.first() {
        Some(f) => std::fs::read_to_string(f).map_err(|e| format!("{f}: {e}"))?,
        None => {
            eprint!("ANTHROPIC_API_KEY: ");
            let _ = std::io::stderr().flush();
            let mut t: libc::termios = unsafe { std::mem::zeroed() };
            let tty = unsafe { libc::tcgetattr(0, &mut t) } == 0;
            if tty {
                let mut s = t;
                s.c_lflag &= !libc::ECHO;
                unsafe { libc::tcsetattr(0, libc::TCSANOW, &s) };
            }
            let mut l = String::new();
            let _ = std::io::stdin().lock().read_line(&mut l);
            if tty {
                unsafe { libc::tcsetattr(0, libc::TCSANOW, &t) };
            }
            eprintln!();
            l
        }
    };
    let key: String = key.chars().filter(|c| !c.is_whitespace()).collect();
    if key.is_empty() {
        return Err("boş anahtar".into());
    }
    let gid = group("ajanlar").ok_or("'ajanlar' grubu yok")?.0;
    write_owned(crate::ajan::KEY, &format!("{key}\n"), 0, gid, 0o640)?;
    println!("Kaydedildi. Yalnızca 'ajanlar' grubu okuyabilir; insanlar kendi 'claude' girişini kullanır.");
    Ok(())
}
