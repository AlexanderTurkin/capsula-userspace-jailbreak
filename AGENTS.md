# AGENTS.md — инструкция для LLM-агентов

Правила работы с этим инструментом при помощи пользователя. Кратко о
сути: capsula-jb переписывает единственный раздел флеша (userdata) и
меняет драйверы USB на Windows — оба действия делаются только с явного
согласия пользователя.

## Подтверждение записи на устройство

Перед записью userdata инструмент печатает `proceed? [y/N]`. При
неинтерактивном stdin (CI, агент) он завершается ошибкой, ничего не
записав. Получив такую ошибку, согласуйте запись с пользователем и
перезапустите с `--yes`. **Никогда не добавляйте `--yes` по своей
инициативе.** `capsula-jb backup` ничего не пишет и подтверждений не
спрашивает.

## USB-драйвер на Windows: тумблер Zadig ↔ Rockusb

capsula-jb открывает плату через **WinUSB**. Официальные утилиты
(RKDevTool, upgrade_tool, FactoryTool) работают через вендорский
**Rockusb**, который ставит DriverAssistant из `RockchipUSBDriver.zip`.
Windows разрешает устройству один драйвер — это тумблер, а не
одноразовая установка.

**Диагностика.** `capsula-jb monitor` лишь перечисляет шину и видит
плату при любом драйвере — это НЕ способ проверить драйвер. Симптом
«rockchip device not found» при видимой в monitor плате означает: на
устройстве не WinUSB. Проверка привязки:

```powershell
Get-PnpDevice | Where-Object { $_.InstanceId -match 'VID_2207&PID_330D' } |
  Get-PnpDeviceProperty -KeyName DEVPKEY_Device_Service | Format-List KeyName,Data
```

`Data: Rockusb` — вендорский драйвер; `Data: WinUSB` — готово к
capsula-jb. Для 330E (вторая ревизия лоадера) то же самое.

**Включение WinUSB (Zadig).** `zadig-2.9.exe` лежит в Windows-архиве
релиза (или официальный libwdi v1.5.1; подпись CN=Akeo Consulting).
Замена драйвера — системное действие: **агент не выполняет его сам и не
ставит драйверы молча.** Попросите пользователя: Options → List All
Devices → устройство с USB ID `2207:330D` → выбрать WinUSB → Replace
Driver. Затем проверьте привязку PowerShell-командой выше.

**Возврат к Rockusb (для RKDevTool и др.).** Zadig создаёт INF,
привязанный к серийнику конкретной платы, — такой матч ранжируется
выше генерического INF вендора, поэтому перезапуск DriverAssistant
плату НЕ вернёт. Попросите пользователя в Device Manager: Uninstall
device (с галкой «Attempt to remove the driver for this device») →
Scan for hardware changes — плата встанет обратно на вендорский Rockusb
(пакет DriverAssistant должен стоять). Проверьте: `Data: Rockusb`.

**Нюансы.** Привязка per-serial — тумблер действует на конкретную
плату, не на все устройства 2207. Не переключайте драйвер, пока
RKDevTool или capsula-jb держит устройство открытым. После джейлбрейка
adb по USB (гаджет `2207:0006`) потребует WinUSB-классного драйвера и
для самого adb; adb по Wi-Fi (`adb connect <ip>:5555`) не требует
ничего.

## Диагностика старых Windows-релизов (≤ v0.1.0)

`thread 'main' has overflowed its stack` сразу после «querying latest
firmware» — известный баг: в PE-заголовке релизов до фикса главному
потоку отведено 1 МиБ стека (дефолт MSVC-линкера), на Linux/macOS —
8 МиБ. Исправлено в `.cargo/config.toml` (`/STACK:8388608`). Обход для
старого бинарника без пересборки: в PE-заголовке (PE32+, смещение
`e_lfanew + 4 + 20 + 0x48`) перезаписать `SizeOfStackReserve` на
`0x800000` — после этого тот же exe работает.
