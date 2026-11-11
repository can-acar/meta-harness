# metahernes

Kendi çekirdeğiyle açılan minimal Linux ve onu çalıştıran, **sıfırdan Rust ile yazılmış** sanal makine
yöneticisi (VMM). QEMU yok, Docker yok. İçinde Claude Code CLI, **insan** ve **ajan** kullanıcılarıyla çalışır.

```
 Ana makine ── metahernes (Rust VMM, ~700 KB)
               ├─ hipervizör arka ucu:  KVM (Linux) ✔ │ HVF (macOS) ✗ │ WHP (Windows) ✗
               ├─ ortak aygıt modeli:   16550A seri, i8042, virtio-mmio blok + rng + ağ (OS'tan bağımsız)
               ├─ kullanıcı alanı NAT:  smoltcp + std::net — root/TAP gerekmez, SSH yönlendirme
               └─ önyükleme:            bzImage 64-bit giriş, sayfa tabloları, GDT, MP tablosu
                    │
                    ▼
               vmlinuz (kendi çekirdeğimiz, ~2.8 MB, modülsüz)
               └─ metahernes.img (ext4)
                  ├─ mh  (Rust, statik, ~1.4 MB) = PID 1 init + mh-ajan + mh-kullanici + mh-anahtar + mh-kapat
                  ├─ insanlar  ── SSH anahtarıyla girer, kendi `claude` hesabı
                  ├─ ajanlar   ── ajan-gelistirici / ajan-inceleyici / ajan-operasyon (ayrı Linux kullanıcıları)
                  └─ /srv/proje ── ortak çalışma alanı
```

## Durum (bu sürüm)

| Parça | Durum |
|---|---|
| `metahernes` VMM, Linux/KVM | ✔ Kendi çekirdeğini açıyor: virtio blok disk, ext4 kök, seri konsol (girdi/çıktı), virtio-rng, kvm-clock, temiz kapanış, Ctrl-a x |
| `mh` (init + araçlar) | ✔ Bash betiklerinin yerini aldı: bağlama noktaları, ioctl ile ağ, servis gözetimi (geri çekilmeli yeniden başlatma), zombi toplama, kapanış; kullanıcı/ajan/anahtar yönetimi |
| VMM'de ağ (virtio-net + kullanıcı alanı NAT) | ✔ TCP/UDP/DNS, root gerektirmez; ana makine `127.0.0.1:2222` → misafir SSH; ek `--port ANA:MISAFIR`. Test: indirme/yükleme bütünlüğü (SHA-256), Claude Code API'ye ulaşıyor. ICMP (ping) yok |
| SMP (birden çok vCPU) | ⚠ İkinci işlemci iç içe test ortamında açılmadı; varsayılan 1 vCPU |
| macOS (Hypervisor.framework), Windows (WHP) | ✗ Planlandı; aygıt modeli ve önyükleme kodu bunlarla ortak kullanılacak şekilde ayrı tutuldu |

## Derleme (Debian/Ubuntu, root)

```bash
sudo apt install build-essential flex bison bc libelf-dev libssl-dev mmdebstrap e2fsprogs curl xz-utils
# Rust: https://rustup.rs
cp ~/.ssh/id_ed25519.pub keys/can.pub            # kullanicilar.conf'taki insan adıyla eşleşir
sudo ./build.sh                                  # cekirdek + rust + rootfs + imaj -> cikti/
```
Adımlar ayrı ayrı da çalışır: `./build.sh cekirdek | rust | rootfs | imaj`.
Ayarlar: `KERNEL_VERSION` (varsayılan 6.12), `KERNEL_SRC`, `MH_SUITE`/`MH_MIRROR`, `CLAUDE_VERSION`, `MH_ARACLAR`, `CODEX_VERSION`, `MH_DISK`, `ANTHROPIC_API_KEY`.

## Çalıştırma

```bash
cd cikti
./metahernes bilgi                               # hipervizör desteğini gösterir
./metahernes calistir                            # vmlinuz + metahernes.img, konsol bu terminalde
./metahernes calistir --bellek 1024 --port 8080:80   # ek port yönlendirme
ssh -p 2222 can@localhost                        # başka bir terminalden insan kullanıcı olarak gir
```
Konsoldan çıkış: içeride `mh-kapat`, ya da Ctrl-a x. `/dev/kvm` erişimi gerekir (`kvm` grubu).
Misafir ağı: 10.0.2.15, ağ geçidi 10.0.2.2 (= ana makinenin 127.0.0.1'i), DNS 10.0.2.3 (ana makinenin DNS'ine iletilir).

Henüz kendi VMM'imizin desteklemediği platformlar için geçici yedek: `araclar/qemu/metahernes.sh`.

## Kullanıcılar ve Claude Code

| Komut | Ne yapar |
|---|---|
| `claude` | İnsan kullanıcı kendi hesabıyla çalışır |
| `mh-ajan liste` | Ajanlar ve rolleri |
| `mh-ajan gelistirici "testleri yaz"` | Görevi ajanın Linux hesabında `claude -p` ile yaptırır; kayıt `~/.metahernes/kayitlar/` |
| `mh-ajan inceleyici -i` | Ajan hesabında etkileşimli oturum |
| `sudo mh-kullanici insan ali ali.pub [--yonetici]` / `ajan <ad> <rol>` / `sil` / `liste` | Hesap yönetimi |
| `sudo mh-anahtar` | Ajanların ortak API anahtarı (yalnızca `ajanlar` grubu okur) |
| `sudo mh-kullanici ajan codex-1 gelistirici codex` | Ajanı Claude Code yerine Codex (`codex`) ya da Antigravity (`agy`) ile çalıştırır |
| `mh-ajan codex-1 giris` | Ajanın **kendi** hesabıyla giriş (codex: cihaz kodu, agy: OAuth); kimlik ajanın evinde kalır |
| `sudo mh-anahtar --ajan codex-1 [dosya]` | Yalnızca o ajana özel anahtar (claude/codex: API anahtarı, agy: OAuth belirteç dosyası) |

### Codex ve Antigravity ajanları, ayrı kimlikler

Her ajan ayrı bir Linux hesabıdır, bu yüzden her ajanın kimlik bilgisi kendi evinde durur
(`~/.codex/auth.json`, `~/.gemini/antigravity-cli/antigravity-oauth-token`, `~/.metahernes/anahtarlar/<araç>`).
Aynı araçtan farklı hesaplarla birden çok örnek açmak için birden çok ajan tanımlayın
(`kullanicilar.conf`: `ajan codex-1 gelistirici codex`, `ajan codex-2 gelistirici codex`, `ajan agy-1 inceleyici agy`)
ve hepsini aynı anda çalıştırın. Araçlar imaja `MH_ARACLAR=claude,codex,agy sudo ./build.sh` ile konur.

Sınırlar: Codex ve agy için rolün `settings.json` araç kuralları geçerli değildir; yetki sınırı Linux izinleri
(`proje` grubu) ve rolün `codex.toml` / `agy-bayraklar` dosyalarıdır. Antigravity API anahtarı kabul etmez,
her agy ajanı bir kez `mh-ajan <ad> giris` ile (ya da başka makineden kopyalanan belirteçle) oturum açar.

Roller `/etc/metahernes/roller/` altında: `CLAUDE.md` (talimatlar), `settings.json` (Claude Code izinleri), `proje-yazma`.
İzinler iki katmanlı: Claude Code kuralları **ve** Linux izinleri (ör. `inceleyici` `/srv/proje`ye çekirdek düzeyinde yazamaz).

## Kod düzeni

```
crates/vmm/        metahernes-vmm kütüphanesi
  mem.rs           misafir belleği
  boot.rs          Linux önyükleme protokolü, sayfa tabloları, GDT, MP tablosu
  devices.rs       veriyolu, 16550A, i8042
  virtio.rs        sanal kuyruk, virtio-mmio, blok, rng
  net.rs           virtio-net + kullanıcı alanı NAT (TCP vekili, UDP/DNS, port yönlendirme)
  kvm.rs           Linux/KVM arka ucu
crates/metahernes/ ana makine CLI'si
crates/mh/         misafir ikilisi (init, ajan, kullanici, anahtar, kapat)
kernel/            çekirdek yapılandırma parçası
rootfs/            kök dosya sistemi katmanı ve kurulum betiği
araclar/qemu/      geçici QEMU yedek başlatıcıları
```

Geliştirici bayrakları: `MH_DEBUG=1` (quiet kapalı, triple-fault yazmaçları, bilinmeyen G/Ç portları), `MH_TRACE=1` (tek adım izleme).

## Yol haritası
1. SMP: AP başlatmanın gerçek KVM'de doğrulanması/düzeltilmesi.
2. macOS arka ucu (Hypervisor.framework; Apple Silicon için arm64 çekirdek ve önyükleme).
3. Windows arka ucu (Windows Hypervisor Platform).
4. virtio-fs/9p ile ana makineden klasör paylaşımı (`--paylas`).
5. Ağ: olay tabanlı bekleme (şu an 1 ms yoklama), ICMP, IPv6.
