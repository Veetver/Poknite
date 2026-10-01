#!/usr/bin/env bash
set -euo pipefail
task_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$task_root"
if [[ ${POKNITE_SKIP_BUILD:-0} != 1 ]]; then
    cargo build -p poknite-server -p poknite-desktop --release --locked
fi
task_release="${POKNITE_RELEASE_DIR:-$task_root/target/release}"
python3 tools/check_sizes.py --release-dir "$task_release"
[[ -x "$task_release/poknited" && -x "$task_release/poknite" ]]
[[ $(stat -c %s "$task_release/poknited") -le 10485760 ]]
[[ $(stat -c %s "$task_release/poknite") -le 12582912 ]]
mkdir -p dist/linux/poknite/deploy
install -m 0755 "$task_release/poknited" "$task_release/poknite" dist/linux/poknite/
cp deploy/poknite.toml deploy/poknite.service deploy/poknite.logrotate deploy/install.sh dist/linux/poknite/deploy/
cp README.md CONTRIBUTING.md LICENSE THIRD_PARTY_NOTICES.txt dist/linux/poknite/
cp -r docs dist/linux/poknite/
mkdir -p dist/linux/poknite/android
cp android/README.md dist/linux/poknite/android/
tar -C dist/linux -czf dist/poknite-linux-x64.tar.gz poknite
sha256sum dist/poknite-linux-x64.tar.gz > dist/poknite-linux-x64.tar.gz.sha256
