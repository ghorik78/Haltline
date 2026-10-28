# Haltline — отладка STM32 в Zed

Расширение для [Zed](https://zed.dev), которое добавляет отладчик для STM32 и других микроконтроллеров ARM Cortex-M, по возможностям похожий на Cortex-Debug из VS Code.

Расширение состоит из двух частей:

* **Zed-расширение** (`src/`, собирается в `extension.wasm`) регистрирует отладчик `Haltline` и схему его конфигурации, а также находит или скачивает адаптер.
* **`haltline-dap`** (`adapter/`) — адаптер Debug Adapter Protocol. Он запускает GDB-сервер (OpenOCD, ST-LINK_gdbserver, st-util, pyOCD, J-Link или QEMU) и `arm-none-eabi-gdb`/`gdb-multiarch`, а затем переводит запросы Zed в команды GDB/MI.

## Возможности

| Что | Где в Zed |
|---|---|
| Все регистры ядра: r0–r12, sp, lr, pc, xPSR с разбором флагов и номера исключения, MSP/PSP/PRIMASK/BASEPRI/FAULTMASK/CONTROL, FPU (d0–d15, fpscr), регистры отказов SCB (CFSR/HFSR/MMFAR/BFAR) с расшифровкой битов | панель Variables → область **Registers** |
| SVD-файлы: все периферийные блоки, регистры и битовые поля с перечислениями. Значения можно менять, в том числе отдельные поля (`MODE = Output`). На регистр можно поставить watchpoint | область **Peripherals** (нужен `svdFile`) |
| Шаги: step over / into / out, continue, pause, **reset** (кнопка Restart), stop | панель отладки |
| Обычные, условные (`condition`) точки останова, по числу срабатываний (`hitCondition`), logpoint (`logMessage`), на функции, на адресе, data breakpoints (watchpoints на запись/чтение/доступ), остановка на HardFault/MemManage/BusFault/UsageFault | gutter, Breakpoints, контекстное меню переменной |
| Ассемблер с шагом по инструкциям: дизассемблированная функция с исходными строками в комментариях; в этом файле тоже можно ставить точки останова | открывается вместо `.c`-файла (см. ниже) |
| Стек вызовов, в том числе через границу прерывания (`<exception entry>`) | Frames |
| Значения переменных в рантайме: Locals, Static, Globals, вложенные структуры, массивы (большие делятся на диапазоны), указатели, watch, подсказки при наведении, изменение значений | Variables, Watch, hover |
| Память: просмотр и запись | Memory View |
| Разбор HardFault: причина (CFSR/HFSR), адрес ошибки и строка кода, где он произошёл | консоль отладки, `:fault` |
| RTOS: задачи FreeRTOS/Zephyr как потоки (через OpenOCD `rtos`) | Threads |

## Установка

### Готовый архив (рекомендуется)

Кнопки «установить из файла» в Zed нет, но Zed загружает готовое расширение, если распаковать его в свою папку `extensions/installed`. Это делают скрипты из релиза (они же ставят адаптер):

```sh
# Linux / macOS
curl -fsSL https://github.com/ghorik78/Haltline/releases/latest/download/install.sh | bash
```

```powershell
# Windows
irm https://github.com/ghorik78/Haltline/releases/latest/download/install.ps1 | iex
```

После этого перезапустите Zed (или выполните `zed: reload extensions`).

Вручную: скачайте `haltline-zed-extension.tar.gz` (или `.zip`) из [Releases](https://github.com/ghorik78/Haltline/releases) и распакуйте так, чтобы получилось:

| ОС | Путь |
|---|---|
| Linux | `~/.local/share/zed/extensions/installed/haltline/extension.toml` |
| macOS | `~/Library/Application Support/Zed/extensions/installed/haltline/extension.toml` |
| Windows | `%LOCALAPPDATA%\Zed\extensions\installed\haltline\extension.toml` |

Адаптер `haltline-dap` расширение при первом запуске отладки скачивает само (релиз той же версии с GitHub). Если нужен свой бинарник, он берётся по порядку отсюда:

1. настройка Zed `"dap": { "Haltline": { "binary": "/path/to/haltline-dap" } }`;
2. `"adapterPath"` в конфигурации отладки;
3. `haltline-dap` в `PATH` (например, после `cargo install --path adapter`).

### Из исходников (dev extension)

```sh
git clone https://github.com/ghorik78/Haltline && cd Haltline
cargo install --path adapter          # адаптер в ~/.cargo/bin
```

Затем в Zed: `Extensions` → `Install Dev Extension` → выберите папку репозитория. Zed сам соберёт wasm (нужен Rust из rustup).

### Что нужно на компьютере

* GDB для ARM: `arm-none-eabi-gdb` (Arm GNU Toolchain, STM32CubeCLT) или `gdb-multiarch`. Ищется в `PATH` и в стандартных папках STM32CubeCLT/Arm toolchain, путь можно задать через `gdbPath`.
* GDB-сервер для вашего программатора: OpenOCD (ST-LINK, J-Link, CMSIS-DAP), ST-LINK_gdbserver (STM32CubeCLT), st-util, pyOCD или SEGGER J-Link. Для опытов без платы подойдёт QEMU.

## Настройка

Создайте `.zed/debug.json` в проекте. Минимальный вариант для OpenOCD и ST-LINK:

```json
[
  {
    "label": "STM32F4 (OpenOCD)",
    "adapter": "Haltline",
    "request": "launch",
    "executable": "build/firmware.elf",
    "device": "STM32F407VG",
    "svdFile": "STM32F407.svd"
  }
]
```

При `launch` адаптер запускает OpenOCD (`interface/stlink.cfg` + `target/stm32f4x.cfg` по `device`), подключает GDB, делает reset, прошивает ELF, снова делает reset и останавливается на `main`. Перед отладкой можно собирать проект: добавьте `"build": { "command": "make", "args": ["-j8"] }`.

Другие варианты:

```json
[
  {
    "label": "OpenOCD со своими конфигами",
    "adapter": "Haltline",
    "request": "launch",
    "executable": "build/app.elf",
    "configFiles": ["interface/stlink.cfg", "target/stm32g4x.cfg"],
    "svdFile": "STM32G474.svd",
    "rtos": "FreeRTOS"
  },
  {
    "label": "ST-LINK GDB server (STM32CubeCLT)",
    "adapter": "Haltline",
    "request": "launch",
    "serverType": "stlink",
    "executable": "build/app.elf",
    "svdFile": "STM32H743.svd"
  },
  {
    "label": "J-Link",
    "adapter": "Haltline",
    "request": "launch",
    "serverType": "jlink",
    "device": "STM32F407VG",
    "executable": "build/app.elf"
  },
  {
    "label": "pyOCD",
    "adapter": "Haltline",
    "request": "launch",
    "serverType": "pyocd",
    "device": "STM32F103C8",
    "executable": "build/app.elf"
  },
  {
    "label": "Подключиться к работающей плате без сброса",
    "adapter": "Haltline",
    "request": "attach",
    "executable": "build/app.elf",
    "device": "STM32F407VG"
  },
  {
    "label": "Уже запущенный GDB-сервер",
    "adapter": "Haltline",
    "request": "launch",
    "serverType": "external",
    "gdbTarget": "localhost:3333",
    "executable": "build/app.elf"
  },
  {
    "label": "QEMU (без платы)",
    "adapter": "Haltline",
    "request": "launch",
    "serverType": "qemu",
    "machine": "netduinoplus2",
    "executable": "build/app.elf"
  }
]
```

Все параметры с описаниями — в [`debug_adapter_schemas/Haltline.json`](debug_adapter_schemas/Haltline.json), Zed подсказывает их при редактировании `debug.json`. Самые полезные:

| Параметр | Назначение |
|---|---|
| `serverType` | `openocd` (по умолчанию), `stlink`, `stutil`, `pyocd`, `jlink`, `qemu`, `external` |
| `device`, `probe`, `interface` | модель МК, программатор (`stlink`, `jlink`, `cmsis-dap`…), `swd`/`jtag` |
| `configFiles`, `openocdCommands`, `serverArgs` | тонкая настройка сервера |
| `svdFile` | SVD-файл для просмотра периферии |
| `runToEntryPoint` / `stopOnEntry` | где остановиться после старта (по умолчанию `main`) |
| `loadImage`, `reloadOnRestart` | прошивать ли при запуске и при Restart |
| `verifyAfterLoad` | после прошивки сравнить память МК с ELF (`compare-sections`); при расхождении запуск прерывается |
| `preLaunchCommands`, `postLaunchCommands`, `overrideLaunchCommands`, `resetCommands` | свои команды GDB на этапах запуска |
| `showDisassembly` | `auto` / `always` / `never` |
| `semihosting` | вывод `printf` через отладчик в консоль Zed (OpenOCD, st-util, pyOCD, QEMU) |
| `forceHardwareBreakpoints`, `hexValues`, `debugOutput` | аппаратные точки останова, hex по умолчанию, лог GDB/MI |

SVD-файлы для STM32 есть в пакетах STM32CubeIDE/STM32CubeCLT (`.../STMicroelectronics_CMSIS_SVD/`), в CMSIS-паках ST и в репозитории [cmsis-svd-data](https://github.com/cmsis-svd/cmsis-svd-data).

### Попробовать без платы

Откройте в Zed папку `adapter/tests/firmware`: там есть `.zed/debug.json`, который собирает тестовую прошивку (`make`) и отлаживает её в QEMU на эмулированном STM32F405. Нужны `arm-none-eabi-gcc`, `make`, `qemu-system-arm` и `gdb-multiarch` (или `arm-none-eabi-gdb`).

## Работа в Zed

* **Регистры.** Область *Registers* в панели переменных. Флаги xPSR/CONTROL раскрываются, номер исключения подписан (`3 (HardFault)`). Значение регистра можно изменить прямо в панели.
* **Периферия.** Область *Peripherals*: блок → регистр → поля. Регистры читаются, только когда блок раскрыт. Регистры, у которых в SVD указано, что чтение имеет побочный эффект (`readAction`), автоматически не читаются; прочитать такой регистр явно можно командой `:svd USART1.DR`.
* **Ассемблер и шаг по инструкциям.** Своего окна дизассемблера в Zed нет, поэтому адаптер пишет дизассемблированную функцию в `.s`-файл (во временной папке) и показывает текущую инструкцию в нём. Включить можно двумя способами:
  * в настройках Zed: `"debugger": { "stepping_granularity": "instruction" }` — тогда шаги идут по инструкциям, а текущий кадр показывается в ассемблере;
  * командой `:asm on` в консоли отладки (`:asm off` / `:asm auto`).

  Код без исходников (библиотеки, ROM) показывается в ассемблере всегда. В `.s`-файле можно ставить точки останова на строках инструкций.
* **Прошивка.** Ход прошивки выводится в консоль отладки по мере выполнения: этапы (стирание, передача данных с процентами, запись во флеш, проверка), время каждого этапа и итог, например `Flashed 48.2 KiB in 6.3 s (erase 2.1 s, write 0.4 s, program 3.8 s)`. Сообщения OpenOCD/J-Link тоже появляются сразу, а не после окончания. GDB сообщает только о передаче данных, поэтому этапы стирания и записи определяются по паузам между его сообщениями. Окно прогресса Zed не поддерживает, отсюда вывод в консоль. Кнопка Stop во время прошивки в Zed не срабатывает (до первой остановки у сессии нет потока), поэтому прервать прошивку можно, закрыв сессию крестиком в списке сессий: адаптер сразу останавливает GDB и GDB-сервер.
* **Reset.** Кнопка *Restart* сбрасывает МК (и при `reloadOnRestart` перепрошивает) и снова бежит до `main`. В консоли: `:reset` — сброс с остановкой, `:reset run` — сброс и запуск.
* **Точки останова.** Условие и число срабатываний задаются в свойствах точки. Форматы `hitCondition`: `5` или `>=5` — с пятого срабатывания, `==5` — только пятое, `>5`, `<5`, `<=5`, `%5` — каждое пятое. Logpoint пишет сообщение в консоль и не останавливает выполнение, выражения подставляются в фигурных скобках: `i={i}, x={p->x}`.
* **Watchpoint (data breakpoint).** Контекстное меню переменной или регистра периферии. В Cortex-M всего 4 компаратора DWT (2 в M0), аппаратных точек останова 6 (4 в M0).
* **Отказы.** Фильтр «Faults» в списке точек останова (включён по умолчанию) останавливает выполнение при входе в обработчик отказа. В консоли появляется разбор: флаги CFSR/HFSR, адрес ошибки, сохранённый на стеке PC и строка исходника.

### Консоль отладки

| Ввод | Действие |
|---|---|
| `g_counter`, `buf[3]`, `*ptr`, `sizeof(struct s)` | вычислить выражение C |
| `info registers`, `x/8xw 0x20000000`, `monitor reset halt`, `p/x var` | команда GDB (распознаётся автоматически) |
| `> команда` | принудительно выполнить как команду GDB |
| `:asm [on\|off\|auto]` | ассемблерный вид текущего кадра |
| `:hex [on\|off]` | показ переменных в hex |
| `:reset [run]` | сброс МК |
| `:fault` | разбор текущего отказа |
| `:svd [PERIPH[.REG]]` | список периферии или значения регистров |
| `:mi -команда` | «сырая» команда GDB/MI |
| `:help` | справка |

## Ограничения

* В Zed нет отдельных панелей регистров и дизассемблера, поэтому регистры и периферия показаны в панели переменных, а ассемблер — в `.s`-файлах.
* SWO/ITM-трассировка и RTT пока не поддерживаются.
* QEMU не даёт отладчику записывать в регистры эмулируемой периферии (чтение работает). На реальном МК запись работает.

## Разработка

```
src/                      Zed-расширение (wasm)
debug_adapter_schemas/    JSON-схема конфигурации
adapter/src/              haltline-dap
  mi/                     парсер и клиент GDB/MI
  dap.rs                  протокол DAP
  session/                логика сессии: запуск, точки останова, инспекция
  svd.rs, cortex.rs       SVD и знания о Cortex-M
  disasm.rs               дизассемблер в .s-файлы
  server.rs, config.rs    GDB-серверы и параметры
adapter/tests/            тесты бинарника и сквозные тесты на QEMU
adapter/tests/firmware/   тестовая прошивка для STM32F405 и SVD
scripts/                  упаковка и установка
```

Тесты:

```sh
cargo test --workspace
```

Модульные тесты и тесты сессии с поддельным GDB работают везде. Сквозные тесты (`adapter/tests/e2e_qemu.rs`) запускают настоящий адаптер с QEMU (`netduinoplus2`, STM32F405) и `gdb-multiarch` на тестовой прошивке. Если инструментов нет, эти тесты пропускаются; `HALTLINE_E2E=1` превращает пропуск в ошибку. Пути к инструментам можно задать через `HALTLINE_QEMU`, `HALTLINE_GDB`, `HALTLINE_CC`. В Ubuntu нужны пакеты `gcc-arm-none-eabi gdb-multiarch qemu-system-arm`.

### CI/CD

* `.github/workflows/ci.yml` — на каждый push и PR: `rustfmt`, `clippy`, все тесты на Linux (со сквозными на QEMU), macOS и Windows, сборка wasm-расширения.
* `.github/workflows/release.yml` — по тегу `vX.Y.Z` собирает `haltline-dap` для Linux (x86_64/aarch64, musl), macOS (x86_64/arm64), Windows (x86_64/arm64), упаковывает расширение и публикует GitHub Release с архивами, скриптами установки и `SHA256SUMS`.

Выпуск версии: поднимите версию в `Cargo.toml`, `adapter/Cargo.toml` и `extension.toml` (тест `versions_are_in_sync` следит, чтобы они совпадали), затем:

```sh
git tag v0.1.0 && git push origin v0.1.0
```

## Лицензия

MIT
