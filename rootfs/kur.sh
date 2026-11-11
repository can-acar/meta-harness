#!/bin/bash
# chroot içinde çalışır: grupları, dizinleri ve kullanicilar.conf'taki hesapları kurar.
set -euo pipefail
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

for g in insanlar ajanlar proje yonetici; do getent group $g >/dev/null || groupadd $g; done
install -d -m 2775 -g proje /srv/proje
install -d -m 2775 -g proje /srv/proje/.ciktilar
install -d -m 755 /etc/metahernes /var/log/metahernes
chmod 440 /etc/sudoers.d/metahernes
passwd -d root >/dev/null   # yalnızca seri konsoldan otomatik giriş (SSH'ta root kapalı)

# Paylaşılan klasör bağlandığında ajanların yazabilmesi için izinleri misafir tarafında ayarla
cat > /etc/metahernes/acilis.sh <<'EOF'
#!/bin/bash
if mountpoint -q /srv/proje; then
  chgrp -R proje /srv/proje 2>/dev/null; chmod -R g+rwX /srv/proje 2>/dev/null
  find /srv/proje -type d -exec chmod g+s {} + 2>/dev/null
  install -d -m 2775 -g proje /srv/proje/.ciktilar
fi
EOF
chmod 755 /etc/metahernes/acilis.sh

grep -Ev '^\s*(#|$)' /tmp/kullanicilar.conf > /tmp/kullanicilar.temiz
while read -r tip ad ek; do
  case "$tip" in
    insan) anahtar=""; [ -f "/tmp/mh-keys/$ad.pub" ] && anahtar="/tmp/mh-keys/$ad.pub"
           mh-kullanici insan "$ad" $anahtar $ek ;;
    ajan)  mh-kullanici ajan "$ad" "$ek" ;;
  esac
done < /tmp/kullanicilar.temiz

if [ -s /tmp/mh-ajan.key ]; then mh-anahtar /tmp/mh-ajan.key; fi
