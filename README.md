# GMod debug bridge: gmod_mcp (addon) + gmod-mcp (MCP server)

**English** | [Русский](README_RU.md)

A single system for driving a running Garry's Mod instance from the
outside:

- **`gmod_mcp/`** — a Lua addon living inside the game across all
  three Lua realms (server / client / menu). Reads commands, executes
  them, writes replies.
- **`gmod-mcp/`** — a Rust binary implementing the MCP protocol and
  the command transport (files in `data/gmod_mcp/`, menu-realm
  replies via `console.log`). Registered as MCP server `gmod` in
  `mcp_config.json`, pointing at `_devtools/gmod-mcp/gmod-mcp.exe`.

Neither half works without the other: the addon is the executor, the
server is the only front end.

> **WARNING: this is remote code execution with full access to the
> game's Lua API.** Local debugging only. `gmod_mcp` is never
> published; before releasing any addon verify it is not in the build
> (`addon.txt` says exactly that: "remove before publishing").

---

## Requirements

| Requirement | Why |
|---|---|
| GMod launched with `-condebug` | The menu realm replies via `MsgN` lines written into `garrysmod/console.log`. Without the flag there is no log and every menu call times out. |
| `gmod_mcp` addon installed | The in-game half of the bridge (see Installation). |
| `GMOD_DATA` (opt.) | Overrides the `garrysmod/data` path. |
| `GMOD_EXE` (opt.) | Overrides exe lookup: `gmod_win64.exe` → `bin/win64/gmod.exe` → `gmod.exe` in the GarrysMod root. |

## Installing gmod_mcp

Source: `_devtools/gmod-mcp/addon/`. Installed copies:

| File | Destination | Realm |
|---|---|---|
| `lua/autorun/gmod_mcp.lua` | `garrysmod/addons/gmod_mcp/` | server + client (in-map) |
| `lua/menu/gmod_mcp_menu.lua` | `garrysmod/lua/menu/` | menu-state |

Menu-state **does not support addons** — `gmod_mcp_menu.lua` is loaded
by an `include("menu/gmod_mcp_menu.lua")` line inside the engine file
`garrysmod/lua/includes/init_menu.lua`. If the menu bridge is dead,
check that the include survived a game update and that the installed
file matches the source (`diff` both copies).

## Building & deploying gmod-mcp

```bash
cd _devtools/gmod-mcp
CARGO_TARGET_DIR=C:/Users/1/.cargo-target/gmod-mcp cargo build --release
```

- `CARGO_TARGET_DIR` — **ASCII path only**: the mingw linker fails on
  Cyrillic in the repo path.
- Toolchain: `stable-x86_64-pc-windows-gnu` (pinned in rust-toolchain.toml).
- Deploy: Windows refuses to overwrite a running exe — rename the old
  one to `gmod-mcp.old.exe`, then copy the new binary into place as
  `gmod-mcp.exe`. The MCP client picks it up on the next server
  respawn (or kill `gmod-mcp.exe` — the client recreates it on call).

---

## Realms

Every command executes inside one of the game's Lua realms:

| realm | Where it runs | When alive |
|---|---|---|
| `server` | server-state (map session) | a map is loaded |
| `client` | client-state (map session) | a map is loaded |
| `menu` | menu-state (main menu) | **only while GameUI is visible and the window is not minimized** |

> **Menu caveat.** While in-game, on the loading screen, or with the
> window minimized, menu-state does not tick. Commands accumulate in
> `cmd_menu_*.json` and run when the realm wakes up — the call still
> reports TIMEOUT, so **do not retry blindly**: the command will
> execute twice.

---

## Bridge protocol (low level)

### Commands (all realms)

Drop `garrysmod/data/gmod_mcp/cmd_<realm>_<id>.json`:

```json
{"id": "<12 hex>", "kind": "lua", "code": "return 1+1"}
```

Realm prefixes: `cmd_srv_`, `cmd_cl_`, `cmd_menu_`. A poll timer
(~0.02 s) scans the directory, reads the file, deletes it, runs the
command, and writes the reply. Malformed JSON is discarded silently
(the file is still consumed).

### Replies

| Realm | Channel |
|---|---|
| server / client | `out_<id>.txt` — plain text: captured `print` output, then `=> <return>` |
| menu | `GMCPO|<id>|<seq>/<total>|<hex>` lines in `garrysmod/console.log`; hex chunks of 1800 chars, reassembled seq=1..total |

**Why the menu channel is a log file:** `file.Write` is broken in
menu-state — its `includes/extensions/file.lua` wrapper calls
`file.Open(path,"wb","DATA")`, which *creates* the file, but the
returned handle has no `:Write` method — the call raises a runtime
error that kills whichever timer invoked it. `MsgN` is engine-logged
into console.log and works reliably.

### Command kinds

**In every realm:**

| kind | Fields | Effect | Reply |
|---|---|---|---|
| `lua` | `code` | `RunString` the code (menu has only RunString — no CompileString/loadstring) inside `pcall` | print output + `=>` return |
| `console` | `command` | server: `game.ConsoleCommand(str)`; client/menu: `RunConsoleCommand(unpack(words))` | `menu console: <cmd>` / `console: <cmd>` |
| `shot` | `pos`,`ang`,`fov`,`hud`,`drawviewer` (in-map) | renders and saves a screenshot | `shot_<id>.jpg` |
| `cam` | `on`,`pos`,`ang`,`fov` | persistent debug camera (client) | ack |
| `batch` | `cmds` = array of command objects | runs every sub-command in one round trip; replies joined as `[i] <result>` | joined text |

**Menu-realm only:**

| kind | Fields | Effect | Reply |
|---|---|---|---|
| `js` | `code` | `pnlMainMenu.HTML:QueueJavascript(code)` — arbitrary JS on the menu page | `ok (queued)` |
| `js_eval` | `code` | eval JS; injected wrapper returns the result via `window.lua.Run("GMCP_outh('<id>','<hex>')")` | `{"ok":bool,"result"|"error":...}` |
| `shot` | — | engine `screenshot` → `garrysmod/screenshots/Snapshot*.tga`, returns the file name | `ok <Snapshot.tga>` |

`js_eval` details:

- `code` runs inside `(function(){ ... eval("code") ... })()` — pass
  an expression or an IIFE with a `return`.
- Result — JSON.stringify → UTF-8 → hex before `lua.Run` — mandatory,
  because `lua.Run(fmt,...)` performs `fmt:format(args)` and any `%`
  in the data breaks the call.
- A `timer.Simple(8)` safety writes
  `ERROR: js_eval timeout (lua.Run/GMCP_outh channel dead?)` if the
  page never answers (menu not loaded, dead binding).

### Menu-state capability matrix (measured live)

**PRESENT:** `RunString`, `file.Read/Find/Delete/CreateDir`,
`file.Open` (creates a file, but the handle has no write methods),
`MsgN` → console.log, `sql`, `cookie`, `engine.*`, `serverlist.*`,
`steamworks.*`, `gui.*`, `permissions.*`, vgui/derma, `timer`, `hook`,
`concommand`, `RunConsoleCommand`, `pnlMainMenu` (with `.HTML`; both
`QueueJavascript` and `RunJavascript` exist), `IsInGame`,
`IsHostingGame`, `GetMapList`, `GetLoadStatus`, `IsInLoading`,
`gameevent.Listen`.

**MISSING:** `Entity`, `Player`, `net.*`, `render.Capture`,
`CompileString`, `loadstring`, `http`, writing `file.Open`/
`file.Append`/`file.Write`, `print` → log (print goes into the
bridge buffer; only `MsgN` reaches console.log).

### Known menu-state traps

- **`window.lua.Run(fmt, ...)` calls `fmt:format(args)`** — a `%`
  anywhere in the data kills the call. All JS→Lua traffic is
  therefore hex (ASCII only).
- **`RunString` does not isolate errors in menu-state** — an
  unguarded nil call kills the invoking tick/timer. The bridge wraps
  code in `pcall` twice and the whole dispatcher once more; a dead
  poll timer was historically the #1 cause of a "dead" realm.
- **Menu-state sleeps** while a map is loaded, during loading, or
  with the window minimized — commands accumulate in
  `cmd_menu_*.json` and run on wakeup. TIMEOUT does not mean "lost" —
  do not retry blindly.
- Menu screenshots go through the engine `screenshot` command only
  (TGA in `garrysmod/screenshots/`); `render.Capture` is unavailable.
- `console.log` inside the page → `DHTML.ConsoleMessage`; the menu
  page's `lua.Run` binding is registered in `mainmenu.lua` via
  `AddFunction`.

### Hot-swapping the bridge without restarting the game

Via a working `kind=lua`:

```lua
GMOD_MCP_MENU_LOADED=nil
local s = file.Read("lua/menu/gmod_mcp_menu.lua","GAME")
if s then RunString(s) end
```

`timer.Create("GmodMcpMenuPoll", ...)` inside the file overwrites the
old ticker with the new code. A `=> hp <bytes>` reply means the file
was re-read and executed.

### Data directory

`garrysmod/data/gmod_mcp/`:

| File | Role |
|---|---|
| `cmd_{srv,cl,menu}_*.json` | command queue (deleted when consumed) |
| `out_<id>.txt` | server/client replies |
| `shot_<id>.jpg` | in-map screenshots |
| `errors.txt` | collected Lua errors (in-map; `gmod_errors` reads and clears) |

---

## MCP response conventions

- `print()` output is captured into the reply.
- A `return`ed value appears after `=> ` on the last line.
- Serialize tables explicitly: `return util.TableToJSON(t)`.
- Calls are synchronous, timeout 12–25 s.

## Error format

All usage errors are in English, single form:

```
UsageError: <what went wrong>
Usage: <tool>(<signature>) — <variants>
```

Execution errors (not validation):
`TIMEOUT: ...`, `ERROR: ...`, `IOError: ...`, `SYNTAX: ...`.

| Error | Meaning | Remedy |
|---|---|---|
| `UsageError: missing required argument 'x'` | required argument absent/empty | see the Usage line |
| `UsageError: unknown action 'x'` | action not in the tool's list | use the listed ones |
| `UsageError: invalid realm 'x'` | realm not server/client/menu | pick a valid one |
| `TIMEOUT: menu realm did not respond` | menu-state asleep / bridge dead | return to main menu, unminimize window |
| `TIMEOUT (15s): no reply from game` | realm not loaded / addon missing | load a map / check gmod_mcp |
| `ERROR: js_eval timeout ...` | page-side `lua.Run`→`GMCP_outh` dead | menu page not ready — retry after a working `state` |

---

# MCP tools

## gmod_lua — arbitrary Lua

```
gmod_lua(realm='server'|'client'|'menu', code='<lua>')
```

| Parameter | Type | Req. | Notes |
|---|---|---|---|
| realm | enum | yes | see realm table |
| code | string | yes | Lua chunk via `RunString` |

**Returns:** captured print output + `=> <return>`.

**Example:**
```
gmod_lua(realm='server', code='return game.GetMap()')
→ '=> gm_flatgrass'
```

**Edge cases:** syntax errors → `SYNTAX: ...`; runtime — `ERROR: ...`;
menu realm lacks `CompileString`/`loadstring` — code goes through
`RunString` only.

---

## gmod_batch — several commands, one round trip

```
gmod_batch(realm='server'|'client'|'menu',
    cmds=[{kind:'lua',code:'...'} | {kind:'console',command:'...'}])
```

| Parameter | Type | Req. | Notes |
|---|---|---|---|
| realm | enum | yes | see realm table |
| cmds | array | yes | command objects; menu also accepts `js`/`js_eval`, srv/cl accept `shot`/`cam` |

**Returns:** joined sub-replies as `[i] <result>` lines. Sub-commands
without a `kind`-required field (`code`/`command`) → UsageError.
Async kinds (`js_eval`, `shot`) reply under `<cid>_<i>`, not inside
the joined text.

**Edge cases:** nested `batch` kind is rejected; console commands are
queued by the engine — reading a cvar right after setting it in the
same batch shows the old value.

---

## gmod_console — console command

```
gmod_console(realm='server'|'client'|'menu', command='sv_cheats 1')
```

| Parameter | Type | Req. | Notes |
|---|---|---|---|
| realm | enum | yes | client → `RunConsoleCommand`, server → `game.ConsoleCommand`, menu → `RunConsoleCommand` in menu-state |
| command | string | yes | `map gm_flatgrass`, `disconnect`, `sv_cheats 1` |

**Edge cases:** server-side restricted/cheat cvars behave as usual;
menu realm accepts engine `map`, `disconnect`, `retry`, `gamemode`,
`gamemenucommand`.

---

## gmod_screenshot

```
gmod_screenshot(target='menu')
gmod_screenshot(target='world', x=<n>, y=<n>, z=<n>
    [, pitch, yaw, roll, fov=75, hide_hud=true, drawviewer=false])
```

| Parameter | Type | Req. | Default | Notes |
|---|---|---|---|---|
| target | enum | no | `world` | `world` = in-map scene, `menu` = main menu |
| x, y, z | number | world | — | camera position (Source units) |
| pitch, yaw, roll | number | no | 0 | camera angles |
| fov | number | no | 75 | degrees |
| hide_hud | bool | no | true | hide the HUD |
| drawviewer | bool | no | false | render the player model |

**Returns:** a JPEG image + a text ack.

**Edge cases:** `world` requires `x,y,z,pitch,yaw` — omitting them
yields a UsageError with the list. `menu` takes no arguments and works
with no map loaded (engine `screenshot` →
`screenshots/Snapshot*.tga`; no `render.Capture` in menu-state).

---

## gmod_view — convenient player views

```
gmod_view(mode='orbit' [, dist=110, yaw=0, pitch=15, target_height=45, fov=60])
gmod_view(mode='fp' [, fov=75])
```

| Parameter | Type | Default | Notes |
|---|---|---|---|
| mode | enum | `orbit` | `orbit` or `fp` — anything else = UsageError |
| dist | number | 110 | orbit camera distance |
| yaw | number | 0 | 0 = face front, 180 = behind |
| pitch | number | 15 | downward camera tilt |
| target_height | number | 45 | aim height above the feet |
| fov | number | 60/75 | |

**Edge cases:** with no player it replies `NO PLAYER`; `fp`
approximates eye height (+64 units).

---

## gmod_camera — persistent debug camera

```
gmod_camera(enabled=true, x, y, z [, pitch, yaw, fov, drawviewer=true])
gmod_camera(enabled=false)
```

Stays active until disabled — do not forget to restore the view.

---

## gmod_errors

```
gmod_errors()
```

Reads **and clears** accumulated Lua errors from both map realms.
Always clear before reproduction so stale noise is not confused with
fresh errors.

---

## gmod_status

```
gmod_status()
→ 'gmod running=True server=alive client=alive menu=alive | gamemode=sandbox fullscreen=1'
```

**Edge case:** `gamemode` may reflect a ConVar rather than the loaded
mode — verify `engine.ActiveGamemode()` in the server realm.

---

## gmod_launch / gmod_restart / gmod_map

```
gmod_launch([mapname='gm_construct', gamemode='', extra_args=''])
gmod_restart([mapname='gm_construct', gamemode=''])
gmod_map(map='gm_flatgrass' [, gamemode=''])
```

| Parameter | Notes |
|---|---|
| mapname/map | map name without `.bsp`; empty `mapname` = launch into the main menu |
| gamemode | gamemode folder name (`sandbox`, `jazztronauts`, ...) |
| extra_args | appended verbatim to the command line (`+maxplayers 2`) |

**Edge cases:**
- `gmod_launch` replies `already running` if the game process is alive.
- `gmod_restart` has no `extra_args` — for Jazztronauts use
  `gmod_launch` with `+maxplayers 2` (singleplayer is forbidden by the
  gamemode).
- `gmod_map` uses `changelevel` in-game; empty `map` — UsageError.
- `changelevel` may silently no-op — verify with `game.GetMap()`.

---

## gmod_display

```
gmod_display(mode='get')
gmod_display(mode='set', fullscreen=0|1 [, width, height])
```

`set` with neither `fullscreen` nor `width`+`height` — UsageError
("nothing to apply"). Applied via `mat_setvideomode` (renderer
restart).

---

## gmod_player — local player control

Requires a spawned player. Keys go to the client realm (`+commands`),
state to the server realm (`player.GetAll()[1]`).

| action | Parameters | Realm | Returns |
|---|---|---|---|
| `info` | — | server | `pos ang vel hp armor ground movetype weapon` (movetype 2=WALK, 8=NOCLIP) |
| `move` | `dir` = `forward`/`back`/`left`/`right` or array; `dur` s (def 1.0); `run` bool | client | holds `+forward`/`+moveleft`/`+speed`, releases via `timer.Simple` |
| `look` | `pitch`,`yaw` absolute; `rel=true` delta; or `x,y,z` world point | server+client | resulting angles |
| `teleport` | `x,y,z` [, `yaw`] | server | zeroes velocity |
| `noclip` | `on` bool (omit = toggle) | server | `SetMoveType` — no sv_cheats needed |
| `press` | `key` = jump/use/attack/attack2/reload/duck/walk/speed/zoom; `dur` (def 0.2) | client | single keypress |
| `hold` | `cmd` = `+command` name (e.g. `forward`); `on=false` releases | client | arbitrary hold |
| `stop` | — | client | releases ALL `+commands` — antidote for stuck keys |
| `speed` | `walk`/`run`/`jump` (≥1 required) | server | new speeds |
| `health` | `hp`/`armor`/`god` (≥1 required) | server | new values |
| `give` | `class` = entity class `[a-z0-9_.-]`; `select=true` into hands | server | `gave <class>` or `give failed` |

**Edge cases:**
- `look rel=true`: the delta is computed on the server and the client
  receives a ready-made absolute angle — otherwise the client would
  apply the delta on top of the angle the server already rotated.
- `give` legitimately returns `give failed` — the jazztronauts
  gamemode restricts `Give()`; not a bridge bug.
- No player → `no player` in the reply text.

---

## gmod_menu — main-menu automation (VERIFIED IN GAME)

The main menu is an AngularJS page (`garrysmod/html/menu.html`) inside
the `pnlMainMenu` DHTML panel. The bridge works both ways: Lua→JS via
`pnlMainMenu.HTML:QueueJavascript`, JS→Lua via the `window.lua.Run`
binding → `GMCP_outh` → `MsgN` → console.log.

### actions

| action | Parameters | Returns / effect |
|---|---|---|
| `buttons` | — | every visible clickable element: `press "name" — "text" @ (x,y) wxh` plus current route and inGame flag |
| `press` | `name` (from `buttons`), `times`=1, `interval_ms`=120 | sends mousedown+mouseup+click to the button center `times` times |
| `navigate` | `route` (slashes optional) | sets `location.hash='#/<route>/'` |
| `click` | `selector` = CSS selector | clicks the first match; `no element matching` otherwise |
| `js` | `code` | fire-and-forget JS on the menu page |
| `js_eval` | `code` = expression or IIFE | `{"ok":true,"result":...}` / `{"ok":false,"error":...}` |
| `state` | — | `{route, inGame, map, gamemode, gamemodeTitle, version, branch, problems}` |
| `loading` | — | `{loading, status, downloads}` during map load |
| `start_map` | `map` req.; `gamemode`, `maxplayers`=1, `lan`=true, `hostname`, `cheats` | replays the exact "Start Game" sequence (below) |
| `gamemode` | `name` | convar `gamemode <name>`; verify via `state.gamemode` |
| `list_maps` | `category` — substring filter | JSON `{Category:[maps]}` from `GetMapList()` |
| `list_gamemodes` | — | JSON array from `engine.GetGamemodes()` |
| `connect` | `address` = `ip:port`, `password` opt. | `connect` (with `password` first, if given) |
| `disconnect` | — | leave the session back to menu |
| `quit` | — | `engine.CloseGame()` — exits GMod |
| `gameui` | `command` | `RunGameUICommand(cmd)` — `OpenOptionsDialog`, `OpenServerBrowser`, `QuitNoConfirm`, ... |

### Button names (how `name` for `press` is formed)

`buttons` outputs a stable semantic name per element:
`id` → `ng-click` function → link route → text slug → `btn_<i>`.
Duplicates get `_2`, `_3`, ... Typical main-screen names:
`newgame`, `servers`, `addons`, `dupes`, `saves`, `demos`,
`menuoption` (settings), `menuoption_2` (quit), `BackToMenu`,
`startgame`, `selectmap`, `switchcategory`, `togglegamemodes`,
`toggleproblems`, `togglegames`, `togglelanguage`, `version`.

### The `start_map` recipe

Byte-for-byte the same sequence as the native "Start Game" button
(`control.NewGame.js`):
`hook.Run('StartGame')` → `progress_enable` → `disconnect` →
`maxplayers N` → `gamemode <gm>` (if given) → `sv_lan 0/1` →
`hostname` (if given) → `sv_cheats 0/1` → `map <name>`.

**For jazztronauts always `maxplayers≥2`** — the gamemode forbids
singleplayer.

### Recipes

**Launch jazztronauts via the menu:**
```
gmod_menu action=gamemode name=jazztronauts
gmod_menu action=start_map map=jazz_bar gamemode=jazztronauts maxplayers=2
```

**Walk the menu by buttons:**
```
gmod_menu action=buttons             # read names and coordinates
gmod_menu action=press name=servers  # → server browser screen
gmod_menu action=buttons             # the screen's new button set
gmod_menu action=state               # check route/inGame
```

**Read any menu data:**
```
gmod_menu action=js_eval code='gScope.MapList.Sandbox.length'
```

---

## Debugging "bridge not responding" — decision tree

1. `cmd_menu_*.json` piling up → menu realm asleep (in-game /
   minimized window) or dead poll timer → check the `console.log`
   tail for `[MENU ERROR]` / `Timer Failed! [GmodMcpMenuPoll]`.
2. Commands consumed, no reply → for menu look for `GMCPO|` in
   `console.log`; for srv/cl — `out_<id>.txt` (0 bytes = write failure).
3. `Timer Failed!` right after a command → handler crashed; the stack
   in the log points at a line in `gmod_mcp_menu.lua` / `gmod_mcp.lua`.
4. Total silence → `init_menu.lua` no longer includes the file / game
   launched without `-condebug` / realm not loaded yet.
5. `js_eval` times out but `lua` works → the menu page
   (`pnlMainMenu`) is not ready or `window.lua` is missing → retry
   after `state` answers.

---

## Security

The bridge is remote code execution with full access to the game's
Lua API. Local debugging only; `gmod_mcp` is never published.
