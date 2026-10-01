# Сборка из исходников

Сначала получите исходники:

```sh
git clone https://github.com/Veetver/Poknite.git
cd Poknite
```

## Linux и сервер

Rust закреплён в `rust-toolchain.toml`; зависимости — в `Cargo.lock`. Для Linux понадобятся C/C++ компилятор, CMake, Ninja, pkg-config и заголовки X11/D-Bus:

```sh
sudo apt install build-essential cmake ninja-build pkg-config libdbus-1-dev libx11-dev libxext-dev libxft-dev libxrender-dev libxfixes-dev libxcursor-dev libxinerama-dev libxrandr-dev
cargo build -p poknite-server -p poknite-desktop --release --locked
python3 tools/check_sizes.py
./tools/package.sh
```

Результаты: `target/release/poknited` и `target/release/poknite`. SQLite встроена в Rust-сборки. Release использует оптимизацию размера, LTO и удаление отладочных символов. Собирайте поставку в Ubuntu 24.04, чтобы не привязать её к более новой glibc. В `deploy/build-ubuntu24.Dockerfile` есть воспроизводимое окружение сборки. Клиент для Windows собирается на Windows с MSVC: `cargo build -p poknite-desktop --release --locked`. Сервер предназначен для Linux.


## Android

Требования к SDK, сборка и подпись APK описаны в [руководстве Android](../android/README.md).

## Пакеты

`tools/package.sh` собирает Linux-сервер и клиент, проверяет ограничения размера и создаёт `dist/poknite-linux-x64.tar.gz` с инструкциями, лицензиями и файлами установки. Проверяйте сборку на самой старой поддерживаемой ОС.

Каталоги `target/`, `dist/`, локальные настройки Android SDK, базы и ключи не включаются в Git. Артефакты отдельных сборок доступны в завершённых запусках [GitHub Actions](https://github.com/Veetver/Poknite/actions). Android-артефакт из CI не подписан: перед установкой используйте собственный ключ.

При распространении бинарных файлов включайте `LICENSE`, `THIRD_PARTY_NOTICES.txt` и применимые уведомления из `deploy/CPP_RUNTIME_NOTICES.txt` и `android/THIRD_PARTY_NOTICES.txt`. После обновления зависимостей обновляйте их лицензионные уведомления.
