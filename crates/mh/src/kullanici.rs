//! mh-kullanici (insan/ajan hesapları) ve mh-anahtar (ajanların ortak API anahtarı).

use crate::ajan::{Arac, AGY_BELIRTEC, OZEL_ANAHTAR_DIZINI};
use crate::util::{chmod, chown, group, passwd, require_root, run_ok, write_owned, Res};
use std::io::{BufRead, Write};

const ROLLER: &str = "/etc/metahernes/roller";
const YARDIM: &str = "\
mh-kullanici insan <ad> [ssh-açık-anahtar-dosyası] [--yonetici]
mh-kullanici ajan  <ad> <rol> [araç]  rol: /etc/metahernes/roller altındakiler; araç: claude (varsayılan) | codex | agy
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

fn ajan(ad: &str, rol: &str, arac: Arac) -> Res {
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
    arac_kur(arac, &rd, &ev, p.uid, p.gid)?;
    write_owned(&format!("{ev}/.gitconfig"), &format!("[user]\n\tname = {k}\n\temail = {k}@metahernes.local\n"), p.uid, p.gid, 0o644)?;
    std::fs::create_dir_all("/etc/metahernes/ajanlar").map_err(|e| e.to_string())?;
    std::fs::write(format!("/etc/metahernes/ajanlar/{ad}.rol"), format!("{rol}\n")).map_err(|e| e.to_string())?;
    std::fs::write(format!("/etc/metahernes/ajanlar/{ad}.arac"), format!("{}\n", arac.ad())).map_err(|e| e.to_string())?;
    onboarding(&ev, p.uid, p.gid)?;
    println!("ajan eklendi: {ad} (rol: {rol}, araç: {}, kullanıcı: {k})", arac.ad());
    if arac != Arac::Claude {
        println!("kimlik: mh-ajan {ad} giris  ya da  sudo mh-anahtar --ajan {ad} <dosya>");
    }
    Ok(())
}

/// Sahibi ajan olan dizin zinciri oluşturur (ev altındaki göreli yol).
fn ajan_dizini(ev: &str, goreli: &str, uid: u32, gid: u32, mod_: u32) -> Res<String> {
    let mut yol = ev.to_string();
    for parca in goreli.split('/').filter(|p| !p.is_empty()) {
        yol = format!("{yol}/{parca}");
        if !std::path::Path::new(&yol).is_dir() {
            std::fs::create_dir(&yol).map_err(|e| format!("{yol}: {e}"))?;
            chown(&yol, uid, gid)?;
            chmod(&yol, mod_)?;
        }
    }
    Ok(yol)
}

/// Rolün araca özel dosyalarını ajanın evine kopyalar. Talimatlar (CLAUDE.md) her araca verilir.
fn arac_kur(arac: Arac, rd: &str, ev: &str, uid: u32, gid: u32) -> Res {
    let talimat = std::fs::read_to_string(format!("{rd}/CLAUDE.md")).unwrap_or_default();
    match arac {
        Arac::Claude => {}
        Arac::Codex => {
            let d = ajan_dizini(ev, ".codex", uid, gid, 0o700)?;
            let ayar = std::fs::read_to_string(format!("{rd}/codex.toml")).map_err(|e| format!("{rd}/codex.toml: {e}"))?;
            write_owned(&format!("{d}/config.toml"), &ayar, uid, gid, 0o644)?;
            write_owned(&format!("{d}/AGENTS.md"), &talimat, uid, gid, 0o644)?;
        }
        Arac::Agy => {
            let d = ajan_dizini(ev, ".gemini", uid, gid, 0o700)?;
            write_owned(&format!("{d}/GEMINI.md"), &talimat, uid, gid, 0o644)?;
            let m = ajan_dizini(ev, ".metahernes", uid, gid, 0o700)?;
            let bayrak = std::fs::read_to_string(format!("{rd}/agy-bayraklar")).unwrap_or_default();
            write_owned(&format!("{m}/agy-bayraklar"), &bayrak, uid, gid, 0o644)?;
        }
    }
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
            ajan(ad, rol, Arac::Claude)
        }
        ["ajan", ad, rol, arac] => {
            require_root()?;
            ajan(ad, rol, Arac::coz(arac)?)
        }
        ["sil", ad] => {
            require_root()?;
            gecerli_ad(ad)?;
            if passwd(&format!("ajan-{ad}")).is_some() {
                run_ok(&["userdel", "-r", &format!("ajan-{ad}")])?;
                let _ = std::fs::remove_file(format!("/etc/metahernes/ajanlar/{ad}.rol"));
                let _ = std::fs::remove_file(format!("/etc/metahernes/ajanlar/{ad}.arac"));
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

fn gizli_oku(soru: &str) -> String {
    eprint!("{soru}: ");
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

/// mh-anahtar [dosya]                ajanların ortak Anthropic API anahtarı (yalnızca 'ajanlar' grubu okuyabilir)
/// mh-anahtar --ajan <ad> [dosya]    yalnızca o ajanın kendi kimliği (aracına göre):
///     claude: Anthropic API anahtarı, codex: OpenAI API anahtarı, agy: OAuth belirteç dosyası
pub fn anahtar(args: &[String]) -> Res {
    require_root()?;
    if args.first().map(String::as_str) == Some("--ajan") {
        let ad = args.get(1).ok_or("--ajan için ajan adı gerekli")?;
        return ajan_anahtari(ad, args.get(2).map(String::as_str));
    }
    let key = match args.first() {
        Some(f) => std::fs::read_to_string(f).map_err(|e| format!("{f}: {e}"))?,
        None => gizli_oku("ANTHROPIC_API_KEY"),
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

/// Ajana özel kimlik: ajanın evinde, yalnızca o ajanın okuyabileceği (0600) bir dosyaya yazılır.
fn ajan_anahtari(ad: &str, dosya: Option<&str>) -> Res {
    gecerli_ad(ad)?;
    let p = passwd(&format!("ajan-{ad}")).ok_or_else(|| format!("Böyle bir ajan yok: {ad}"))?;
    let arac = crate::ajan::arac(ad);
    if arac == Arac::Agy {
        // OAuth belirteci başka bir makinedeki `agy` girişinden kopyalanır; içerik olduğu gibi saklanır
        let f = dosya.ok_or("agy için belirteç dosyası gerekli (~/.gemini/antigravity-cli/antigravity-oauth-token) ya da: mh-ajan <ad> giris")?;
        let icerik = std::fs::read_to_string(f).map_err(|e| format!("{f}: {e}"))?;
        let (dizin, dosya_adi) = AGY_BELIRTEC.rsplit_once('/').unwrap_or(("", AGY_BELIRTEC));
        let d = ajan_dizini(&p.home, dizin, p.uid, p.gid, 0o700)?;
        write_owned(&format!("{d}/{dosya_adi}"), &icerik, p.uid, p.gid, 0o600)?;
    } else {
        let key = match dosya {
            Some(f) => std::fs::read_to_string(f).map_err(|e| format!("{f}: {e}"))?,
            None => gizli_oku(if arac == Arac::Codex { "OPENAI_API_KEY" } else { "ANTHROPIC_API_KEY" }),
        };
        let key: String = key.chars().filter(|c| !c.is_whitespace()).collect();
        if key.is_empty() {
            return Err("boş anahtar".into());
        }
        let d = ajan_dizini(&p.home, OZEL_ANAHTAR_DIZINI, p.uid, p.gid, 0o700)?;
        write_owned(&format!("{d}/{}", arac.ad()), &format!("{key}\n"), p.uid, p.gid, 0o600)?;
    }
    println!("Kaydedildi: {ad} ({}). Yalnızca ajan-{ad} okuyabilir.", arac.ad());
    Ok(())
}
