//! mh-ajan (insanlar için) ve ajan-calistir (sudo ile ajan hesabında çalışan uç).

use crate::util::{group, passwd, user_by_uid, Res};
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

pub const CALISTIR: &str = "/usr/local/lib/metahernes/ajan-calistir";
pub const KEY: &str = "/etc/metahernes/ajan.key";

const YARDIM: &str = "\
mh-ajan liste
mh-ajan <ad> \"<görev>\" [-d DIZIN]     tek seferlik (claude -p), çıktı + kayıt
mh-ajan <ad> -i [-d DIZIN]            ajanın hesabında etkileşimli oturum";

pub fn rol(ad: &str) -> String {
    std::fs::read_to_string(format!("/etc/metahernes/ajanlar/{ad}.rol")).map(|s| s.trim().to_string()).unwrap_or_else(|_| "?".into())
}

pub fn main(args: &[String]) -> Res {
    match args.first().map(String::as_str) {
        None | Some("-h") | Some("--yardim") => {
            println!("{YARDIM}");
            Ok(())
        }
        Some("liste") => {
            println!("{:<22} ROL", "AJAN");
            for u in group("ajanlar").map(|g| g.1).unwrap_or_default() {
                let ad = u.strip_prefix("ajan-").unwrap_or(&u);
                println!("{ad:<22} {}", rol(ad));
            }
            Ok(())
        }
        Some(ad) => {
            let kullanici = format!("ajan-{ad}");
            if passwd(&kullanici).is_none() {
                return Err(format!("Böyle bir ajan yok: {ad} (mh-ajan liste)"));
            }
            let mut dizin = "/srv/proje".to_string();
            let mut etkilesimli = false;
            let mut gorev = Vec::new();
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "-d" => dizin = it.next().ok_or("-d için dizin gerekli")?.clone(),
                    "-i" => etkilesimli = true,
                    _ => gorev.push(a.clone()),
                }
            }
            let mut sudo = Command::new("sudo");
            sudo.args(["-u", &kullanici, "-H", CALISTIR, &dizin]);
            if etkilesimli {
                let e = sudo.arg("-i").exec();
                return Err(format!("sudo: {e}"));
            }
            if gorev.is_empty() {
                return Err("Görev metni gerekli.".into());
            }
            let gorev = gorev.join(" ");
            calistir_kayitli(sudo.arg(&gorev), ad, &dizin, &gorev)
        }
    }
}

/// Çıktıyı hem terminale hem ~/.metahernes/kayitlar/ altına yazar.
fn calistir_kayitli(cmd: &mut Command, ad: &str, dizin: &str, gorev: &str) -> Res {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let kd = format!("{home}/.metahernes/kayitlar");
    std::fs::create_dir_all(&kd).map_err(|e| format!("{kd}: {e}"))?;
    let zaman = Command::new("date").arg("+%Y%m%d-%H%M%S").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    let yol = format!("{kd}/{zaman}-{ad}.log");
    let mut f = std::fs::File::create(&yol).map_err(|e| format!("{yol}: {e}"))?;
    let isteyen = user_by_uid(unsafe { libc::getuid() }).unwrap_or_default();
    let _ = writeln!(f, "# ajan: {ad}  dizin: {dizin}  isteyen: {isteyen}\n# görev: {gorev}\n");
    let f = Arc::new(Mutex::new(f));

    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("sudo: {e}"))?;
    let kopyala = |mut r: Box<dyn Read + Send>, f: Arc<Mutex<std::fs::File>>, hata: bool| {
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = r.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let _ = if hata { std::io::stderr().write_all(&buf[..n]) } else { std::io::stdout().write_all(&buf[..n]) };
                let _ = f.lock().unwrap().write_all(&buf[..n]);
            }
        })
    };
    let a = kopyala(Box::new(child.stdout.take().unwrap()), f.clone(), false);
    let b = kopyala(Box::new(child.stderr.take().unwrap()), f, true);
    let st = child.wait().map_err(|e| e.to_string())?;
    let _ = (a.join(), b.join());
    eprintln!("(kayıt: {yol})");
    if st.success() { Ok(()) } else { Err(format!("ajan hata ile bitti ({st})")) }
}

/// sudo tarafından ajan kullanıcısı olarak çağrılır: ajan-calistir <dizin> (-i | "<görev>")
pub fn calistir(args: &[String]) -> Res {
    let [dizin, rest @ ..] = args else { return Err("kullanım: ajan-calistir <dizin> (-i | \"<görev>\")".into()) };
    let dizin = std::fs::canonicalize(dizin).map_err(|_| format!("Dizin yok: {dizin}"))?;
    let home = std::env::var("HOME").unwrap_or_default();
    let ds = dizin.to_string_lossy();
    let izinli = ds.starts_with("/srv/") || (!home.is_empty() && (ds == home || ds.starts_with(&format!("{home}/"))));
    if !izinli {
        return Err(format!("İzin verilmeyen dizin: {ds}"));
    }
    std::env::set_current_dir(&dizin).map_err(|e| e.to_string())?;
    if std::fs::File::open(KEY).is_err() {
        return Err("API anahtarı yok. Yönetici: sudo mh-anahtar".into());
    }
    let mut c = Command::new("/usr/local/bin/claude");
    match rest.first().map(String::as_str) {
        Some("-i") => {}
        Some(gorev) => {
            c.arg("-p").arg(gorev).stdin(Stdio::null());
        }
        None => return Err("görev ya da -i gerekli".into()),
    }
    Err(format!("claude çalıştırılamadı: {}", c.exec()))
}
