# AGENTS.md — инструкция для LLM-агентов: разработка

Инструкция по прошивке колонки для конечного пользователя — отдельный
файл `resources/packaging/AGENTS.md`; он же кладётся в релизные
архивы. Этот файл — о работе над репозиторием.

## Сборка и тесты

- `cargo build --release` — нужен Rust и C-компилятор (lwext4
  собирается через `cc`).
- `cargo test --workspace` — тесты userdata гоняют настоящий lwext4 на
  синтетических образах и сериализованы общим мьютексом (у lwext4
  глобальное состояние); протокольные тесты работают на MockTransport,
  без железа. Единственный тест с реальными данными скачивает OTA с
  VK CDN в момент запуска и выключен по умолчанию (см. tests/README.md).
- Бинарные образцы прошивок в репозитории не хранятся и не
  распространяются.

## Ограничения, которые нельзя ломать

- `.cargo/config.toml` резервирует 8 МиБ стека главного потока на
  Windows-таргетах (`/STACK:8388608`). Парсинг OTA-манифеста рекурсивен
  и требует больше дефолтного для MSVC-линкера 1 МиБ — конфиг не
  удалять и не занижать.

## adbd

Оверлейный `adbd` собирается герметично в контейнере (Debian bullseye,
кросс-тулчейн, стоковый ABI glibc ≤ 2.33 / libcrypto 1.1):

```sh
docker build -t capsula-adbd docker/adbd
docker run --rm -v docker/adbd:/work capsula-adbd /work/build.sh
```

## Релизы

Тег `v*` запускает `.github/workflows/release.yml`: сборка adbd в
Docker → матрица из 5 платформ → публикация GitHub Release.

- В Windows-архив вкладываются `RockchipUSBDriver.zip` (вендорский
  драйвер Rockusb для официальных утилит) и `zadig-2.9.exe` (перевод
  платы на WinUSB для capsula-jb). Оба пинуются SHA256 в env
  воркфлоу: при обновлении upstream обновляйте пин, при изменении
  суммы сборка падает, а не катит непроверенный бинарник.
- В каждый архив кладутся `resources/packaging/QUICKSTART.txt`,
  `resources/packaging/AGENTS.md` (инструкция для агентов по прошивке)
  и, для Windows, `resources/packaging/windows-DRIVER-SETUP.txt`.
- Изменения упаковки проверяйте локально: `TARGET=... ARCHIVE=...
  TAG=vX.Y.Z bash .github/scripts/package.sh` после сборки.
