<div align="center">

# Haltline

**Отладка STM32 и ARM Cortex-M в редакторе [Zed](https://zed.dev)**

[![CI](https://github.com/ghorik78/Haltline/actions/workflows/ci.yml/badge.svg)](https://github.com/ghorik78/Haltline/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/ghorik78/Haltline)](https://github.com/ghorik78/Haltline/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Platforms](https://img.shields.io/badge/platforms-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey)

[English](README.md) · **Русский**

</div>

Haltline добавляет в Zed отладчик прошивок, похожий на Cortex-Debug из VS Code. Он прошивает микроконтроллер, останавливается на `main` и показывает регистры ядра, регистры периферии из SVD-файлов, переменные и память. Есть дизассемблер, точки останова и watchpoints, а при HardFault отладчик объясняет его причину. Работает с OpenOCD, ST-LINK GDB server, st-util, pyOCD, SEGGER J-Link и QEMU.

## Содержание

- [Возможности](#возможности)
- [Как это устроено](#как-это-устроено)
- [Требования](#требования)
- [Установка](#установка)
- [Обновление](#обновление)
- [Быстрый старт](#быстрый-старт)
- [Примеры настроек](#примеры-настроек)
- [Задачи RTOS (FreeRTOS, Zephyr, NuttX)](#задачи-rtos-freertos-zephyr-nuttx)
- [Справочник настроек](#справочник-настроек)
- [Работа с отладчиком](#работа-с-отладчиком)
- [Решение проблем](#решение-проблем)
- [Ограничения](#ограничения)
- [Разработка](#разработка)
- [Лицензия](#лицензия)

## Возможности

| Что | Подробности |
|---|---|
| **GDB-серверы** | OpenOCD, ST-LINK_gdbserver (STM32CubeIDE / STM32CubeCLT), st-util, pyOCD, SEGGER J-Link, QEMU или любой уже запущенный сервер |
| **Прошивка** | Прошивает при запуске и, по желанию, при перезапуске. Показывает этапы (стирание, запись, программирование, проверка) с прогрессом и может проверить флеш после записи |
| **Регистры ядра** | r0–r12, SP, LR, PC; xPSR с разбором флагов и названием исключения; MSP/PSP, PRIMASK, BASEPRI, FAULTMASK, CONTROL; регистры FPU |
| **Периферия (SVD)** | Все блоки, регистры и битовые поля с перечислениями. Регистры и отдельные поля можно менять (`MODE = Output`). Регистры, чтение которых имеет побочный эффект, автоматически не читаются |
| **Точки останова** | Обычные, условные, по числу срабатываний, logpoints, на функции, на адресе инструкции, data breakpoints (watchpoints на чтение, запись или доступ) |
| **Разбор отказов** | Остановка при входе в обработчик отказа и объяснение причины: биты CFSR/HFSR, адрес ошибки и строка исходника, где она произошла |
| **Дизассемблер** | Шаг по инструкциям в ассемблерном виде, исходные строки в комментариях. Точки останова можно ставить на инструкции |
| **Стек вызовов** | В том числе через границу прерывания (`<exception entry>`) |
| **Переменные** | Локальные, статические и глобальные; структуры, массивы (большие делятся на диапазоны), указатели; watch, подсказки при наведении, изменение значений, hex |
| **Память** | Просмотр и запись в окне Memory в Zed |
| **RTOS** | Задачи FreeRTOS, Zephyr и NuttX показываются как потоки (через OpenOCD, pyOCD или J-Link), см. [Задачи RTOS](#задачи-rtos-freertos-zephyr-nuttx) |
| **Semihosting** | Вывод `printf` в консоль отладки |

## Как это устроено

```
Zed ──DAP──▶ haltline-dap ──GDB/MI──▶ arm-none-eabi-gdb ──▶ GDB-сервер ──▶ программатор ──▶ МК
             (Haltline)                                      (OpenOCD, J-Link, …)
```

Haltline состоит из двух частей:

* **Расширение Zed** регистрирует отладчик `Haltline` и схему его настроек, а также находит или скачивает адаптер.
* **`haltline-dap`** — небольшая программа, реализующая Debug Adapter Protocol. Она запускает GDB-сервер и GDB и переводит запросы Zed в команды GDB/MI.

## Требования

* **Zed** с поддержкой отладчика (проверено на Zed 1.26).
* **GDB для ARM**: `arm-none-eabi-gdb` (Arm GNU Toolchain, STM32CubeCLT) или `gdb-multiarch`. Haltline ищет его в `PATH` и в стандартных папках STM32CubeCLT и Arm toolchain.
* **GDB-сервер для вашего программатора**: OpenOCD (ST-LINK, J-Link, CMSIS-DAP), ST-LINK_gdbserver, st-util, pyOCD или SEGGER J-Link. Чтобы попробовать Haltline без платы, подойдёт QEMU (`qemu-system-arm`).
* **ELF-файл** прошивки, собранный с отладочной информацией (`-g`).

Готовые сборки адаптера: Linux (x86_64, aarch64), macOS (Intel, Apple Silicon), Windows (x64, ARM64).

В Ubuntu/Debian открытые инструменты ставятся так:

```sh
sudo apt install gdb-multiarch openocd
```

## Установка

Haltline пока нет в каталоге расширений Zed, поэтому он ставится из релиза на GitHub.

### Скрипт установки (рекомендуется)

Скрипт ставит расширение и адаптер той же версии в папку данных Zed.

**Linux / macOS**

```sh
curl -fsSL https://github.com/ghorik78/Haltline/releases/latest/download/install.sh | bash
```

**Windows (PowerShell)**

```powershell
irm https://github.com/ghorik78/Haltline/releases/latest/download/install.ps1 | iex
```

Затем перезапустите Zed или выполните `zed: reload extensions` из палитры команд.

Параметры скрипта:

| Linux / macOS | Windows | Назначение |
|---|---|---|
| `--version v0.1.1` | `-Version v0.1.1` | Поставить определённый релиз вместо последнего |
| `--no-adapter` | `-NoAdapter` | Поставить только расширение; адаптер оно скачает при первом запуске |
| `--from DIR` | `-From DIR` | Взять уже скачанные архивы (установка без интернета) |
| `--zed-data DIR` | `-ZedData DIR` | Папка данных Zed, если она не на стандартном месте |

Чтобы передать параметры скрипту, запущенному через конвейер, используйте `bash -s --`, например:

```sh
curl -fsSL https://github.com/ghorik78/Haltline/releases/latest/download/install.sh | bash -s -- --version v0.1.1
```

В каждом релизе есть файл `SHA256SUMS` для проверки скачанных архивов.

### Установка вручную

1. Скачайте `haltline-zed-extension.tar.gz` (или `.zip`) со страницы [Releases](https://github.com/ghorik78/Haltline/releases/latest).
2. Распакуйте так, чтобы `extension.toml` оказался здесь:

   | ОС | Путь |
   |---|---|
   | Linux | `~/.local/share/zed/extensions/installed/haltline/extension.toml` |
   | Linux (Flatpak) | `~/.var/app/dev.zed.Zed/data/zed/extensions/installed/haltline/extension.toml` |
   | macOS | `~/Library/Application Support/Zed/extensions/installed/haltline/extension.toml` |
   | Windows | `%LOCALAPPDATA%\Zed\extensions\installed\haltline\extension.toml` |

3. Перезапустите Zed. Адаптер для вашей платформы скачается из того же релиза при первом запуске отладки.

### Из исходников

```sh
git clone https://github.com/ghorik78/Haltline && cd Haltline
cargo install --path adapter      # ставит haltline-dap в ~/.cargo/bin
```

В Zed откройте **Extensions → Install Dev Extension** и выберите папку репозитория. Zed сам соберёт расширение, для этого нужен Rust, установленный через rustup.

### Какой адаптер используется

Расширение берёт первый найденный `haltline-dap` в таком порядке:

1. Настройка Zed `"dap": { "Haltline": { "binary": "/path/to/haltline-dap" } }`
2. `"adapterPath"` в конфигурации отладки
3. `haltline-dap` в `PATH`
4. Адаптер, скачанный из релиза GitHub той же версии, что и расширение

## Обновление

Расширения, установленные не из каталога Zed, сами не обновляются.

* **Установлено скриптом или вручную:** запустите скрипт установки ещё раз. Он заменит расширение и поставит адаптер той же версии. Затем перезапустите Zed. Адаптеры старых версий удаляются, когда расширение в следующий раз скачивает адаптер.
* **Определённая версия** (в том числе для отката): запустите скрипт с `--version vX.Y.Z` (`-Version` в Windows).
* **Dev extension:** выполните `git pull`, нажмите **Rebuild** рядом с Haltline на странице Extensions и обновите адаптер командой `cargo install --path adapter --force`.
* **Свой адаптер** (`dap.Haltline.binary`, `adapterPath` или в `PATH`): обновите его сами, иначе Zed продолжит использовать старый.

Версию адаптера показывает `haltline-dap --version`.

**Удаление:** удалите папки `extensions/installed/haltline` и `extensions/work/haltline` из папки данных Zed.

## Быстрый старт

1. Создайте в проекте файл `.zed/debug.json`:

   ```json
   [
     {
       "label": "Debug firmware",
       "adapter": "Haltline",
       "request": "launch",
       "executable": "build/firmware.elf",
       "device": "STM32F407VG",
       "svdFile": "STM32F407.svd"
     }
   ]
   ```

2. Подключите плату через ST-LINK.
3. В Zed выполните `debugger: start` (или нажмите **+** в панели отладки) и выберите **Debug firmware**.

Haltline запустит OpenOCD с `interface/stlink.cfg` и файлом цели для указанного МК (`target/stm32f4x.cfg`). Затем он прошьёт ELF, сбросит МК и остановится на `main`.

SVD-файлы для STM32 есть в STM32CubeIDE и STM32CubeCLT (папка `STMicroelectronics_CMSIS_SVD`), в CMSIS-паках ST и в репозитории [cmsis-svd-data](https://github.com/cmsis-svd/cmsis-svd-data).

## Примеры настроек

Все примеры добавляются в массив в `.zed/debug.json`. Zed подсказывает параметры и их описания прямо при редактировании файла.

<details>
<summary><b>OpenOCD со своими конфигами, RTOS и сборкой перед отладкой</b></summary>

```json
{
  "label": "STM32G4 + FreeRTOS",
  "adapter": "Haltline",
  "request": "launch",
  "executable": "build/app.elf",
  "configFiles": ["interface/stlink.cfg", "target/stm32g4x.cfg"],
  "svdFile": "STM32G474.svd",
  "rtos": "FreeRTOS",
  "build": { "command": "make", "args": ["-j8"] }
}
```
</details>

<details>
<summary><b>ST-LINK GDB server (STM32CubeCLT)</b></summary>

```json
{
  "label": "ST-LINK GDB server",
  "adapter": "Haltline",
  "request": "launch",
  "serverType": "stlink",
  "executable": "build/app.elf",
  "svdFile": "STM32H743.svd"
}
```

STM32CubeProgrammer находится автоматически в STM32CubeCLT; если нет, укажите `stm32cubeprogrammer`.
</details>

<details>
<summary><b>SEGGER J-Link</b></summary>

```json
{
  "label": "J-Link",
  "adapter": "Haltline",
  "request": "launch",
  "serverType": "jlink",
  "device": "STM32F407VG",
  "executable": "build/app.elf",
  "speed": 4000
}
```
</details>

<details>
<summary><b>pyOCD и st-util</b></summary>

```json
{
  "label": "pyOCD",
  "adapter": "Haltline",
  "request": "launch",
  "serverType": "pyocd",
  "device": "STM32F103C8",
  "executable": "build/app.elf"
},
{
  "label": "st-util",
  "adapter": "Haltline",
  "request": "launch",
  "serverType": "stutil",
  "executable": "build/app.elf"
}
```
</details>

<details>
<summary><b>Подключение к работающей плате без сброса</b></summary>

```json
{
  "label": "Attach",
  "adapter": "Haltline",
  "request": "attach",
  "executable": "build/app.elf",
  "device": "STM32F407VG"
}
```

Добавьте `"continueOnAttach": true`, чтобы программа продолжала работать после подключения.
</details>

<details>
<summary><b>Уже запущенный GDB-сервер</b></summary>

```json
{
  "label": "External server",
  "adapter": "Haltline",
  "request": "launch",
  "serverType": "external",
  "gdbTarget": "localhost:3333",
  "executable": "build/app.elf"
}
```
</details>

<details>
<summary><b>Проверка прошивки, перепрошивка при Restart, semihosting</b></summary>

```json
{
  "label": "Flash, verify, printf",
  "adapter": "Haltline",
  "request": "launch",
  "executable": "build/app.elf",
  "device": "STM32F407VG",
  "verifyAfterLoad": true,
  "reloadOnRestart": true,
  "semihosting": true
}
```

С `reloadOnRestart` кнопка **Restart** заново прошивает пересобранную прошивку.
</details>

<details>
<summary><b>Свои команды GDB</b></summary>

```json
{
  "label": "Custom commands",
  "adapter": "Haltline",
  "request": "launch",
  "executable": "build/app.elf",
  "device": "STM32F407VG",
  "preLaunchCommands": ["monitor adapter speed 1800"],
  "postLaunchCommands": ["set var g_debug_mode = 1"],
  "resetCommands": ["monitor reset init"]
}
```
</details>

<details>
<summary><b>QEMU (без платы)</b></summary>

```json
{
  "label": "QEMU",
  "adapter": "Haltline",
  "request": "launch",
  "serverType": "qemu",
  "machine": "netduinoplus2",
  "executable": "build/app.elf"
}
```

Готовый пример лежит в [`adapter/tests/firmware`](adapter/tests/firmware). Откройте эту папку в Zed и запустите **Test firmware (QEMU, STM32F405)**. Нужны `arm-none-eabi-gcc`, `make`, `qemu-system-arm` и GDB для ARM.
</details>

## Задачи RTOS (FreeRTOS, Zephyr, NuttX)

Haltline может показывать задачи RTOS как потоки. Выберите задачу в списке потоков панели отладки, и вы увидите её стек вызовов и переменные. Список задач составляет GDB-сервер, поэтому эта функция включается в настройках сервера. Кроме того, прошивка должна экспортировать символы, которые сервер ищет.

| GDB-сервер | Как включить | FreeRTOS | Zephyr | NuttX |
|---|---|---|---|---|
| OpenOCD | параметр `"rtos"` | ✅ | ✅ | ✅ только OpenOCD, собранный из git |
| pyOCD | включено по умолчанию | ✅ | ✅ | — |
| J-Link | плагин RTOS в `serverArgs` | ✅ плагин входит в J-Link | ✅ плагин входит в J-Link | ✅ плагин из исходников NuttX |
| ST-LINK GDB server, st-util, QEMU | не поддерживается | — | — | — |

Общее:

* Имена RTOS в OpenOCD чувствительны к регистру: `FreeRTOS`, `Zephyr`, `nuttx`. С `"auto"` OpenOCD перебирает все известные ему RTOS.
* Задачи появляются после запуска планировщика. До этого, например при остановке на `main`, поток один.
* Если задачи не появились, посмотрите сообщения GDB-сервера в консоли отладки. OpenOCD пишет, какого символа не хватает.

### FreeRTOS

```json
{
  "label": "FreeRTOS",
  "adapter": "Haltline",
  "request": "launch",
  "executable": "build/app.elf",
  "device": "STM32F407VG",
  "rtos": "FreeRTOS"
}
```

OpenOCD нужна переменная `uxTopUsedPriority`. В свежих версиях FreeRTOS-Kernel она есть, а в старых нет, например во FreeRTOS из старых пакетов STM32CubeMX. Если в консоли появилось `FreeRTOS: uxTopUsedPriority is not defined`, добавьте в проект файл:

```c
/* freertos_openocd.c */
#include "FreeRTOS.h"

const int __attribute__((used)) uxTopUsedPriority = configMAX_PRIORITIES - 1;
```

Если вы линкуете с `--gc-sections`, добавьте в флаги линкера ещё `-Wl,--undefined=uxTopUsedPriority`, чтобы переменная не выбрасывалась.

### Zephyr

Добавьте в `prj.conf`:

```ini
CONFIG_DEBUG_THREAD_INFO=y
```

Эта опция экспортирует сведения о потоках, нужные отладчикам, и включает имена потоков. Затем укажите `"rtos": "Zephyr"` (для OpenOCD). Файл прошивки — `build/zephyr/zephyr.elf`.

### NuttX

* **Нужен OpenOCD, собранный из git.** В последнем релизе OpenOCD 0.12.0 (он и стоит в дистрибутивах Linux) поддержка NuttX устарела и с современным NuttX не работает. У сборки из git `openocd --version` показывает версию вида `0.12.0+dev-…`. Укажите путь к ней в `serverPath`.
* **NuttX должен экспортировать `g_tcbinfo`.** NuttX 12.4 и новее делает это на Cortex-M всегда. В старых версиях, например 12.0, нужно включить `CONFIG_DEBUG_TCBINFO=y`. Для отладочной информации собирайте с `CONFIG_DEBUG_SYMBOLS=y`.

```json
{
  "label": "NuttX",
  "adapter": "Haltline",
  "request": "launch",
  "executable": "nuttx/nuttx",
  "device": "STM32F407VG",
  "rtos": "nuttx",
  "serverPath": "/opt/openocd-git/bin/openocd"
}
```

### pyOCD

pyOCD сам находит FreeRTOS и Zephyr, так что с `"serverType": "pyocd"` дополнительные настройки не нужны. Требования к прошивке те же: для Zephyr нужен `CONFIG_DEBUG_THREAD_INFO=y`.

### J-Link

J-Link подключает поддержку RTOS через плагины, которые передаются параметром `-rtos`. Плагины для FreeRTOS и Zephyr входят в пакет J-Link:

```json
{
  "label": "J-Link + FreeRTOS",
  "adapter": "Haltline",
  "request": "launch",
  "serverType": "jlink",
  "device": "STM32F407VG",
  "executable": "build/app.elf",
  "serverArgs": ["-rtos", "GDBServer/RTOSPlugin_FreeRTOS"]
}
```

Для Zephyr используйте `GDBServer/RTOSPlugin_Zephyr`; ему тоже нужен `CONFIG_DEBUG_THREAD_INFO=y`. Для NuttX соберите плагин из исходников NuttX командой `make -C tools -f Makefile.host jlink-nuttx` и укажите путь к нему: `"serverArgs": ["-rtos", "/path/to/nuttx/tools/jlink-nuttx.so"]` (`.dylib` в macOS, `.dll` в Windows).

## Справочник настроек

Полная схема с описаниями — в [`debug_adapter_schemas/Haltline.json`](debug_adapter_schemas/Haltline.json). Относительные пути считаются от `cwd`, по умолчанию это корень проекта.

### Основные

| Параметр | По умолчанию | Описание |
|---|---|---|
| `request` | — | `launch` прошивает и сбрасывает МК; `attach` подключается к работающему МК без сброса |
| `executable` | — | ELF-файл прошивки. Обязателен для `launch` |
| `cwd` | корень проекта | Папка для относительных путей и запуска GDB-сервера |
| `svdFile` | — | CMSIS-SVD файл; включает вид **Peripherals** |

### GDB-сервер

| Параметр | По умолчанию | Описание |
|---|---|---|
| `serverType` | `openocd` | `openocd`, `stlink`, `stutil`, `pyocd`, `jlink`, `qemu`, `external` |
| `serverPath` | авто | Путь к программе сервера |
| `serverArgs` | `[]` | Дополнительные аргументы сервера |
| `serverEnv` | `{}` | Дополнительные переменные окружения сервера |
| `serverReadyText` | авто | Текст в выводе сервера, означающий, что он принимает подключения |
| `serverTimeout` | `15000` | Сколько миллисекунд ждать запуска сервера |
| `gdbTarget` | — | `host:port` сервера. Обязателен для `external` |
| `port` | свободный порт | Порт GDB для сервера, который запускает Haltline |
| `gdbPath` | авто | `arm-none-eabi-gdb` или `gdb-multiarch` |
| `gdbArgs` | `[]` | Дополнительные аргументы GDB |

### Программатор и МК

| Параметр | По умолчанию | Описание |
|---|---|---|
| `device` | — | Модель МК, например `STM32F407VG`. По ней выбирается файл цели OpenOCD (STM32 C0, F0–F7, G0, G4, H5, H7, L0–L5, U0, U5, WB, WL, MP1). Обязателен для J-Link |
| `probe` | `stlink` | `stlink`, `stlink-dap`, `jlink`, `cmsis-dap`, `daplink`, `picoprobe`. По нему выбирается файл интерфейса OpenOCD |
| `interface` | `swd` | `swd` или `jtag` |
| `speed` | как у сервера | Частота программатора в кГц |
| `serialNumber` | — | Серийный номер программатора, если подключено несколько |
| `configFiles` | по `device` | Конфиги OpenOCD (`-f`) |
| `searchDir` | `[]` | Папки скриптов OpenOCD (`-s`) |
| `openocdCommands` | `[]` | Дополнительные команды OpenOCD (`-c`) |
| `rtos` | — | Поддержка RTOS в OpenOCD: `FreeRTOS`, `Zephyr`, `nuttx`, `auto`, … (регистр важен), см. [Задачи RTOS](#задачи-rtos-freertos-zephyr-nuttx) |
| `targetId` | по `device` | Имя цели pyOCD |
| `machine` | `netduinoplus2` | Машина QEMU |
| `stm32cubeprogrammer` | авто | Папка `bin` STM32CubeProgrammer для ST-LINK GDB server |
| `semihosting` | `false` | Показывать вывод `printf` программы в консоли отладки (OpenOCD, st-util, pyOCD, QEMU) |

### Прошивка и запуск

| Параметр | По умолчанию | Описание |
|---|---|---|
| `loadImage` | `true` (`false` для QEMU) | Прошивать при запуске |
| `verifyAfterLoad` | `false` | После прошивки сравнить память МК с ELF; при расхождении запуск прерывается |
| `loadTimeout` | `180000` | Сколько миллисекунд отводится на прошивку |
| `reloadOnRestart` | `false` | Прошивать заново при **Restart** |
| `runToEntryPoint` | `main` | До какой функции выполнять программу после старта и Restart; `""` отключает |
| `stopOnEntry` | `false` | Остановиться сразу после сброса, в `Reset_Handler` |
| `continueOnAttach` | `false` | Не останавливать программу после подключения |
| `resetCommands` | зависит от сервера | Команды сброса с остановкой МК (см. ниже) |

Команды сброса по умолчанию: `monitor reset halt` (OpenOCD, pyOCD, external), `monitor reset` (ST-LINK, st-util), `monitor reset` + `monitor halt` (J-Link), `monitor system_reset` (QEMU).

### Свои команды GDB

| Параметр | Когда выполняется |
|---|---|
| `preLaunchCommands` / `postLaunchCommands` | До сброса и прошивки / после них |
| `overrideLaunchCommands` | Вместо всей последовательности запуска (сброс, прошивка, сброс) |
| `preAttachCommands` / `postAttachCommands` / `overrideAttachCommands` | То же для `attach` |
| `preRestartCommands` / `postRestartCommands` / `overrideRestartCommands` | То же для **Restart** |

Команда, начинающаяся с `-`, отправляется как команда GDB/MI; остальные выполняются как команды консоли GDB.

### Отображение и диагностика

| Параметр | По умолчанию | Описание |
|---|---|---|
| `showDisassembly` | `auto` | `auto`: при шаге по инструкциям или если нет исходника; `always`; `never` |
| `hexValues` | `false` | Показывать значения в hex |
| `showGlobals` | `true` | Показывать области **Globals** и **Static** |
| `forceHardwareBreakpoints` | `false` | Всегда использовать аппаратные точки останова |
| `sourceFileMap` | `{}` | Замена путей к исходникам, например `{"/build/src": "/home/me/project/src"}` |
| `debugOutput` | `false` | Писать в консоль отладки все команды и ответы GDB/MI |
| `adapterPath` | — | Использовать этот `haltline-dap` вместо скачанного |

## Работа с отладчиком

* **Прошивка.** Консоль отладки показывает каждый этап по ходу дела: стирание, запись с процентами, программирование и проверку. В конце выводится итог, например `Flashed 48.2 KiB in 6.3 s (erase 2.1 s, write 0.4 s, program 3.8 s)`. Чтобы прервать прошивку, закройте сессию крестиком **×** в списке сессий.
* **Регистры.** Они в области **Registers** панели Variables. Флаги xPSR и CONTROL раскрываются, значения можно менять прямо в панели.
* **Периферия.** Она в области **Peripherals**: блок → регистр → поля. Регистры читаются, только когда раскрыт их блок. Регистр, чтение которого имеет побочный эффект (например, `USART1.DR`), можно прочитать командой `:svd USART1.DR`.
* **Ассемблер.** Включите в настройках Zed `"debugger": { "stepping_granularity": "instruction" }` или выполните `:asm on`. Код без исходников всегда показывается в ассемблере.
* **Число срабатываний точки останова.** `5` или `>=5` (с пятого срабатывания), `==5`, `>5`, `<5`, `<=5`, `%5` (каждое пятое).
* **Logpoint** выводит сообщение и не останавливает программу. Выражения пишутся в фигурных скобках: `i={i}, x={p->x}`.
* **Watchpoint.** Ставится из контекстного меню переменной или регистра периферии. В Cortex-M 4 компаратора DWT (2 в M0).
* **Отказы.** Фильтр **Faults** в списке точек останова включён по умолчанию. Он останавливает выполнение на HardFault, MemManage, BusFault и UsageFault и объясняет причину в консоли.
* **Restart** сбрасывает МК (и перепрошивает при `reloadOnRestart`), затем снова выполняет программу до `main`.

### Консоль отладки

| Ввод | Действие |
|---|---|
| `g_counter`, `buf[3]`, `*ptr` | Вычислить выражение C |
| `info registers`, `x/8xw 0x20000000`, `monitor reset halt` | Команда GDB (распознаётся автоматически) |
| `> команда` | Всегда выполнить как команду GDB |
| `:asm [on\|off\|auto]` | Ассемблерный вид текущего кадра |
| `:hex [on\|off]` | Показ значений в hex |
| `:reset [run]` | Сбросить МК и остаться в остановке, или сбросить и запустить |
| `:fault` | Объяснить текущий отказ |
| `:svd [PERIPH[.REG]]` | Список периферии или чтение регистров |
| `:mi -команда` | Отправить «сырую» команду GDB/MI |
| `:help` | Справка |

## Решение проблем

| Проблема | Что делать |
|---|---|
| Программатор не найден | Проверьте USB-подключение и драйверы (в Linux — правила udev). Укажите `probe` или `configFiles`, а если программаторов несколько — `serialNumber` |
| Не найден GDB | Установите `gdb-multiarch` или Arm GNU Toolchain, либо укажите `gdbPath` |
| Сервер не запускается | Его вывод есть в консоли отладки. Проверьте `serverPath` и файлы конфигурации |
| Прошивка не укладывается в тайм-аут | Увеличьте `loadTimeout` и уменьшите `speed` |
| Точки останова не срабатывают или исходник не находится | Собирайте с `-g`. Если прошивка собиралась в другой папке, используйте `sourceFileMap` |
| Что-то другое | Включите `"debugOutput": true`, чтобы видеть обмен с GDB/MI, и откройте логи адаптера командой `dev: open debug adapter logs` |

Сообщения об ошибках принимаются в [Issues](https://github.com/ghorik78/Haltline/issues). Приложите, пожалуйста, ваш `debug.json` и вывод консоли отладки.

## Ограничения

* В Zed нет отдельных панелей регистров, периферии и дизассемблера. Регистры и периферия показаны в панели Variables, а ассемблер — в сгенерированных `.s`-файлах.
* Zed не показывает полосы прогресса адаптеров отладки, поэтому ход прошивки выводится в консоль отладки.
* SWO/ITM-трассировка и RTT пока не поддерживаются.
* QEMU не даёт записывать в регистры эмулируемой периферии. На реальном МК запись работает.

## Разработка

```
src/                      расширение Zed (WebAssembly)
debug_adapter_schemas/    схема настроек
adapter/src/              haltline-dap: DAP, GDB/MI, сессия, SVD, Cortex-M, прошивка, дизассемблер
adapter/tests/            тесты адаптера и сквозные тесты на QEMU
scripts/                  упаковка и скрипты установки
```

```sh
cargo test --workspace
```

Сквозные тесты запускают настоящий адаптер с QEMU и GDB. Если инструментов нет, тесты пропускаются; с `HALTLINE_E2E=1` отсутствие инструмента считается ошибкой. Пути к инструментам задаются через `HALTLINE_QEMU`, `HALTLINE_GDB` и `HALTLINE_CC`.

**Выпуск версии.** Укажите одинаковую версию в `Cargo.toml`, `adapter/Cargo.toml` и `extension.toml`, затем запушьте тег:

```sh
git tag v0.1.2 && git push origin v0.1.2
```

GitHub Actions соберёт адаптер для всех платформ, упакует расширение и опубликует релиз.

## Лицензия

[MIT](LICENSE)
