#!/bin/bash
# metahernes imaj derleyici — Linux (Debian/Ubuntu) bir derleme makinesinde root olarak çalıştırın.
#   sudo ./build.sh                 # çekirdek + rootfs + disk imajı -> cikti/
#   sudo ./build.sh cekirdek|rust|rootfs|imaj
# Ayarlar (ortam değişkeni):
#   KERNEL_VERSION=6.12        kernel.org'dan indirilecek sürüm
#   KERNEL_SRC=/yol            hazır çekirdek kaynak dizini ya da .tar.(xz|gz|bz2) (indirme yapılmaz)
#   MH_SUITE=noble  MH_MIRROR=http://archive.ubuntu.com/ubuntu   (Debian: bookworm + http://deb.debian.org/debian)
#   CLAUDE_VERSION=latest      Claude Code sürümü (npm)
#   MH_ARACLAR=claude          imaja konacak ajan araçları: claude,codex,agy (virgülle)
#   CODEX_VERSION=latest       OpenAI Codex CLI sürümü (GitHub sürümü, ör. 0.60.0)
#   MH_DISK=4G                 disk imajı boyutu
#   ANTHROPIC_API_KEY=...      verilirse ajanların ortak anahtarı imaja yazılır (sonradan: mh-anahtar)
set -euo pipefail
cd "$(dirname "$(readlink -f "$0")")"
KOK=$PWD; IS=$KOK/is; CIKTI=$KOK/cikti
KERNEL_VERSION=${KERNEL_VERSION:-6.12}
MH_SUITE=${MH_SUITE:-noble}
MH_MIRROR=${MH_MIRROR:-http://archive.ubuntu.com/ubuntu}
CLAUDE_VERSION=${CLAUDE_VERSION:-latest}
MH_ARACLAR=${MH_ARACLAR:-claude}
CODEX_VERSION=${CODEX_VERSION:-latest}
MH_DISK=${MH_DISK:-4G}
PAKETLER="bash,ca-certificates,openssh-server,git,curl,sudo,iproute2,iputils-ping,procps,less,nano,ripgrep,util-linux,passwd"
mkdir -p "$IS" "$CIKTI"
adim() { echo -e "\n\033[1;36m==> $*\033[0m"; }

bagimliliklar() {
  local eksik=()
  for b in gcc make flex bison bc mmdebstrap mke2fs curl xz; do command -v $b >/dev/null || eksik+=($b); done
  if [ ${#eksik[@]} -gt 0 ]; then
    echo "Eksik araçlar: ${eksik[*]}"
    echo "Kurulum: apt-get install build-essential flex bison bc libelf-dev libssl-dev mmdebstrap e2fsprogs curl xz-utils"
    exit 1
  fi
}

cekirdek() {
  adim "Çekirdek"
  local src
  if [ -n "${KERNEL_SRC:-}" ] && [ -d "$KERNEL_SRC" ]; then src=$KERNEL_SRC
  else
    local tar=${KERNEL_SRC:-$IS/linux-$KERNEL_VERSION.tar.xz}
    if [ ! -f "$tar" ]; then
      curl -fL --progress-bar -o "$tar" "https://cdn.kernel.org/pub/linux/kernel/v${KERNEL_VERSION%%.*}.x/linux-$KERNEL_VERSION.tar.xz"
    fi
    src=$IS/linux-src
    if [ ! -f "$src/Makefile" ]; then rm -rf "$src"; mkdir -p "$src"; tar xf "$tar" -C "$src" --strip-components=1; fi
  fi
  ( cd "$src"
    make -s tinyconfig
    ./scripts/kconfig/merge_config.sh -m .config "$KOK/kernel/metahernes.config" >/dev/null
    make -s olddefconfig
    make -s -j"$(nproc)" bzImage )
  cp "$src/arch/x86/boot/bzImage" "$CIKTI/vmlinuz"
  echo "çekirdek: $(du -h "$CIKTI/vmlinuz" | cut -f1)  ($(cd "$src" && make -s kernelrelease))"
}

rootfs() {
  adim "Kök dosya sistemi ($MH_SUITE)"
  local R=$IS/rootfs
  rm -rf "$R"
  local bilesen="main"; [[ "$MH_MIRROR" == *ubuntu* ]] && bilesen="main,universe"
  mmdebstrap --mode=root --variant=minbase --components="$bilesen" --include="$PAKETLER" \
    --dpkgopt='path-exclude=/usr/share/man/*' --dpkgopt='path-exclude=/usr/share/doc/*' \
    --dpkgopt='path-include=/usr/share/doc/*/copyright' \
    --dpkgopt='path-exclude=/usr/share/locale/*' --dpkgopt='path-include=/usr/share/locale/locale.alias' \
    "$MH_SUITE" "$R" "$MH_MIRROR"

  adim "Claude Code ($CLAUDE_VERSION)"
  local paket=@anthropic-ai/claude-code-linux-x64 surum=$CLAUDE_VERSION
  if [ "$surum" = latest ]; then
    surum=$(curl -fsS "https://registry.npmjs.org/$paket/latest" | grep -o '"version":"[^"]*"' | head -1 | cut -d'"' -f4)
  fi
  local tgz=$IS/claude-code-$surum.tgz
  [ -f "$tgz" ] || curl -fL --progress-bar -o "$tgz" "https://registry.npmjs.org/$paket/-/claude-code-linux-x64-$surum.tgz"
  mkdir -p "$R/opt/claude-code"
  tar xzf "$tgz" -C "$R/opt/claude-code" --strip-components=1 package/claude
  ln -sf /opt/claude-code/claude "$R/usr/local/bin/claude"
  echo "$surum" > "$R/opt/claude-code/VERSION"

  [[ ",$MH_ARACLAR," == *,codex,* ]] && codex_kur "$R"
  [[ ",$MH_ARACLAR," == *,agy,* ]] && agy_kur "$R"

  adim "mh (Rust) misafir ikilisi"
  local MH=$CIKTI/mh
  [ -x "$MH" ] || mh_derle
  install -D -m755 "$MH" "$R/usr/local/bin/mh"
  install -d "$R/usr/local/sbin" "$R/usr/local/lib/metahernes"
  ln -sf /usr/local/bin/mh "$R/usr/sbin/mh-init"
  ln -sf mh "$R/usr/local/bin/mh-ajan"
  for k in mh-kullanici mh-anahtar mh-kapat; do ln -sf /usr/local/bin/mh "$R/usr/local/sbin/$k"; done
  # sudo hedefi sabit bir dosya olmalı (sembolik bağ değil)
  install -m755 "$MH" "$R/usr/local/lib/metahernes/ajan-calistir"

  adim "metahernes katmanı + kullanıcılar"
  cp -a --no-preserve=ownership "$KOK/rootfs/overlay/." "$R/"
  mkdir -p "$R/tmp/mh-keys"
  cp "$KOK/kullanicilar.conf" "$R/tmp/kullanicilar.conf"
  cp "$KOK"/keys/*.pub "$R/tmp/mh-keys/" 2>/dev/null || true
  [ -n "${ANTHROPIC_API_KEY:-}" ] && (umask 077; printf '%s' "$ANTHROPIC_API_KEY" > "$R/tmp/mh-ajan.key")
  cp "$KOK/rootfs/kur.sh" "$R/tmp/kur.sh"
  chroot "$R" /bin/bash /tmp/kur.sh
  rm -rf "$R/tmp/"* "$R/var/cache/apt/"*.bin "$R/var/lib/apt/lists/"* "$R/etc/ssh/ssh_host_"*
  echo "rootfs: $(du -sh "$R" | cut -f1)"
}

codex_kur() {
  local R=$1
  adim "OpenAI Codex CLI ($CODEX_VERSION)"
  # Statik musl ikilisi: Node gerekmez
  local varlik=codex-x86_64-unknown-linux-musl url
  if [ "$CODEX_VERSION" = latest ]; then url=https://github.com/openai/codex/releases/latest/download/$varlik.tar.gz
  else url=https://github.com/openai/codex/releases/download/rust-v$CODEX_VERSION/$varlik.tar.gz; fi
  local tgz=$IS/codex-$CODEX_VERSION.tar.gz
  [ -f "$tgz" ] || curl -fL --progress-bar -o "$tgz" "$url"
  mkdir -p "$R/opt/codex"
  tar xzf "$tgz" -C "$R/opt/codex"
  mv "$R/opt/codex/$varlik" "$R/opt/codex/codex"
  ln -sf /opt/codex/codex "$R/usr/local/bin/codex"
}

agy_kur() {
  local R=$1
  adim "Google Antigravity CLI (agy)"
  # Resmî kurulum betiği ikiliyi $HOME/.local/bin'e koyar; geçici bir ev dizini kullanıp imaja taşıyoruz
  local ev=$IS/agy-ev
  if [ ! -e "$ev/.local/bin/agy" ]; then
    rm -rf "$ev"; mkdir -p "$ev"
    curl -fsSL https://antigravity.google/cli/install.sh | HOME=$ev SHELL=/bin/bash bash
  fi
  mkdir -p "$R/opt/agy"
  cp -L "$ev/.local/bin/agy" "$R/opt/agy/agy"
  chmod 755 "$R/opt/agy/agy"
  ln -sf /opt/agy/agy "$R/usr/local/bin/agy"
}

mh_derle() {
  adim "Rust ikilileri (mh: statik misafir ikilisi, metahernes: VMM)"
  command -v cargo >/dev/null || { echo "Rust gerekli: https://rustup.rs" >&2; exit 1; }
  ( cd "$KOK"
    RUSTFLAGS="-C target-feature=+crt-static" cargo build --release -p mh --target x86_64-unknown-linux-gnu
    cargo build --release -p metahernes )
  install -m755 "$KOK/target/x86_64-unknown-linux-gnu/release/mh" "$CIKTI/mh"
  install -m755 "$KOK/target/release/metahernes" "$CIKTI/metahernes"
}

imaj() {
  adim "Disk imajı ($MH_DISK)"
  rm -f "$CIKTI/metahernes.img"
  mke2fs -q -t ext4 -L metahernes -d "$IS/rootfs" "$CIKTI/metahernes.img" "$MH_DISK"
  echo "hazır: cd cikti && ./metahernes calistir"
}

[ "$(id -u)" = 0 ] || { echo "root olarak çalıştırın: sudo $0" >&2; exit 1; }
bagimliliklar
case "${1:-hepsi}" in
  cekirdek) cekirdek;; rust) mh_derle;; rootfs) rootfs;; imaj) imaj;;
  hepsi) cekirdek; mh_derle; rootfs; imaj;;
  *) sed -n '2,14p' "$0"; exit 1;;
esac
