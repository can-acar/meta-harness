//! mh — metahernes'in içinde çalışan tek ikili.
//! Adına göre davranır (sembolik bağlar): mh-init, mh-ajan, mh-kullanici, mh-anahtar, mh-kapat, ajan-calistir.
//! Doğrudan da çağrılabilir: `mh ajan liste`, `mh kullanici liste` ...

mod ajan;
mod init;
mod kullanici;
mod util;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut ad = util::prog_name();
    if std::process::id() == 1 {
        init::main();
    }
    if ad == "mh" {
        if args.is_empty() {
            println!("mh {} — komutlar: ajan, kullanici, anahtar, kapat, init", env!("CARGO_PKG_VERSION"));
            return;
        }
        ad = format!("mh-{}", args.remove(0));
    }
    let r = match ad.as_str() {
        "mh-init" => init::main(),
        "mh-ajan" => ajan::main(&args),
        "ajan-calistir" => ajan::calistir(&args),
        "mh-kullanici" => kullanici::main(&args),
        "mh-anahtar" => kullanici::anahtar(&args),
        "mh-kapat" => init::kapat(),
        other => Err(format!("bilinmeyen komut: {other}")),
    };
    if let Err(e) = r {
        eprintln!("{}: {e}", util::prog_name());
        std::process::exit(1);
    }
}
