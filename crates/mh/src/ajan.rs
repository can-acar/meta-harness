//! mh-ajan (insanlar için) ve ajan-calistir (sudo ile ajan hesabında çalışan uç).

use crate::util::{group, passwd, user_by_uid, Res};
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

pub const CALISTIR: &str = "/usr/local/lib/metahernes/ajan-calistir";
pub const KEY: &str = "/etc/metahernes/ajan.key";
/// Ajan evinde, ajana özel kimlik bilgilerinin dizini (araç adına göre dosyalar).
pub const OZEL_ANAHTAR_DIZINI: &str = ".metahernes/anahtarlar";
/// Antigravity CLI'ın OAuth belirteci (ajan evine göre).
pub const AGY_BELIRTEC: &str = ".gemini/antigravity-cli/antigravity-oauth-token";

const YARDIM: &str = "\
mh-ajan liste
mh-ajan <ad> \"<görev>\" [-d DIZIN]     tek seferlik (claude -p | codex exec | agy -p), çıktı + kayıt
mh-ajan <ad> -i [-d DIZIN]            ajanın hesabında etkileşimli oturum
mh-ajan <ad> giris                    ajanın kendi hesabıyla oturum aç (codex: cihaz kodu, agy: OAuth)

Her ajan ayrı bir Linux hesabıdır; kimlik bilgileri ajanın evinde durur. Aynı anda birden çok
ajan (ve aynı ajanla birden çok görev) çalıştırılabilir.";

/// Ajanın içinde çalışan kodlama aracı.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arac {
    Claude,
    Codex,
    Agy,
}

impl Arac {
    pub const HEPSI: [Arac; 3] = [Arac::Claude, Arac::Codex, Arac::Agy];

    pub fn ad(self) -> &'static str {
        match self {
            Arac::Claude => "claude",
            Arac::Codex => "codex",
            Arac::Agy => "agy",
        }
    }

    pub fn coz(s: &str) -> Res<Arac> {
        Arac::HEPSI.into_iter().find(|a| a.ad() == s).ok_or_else(|| format!("bilinmeyen araç: {s} (claude | codex | agy)"))
    }

    pub fn ikili(self) -> String {
        format!("/usr/local/bin/{}", self.ad())
    }
}

pub fn rol(ad: &str) -> String {
    std::fs::read_to_string(format!("/etc/metahernes/ajanlar/{ad}.rol")).map(|s| s.trim().to_string()).unwrap_or_else(|_| "?".into())
}

/// Ajanın aracı; dosya yoksa (eski kurulumlar) claude.
pub fn arac(ad: &str) -> Arac {
    std::fs::read_to_string(format!("/etc/metahernes/ajanlar/{ad}.arac")).ok().and_then(|s| Arac::coz(s.trim()).ok()).unwrap_or(Arac::Claude)
}

pub fn main(args: &[String]) -> Res {
    match args.first().map(String::as_str) {
        None | Some("-h") | Some("--yardim") => {
            println!("{YARDIM}");
            Ok(())
        }
        Some("liste") => {
            println!("{:<22} {:<14} ARAÇ", "AJAN", "ROL");
            for u in group("ajanlar").map(|g| g.1).unwrap_or_default() {
                let ad = u.strip_prefix("ajan-").unwrap_or(&u);
                println!("{ad:<22} {:<14} {}", rol(ad), arac(ad).ad());
            }
            Ok(())
        }
        Some(ad) => {
            let kullanici = format!("ajan-{ad}");
            if passwd(&kullanici).is_none() {
                return Err(format!("Böyle bir ajan yok: {ad} (mh-ajan liste)"));
            }
            let mut dizin = "/srv/proje".to_string();
            let mut kip = None;
            let mut gorev = Vec::new();
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "-d" => dizin = it.next().ok_or("-d için dizin gerekli")?.clone(),
                    "-i" => kip = Some("-i"),
                    "giris" if gorev.is_empty() => kip = Some("--giris"),
                    _ => gorev.push(a.clone()),
                }
            }
            let mut sudo = Command::new("sudo");
            sudo.args(["-u", &kullanici, "-H", CALISTIR, &dizin]);
            if let Some(k) = kip {
                let e = sudo.arg(k).exec();
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
    // Süreç numarası: aynı saniyede başlayan eşzamanlı görevler birbirinin kaydını ezmesin
    let yol = format!("{kd}/{zaman}-{ad}-{}.log", std::process::id());
    let mut f = std::fs::File::create(&yol).map_err(|e| format!("{yol}: {e}"))?;
    let isteyen = user_by_uid(unsafe { libc::getuid() }).unwrap_or_default();
    let _ = writeln!(f, "# ajan: {ad}  araç: {}  dizin: {dizin}  isteyen: {isteyen}\n# görev: {gorev}\n", arac(ad).ad());
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

/// sudo tarafından ajan kullanıcısı olarak çağrılır: ajan-calistir <dizin> (-i | --giris | "<görev>")
/// Araç, çağıranın sözüne değil /etc/metahernes/ajanlar/<ad>.arac dosyasına göre seçilir.
pub fn calistir(args: &[String]) -> Res {
    let [dizin, rest @ ..] = args else { return Err("kullanım: ajan-calistir <dizin> (-i | --giris | \"<görev>\")".into()) };
    let dizin = std::fs::canonicalize(dizin).map_err(|_| format!("Dizin yok: {dizin}"))?;
    let home = std::env::var("HOME").unwrap_or_default();
    let ds = dizin.to_string_lossy();
    let izinli = ds.starts_with("/srv/") || (!home.is_empty() && (ds == home || ds.starts_with(&format!("{home}/"))));
    if !izinli {
        return Err(format!("İzin verilmeyen dizin: {ds}"));
    }
    std::env::set_current_dir(&dizin).map_err(|e| e.to_string())?;
    let ben = user_by_uid(unsafe { libc::getuid() }).unwrap_or_default();
    let ad = ben.strip_prefix("ajan-").ok_or("ajan-calistir yalnızca ajan hesaplarında çalışır")?;
    let arac = arac(ad);
    let kip = match rest.first().map(String::as_str) {
        Some("-i") => Kip::Etkilesimli,
        Some("--giris") => Kip::Giris,
        Some(g) => Kip::Gorev(g),
        None => return Err("görev, -i ya da --giris gerekli".into()),
    };
    let mut c = komut(arac, &home, &kip)?;
    Err(format!("{} çalıştırılamadı: {}", arac.ad(), c.exec()))
}

pub enum Kip<'a> {
    Etkilesimli,
    Giris,
    Gorev(&'a str),
}

/// Ajana özel anahtar dosyası (varsa, boş değilse) içeriği.
fn ozel_anahtar(home: &str, arac: Arac) -> Option<String> {
    let s = std::fs::read_to_string(format!("{home}/{OZEL_ANAHTAR_DIZINI}/{}", arac.ad())).ok()?;
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Araca göre çalıştırılacak komutu kurar; kimlik bilgisi yoksa açıklayıcı hata verir.
pub fn komut(arac: Arac, home: &str, kip: &Kip) -> Res<Command> {
    let mut c = Command::new(arac.ikili());
    let giris_ipucu = |ne: &str| {
        let ad = std::env::var("USER").unwrap_or_default();
        let ad = ad.strip_prefix("ajan-").unwrap_or(&ad).to_string();
        format!("{ne} yok. Ajanın kendi hesabıyla: mh-ajan {ad} giris  (ya da yönetici: sudo mh-anahtar --ajan {ad})")
    };
    match arac {
        Arac::Claude => {
            // apiKeyHelper önce ajana özel anahtarı, yoksa ortak anahtarı okur (roller/*/settings.json)
            let ortak = std::fs::File::open(KEY).is_ok();
            if !matches!(kip, Kip::Giris) && !ortak && ozel_anahtar(home, arac).is_none() {
                return Err("API anahtarı yok. Yönetici: sudo mh-anahtar  ya da  sudo mh-anahtar --ajan <ad>".into());
            }
            match kip {
                Kip::Etkilesimli | Kip::Giris => {}
                Kip::Gorev(g) => {
                    c.arg("-p").arg(g).stdin(Stdio::null());
                }
            }
        }
        Arac::Codex => {
            // Öncelik: ajana özel API anahtarı (ortam değişkeni), yoksa `codex login` ile ~/.codex/auth.json
            if let Some(k) = ozel_anahtar(home, arac) {
                c.env("OPENAI_API_KEY", &k).env("CODEX_API_KEY", &k);
            } else if !matches!(kip, Kip::Giris) && !std::path::Path::new(&format!("{home}/.codex/auth.json")).exists() {
                return Err(giris_ipucu("Codex kimliği"));
            }
            match kip {
                Kip::Etkilesimli => {}
                Kip::Giris => {
                    c.args(["login", "--device-auth"]);
                }
                Kip::Gorev(g) => {
                    c.args(["exec", "--skip-git-repo-check", g]).stdin(Stdio::null());
                }
            }
        }
        Arac::Agy => {
            // Antigravity CLI API anahtarı kabul etmiyor; her ajan kendi OAuth girişini yapar
            if matches!(kip, Kip::Gorev(_)) && !std::path::Path::new(&format!("{home}/{AGY_BELIRTEC}")).exists() {
                return Err(giris_ipucu("Antigravity oturumu"));
            }
            match kip {
                Kip::Etkilesimli | Kip::Giris => {}
                Kip::Gorev(g) => {
                    // Rolün ek bayrakları (ör. --dangerously-skip-permissions); --print en sonda olmalı
                    let bayraklar = std::fs::read_to_string(format!("{home}/.metahernes/agy-bayraklar")).unwrap_or_default();
                    c.args(bayraklar.split_whitespace());
                    // stdin kapalı: agy -p TTY olmayan çıktıda stdin beklemesin
                    c.arg("--print").arg(g).stdin(Stdio::null());
                }
            }
        }
    }
    Ok(c)
}

#[cfg(test)]
mod testler {
    use super::*;

    fn argv(c: &Command) -> Vec<String> {
        std::iter::once(c.get_program()).chain(c.get_args()).map(|s| s.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn arac_adlari() {
        for a in Arac::HEPSI {
            assert_eq!(Arac::coz(a.ad()), Ok(a));
        }
        assert!(Arac::coz("gemini").is_err());
    }

    #[test]
    fn codex_ozel_anahtar_ve_komut() {
        let ev = std::env::temp_dir().join(format!("mh-test-codex-{}", std::process::id()));
        let ev_s = ev.to_string_lossy().into_owned();
        // Kimlik yoksa görev reddedilir, giriş serbesttir
        assert!(komut(Arac::Codex, &ev_s, &Kip::Gorev("x")).is_err());
        let c = komut(Arac::Codex, &ev_s, &Kip::Giris).unwrap();
        assert_eq!(argv(&c), ["/usr/local/bin/codex", "login", "--device-auth"]);

        std::fs::create_dir_all(ev.join(OZEL_ANAHTAR_DIZINI)).unwrap();
        std::fs::write(ev.join(OZEL_ANAHTAR_DIZINI).join("codex"), "sk-deneme\n").unwrap();
        let c = komut(Arac::Codex, &ev_s, &Kip::Gorev("testleri yaz")).unwrap();
        assert_eq!(argv(&c), ["/usr/local/bin/codex", "exec", "--skip-git-repo-check", "testleri yaz"]);
        let env: Vec<_> = c.get_envs().map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned()))).collect();
        assert!(env.contains(&("OPENAI_API_KEY".into(), Some("sk-deneme".into()))));
        let _ = std::fs::remove_dir_all(&ev);
    }

    #[test]
    fn agy_bayraklar_ve_print_sonda() {
        let ev = std::env::temp_dir().join(format!("mh-test-agy-{}", std::process::id()));
        let ev_s = ev.to_string_lossy().into_owned();
        assert!(komut(Arac::Agy, &ev_s, &Kip::Gorev("x")).is_err());
        let b = ev.join(AGY_BELIRTEC);
        std::fs::create_dir_all(b.parent().unwrap()).unwrap();
        std::fs::write(&b, "{}").unwrap();
        std::fs::create_dir_all(ev.join(".metahernes")).unwrap();
        std::fs::write(ev.join(".metahernes/agy-bayraklar"), "--dangerously-skip-permissions\n").unwrap();
        let c = komut(Arac::Agy, &ev_s, &Kip::Gorev("incele")).unwrap();
        assert_eq!(argv(&c), ["/usr/local/bin/agy", "--dangerously-skip-permissions", "--print", "incele"]);
        let _ = std::fs::remove_dir_all(&ev);
    }
}
