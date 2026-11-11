# metahernes — Claude Code için proje belleği

Bu dosya, metahernes'in claude.ai sohbetinde (2026-11-03/04) baştan bu yana nasıl geliştiğini, verilen kararları ve
şu anki durumu içerir. Claude Code bu depoda çalışırken önce burayı okusun.

## Amaç (kullanıcının istekleri)
- Kendi çekirdeği olan, bash çalıştıran, internete çıkan, **başka bir işletim sisteminin üzerinde** çalışan minimal Linux.
- Docker **yok**, ama konteyner kadar küçük ve kendi kendine yeten.
- İçinde **Claude Code CLI** farklı kullanıcılarla çalışacak: hem **gerçek kişiler** (SSH ile kendi hesapları) hem **ajan hesapları** (rol bazlı).
- **Windows, Linux ve macOS**'ta çalışmalı; **QEMU olmadan**. Gerekirse Rust ile yeniden yazılacak.
- Kullanıcı kararı: **sıfırdan kendi VMM'imiz** (KVM/HVF/WHP'ye doğrudan, libkrun/Firecracker değil).
- Kullanıcı kararı: sistemin içindeki parçalar (init, ajan/kullanıcı araçları) **tek bir Rust ikilisi `mh`**.
- Dil: kullanıcıyla Türkçe konuşulur; kod içi adlar, yorumlar, CLI mesajları ve belgeler Türkçe.

## Geçmiş (sürümler)
1. **v0 (bash + QEMU):** tinyconfig tabanlı çekirdek, Ubuntu minbase rootfs (mmdebstrap), Claude Code yerel ikilisi
   (npm `@anthropic-ai/claude-code-linux-x64`), bash init ve araçlar, QEMU başlatıcı. Kullanıcı "QEMU olmadan olur mu?" diye sordu.
2. **v0.1 (Rust):** `metahernes` VMM (KVM) kendi çekirdeğimizi açıyor; `mh` bash betiklerinin yerini aldı.
3. **v0.2:** VMM'e virtio-net + root gerektirmeyen kullanıcı alanı NAT (smoltcp) eklendi; Claude Code VMM içinde API'ye ulaşıyor.

## Mimari
```
Ana makine ── metahernes (crates/metahernes, CLI)  ──>  metahernes-vmm (crates/vmm)
                ├─ hipervizör arayüzü: hv.rs (Hypervisor / Vm / Vcpu trait'leri, ortak VcpuExit)
                ├─ arka uçlar: kvm.rs (Linux) ✔ | whp.rs (Windows: yalnızca algılama) | HVF (macOS) ✗
                ├─ OS'tan bağımsız: vm.rs (aygıt kurulumu + vCPU döngüsü), mem.rs (mmap/VirtualAlloc),
                │  boot.rs (+ ortak uzun kip yazmaç durumu), devices.rs, virtio.rs, net.rs, console.rs
                └─ misafir: vmlinuz (kendi çekirdeğimiz) + metahernes.img (ext4)
                      └─ mh (crates/mh, statik): PID 1 init + mh-ajan + mh-kullanici + mh-anahtar + mh-kapat + ajan-calistir
```
- **Yeni arka uç eklemek:** `hv::Hypervisor/Vm/Vcpu`'yu uygula, `lib.rs::run`'da platforma göre seç. Uzun kip
  yazmaçları `boot::long_mode_state`'ten alınır. WHP'de `WinHvPlatform.dll` çalışma anında yüklenir (özellik kapalıysa
  `whp::durum()` açıklama + `Enable-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform` gösterir).
  CI ana makine kodunu Windows/macOS hedefleri için de clippy'den geçirir (yalnızca derleme; çalıştırma denenmedi).
- **Önyükleme (boot.rs):** bzImage 64-bit giriş (yükleme `pref_address`, giriş +0x200), zero page 0x7000, cmdline 0x20000,
  sayfa tabloları 0x9000 (ilk 4 GiB, 2 MiB sayfalar), GDT 0x500, MP tablosu 0x9fc00 (ACPI tablosu yok). RAM ≤ 3 GiB.
- **Aygıtlar:** 16550A seri 0x3f8/IRQ4, i8042 0x60 (yalnızca 0xFE sıfırlama → VMM kapanır), virtio-mmio v2 0xd0000000 + n·0x1000,
  IRQ 5+n: blok(0), rng(1), ağ(2). Çekirdeğe `virtio_mmio.device=4K@ADR:IRQ` ile bildirilir.
- **Kapanış:** misafir `reboot=k` ile yeniden başlatır → i8042 → VMM çıkar. QEMU'da `-no-reboot` aynı işi görür.
- **Konsol:** stdin → seri; Ctrl-a x çık, Ctrl-a Ctrl-a Ctrl-a gönder. Terminal ham kipte.
- **Ağ (net.rs):** misafir 10.0.2.15, ağ geçidi 10.0.2.2 (= ana makine 127.0.0.1), DNS 10.0.2.3 (ana makinenin resolv.conf'una).
  TCP: SYN görülünce smoltcp'de o hedefe dinleyen soket + ana makinede TcpStream (ayrı iş parçacığında connect). UDP: elle
  çözülür/kurulur (UDP sağlama toplamı 0). Port yönlendirme: 127.0.0.1:P → misafir :Q (varsayılan 2222→22). Yalnızca std::net.
- **Misafir init (mh/init.rs):** proc/sys/dev/devpts/shm/run/tmp bağlar, ağı **ioctl** ile kurar (aygıtı olan ilk arayüz;
  `sit0` gibi sanal arayüzler atlanır), `mh_paylas=1` varsa 9p, `/etc/metahernes/acilis.sh`, sshd + agetty gözetimi
  (yoksa atla, çöküşte üstel geri çekilme), zombi toplama, SIGUSR2/TERM → kapanış.

## Kullanıcı modeli (rootfs/, mh/)
- Gruplar: `insanlar`, `ajanlar`, `proje`, `yonetici`. Ortak alan `/srv/proje` (root:proje, 2775).
- İnsanlar: `mh-kullanici insan <ad> [anahtar.pub] [--yonetici]`, şifre `*` (kilitli değil, yalnızca SSH anahtarı).
  sshd: `AllowGroups insanlar`, root ve şifreyle giriş kapalı, `UsePAM no`.
- Ajanlar: `ajan-<ad>`, ev `/var/lib/ajanlar/<ad>`, rol `/etc/metahernes/roller/<rol>/{CLAUDE.md,settings.json,proje-yazma}`
  (gelistirici, inceleyici, operasyon). `inceleyici` `proje` grubunda değil → çekirdek düzeyinde salt okunur.
- `mh-ajan <ad> "<görev>"` → `sudo -u ajan-<ad> -H /usr/local/lib/metahernes/ajan-calistir <dizin> <görev>` → `claude -p`
  (stdin /dev/null); kayıt `~/.metahernes/kayitlar/`. `-i` etkileşimli. ajan-calistir yalnızca /srv/* ve kendi evini kabul eder.
- Ortak API anahtarı `/etc/metahernes/ajan.key` (root:ajanlar 0640), roller `apiKeyHelper: cat …` ile okur.
- `~/.claude.json` önceden doldurulur (onboarding + /srv/proje güven sorusu atlanır).
- sudoers: `%insanlar ALL=(%ajanlar) NOPASSWD: ajan-calistir`, `%yonetici ALL=(ALL:ALL) NOPASSWD: ALL`.

## Derleme ve test
```bash
sudo ./build.sh                 # cekirdek + rust + rootfs + imaj -> cikti/
sudo ./build.sh cekirdek|rust|rootfs|imaj
cargo test                      # 11 birim testi (boot, seri, i8042, virtio-blk, NAT TCP/UDP uçtan uca)
cd cikti && ./metahernes calistir [--bellek 2048] [--cpu 1] [--ssh 2222] [--port A:M] [--ag-yok] [--ek "..."]
```
- `mh` statik: `RUSTFLAGS="-C target-feature=+crt-static" cargo build --release -p mh --target x86_64-unknown-linux-gnu`
  (NSS kullanılmaz; /etc/passwd ve /etc/group doğrudan okunur).
- Çekirdek: `make tinyconfig` + `kernel/metahernes.config` (merge_config.sh). Varsayılan indirme kernel.org 6.12; testler
  Ubuntu `linux-source-6.8.0` ile yapıldı (`KERNEL_SRC=...`).
- Hata ayıklama: `MH_DEBUG=1` (quiet kalkar, triple-fault yazmaçları, bilinmeyen G/Ç portları), `MH_TRACE=1` (tek adım izi),
  `MH_TEST_CODE=1` (uzun kip denetimi: seri porta 'X').
- **Geliştirme ortamında KVM yoktu.** Yöntem: QEMU TCG (`-cpu max`, iç içe SVM) içinde metahernes açılır, orada `/dev/kvm`
  görünür ve kendi VMM'imiz içeride çalıştırılır (yavaş ama gerçek KVM yolu). Ortamın HTTPS'i TLS denetleyen vekil
  üzerinden geçtiği için curl/Claude Code "self-signed" hatası verdi; `NODE_EXTRA_CA_CERTS` ile API'ye ulaşıldı
  ("Invalid API key" = bağlantı tamam). Gerçek anahtarla ve gerçek KVM'li bir makinede **hiç** denenmedi.

## Doğrulanmış (iç içe testte)
Kendi çekirdeğiyle açılış, virtio-blk + ext4, seri girdi/çıktı, diske yazma, kvm-clock, `mh` init + sshd,
SSH yönlendirmesi, DNS, HTTP indirme (SHA-256 tuttu), SCP yükleme (SHA-256 tuttu), Claude Code → API, `mh-kapat` ile
temiz kapanış, Ctrl-a x, kullanıcı/ajan/anahtar izinleri, inceleyici salt okunur.

## Bilinen sorunlar / dersler
- **SMP:** 2 vCPU'da "CPU1 failed to report alive state". İç içe TCG yavaşlığı mı, AP kurulumu mu belli değil.
  Varsayılan `--cpu 1`. (Tüm vCPU'lara aynı uzun kip yazmaçları + LINT0/1 yazılıyor; MP tablosu 2 işlemciyi gösteriyor.)
- **5 seviyeli sayfalama kapalı** (`CONFIG_X86_5LEVEL=n`): açıcı, 1 MiB altı tramboline atlayıp üçlü hataya düşüyordu.
- Çekirdek parçasında KVM/KVM_AMD/KVM_INTEL açık (iç içe test için), VIRTIO_MMIO, PARAVIRT/KVM_GUEST açık.
- usrmerge: overlay'e `sbin/` koyma (`/sbin` sembolik bağ); init `/usr/sbin/mh-init` → `/usr/local/bin/mh`.
- `passwd -l` + `UsePAM no` SSH'ı kilitler → `usermod -p '*'` kullanılır.
- chroot'ta `/dev/fd` yok → kur.sh'ta süreç ikamesi (`<(...)`) kullanma.
- `pkill -f <desen>` aynı komut satırını içeren kabuğu da öldürür; süreçleri `ps -eo comm` ile bul.
- Ağ iş parçacığı boşta 1 ms yokluyor; ICMP ve IPv6 yok. `--paylas` (9p) yalnızca QEMU yedeğinde.

## Yol haritası (kullanıcı henüz seçmedi)
1. macOS arka ucu (Hypervisor.framework; Apple Silicon için arm64 çekirdek + önyükleme). Test için Mac gerekir.
2. Windows arka ucu (Windows Hypervisor Platform). `mem.rs` şu an `libc::mmap` kullanıyor → Windows için VirtualAlloc.
3. Klasör paylaşımı (virtio-fs ya da 9p) — `--paylas`.
4. SMP'nin gerçek KVM'de doğrulanması/düzeltilmesi.
5. Ağ: olay tabanlı bekleme, ICMP, IPv6.

## Dosya düzeni
```
Cargo.toml                 çalışma alanı (vmm, metahernes, mh)
crates/vmm/src/            mem, boot, devices, virtio, net, console, kvm, lib
crates/metahernes/src/     ana makine CLI (clap): calistir, bilgi
crates/mh/src/             main (argv[0] ile dağıtım), init, ajan, kullanici, util
kernel/metahernes.config   çekirdek parçası
rootfs/kur.sh              chroot kurulumu (gruplar, kullanicilar.conf)
rootfs/overlay/            /etc dosyaları, roller, sshd/sudoers
kullanicilar.conf          imaja işlenecek insanlar/ajanlar
keys/<ad>.pub              insanların SSH anahtarları (build sırasında)
araclar/qemu/              GEÇİCİ QEMU yedek başlatıcıları (yalnızca desteklenmeyen platformlar/test)
build.sh                   tüm derleme
```
