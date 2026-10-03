# Автоматические релизы

Выпуск запускается при отправке тега `vMAJOR.MINOR.PATCH`, например `v0.2.0`.
Теги вида `v0.2.0-rc.1`, `v0.2.0-alpha.1` и `v0.2.0-beta.1` создают pre-release.
Коммит тега должен входить в историю основной ветки репозитория.

Workflow `Release Poknite` повторяет проверки Linux, Windows и Android на коммите
тега, упаковывает их сборки, подписывает APK и создаёт GitHub attestations для
файлов поставки. Релиз публикуется после загрузки всех файлов. При ошибке сборки,
подписи или проверки публикация не выполняется; ошибка загрузки оставляет черновик.
Повторный запуск может завершить черновик, но не меняет уже опубликованный релиз.

## Настройка репозитория

Этот workflow рассчитан на **публичный репозиторий**: для него GitHub attestations
доступны на бесплатных тарифах. Приватные репозитории требуют GitHub Enterprise Cloud.
Текущая конфигурация останавливает выпуск в приватном репозитории.
Перед сменой видимости проверьте, что вся история может быть опубликована.

В Settings → Secrets and variables → Actions добавьте:

| Тип | Имя | Значение |
| --- | --- | --- |
| Secret | `ANDROID_KEYSTORE_BASE64` | Файл постоянного ключа PKCS12/JKS, закодированный base64 одной строкой |
| Secret | `ANDROID_KEYSTORE_PASSWORD` | Пароль хранилища ключа |
| Secret | `ANDROID_KEY_ALIAS` | Имя ключа, например `poknite` |
| Secret | `ANDROID_KEY_PASSWORD` | Пароль самого ключа; для PKCS12 обычно совпадает с паролем хранилища |
| Variable | `ANDROID_CERT_SHA256` | SHA-256 отпечаток сертификата Android: 64 шестнадцатеричных символа, допустимы двоеточия |

Релиз использует автоматический `GITHUB_TOKEN` с правами публикации и OIDC.
Отдельный PAT или ключ подписи архивов не нужен. Обычные проверки и сборки из PR
не получают ключ Android. Внешние Actions в задании подписи закреплены по коммитам.

Ключи и пароли не добавляйте в Git. Храните резервную копию ключа и пароля в
защищённом месте вне рабочей копии. Все обновления APK подписываются тем же ключом.
Утрата ключа лишает возможности обновить существующую установку обычным способом.
Создание собственного Android-ключа и подпись APK не требуют платного сертификата.

Команды создания ключа и получения подписанного APK описаны в
[руководстве Android](../android/README.md#подпись-и-установка).
Получить отпечаток сертификата можно командой `keytool -list -v -keystore /secure/location/poknite.p12 -alias poknite`.
Пароли она запрашивает интерактивно. Значение secret с ключом подготовьте через
`base64 -w0 /secure/location/poknite.p12 > /secure/location/poknite.base64`;
загрузите файл через `gh secret set ANDROID_KEYSTORE_BASE64 < /secure/location/poknite.base64`
и удалите временный base64-файл после успешной загрузки.

## Выпуск версии

1. Установите одинаковую версию в `[workspace.package].version` файла `Cargo.toml`
   и `versionName` в `android/app/build.gradle.kts`.
2. Для каждого следующего выпуска увеличивайте Android `versionCode` относительно
   предыдущего APK, включая предварительные версии. Без этого обновление не установится.
3. Обновите `Cargo.lock`, если изменились версии Rust-пакетов, и отправьте изменения
   вместе с workflow в основную ветку. Дождитесь успешных проверок.
4. Проверьте версию и отправьте тег на нужный коммит:

   ```sh
   python3 tools/release_metadata.py v0.2.0
   git tag -a v0.2.0 -m 'Poknite 0.2.0'
   git push origin v0.2.0
   ```

После успешного `Release Poknite` файлы появятся на странице Releases:

- `poknite-<версия>-linux-x64.tar.gz` — сервер, клиент, установка, документация и лицензии.
- `poknite-<версия>-windows-x64.zip` — клиент, документация и лицензии.
- `poknite-<версия>-android.apk` — подписанный APK для установки.
- `poknite-<версия>-android.zip` — тот же APK вместе с инструкциями и лицензиями.
- `SHA256SUMS` — контрольные суммы поставки.
- `attestation.sigstore.json` — подписи GitHub для файлов и списка контрольных сумм.

При распространении APK также предоставляйте лицензии из Android-архива.
Windows `.exe` не подписан Authenticode: подпись GitHub подтверждает происхождение
архива, но не заменяет сертификат издателя в Windows.

Если выпуск остановился, исправьте причину и используйте **Re-run all jobs** в Actions.
Для ручного запуска workflow выберите существующий тег версии, а не ветку.

## Проверка скачанного релиза

Установите актуальную GitHub CLI с командой `gh attestation`.
Для Linux-архива версии 0.2.0:

```sh
gh attestation verify poknite-0.2.0-linux-x64.tar.gz \
  --repo Veetver/Poknite \
  --bundle attestation.sigstore.json \
  --cert-identity 'https://github.com/Veetver/Poknite/.github/workflows/release.yml@refs/tags/v0.2.0'
```

Аналогично проверяются Windows-архив и APK. Если скачаны все файлы поставки,
проверьте подписанный `SHA256SUMS` той же командой, заменив имя проверяемого файла,
затем выполните `sha256sum --check SHA256SUMS`.
Для ограничения проверки конкретным коммитом добавьте `--source-digest <SHA-коммита>`.

Проверка APK средствами Android SDK:

```sh
"$ANDROID_HOME/build-tools/36.0.0/apksigner" verify --verbose --print-certs poknite-0.2.0-android.apk
```

Сравните SHA-256 сертификата с доверенным значением `ANDROID_CERT_SHA256`.

Документация инструментов:
[GitHub attestations](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations),
[проверка через GitHub CLI](https://cli.github.com/manual/gh_attestation_verify),
[apksigner](https://developer.android.com/tools/apksigner).
