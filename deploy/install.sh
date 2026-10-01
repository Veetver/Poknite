#!/usr/bin/env bash
set -euo pipefail
if [[ $EUID != 0 ]]; then
    printf 'Запустите установку через sudo.\n' >&2
    exit 1
fi
if [[ $# != 2 ]]; then
    printf 'Использование: install.sh /путь/к/cert.pem /путь/к/key.pem\n' >&2
    exit 1
fi
task_source="$(cd "$(dirname "$0")" && pwd)"
task_binary="$task_source/../poknited"
if [[ ! -x "$task_binary" ]]; then
    task_binary="$task_source/../target/release/poknited"
fi
[[ -x "$task_binary" ]] || { printf 'Сначала соберите poknited.\n' >&2; exit 1; }
openssl x509 -in "$1" -noout >/dev/null
openssl x509 -in "$1" -checkend 0 -noout >/dev/null
openssl pkey -in "$2" -noout >/dev/null
task_check="$(mktemp -d)"
trap 'rm -rf "$task_check"' EXIT
openssl x509 -in "$1" -pubkey -noout > "$task_check/certificate.pub"
openssl pkey -in "$2" -pubout > "$task_check/key.pub"
cmp -s "$task_check/certificate.pub" "$task_check/key.pub" || { printf 'Ключ не соответствует сертификату.\n' >&2; exit 1; }
getent passwd poknite >/dev/null || useradd --system --home-dir /var/lib/poknite --shell /usr/sbin/nologin poknite
install -d -m 0750 -o root -g poknite /etc/poknite
install -d -m 0700 -o poknite -g poknite /var/lib/poknite /var/log/poknite
install -m 0755 "$task_binary" /usr/local/bin/poknited
if [[ ! -f /etc/poknite/poknite.toml ]]; then
    install -m 0640 -o root -g poknite "$task_source/poknite.toml" /etc/poknite/poknite.toml
fi
install -m 0640 -o root -g poknite "$1" /etc/poknite/cert.pem
install -m 0640 -o root -g poknite "$2" /etc/poknite/key.pem
install -m 0644 "$task_source/poknite.service" /etc/systemd/system/poknite.service
install -m 0644 "$task_source/poknite.logrotate" /etc/logrotate.d/poknite
runuser -u poknite -- /usr/local/bin/poknited --config /etc/poknite/poknite.toml init
systemctl daemon-reload
systemctl enable --now poknite
printf 'Сервер установлен. Добавьте пользователя и получите приглашение по README.\n'
