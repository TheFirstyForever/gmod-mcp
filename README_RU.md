# GMod debug-мост: gmod_mcp (аддон) + gmod-mcp (MCP-сервер)

[English](README.md) | **Русский**

Единая система управления запущенным Garry's Mod извне:

- **`gmod_mcp/`** — Lua-аддон, живущий внутри игры во всех трёх
  Lua-реалмах (server / client / menu). Читает команды, выполняет их,
  пишет ответы.
- **`gmod-mcp/`** — Rust-бинарь, реализующий протокол MCP и
  транспорт команд (файлы в `data/gmod_mcp/`, ответы menu-реалма
  через `console.log`). Зарегистрирован как сервер `gmod` в
  `mcp_config.json`, указывает на `_devtools/gmod-mcp/gmod-mcp.exe`.

Одно без другого не работает: аддон — исполнитель, сервер —
единственный фронт.

> **ВНИМАНИЕ: это remote code execution с полным доступом к Lua API
> игры.** Только локальная отладка. `gmod_mcp` никогда не публикуется;
> перед релизом аддона проверь, что он не попал в сборку
> (`addon.txt` так и говорит: «удалить перед публикацией»).

---

## Требования

| Требование | Зачем |
|---|---|
| GMod запущен с `-condebug` | Реалм menu отвечает строками `MsgN` в `garrysmod/console.log`. Без флага лога нет и все menu-вызовы уходят в таймаут. |
| Установленный аддон `gmod_mcp` | Игровая сторона моста (см. «Установка»). |
| `GMOD_DATA` (опц.) | Переопределяет путь к `garrysmod/data`. |
| `GMOD_EXE` (опц.) | Переопределяет поиск exe: `gmod_win64.exe` → `bin/win64/gmod.exe` → `gmod.exe` в корне GarrysMod. |

## Установка gmod_mcp

Исходник: `_devtools/gmod-mcp/addon/`. Установленные копии:

| Файл | Куда | Реалм |
|---|---|---|
| `lua/autorun/gmod_mcp.lua` | `garrysmod/addons/gmod_mcp/` | server + client (карта) |
| `lua/menu/gmod_mcp_menu.lua` | `garrysmod/lua/menu/` | menu-state |

Menu-state **не поддерживает аддоны** — `gmod_mcp_menu.lua` грузится
строкой `include("menu/gmod_mcp_menu.lua")` внутри движкового файла
`garrysmod/lua/includes/init_menu.lua`. Если мост в меню не поднимается —
проверь, что include пережил обновление игры и установленный файл
совпадает с исходником (`diff` обеих копий).

## Сборка и деплой gmod-mcp

```bash
cd _devtools/gmod-mcp
CARGO_TARGET_DIR=C:/Users/1/.cargo-target/gmod-mcp cargo build --release
```

- `CARGO_TARGET_DIR` — **только ASCII-путь**: mingw-линкер падает на
  кириллице в пути репозитория.
- Тулчейн: `stable-x86_64-pc-windows-gnu` (запинен в rust-toolchain.toml).
- Деплой: запущенный exe в Windows не перезаписывается — переименуй
  старый в `gmod-mcp.old.exe`, затем скопируй новый бинарь на место
  `gmod-mcp.exe`. MCP-клиент подхватит его при следующем пересоздании
  сервера (или убей `gmod-mcp.exe` — клиент пересоздаст при вызове).

---

## Реалмы

Каждая команда исполняется в одном из Lua-реалмов игры:

| realm | Где исполняется | Когда жив |
|---|---|---|
| `server` | server-state (сессия карты) | карта загружена |
| `client` | client-state (сессия карты) | карта загружена |
| `menu` | menu-state (главное меню) | **только пока виден GameUI и окно не свёрнуто** |

> **Важно про menu.** В игре, на экране загрузки или при свёрнутом окне
> menu-state не тикает. Команды копятся в `cmd_menu_*.json` и
> выполнятся при пробуждении — вызов при этом отвечает TIMEOUT, так
> что **не ретрай вслепую**: команда исполнится дважды.

---

## Протокол моста (низкоуровневый)

### Команды (все реалмы)

Кладём `garrysmod/data/gmod_mcp/cmd_<realm>_<id>.json`:

```json
{"id": "<12 hex>", "kind": "lua", "code": "return 1+1"}
```

Префиксы реалмов: `cmd_srv_`, `cmd_cl_`, `cmd_menu_`. Poll-таймер
(~0.25 с) сканирует каталог, читает файл, удаляет его, выполняет
команду, пишет ответ. Битый JSON отбрасывается молча (файл всё равно
съедается).

### Ответы

| Реалм | Канал |
|---|---|
| server / client | `out_<id>.txt` — чистый текст: захваченный `print`, затем `=> <return>` |
| menu | строки `GMCPO|<id>|<seq>/<total>|<hex>` в `garrysmod/console.log`; hex-чанки по 1800 символов, собираются seq=1..total |

**Почему канал menu — лог-файл:** `file.Write` в menu-state сломан —
его обёртка из `includes/extensions/file.lua` вызывает
`file.Open(path,"wb","DATA")`, которая *создаёт* файл, но у
возвращённого хэндла нет метода `:Write` — вызов бросает рантайм-
ошибку, убивающую вызвавший таймер. `MsgN` же пишется движком в
console.log и работает надёжно.

### Kind'ы команд

**Во всех реалмах:**

| kind | Поля | Эффект | Ответ |
|---|---|---|---|
| `lua` | `code` | `RunString` кода (в menu только RunString — CompileString/loadstring отсутствуют) в `pcall` | print-вывод + `=>` return |
| `console` | `command` | `RunConsoleCommand(unpack(words))` | `menu console: <cmd>` / ack реалма |
| `shot` | `pos`,`ang`,`fov`,`hud`,`drawviewer` (карта) | рендерит и сохраняет скриншот | `shot_<id>.jpg` |
| `cam` | `on`,`pos`,`ang`,`fov` | постоянная отладочная камера (client) | ack |

**Только menu-реалм:**

| kind | Поля | Эффект | Ответ |
|---|---|---|---|
| `js` | `code` | `pnlMainMenu.HTML:QueueJavascript(code)` — произвольный JS на странице меню | `ok (queued)` |
| `js_eval` | `code` | eval JS; инжектированная обёртка возвращает результат через `window.lua.Run("GMCP_outh('<id>','<hex>')")` | `{"ok":bool,"result"|"error":...}` |
| `shot` | — | движковый `screenshot` → `garrysmod/screenshots/Snapshot*.tga`, возвращает имя файла | `ok <Snapshot.tga>` |

Детали `js_eval`:

- `code` выполняется внутри `(function(){ ... eval("code") ... })()` —
  передавай выражение или IIFE с `return`.
- Результат — JSON.stringify → UTF-8 → hex перед `lua.Run` —
  обязательно, т.к. `lua.Run(fmt,...)` выполняет `fmt:format(args)`
  и любой `%` в данных ломает вызов.
- Страховочный `timer.Simple(8)` пишет
  `ERROR: js_eval timeout (lua.Run/GMCP_outh channel dead?)`, если
  страница не ответила (меню не загружено, биндинг мёртв).

### Capability-матрица menu-state (замерено вживую)

**ЕСТЬ:** `RunString`, `file.Read/Find/Delete/CreateDir`,
`file.Open` (создаёт файл, но у хэндла нет write-методов),
`MsgN` → console.log, `sql`, `cookie`, `engine.*`, `serverlist.*`,
`steamworks.*`, `gui.*`, `permissions.*`, vgui/derma, `timer`, `hook`,
`concommand`, `RunConsoleCommand`, `pnlMainMenu` (с `.HTML`; есть и
`QueueJavascript`, и `RunJavascript`), `IsInGame`, `IsHostingGame`,
`GetMapList`, `GetLoadStatus`, `IsInLoading`, `gameevent.Listen`.

**НЕТ:** `Entity`, `Player`, `net.*`, `render.Capture`,
`CompileString`, `loadstring`, `http`, записывающие `file.Open`/
`file.Append`/`file.Write`, `print` → лог (print уходит в буфер
моста; в console.log попадает только `MsgN`).

### Известные ловушки menu-state

- **`window.lua.Run(fmt, ...)` делает `fmt:format(args)`** — `%` в
  любом месте данных убивает вызов. Поэтому весь JS→Lua трафик —
  hex (только ASCII).
- **`RunString` не изолирует ошибки в menu-state** — необёрнутый
  nil-call убивает вызвавший тик/таймер. Мост оборачивает код в
  `pcall` дважды, а весь диспетчер — ещё раз; убитый poll-таймер
  исторически был главной причиной «мёртвого» реалма.
- **Menu-state спит**, когда загружена карта, идёт загрузка или окно
  свёрнуто — команды копятся в `cmd_menu_*.json` и исполнятся при
  пробуждении. TIMEOUT не значит «потеряно» — не ретрай вслепую.
- Скриншот меню — только движковой командой `screenshot` (TGA в
  `garrysmod/screenshots/`); `render.Capture` недоступен.
- `console.log` внутри страницы → `DHTML.ConsoleMessage`; биндинг
  `lua.Run` страницы меню зарегистрирован в `mainmenu.lua` через
  `AddFunction`.

### Горячая замена моста без рестарта игры

Через работающий `kind=lua`:

```lua
GMOD_MCP_MENU_LOADED=nil
local s = file.Read("lua/menu/gmod_mcp_menu.lua","GAME")
if s then RunString(s) end
```

`timer.Create("GmodMcpMenuPoll", ...)` внутри файла перезаписывает
старый тикер новым кодом. Ответ `=> hp <bytes>` = файл перечитан и
исполнен.

### Каталог данных

`garrysmod/data/gmod_mcp/`:

| Файл | Роль |
|---|---|
| `cmd_{srv,cl,menu}_*.json` | очередь команд (удаляются при обработке) |
| `out_<id>.txt` | ответы server/client |
| `shot_<id>.jpg` | скриншоты в карте |
| `errors.txt` | собранные Lua-ошибки (карта; `gmod_errors` читает и очищает) |

---

## Конвенции ответов MCP-инструментов

- Вывод `print()` собирается в ответ.
- Возвращаемое `return` значение идёт после `=> ` в последней строке.
- Таблицы сериализуются явно: `return util.TableToJSON(t)`.
- Вызовы синхронные, таймаут 12–25 с.

## Формат ошибок

Все ошибки использования — на английском, единый вид:

```
UsageError: <что не так>
Usage: <инструмент>(<сигнатура>) — <варианты>
```

Ошибки исполнения (не валидация):
`TIMEOUT: ...`, `ERROR: ...`, `IOError: ...`, `SYNTAX: ...`.

| Ошибка | Значение | Что делать |
|---|---|---|
| `UsageError: missing required argument 'x'` | обязательный аргумент отсутствует/пуст | смотри строку Usage |
| `UsageError: unknown action 'x'` | action не из списка инструмента | используй перечисленные |
| `UsageError: invalid realm 'x'` | realm не server/client/menu | выбери валидный |
| `TIMEOUT: menu realm did not respond` | menu-state спит / мост мёртв | вернись в главное меню, разверни окно |
| `TIMEOUT (15s): no reply from game` | реалм не загружен / нет аддона | загрузи карту / проверь gmod_mcp |
| `ERROR: js_eval timeout ...` | страничный `lua.Run`→`GMCP_outh` мёртв | страница меню не готова — ретрай после рабочего `state` |

---

# Инструменты MCP

## gmod_lua — произвольный Lua

```
gmod_lua(realm='server'|'client'|'menu', code='<lua>')
```

| Параметр | Тип | Обяз. | Примечание |
|---|---|---|---|
| realm | enum | да | см. таблицу реалмов |
| code | string | да | Lua-чанк через `RunString` |

**Возврат:** захваченный print-вывод + `=> <return>`.

**Пример:**
```
gmod_lua(realm='server', code='return game.GetMap()')
→ '=> gm_flatgrass'
```

**Краевые случаи:** синтаксические ошибки → `SYNTAX: ...`; рантайм —
`ERROR: ...`; в menu-реалме нет `CompileString`/`loadstring` — код
идёт только через `RunString`.

---

## gmod_console — консольная команда

```
gmod_console(realm='server'|'client'|'menu', command='sv_cheats 1')
```

| Параметр | Тип | Обяз. | Примечание |
|---|---|---|---|
| realm | enum | да | client → `RunConsoleCommand`, server → `game.ConsoleCommand`, menu → `RunConsoleCommand` в menu-state |
| command | string | да | `map gm_flatgrass`, `disconnect`, `sv_cheats 1` |

**Краевые случаи:** серверные restricted/cheat-кваpы действуют как
обычно; menu-реалм принимает движковые `map`, `disconnect`, `retry`,
`gamemode`, `gamemenucommand`.

---

## gmod_screenshot

```
gmod_screenshot(target='menu')
gmod_screenshot(target='world', x=<n>, y=<n>, z=<n>
    [, pitch, yaw, roll, fov=75, hide_hud=true, drawviewer=false])
```

| Параметр | Тип | Обяз. | Default | Примечание |
|---|---|---|---|---|
| target | enum | нет | `world` | `world` = сцена в карте, `menu` = главное меню |
| x, y, z | number | world | — | позиция камеры (Source units) |
| pitch, yaw, roll | number | нет | 0 | углы камеры |
| fov | number | нет | 75 | градусы |
| hide_hud | bool | нет | true | спрятать HUD |
| drawviewer | bool | нет | false | рисовать модель игрока |

**Возврат:** JPEG-изображение + текстовый ack.

**Краевые случаи:** для `world` обязательны `x,y,z,pitch,yaw` — пропуск
даёт UsageError со списком. `menu` не требует аргументов и работает
без загруженной карты (движковый `screenshot` →
`screenshots/Snapshot*.tga`; `render.Capture` в menu-state нет).

---

## gmod_view — удобные виды игрока

```
gmod_view(mode='orbit' [, dist=110, yaw=0, pitch=15, target_height=45, fov=60])
gmod_view(mode='fp' [, fov=75])
```

| Параметр | Тип | Default | Примечание |
|---|---|---|---|
| mode | enum | `orbit` | `orbit` или `fp` — прочее = UsageError |
| dist | number | 110 | дистанция орбитальной камеры |
| yaw | number | 0 | 0 = спереди лица, 180 = со спины |
| pitch | number | 15 | наклон камеры вниз |
| target_height | number | 45 | высота точки прицеливания над ногами |
| fov | number | 60/75 | |

**Краевые случаи:** без игрока отвечает `NO PLAYER`; `fp` аппроксимирует
высоту глаз (+64 единицы).

---

## gmod_camera — постоянная отладочная камера

```
gmod_camera(enabled=true, x, y, z [, pitch, yaw, fov, drawviewer=true])
gmod_camera(enabled=false)
```

Остаётся активной до выключения — не забудь вернуть вид.

---

## gmod_errors

```
gmod_errors()
```

Читает **и очищает** накопленные Lua-ошибки обоих карт-реалмов.
Всегда очищай перед воспроизведением, чтобы старый шум не путать с
новым.

---

## gmod_status

```
gmod_status()
→ 'gmod running=True server=alive client=alive menu=alive | gamemode=sandbox fullscreen=1'
```

**Краевой случай:** `gamemode` может отражать ConVar, а не загруженный
режим — проверяй `engine.ActiveGamemode()` в server realm.

---

## gmod_launch / gmod_restart / gmod_map

```
gmod_launch([mapname='gm_construct', gamemode='', extra_args=''])
gmod_restart([mapname='gm_construct', gamemode=''])
gmod_map(map='gm_flatgrass' [, gamemode=''])
```

| Параметр | Примечание |
|---|---|
| mapname/map | имя карты без `.bsp`; пустой `mapname` = загрузка в главное меню |
| gamemode | имя папки геймода (`sandbox`, `jazztronauts`, ...) |
| extra_args | добавляется в командную строку как есть (`+maxplayers 2`) |

**Краевые случаи:**
- `gmod_launch` отвечает `already running`, если процесс игры жив.
- `gmod_restart` не имеет `extra_args` — для Jazztronauts используй
  `gmod_launch` с `+maxplayers 2` (singleplayer запрещён геймодом).
- `gmod_map` в игре использует `changelevel`; пустой `map` — UsageError.
- `changelevel` может молча не сработать — проверяй `game.GetMap()`.

---

## gmod_display

```
gmod_display(mode='get')
gmod_display(mode='set', fullscreen=0|1 [, width, height])
```

`set` без `fullscreen`/`width`+`height` — UsageError ("nothing to
apply"). Применяется через `mat_setvideomode` (перезапуск рендера).

---

## gmod_player — управление локальным игроком

Нужен игрок на карте. Клавиши — client realm (`+команды`), состояние —
server realm (`player.GetAll()[1]`).

| action | Параметры | Realm | Возврат |
|---|---|---|---|
| `info` | — | server | `pos ang vel hp armor ground movetype weapon` (movetype 2=WALK, 8=NOCLIP) |
| `move` | `dir` = `forward`/`back`/`left`/`right` или массив; `dur` с (def 1.0); `run` bool | client | зажимает `+forward`/`+moveleft`/`+speed` и отпускает по `timer.Simple` |
| `look` | `pitch`,`yaw` абсолютные; `rel=true` дельта; или `x,y,z` точка мира | server+client | итоговые углы |
| `teleport` | `x,y,z` [, `yaw`] | server | обнуляет velocity |
| `noclip` | `on` bool (без = toggle) | server | `SetMoveType` — без sv_cheats |
| `press` | `key` = jump/use/attack/attack2/reload/duck/walk/speed/zoom; `dur` (def 0.2) | client | разовое нажатие |
| `hold` | `cmd` = имя `+команды` (напр. `forward`); `on=false` отпускает | client | произвольный зажим |
| `stop` | — | client | отпускает ВСЕ `+`команды — антидот залипших клавиш |
| `speed` | `walk`/`run`/`jump` (≥1 обязателен) | server | новые скорости |
| `health` | `hp`/`armor`/`god` (≥1 обязателен) | server | новые показатели |
| `give` | `class` = класс entity `[a-z0-9_.-]`; `select=true` в руки | server | `gave <class>` или `give failed` |

**Краевые случаи:**
- `look rel=true`: дельта считается на сервере, клиенту отдаётся
  готовый абсолютный угол — иначе клиент применил бы дельту поверх
  уже повёрнутого сервером угла.
- `give` законно возвращает `give failed` — геймод jazztronauts
  ограничивает `Give()`; это не баг моста.
- Нет игрока → `no player` в тексте ответа.

---

## gmod_menu — автоматизация главного меню (ПРОВЕРЕНО В ИГРЕ)

Главное меню — AngularJS-страница (`garrysmod/html/menu.html`) внутри
DHTML-панели `pnlMainMenu`. Мост работает в обе стороны: Lua→JS через
`pnlMainMenu.HTML:QueueJavascript`, JS→Lua через биндинг
`window.lua.Run` → `GMCP_outh` → `MsgN` → console.log.

### actions

| action | Параметры | Возврат / эффект |
|---|---|---|
| `buttons` | — | все видимые кликабельные элементы: `press "name" — "text" @ (x,y) wxh` + текущий route и флаг inGame |
| `press` | `name` (из `buttons`), `times`=1, `interval_ms`=120 | шлёт mousedown+mouseup+click в центр кнопки `times` раз |
| `navigate` | `route` (слэши опциональны) | ставит `location.hash='#/<route>/'` |
| `click` | `selector` = CSS-селектор | клик по первому совпадению; `no element matching` иначе |
| `js` | `code` | fire-and-forget JS на странице меню |
| `js_eval` | `code` = выражение или IIFE | `{"ok":true,"result":...}` / `{"ok":false,"error":...}` |
| `state` | — | `{route, inGame, map, gamemode, gamemodeTitle, version, branch, problems}` |
| `loading` | — | `{loading, status, downloads}` во время загрузки карты |
| `start_map` | `map` обяз.; `gamemode`, `maxplayers`=1, `lan`=true, `hostname`, `cheats` | повторяет точную последовательность «Начать игру» (ниже) |
| `gamemode` | `name` | convar `gamemode <name>`; проверка — `state.gamemode` |
| `list_maps` | `category` — фильтр-подстрока | JSON `{Категория:[карты]}` из `GetMapList()` |
| `list_gamemodes` | — | JSON-массив `engine.GetGamemodes()` |
| `connect` | `address` = `ip:port`, `password` опц. | `connect` (с `password` перед, если задан) |
| `disconnect` | — | выход из сессии в меню |
| `quit` | — | `engine.CloseGame()` — закрывает GMod |
| `gameui` | `command` | `RunGameUICommand(cmd)` — `OpenOptionsDialog`, `OpenServerBrowser`, `QuitNoConfirm`, ... |

### Имена кнопок (как формируются `name` для `press`)

`buttons` выводит стабильное семантичное имя элемента:
`id` → функция `ng-click` → роут ссылки → slug текста → `btn_<i>`.
Дубликаты получают `_2`, `_3`, ... Типичные имена главного экрана:
`newgame`, `servers`, `addons`, `dupes`, `saves`, `demos`,
`menuoption` (настройки), `menuoption_2` (выход), `BackToMenu`,
`startgame`, `selectmap`, `switchcategory`, `togglegamemodes`,
`toggleproblems`, `togglegames`, `togglelanguage`, `version`.

### Рецепт `start_map`

Побайтово та же последовательность, что у родной кнопки «Начать игру»
(`control.NewGame.js`):
`hook.Run('StartGame')` → `progress_enable` → `disconnect` →
`maxplayers N` → `gamemode <gm>` (если задан) → `sv_lan 0/1` →
`hostname` (если задан) → `sv_cheats 0/1` → `map <name>`.

**Для jazztronauts всегда `maxplayers≥2`** — геймод запрещает
singleplayer.

### Рецепты

**Запуск jazztronauts через меню:**
```
gmod_menu action=gamemode name=jazztronauts
gmod_menu action=start_map map=jazz_bar gamemode=jazztronauts maxplayers=2
```

**Проход по меню кнопками:**
```
gmod_menu action=buttons             # читаем имена и координаты
gmod_menu action=press name=servers  # → экран серверов
gmod_menu action=buttons             # новый набор кнопок экрана
gmod_menu action=state               # проверка route/inGame
```

**Чтение любых данных меню:**
```
gmod_menu action=js_eval code='gScope.MapList.Sandbox.length'
```

---

## Отладка «мост не отвечает» — decision tree

1. `cmd_menu_*.json` копятся → menu-realm спит (в игре / свёрнуто
   окно) или poll-таймер мёртв → смотри хвост `console.log` на
   `[MENU ERROR]` / `Timer Failed! [GmodMcpMenuPoll]`.
2. Команды съедаются, ответа нет → для menu ищи `GMCPO|` в
   `console.log`; для srv/cl — `out_<id>.txt` (0 байт = сбой записи).
3. `Timer Failed!` сразу после команды → обработчик упал; стек в логе
   покажет строку в `gmod_mcp_menu.lua` / `gmod_mcp.lua`.
4. Совсем тихо → `init_menu.lua` больше не инклудит файл / игра без
   `-condebug` / реалм ещё не загружен.
5. `js_eval` таймаутится, но `lua` работает → страница меню
   (`pnlMainMenu`) не готова или нет `window.lua` → ретрай после
   ответа `state`.

---

## Безопасность

Мост = remote code execution с полным доступом к Lua API игры.
Только локальная отладка; `gmod_mcp` никогда не публикуется.
