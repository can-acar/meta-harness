#!/bin/bash
# GEÇİCİ yedek başlatıcı (QEMU). Asıl başlatıcı Rust ile yazılan `metahernes` ikilisidir;
# bu betik yalnızca kendi VMM'imizin henüz desteklemediği platformlar ve geliştirme/test içindir.
#   araclar/qemu/metahernes.sh [--cekirdek cikti/vmlinuz] [--imaj cikti/metahernes.img] [--bellek 2G] [--cpu 2] [--ssh 2222] [--paylas KLASOR]
# Konsoldan çıkış: içeride 'mh-kapat'  |  zorla: Ctrl-a sonra x
set -euo pipefail
D="$(cd "$(dirname "$0")/../../cikti" 2>/dev/null && pwd || pwd)"
BELLEK=2G; CPU=2; SSH=2222; PAYLAS=""; IMAJ="$D/metahernes.img"; CEKIRDEK="$D/vmlinuz"
while [ $# -gt 0 ]; do
  case "$1" in
    --bellek) BELLEK=$2; shift 2;; --cpu) CPU=$2; shift 2;;
    --ssh) SSH=$2; shift 2;;       --paylas) PAYLAS=$(cd "$2" && pwd); shift 2;;
    --imaj) IMAJ=$2; shift 2;;     --cekirdek) CEKIRDEK=$2; shift 2;;
    -h|--yardim) sed -n '2,4p' "$0"; exit 0;;
    *) echo "bilinmeyen seçenek: $1" >&2; exit 1;;
  esac
done
command -v qemu-system-x86_64 >/dev/null || {
  echo "QEMU gerekli:  Debian/Ubuntu: apt install qemu-system-x86 | macOS: brew install qemu | Windows: metahernes.ps1" >&2; exit 1; }

# Donanım hızlandırma: KVM (Linux) > HVF (macOS) > yazılım (TCG, yavaş)
HIZ=(-accel tcg); CPUTIP=max
if [ -w /dev/kvm ]; then HIZ=(-accel kvm); CPUTIP=host
elif [ "$(uname)" = Darwin ] && [ "$(uname -m)" = x86_64 ]; then HIZ=(-accel hvf); CPUTIP=host
else echo "uyarı: donanım hızlandırma yok, yazılım emülasyonu kullanılıyor (yavaş)." >&2; fi

EK=(); CMD="console=ttyS0 root=/dev/vda rw rootfstype=ext4 init=/sbin/mh-init quiet"
if [ -n "$PAYLAS" ]; then
  EK+=(-fsdev "local,id=p0,path=$PAYLAS,security_model=mapped-xattr" -device virtio-9p-pci,fsdev=p0,mount_tag=proje)
  CMD="$CMD mh_paylas=1"
fi

echo "metahernes açılıyor — SSH: ssh -p $SSH <kullanıcı>@localhost   (çıkış: mh-kapat, zorla: Ctrl-a x)"
exec qemu-system-x86_64 "${HIZ[@]}" -cpu "$CPUTIP" -M q35 -smp "$CPU" -m "$BELLEK" \
  -kernel "$CEKIRDEK" -append "$CMD" \
  -drive "file=$IMAJ,if=virtio,format=raw" \
  -netdev "user,id=n0,hostfwd=tcp:127.0.0.1:$SSH-:22" -device virtio-net-pci,netdev=n0 \
  -device virtio-rng-pci \
  ${EK[@]+"${EK[@]}"} -nographic -no-reboot
