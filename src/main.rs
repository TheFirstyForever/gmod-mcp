//! MCP-сервер моста к Garry's Mod через debug-аддон gmod_mcp.
//!
//! Протокол: newline-delimited JSON-RPC 2.0 (MCP stdio).
//! Транспорт в игру: файлы в garrysmod/data/gmod_mcp/
//!   cmd_srv_<id>.json / cmd_cl_<id>.json  — команды (server/client realm)
//!   out_<id>.txt                         — текстовый результат
//!   shot_<id>.jpg                        — скриншот
//!   errors.txt                           — собранные Lua-ошибки

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde_json::{json, Map, Value};
use std::env;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const PROTOCOL_VERSION: &str = "2024-11-05";
const GMOD_PROCS: [&str; 3] = ["gmod_win64.exe", "gmod.exe", "hl2.exe"];

fn gmod_data() -> PathBuf {
    static P: OnceLock<PathBuf> = OnceLock::new();
    P.get_or_init(|| {
        env::var_os("GMOD_DATA").map(PathBuf::from).unwrap_or_else(|| {
            PathBuf::from(
                r"C:\Program Files (x86)\Steam\steamapps\common\GarrysMod\garrysmod\data",
            )
        })
    })
    .clone()
}

fn dir() -> PathBuf {
    gmod_data().join("gmod_mcp")
}

fn gmod_exe() -> String {
    static P: OnceLock<String> = OnceLock::new();
    P.get_or_init(|| {
        if let Ok(v) = env::var("GMOD_EXE") {
            return v;
        }
        const ROOT: &str = r"C:\Program Files (x86)\Steam\steamapps\common\GarrysMod";
        for c in ["gmod_win64.exe", "bin/win64/gmod.exe", "gmod.exe"] {
            let p = format!("{ROOT}/{c}");
            if PathBuf::from(&p).exists() {
                return p;
            }
        }
        format!("{ROOT}/gmod.exe")
    })
    .clone()
}

fn tools() -> Value {
    json!([
        {
            "name": "gmod_lua",
            "description": (
                "Выполнить Lua-код внутри запущенного GMod. realm='server' или 'client'. \
                В коде доступно всё API GMod; print() возвращается в ответ, \
                return-значение — после '=>'. Для таблиц: return util.TableToJSON(t)."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "realm": {"type": "string", "enum": ["server", "client", "menu"]},
                    "code": {"type": "string", "description": "Lua-код для выполнения"},
                },
                "required": ["realm", "code"],
            },
        },
        {
            "name": "gmod_batch",
            "description": (
                "Несколько команд за один round-trip. cmds — массив объектов: \
                {kind:'lua',code:'...'} | {kind:'console',command:'...'}; для menu \
                также {kind:'js'|'js_eval',code:'...'}, для srv/cl — shot/cam. \
                Ответы склеиваются строками '[i] <result>'; асинхронные kind'ы \
                отвечают под id '<cid>_<i>'."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "realm": {"type": "string", "enum": ["server", "client", "menu"]},
                    "cmds": {"type": "array", "items": {"type": "object"},
                             "description": "[{kind:'lua',code:'...'}|{kind:'console',command:'...'}]"},
                },
                "required": ["realm", "cmds"],
            },
        },
        {
            "name": "gmod_screenshot",
            "description": (
                "Скриншот. target='world' (default): сцена из произвольной точки мира \
                (Source units), drawviewer=true рисует модель локального игрока, \
                fov в градусах, hud=false прячет HUD. target='menu': скриншот \
                главного меню GMod (работает без загруженной карты)."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": {"type": "string", "enum": ["world", "menu"], "default": "world"},
                    "x": {"type": "number"}, "y": {"type": "number"}, "z": {"type": "number"},
                    "pitch": {"type": "number", "description": "наклон камеры, +вверх/-вниз"},
                    "yaw": {"type": "number"},
                    "roll": {"type": "number", "default": 0},
                    "fov": {"type": "number", "default": 75},
                    "drawviewer": {"type": "boolean", "default": false},
                    "hide_hud": {"type": "boolean", "default": true},
                },
                "required": [],
            },
        },
        {
            "name": "gmod_view",
            "description": (
                "Удобные виды вокруг локального игрока. \
                mode='orbit': камера на расстоянии dist от игрока, yaw/pitch относительно \
                взгляда игрока, смотрит на высоту target_height над ногами. \
                mode='fp': вид от первого лица (как видит игрок, с руками/вьюмоделью)."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "mode": {"type": "string", "enum": ["orbit", "fp"], "default": "orbit"},
                    "dist": {"type": "number", "default": 110},
                    "yaw": {"type": "number", "default": 0, "description": "0=спереди лица, 180=со спины"},
                    "pitch": {"type": "number", "default": 15, "description": "наклон камеры вниз"},
                    "target_height": {"type": "number", "default": 45},
                    "fov": {"type": "number", "default": 60},
                },
            },
        },
        {
            "name": "gmod_camera",
            "description": (
                "Постоянная отладочная камера: enabled=true переносит вид в точку \
                (остаётся, пока не выключить), false — вернуть обычный вид."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "enabled": {"type": "boolean"},
                    "x": {"type": "number"}, "y": {"type": "number"}, "z": {"type": "number"},
                    "pitch": {"type": "number", "default": 0},
                    "yaw": {"type": "number", "default": 0},
                    "fov": {"type": "number", "default": 75},
                    "drawviewer": {"type": "boolean", "default": true},
                },
                "required": ["enabled"],
            },
        },
        {
            "name": "gmod_errors",
            "description": "Прочитать и очистить собранные Lua-ошибки из игры (оба реалма).",
            "inputSchema": {"type": "object", "properties": {}},
        },
        {
            "name": "gmod_console",
            "description": (
                "Выполнить консольную команду/конвар. realm='client' -> RunConsoleCommand, \
                'server' -> game.ConsoleCommand, 'menu' -> работает даже в главном меню \
                (map/retry/disconnect без загруженной карты)."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "realm": {"type": "string", "enum": ["server", "client", "menu"]},
                    "command": {"type": "string", "description": "например 'sv_cheats 1'"},
                },
                "required": ["realm", "command"],
            },
        },
        {
            "name": "gmod_status",
            "description": (
                "Состояние игры и debug-моста. Поле gamemode может отражать \
                ConVar, а не загруженный режим: проверяй engine.ActiveGamemode() \
                в server realm. См. AGENTS.md."
            ),
            "inputSchema": {"type": "object", "properties": {}},
        },
        {
            "name": "gmod_launch",
            "description": (
                "Запустить GMod. mapname (например 'gm_construct') — сразу грузит карту \
                через +map в аргументах запуска. gamemode — имя папки геймода \
                (sandbox/terrortown/jazztronauts/...), передаётся как +gamemode. \
                Пустой mapname — запуск без карты (сразу главное меню). \
                Для Jazztronauts: mapname='', gamemode='jazztronauts', \
                extra_args='+maxplayers 2'; затем map gm_construct из menu-realm. \
                Иначе +map может выполниться раньше +gamemode, а Singleplayer \
                не подходит геймоду. Подробности в AGENTS.md."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "mapname": {"type": "string", "default": "gm_construct"},
                    "gamemode": {"type": "string", "default": ""},
                    "extra_args": {"type": "string", "default": ""},
                },
            },
        },
        {
            "name": "gmod_restart",
            "description": (
                "Полный перезапуск GMod: завершает процесс игры и запускает заново. \
                РАЗРЕШЁН пользователем для ускорения итераций. mapname — карта для \
                автозагрузки, gamemode — папка геймода (+gamemode). \
                Пустой mapname — запуск без карты (главное меню). \
                Для Jazztronauts нужен +maxplayers 2; у этого инструмента нет \
                extra_args. Закрой игру и используй gmod_launch по AGENTS.md."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "mapname": {"type": "string", "default": "gm_construct"},
                    "gamemode": {"type": "string", "default": ""},
                },
            },
        },
        {
            "name": "gmod_map",
            "description": (
                "Загрузить карту (и опционально сменить геймод). В игре — \
                changelevel без дисконнекта; в главном меню — gamemode+map. \
                gamemode — имя папки геймода (необязательно). Для запуска \
                Jazztronauts сначала создай сервер на 2 слота по AGENTS.md; \
                одного изменения карты недостаточно."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "map": {"type": "string", "description": "имя карты без .bsp"},
                    "gamemode": {"type": "string", "default": ""},
                },
                "required": ["map"],
            },
        },
        {
            "name": "gmod_display",
            "description": (
                "Видео-режим GMod. mode='get' — текущие fullscreen/разрешение; \
                mode='set' — применить: fullscreen (0/1), width, height \
                (через mat_setvideomode, требует перезапуск рендера)."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "mode": {"type": "string", "enum": ["get", "set"], "default": "get"},
                    "fullscreen": {"type": "number", "description": "1=полный экран, 0=оконный"},
                    "width": {"type": "number"},
                    "height": {"type": "number"},
                },
            },
        },
        {
            "name": "gmod_player",
            "description": (
                "Управление локальным игроком (нужен игрок на карте). \
                action='info': pos/ang/vel/hp/armor/ground/movetype/weapon. \
                'move': dir=forward|back|left|right (строка или массив), \
                dur сек (default 1), run=true — зажать +speed. \
                'look': pitch/yaw абсолютные (rel=true — прибавить к текущим); \
                либо x,y,z — смотреть на точку мира. \
                'teleport': x,y,z (+yaw опц.), velocity обнуляется. \
                'noclip': on=true/false/omit=toggle (MOVETYPE_NOCLIP, sv_cheats не нужен). \
                'press': key=jump|use|attack|attack2|reload|duck|walk|speed|zoom, \
                dur (default 0.2). 'hold': cmd='forward' (+/- подставляется), \
                on=false отпускает. 'stop': отпустить ВСЕ клавиши (антизалипание). \
                'speed': walk/run/jump числа. 'health': hp/armor/god. \
                'give': class + select=true взять в руки. \
                Клавиши — client realm (+/-команды); pos/hp/спид — server realm."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["info", "move", "look", "teleport", "noclip",
                                 "press", "hold", "stop", "speed", "health", "give"],
                    },
                    "dir": {"description": "move: forward|back|left|right; строка или массив"},
                    "dur": {"type": "number", "description": "move/press: длительность, сек"},
                    "pitch": {"type": "number"},
                    "yaw": {"type": "number"},
                    "rel": {"type": "boolean", "description": "look: прибавить к текущим углам"},
                    "x": {"type": "number"}, "y": {"type": "number"}, "z": {"type": "number"},
                    "key": {"type": "string", "description": "press: jump|use|attack|attack2|reload|duck|walk|speed|zoom"},
                    "cmd": {"type": "string", "description": "hold: имя +-команды, напр. 'forward'"},
                    "on": {"type": "boolean", "description": "hold/noclip: вкл/выкл"},
                    "walk": {"type": "number"},
                    "run": {"description": "move: зажать +speed (bool); speed: SetRunSpeed (число)"},
                    "jump": {"type": "number", "description": "speed: SetJumpPower"},
                    "hp": {"type": "number"},
                    "armor": {"type": "number"},
                    "god": {"type": "boolean"},
                    "class": {"type": "string", "description": "give: класс entity (weapon_smg1, npc_zombie...)"},
                    "select": {"type": "boolean", "description": "give: сразу взять в руки"},
                },
                "required": ["action"],
            },
        },
        {
            "name": "gmod_menu",
            "description": (
                "Управление главным меню GMod (menu-state + страница меню). \
                ВАЖНО: menu-realm жив только пока виден GameUI — в игре он спит; \
                сначала выйди в меню (gmod_menu disconnect или gmod_console client 'disconnect'). \
                Actions: \
                'buttons' — список ВСЕХ видимых кнопок меню: имя, текст, центр (x,y px), размер. \
                Имена стабильны и семантичны (id/ng-click/route) — нажимай их через press. \
                'press' — нажать кнопку по имени из buttons (name), times=N раз, interval_ms между нажатиями. \
                'navigate' — перейти на экран меню: route=main|newgame|servers|addons|saves|dupes|demos. \
                'click' — клик по произвольному CSS-селектору (selector). \
                'js' — выполнить JS на странице меню без ответа (code). \
                'js_eval' — eval JS с ответом JSON {ok,result|error} (code — выражение или IIFE). \
                'state' — {route,inGame,map,gamemode,version,branch,problems} страницы меню. \
                'start_map' — запуск карты КАК КНОПКА 'Start new game': map, gamemode?, \
                maxplayers?, lan?, hostname?, cheats? — вся цепочка progress_enable/disconnect/map. \
                'connect' — коннект к серверу address (+password?). \
                'disconnect' — выйти в меню из игры. 'quit' — закрыть GMod (engine.CloseGame). \
                'gamemode' — активный геймод (name). 'list_gamemodes' — список. \
                'list_maps' — карты по категориям (category — подстрока, иначе все). \
                'gameui' — RunGameUICommand (command: OpenOptionsDialog|OpenServerBrowser|QuitNoConfirm|Disconnect). \
                'loading' — состояние загрузки карты {loading,status,downloads}."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["buttons", "press", "navigate", "click", "js", "js_eval",
                                 "state", "start_map", "connect", "disconnect", "quit",
                                 "gamemode", "list_gamemodes", "list_maps", "gameui", "loading"],
                    },
                    "code": {"type": "string", "description": "js/js_eval: JS-код (eval — выражение или IIFE с return)"},
                    "name": {"type": "string", "description": "press: имя кнопки из buttons; gamemode: имя геймода"},
                    "selector": {"type": "string", "description": "click: CSS-селектор"},
                    "route": {"type": "string", "description": "navigate: main|newgame|servers|addons|saves|dupes|demos"},
                    "times": {"type": "integer", "description": "press: сколько раз нажать (default 1)"},
                    "interval_ms": {"type": "integer", "description": "press: пауза между нажатиями (default 120)"},
                    "map": {"type": "string", "description": "start_map: имя карты (без .bsp)"},
                    "gamemode": {"type": "string", "description": "start_map: геймод"},
                    "maxplayers": {"type": "integer"},
                    "lan": {"type": "boolean", "description": "start_map: sv_lan (default true)"},
                    "hostname": {"type": "string"},
                    "cheats": {"type": "boolean", "description": "start_map: sv_cheats (default false)"},
                    "address": {"type": "string", "description": "connect: ip:port"},
                    "password": {"type": "string", "description": "connect: пароль сервера"},
                    "category": {"type": "string", "description": "list_maps: фильтр категории (подстрока)"},
                    "command": {"type": "string", "description": "gameui: команда GameUI"},
                },
                "required": ["action"],
            },
        },
    ])
}

/// Python-совместимая truthiness для JSON-значений.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(true, |f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

fn get_f64(a: &Map<String, Value>, k: &str, default: f64) -> f64 {
    a.get(k).and_then(|v| v.as_f64()).unwrap_or(default)
}

/// Значение аргумента как есть (или `default`), без приведения типов — как dict.get в Python.
fn arg_or<'a>(a: &'a Map<String, Value>, k: &str, default: Value) -> Value {
    a.get(k).cloned().unwrap_or(default)
}

/// Дождаться появления файла и окончания записи в него.
fn wait_for(path: &Path, timeout: Duration) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < timeout {
        if path.exists() {
            if let Ok(m1) = fs::metadata(path) {
                std::thread::sleep(Duration::from_millis(10));
                if let Ok(m2) = fs::metadata(path) {
                    if m2.len() == m1.len() {
                        return true;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    false
}

/// 12 hex-символов уникального id команды (как uuid4().hex[:12] в Python-версии).
fn next_cid() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static CTR: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let c = CTR.fetch_add(1, Ordering::Relaxed);
    // xorshift64*: достаточно для уникальности в пределах сессии моста
    let mut x = nanos ^ (c.wrapping_mul(0x9E37_79B9_7F4A_7C15)) ^ ((std::process::id() as u64) << 32);
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    format!("{x:012x}")
}

fn write_cmd(realm: &str, payload: &mut Map<String, Value>) -> Result<String, String> {
    let cid = next_cid();
    payload.insert("id".into(), json!(cid));
    let prefix = match realm {
        "server" => "cmd_srv_",
        "client" => "cmd_cl_",
        "menu" => "cmd_menu_",
        _ => {
            return Err(format!(
                "UsageError: invalid realm '{realm}' — must be 'server', 'client' or 'menu'"
            ))
        }
    };
    let tmp = dir().join(format!("{prefix}{cid}.tmp"));
    let dst = dir().join(format!("{prefix}{cid}.json"));
    let body = serde_json::to_string(payload).map_err(|e| e.to_string())?;
    fs::write(&tmp, body).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &dst).map_err(|e| e.to_string())?;
    Ok(cid)
}

fn read_out(cid: &str, timeout: Duration) -> Option<String> {
    let path = dir().join(format!("out_{cid}.txt"));
    if !wait_for(&path, timeout) {
        return None;
    }
    for _ in 0..20 {
        match fs::read(&path) {
            Ok(bytes) => {
                let txt = String::from_utf8_lossy(&bytes).into_owned();
                let _ = fs::remove_file(&path);
                return Some(txt);
            }
            Err(_) => std::thread::sleep(Duration::from_millis(15)),
        }
    }
    Some("(could not read result file)".into())
}

/// Путь к console.log игры (рядом с data/, при запуске с -condebug).
fn console_log_path() -> PathBuf {
    gmod_data()
        .parent()
        .map(|p| p.join("console.log"))
        .unwrap_or_else(|| PathBuf::from("console.log"))
}

/// Чтение ответа menu-realm. file.Write в menu-state сломан
/// (file.Open создаёт файл, но :Write — nil), поэтому мост пишет
/// ответ строками в garrysmod/console.log через MsgN:
///   GMCPO|<cid>|<seq>/<total>|<hex>
/// Читаем хвост лога с позиции на момент вызова, собираем чанки.
fn read_out_menu(cid: &str, timeout: Duration) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let log = console_log_path();
    let mut pos = fs::metadata(&log).map(|m| m.len()).unwrap_or(0);
    let tag = format!("GMCPO|{cid}|");
    let mut chunks: std::collections::BTreeMap<usize, String> = Default::default();
    let mut total = 0usize;
    let mut buf = String::new();
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(mut f) = fs::File::open(&log) {
            let len = f.metadata().map(|m| m.len()).unwrap_or(0);
            if len < pos {
                pos = 0; // лог урезали/ротировали — перечитать сначала
                buf.clear();
            }
            if f.seek(SeekFrom::Start(pos)).is_ok() {
                let mut bytes = Vec::new();
                if f.read_to_end(&mut bytes).is_ok() {
                    pos += bytes.len() as u64;
                    buf.push_str(&String::from_utf8_lossy(&bytes));
                }
            }
        }
        while let Some(nl) = buf.find('\n') {
            let line = buf[..nl].trim_end_matches('\r').to_string();
            buf.drain(..nl + 1);
            if let Some(rest) = line.strip_prefix(&tag) {
                if let Some((seqt, hex)) = rest.split_once('|') {
                    if let Some((s, t)) = seqt.split_once('/') {
                        if let (Ok(s), Ok(t)) = (s.parse::<usize>(), t.parse::<usize>()) {
                            total = t;
                            chunks.insert(s, hex.to_string());
                        }
                    }
                }
            }
        }
        if total > 0 && chunks.len() >= total {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    if total == 0 || chunks.len() < total {
        return None;
    }
    let mut hex = String::new();
    for i in 1..=total {
        hex.push_str(chunks.get(&i).map(String::as_str).unwrap_or(""));
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for pair in hex.as_bytes().chunks_exact(2) {
        if let Ok(v) = u8::from_str_radix(std::str::from_utf8(pair).unwrap_or("00"), 16) {
            bytes.push(v);
        }
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Отправка команды в menu-realm + ожидание ответа (hex-канал).
fn menu_send(payload: &mut Map<String, Value>, timeout: Duration) -> String {
    match write_cmd("menu", payload) {
        Ok(cid) => read_out_menu(&cid, timeout).unwrap_or_else(|| {
            "TIMEOUT: menu realm did not respond. Menu state only ticks while the \
             GameUI (main menu) is visible and the window is not minimized; \
             in-game it sleeps — disconnect to menu first."
                .into()
        }),
        Err(e) => e,
    }
}

fn menu_lua(code: &str, timeout: Duration) -> String {
    let mut p = Map::new();
    p.insert("kind".into(), json!("lua"));
    p.insert("code".into(), json!(code));
    menu_send(&mut p, timeout)
}

fn menu_js(code: &str) -> String {
    let mut p = Map::new();
    p.insert("kind".into(), json!("js"));
    p.insert("code".into(), json!(code));
    menu_send(&mut p, Duration::from_secs(15))
}

fn menu_js_eval(code: &str, timeout: Duration) -> String {
    let mut p = Map::new();
    p.insert("kind".into(), json!("js_eval"));
    p.insert("code".into(), json!(code));
    menu_send(&mut p, timeout)
}

/// Lua-строковый литерал ("-квотинг).
fn lua_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"))
}

/// JS-строковый литерал ('-квотинг для вставки в eval-обёртки).
fn js_str(s: &str) -> String {
    format!(
        "'{}'",
        s.replace('\\', "\\\\")
            .replace('\'', "\\'")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    )
}

// Сборщик кликабельных элементов меню → массив {name,text,x,y,w,h,el}.
// x,y — центр кнопки в px экрана. Имя: id → ng-click fn → route → slug текста.
const GMCP_COLLECT: &str = r#"function gmcpcoll(){var out=[];var els=document.querySelectorAll('a,button,input[type=button],[ng-click],.button');var seen={};for(var i=0;i<els.length;i++){var el=els[i];if(!el.offsetParent)continue;var rc=el.getBoundingClientRect();if(rc.width<4||rc.height<4)continue;var n=null;if(el.id)n=el.id;if(!n){var m=(el.getAttribute('ng-click')||'').match(/([A-Za-z_]\w*)\s*\(/);if(m)n=m[1].toLowerCase()}if(!n){var h=(el.getAttribute('href')||'').match(/^#\/(\w*)/);if(h)n=h[1]||'main'}if(!n){var t=(el.textContent||'').trim().toLowerCase().replace(/[^a-z0-9а-яё]+/gi,'_').replace(/^_+|_+$/g,'');if(t)n=t.slice(0,32)}if(!n)n='btn_'+i;if(seen[n]){n=n+'_'+(++seen[n])}else{seen[n]=1}out.push({name:n,text:(el.textContent||'').trim().replace(/\s+/g,' ').slice(0,60),x:Math.round(rc.left+rc.width/2),y:Math.round(rc.top+rc.height/2),w:Math.round(rc.width),h:Math.round(rc.height),el:el})}return out}"#;

fn gmod_menu(a: &Map<String, Value>) -> Result<String, String> {
    let act = req_str(a, "action", U_MENU)?;
    Ok(match act {
        "js" => menu_js(req_str(a, "code", U_MENU)?),
        "js_eval" => menu_js_eval(req_str(a, "code", U_MENU)?, Duration::from_secs(12)),
        "buttons" => {
            let code = format!(
                "(function(){{{GMCP_COLLECT};var r={{route:location.hash,inGame:!!(window.gScope&&gScope.InGame),buttons:gmcpcoll()}};for(var i=0;i<r.buttons.length;i++)delete r.buttons[i].el;return r}})()"
            );
            let raw = menu_js_eval(&code, Duration::from_secs(15));
            match serde_json::from_str::<Value>(&raw) {
                Ok(v) if v["ok"].as_bool().unwrap_or(false) => {
                    let mut s = String::new();
                    let route = v["result"]["route"].as_str().unwrap_or("?");
                    let ingame = v["result"]["inGame"].as_bool().unwrap_or(false);
                    s.push_str(&format!("route={route} inGame={ingame}\n"));
                    if let Some(btns) = v["result"]["buttons"].as_array() {
                        s.push_str(&format!("{} buttons:\n", btns.len()));
                        for b in btns {
                            s.push_str(&format!(
                                "  press \"{}\"  — \"{}\" @ ({}, {}) {}x{}\n",
                                b["name"].as_str().unwrap_or("?"),
                                b["text"].as_str().unwrap_or(""),
                                b["x"].as_i64().unwrap_or(0),
                                b["y"].as_i64().unwrap_or(0),
                                b["w"].as_i64().unwrap_or(0),
                                b["h"].as_i64().unwrap_or(0),
                            ));
                        }
                    }
                    s
                }
                _ => raw,
            }
        }
        "press" => {
            let name = req_str(a, "name", U_MENU)?;
            let times = a.get("times").and_then(|v| v.as_i64()).unwrap_or(1);
            let iv = a.get("interval_ms").and_then(|v| v.as_i64()).unwrap_or(120);
            let code = format!(
                "(function(){{{GMCP_COLLECT};var N={jsstr},T={times},IV={iv},done=0;\
                function go(){{var bs=gmcpcoll(),el=null;for(var i=0;i<bs.length;i++)if(bs[i].name===N){{el=bs[i].el;break}}\
                if(!el)return 'no button \"'+N+'\" — run buttons again';\
                var r=el.getBoundingClientRect(),x=r.left+r.width/2,y=r.top+r.height/2;\
                ['mousedown','mouseup','click'].forEach(function(t){{el.dispatchEvent(new MouseEvent(t,{{bubbles:true,cancelable:true,clientX:x,clientY:y}}))}});\
                done++;if(done<T)setTimeout(go,IV)}}\
                go();return {{pressed:T,button:N}}}})()",
                jsstr = js_str(name)
            );
            menu_js_eval(&code, Duration::from_secs(15))
        }
        "navigate" => {
            let route = req_str(a, "route", U_MENU)?.trim_matches(&['#', '/'][..]).to_string();
            menu_js(&format!("location.hash='#/{route}/'"))
        }
        "click" => {
            let sel = req_str(a, "selector", U_MENU)?;
            let code = format!(
                "(function(){{var el=document.querySelector({sel});if(!el)return 'no element matching '+{sel};var r=el.getBoundingClientRect(),x=r.left+r.width/2,y=r.top+r.height/2;['mousedown','mouseup','click'].forEach(function(t){{el.dispatchEvent(new MouseEvent(t,{{bubbles:true,cancelable:true,clientX:x,clientY:y}}))}});return 'clicked @ ('+Math.round(x)+','+Math.round(y)+')'}})()",
                sel = js_str(sel)
            );
            menu_js_eval(&code, Duration::from_secs(12))
        }
        "state" => {
            let code = "(function(){var g=window.gScope||{};return {route:location.hash,inGame:!!g.InGame,map:g.Map||null,gamemode:g.Gamemode||null,gamemodeTitle:g.GamemodeTitle||null,version:g.Version||null,branch:g.Branch||null,problems:g.ProblemCount||0}})()";
            menu_js_eval(code, Duration::from_secs(12))
        }
        "start_map" => {
            let map = req_str(a, "map", U_MENU)?;
            let gm = a.get("gamemode").and_then(|v| v.as_str()).unwrap_or("");
            let mp = a.get("maxplayers").and_then(|v| v.as_i64()).unwrap_or(1);
            let lan = truthy(a.get("lan"));
            let hn = a.get("hostname").and_then(|v| v.as_str()).unwrap_or("");
            let cheats = truthy(a.get("cheats"));
            // Точная последовательность меню (control.NewGame.js StartGame)
            let mut l = String::new();
            l.push_str("hook.Run('StartGame') RunConsoleCommand('progress_enable') RunConsoleCommand('disconnect') ");
            l.push_str(&format!("RunConsoleCommand('maxplayers','{mp}') "));
            if !gm.is_empty() {
                l.push_str(&format!("RunConsoleCommand('gamemode',{}) ", lua_str(gm)));
            }
            l.push_str(&format!(
                "RunConsoleCommand('sv_lan','{}') ",
                if lan { "1" } else { "0" }
            ));
            if !hn.is_empty() {
                l.push_str(&format!("RunConsoleCommand('hostname',{}) ", lua_str(hn)));
            }
            l.push_str(&format!(
                "RunConsoleCommand('sv_cheats','{}') RunConsoleCommand('map',{})",
                if cheats { "1" } else { "0" },
                lua_str(map)
            ));
            menu_lua(&l, Duration::from_secs(15))
        }
        "connect" => {
            let addr = req_str(a, "address", U_MENU)?;
            let pw = a.get("password").and_then(|v| v.as_str()).unwrap_or("");
            let l = if pw.is_empty() {
                format!("RunConsoleCommand('connect',{addr})", addr = lua_str(addr))
            } else {
                format!(
                    "RunConsoleCommand('password',{pw}) RunConsoleCommand('connect',{addr})",
                    pw = lua_str(pw),
                    addr = lua_str(addr)
                )
            };
            menu_lua(&l, Duration::from_secs(15))
        }
        "disconnect" => {
            let mut p = Map::new();
            p.insert("kind".into(), json!("console"));
            p.insert("command".into(), json!("disconnect"));
            menu_send(&mut p, Duration::from_secs(15))
        }
        "quit" => menu_lua("engine.CloseGame()", Duration::from_secs(15)),
        "gamemode" => {
            let mut p = Map::new();
            p.insert("kind".into(), json!("console"));
            p.insert("command".into(), json!(format!("gamemode {}", req_str(a, "name", U_MENU)?)));
            menu_send(&mut p, Duration::from_secs(15))
        }
        "list_gamemodes" => {
            menu_lua("return util.TableToJSON(engine.GetGamemodes())", Duration::from_secs(15))
        }
        "list_maps" => {
            let cat = a.get("category").and_then(|v| v.as_str()).unwrap_or("");
            let l = if cat.is_empty() {
                "return util.TableToJSON(GetMapList())".to_string()
            } else {
                format!(
                    "local l=GetMapList() local r={{}} for k,v in pairs(l) do \
                     if string.lower(k):find({},1,true) then r[k]=v end end \
                     return util.TableToJSON(r)",
                    lua_str(&cat.to_lowercase())
                )
            };
            menu_lua(&l, Duration::from_secs(15))
        }
        "gameui" => menu_lua(
            &format!("RunGameUICommand({})", lua_str(req_str(a, "command", U_MENU)?)),
            Duration::from_secs(15),
        ),
        "loading" => menu_lua(
            "return util.TableToJSON({loading=IsInLoading(),status=(GetLoadStatus and GetLoadStatus() or ''),downloads=(NumDownloadables and NumDownloadables() or 0)})",
            Duration::from_secs(15),
        ),
        other => return Err(uerr(U_MENU, format!("unknown action '{other}'"))),
    })
}

/// Один round-trip на несколько команд: {"kind":"batch","cmds":[{kind,...}]}.
/// Мост исполняет cmds по очереди и склеивает ответы строками "[i] <res>".
/// Асинхронные kind'ы (js_eval, shot) пишут ответы под id '<cid>_<i>'.
fn gmod_batch(realm: &str, cmds: &[Value]) -> Result<String, String> {
    if cmds.is_empty() {
        return Err(uerr(U_BATCH, "'cmds' must be a non-empty array of command objects"));
    }
    for (i, c) in cmds.iter().enumerate() {
        if !c.is_object() {
            return Err(uerr(U_BATCH, format!("cmds[{i}]: must be an object {{kind, ...}}")));
        }
        let kind = c.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let allowed = match realm {
            "menu" => matches!(kind, "lua" | "console" | "js" | "js_eval" | "shot"),
            _ => matches!(kind, "lua" | "console" | "shot" | "cam"),
        };
        if !allowed {
            return Err(uerr(
                U_BATCH,
                format!("cmds[{i}]: unsupported kind '{kind}' for realm '{realm}'"),
            ));
        }
        let need = match kind {
            "lua" | "js" | "js_eval" => Some("code"),
            "console" => Some("command"),
            _ => None,
        };
        if let Some(field) = need {
            if c.get(field).and_then(|v| v.as_str()).map_or(true, str::is_empty) {
                return Err(uerr(
                    U_BATCH,
                    format!("cmds[{i}]: kind '{kind}' requires '{field}'"),
                ));
            }
        }
    }
    let mut p = Map::new();
    p.insert("kind".into(), json!("batch"));
    p.insert("cmds".into(), Value::Array(cmds.to_vec()));
    let timeout = Duration::from_secs(15 + cmds.len() as u64 * 2);
    Ok(if realm == "menu" {
        menu_send(&mut p, timeout)
    } else {
        match write_cmd(realm, &mut p) {
            Ok(cid) => read_out(&cid, timeout)
                .unwrap_or_else(|| "TIMEOUT: no reply from game".into()),
            Err(e) => e,
        }
    })
}

fn gmod_lua(realm: &str, code: &str, timeout: Duration) -> String {
    let mut p = Map::new();
    p.insert("kind".into(), json!("lua"));
    p.insert("code".into(), json!(code));
    let cid = match write_cmd(realm, &mut p) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let res = if realm == "menu" {
        read_out_menu(&cid, timeout)
    } else {
        read_out(&cid, timeout)
    };
    match res {
        Some(t) => t,
        None => format!(
            "TIMEOUT ({}s): no reply from game. Make sure GMod is running, the map is \
             loaded and the gmod_mcp addon is installed.",
            timeout.as_secs()
        ),
    }
}

/// (jpeg-байты, текст результата)
fn shot_result(cid: &str, timeout: Duration) -> (Option<Vec<u8>>, String) {
    let txt = match read_out(cid, timeout) {
        Some(t) => t,
        None => return (None, "TIMEOUT: no screenshot received".into()),
    };
    let shot = dir().join(format!("shot_{cid}.jpg"));
    let mut data = None;
    if shot.exists() {
        if let Ok(b) = fs::read(&shot) {
            data = Some(b);
        }
        let _ = fs::remove_file(&shot);
    }
    (data, txt)
}

fn tga_to_jpg(raw: &[u8]) -> Result<Vec<u8>, String> {
    let img = image::load_from_memory_with_format(raw, image::ImageFormat::Tga)
        .map_err(|e| e.to_string())?
        .to_rgb8();
    let mut buf = Vec::new();
    let mut cur = io::Cursor::new(&mut buf);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cur, 85)
        .encode(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgb8.into(),
        )
        .map_err(|e| e.to_string())?;
    Ok(buf)
}

fn gmod_menu_screenshot(timeout: Duration) -> (Option<Vec<u8>>, String) {
    // Скриншот главного меню: menu-realm дёргает движковый `screenshot`,
    // движок пишет garrysmod/screenshots/Snapshot*.tga (кадр целиком,
    // включая VGUI), мост возвращает имя файла в out_<id>.txt.
    let mut p = Map::new();
    p.insert("kind".into(), json!("shot"));
    let cid = match write_cmd("menu", &mut p) {
        Ok(c) => c,
        Err(e) => return (None, e),
    };
    // menu-поллер сначала пишет промежуточный "shot requested",
    // а финальный "ok <file>" — когда движок допишет Snapshot*.tga.
    // Читаем до финального ответа (в Python-версии из-за этого
    // target='menu' всегда возвращал ack без картинки).
    let deadline = Instant::now() + timeout;
    let txt = loop {
        match read_out(&cid, deadline.saturating_duration_since(Instant::now())) {
            Some(t) if t.trim() == "shot requested" && Instant::now() < deadline => continue,
            Some(t) => break t,
            None => return (None, "TIMEOUT: no menu screenshot received".into()),
        }
    };
    let parts: Vec<&str> = txt.split_whitespace().collect();
    if parts.len() < 2 || parts[0] != "ok" {
        return (None, txt);
    }
    let name = parts[parts.len() - 1];
    let tga = gmod_data()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("screenshots")
        .join(name);
    if !tga.exists() {
        return (None, format!("{txt} (file {} not found)", tga.display()));
    }
    match fs::read(&tga).map_err(|e| e.to_string()).and_then(|raw| tga_to_jpg(&raw)) {
        Ok(jpg) => (Some(jpg), txt),
        Err(e) => (None, format!("{txt} (tga->jpg failed: {e})")),
    }
}

fn gmod_screenshot(a: &Map<String, Value>, timeout: Duration) -> (Option<Vec<u8>>, String) {
    let target = a.get("target").and_then(|v| v.as_str()).unwrap_or("world");
    if target == "menu" {
        return gmod_menu_screenshot(timeout);
    }
    for k in ["x", "y", "z", "pitch", "yaw"] {
        if a.get(k).is_none() {
            return (
                None,
                format!("UsageError: target='world' requires argument '{k}'\n{U_SCREENSHOT}"),
            );
        }
    }
    let mut p = Map::new();
    p.insert("kind".into(), json!("shot"));
    p.insert(
        "pos".into(),
        json!([a["x"].clone(), a["y"].clone(), a["z"].clone()]),
    );
    p.insert(
        "ang".into(),
        json!([
            arg_or(a, "pitch", json!(0)),
            arg_or(a, "yaw", json!(0)),
            arg_or(a, "roll", json!(0)),
        ]),
    );
    p.insert("fov".into(), arg_or(a, "fov", json!(75)));
    p.insert("hud".into(), arg_or(a, "hide_hud", json!(true)));
    p.insert("drawviewer".into(), arg_or(a, "drawviewer", json!(false)));
    let cid = match write_cmd("client", &mut p) {
        Ok(c) => c,
        Err(e) => return (None, e),
    };
    shot_result(&cid, timeout)
}

fn gmod_view(a: &Map<String, Value>) -> (Option<Vec<u8>>, String) {
    // узнать позицию и углы локального игрока
    let res = gmod_lua(
        "client",
        "local p = LocalPlayer() \
         if not IsValid(p) then return 'NO PLAYER' end \
         local pos = p:GetPos() local ang = p:EyeAngles() \
         return string.format('%.1f %.1f %.1f %.1f %.1f', \
         pos.x, pos.y, pos.z, ang.p, ang.y)",
        Duration::from_secs(15),
    );
    if res.starts_with("TIMEOUT") || res.starts_with("ERROR") {
        return (None, res);
    }
    let tail = res.rsplit("=>").next().unwrap_or("");
    let nums: Vec<&str> = tail.split_whitespace().collect();
    let mut vals = Vec::with_capacity(5);
    for tok in nums.iter().take(5) {
        match tok.parse::<f64>() {
            Ok(v) => vals.push(v),
            Err(_) => return (None, format!("could not parse player pos: {res}")),
        }
    }
    if vals.len() < 5 {
        return (None, format!("could not parse player pos: {res}"));
    }
    let (px, py, pz, pp, py_) = (vals[0], vals[1], vals[2], vals[3], vals[4]);

    let mode = a.get("mode").and_then(|v| v.as_str()).unwrap_or("orbit");
    if mode != "orbit" && mode != "fp" {
        return (
            None,
            format!("UsageError: invalid mode '{mode}' — 'orbit' or 'fp'\n{U_VIEW}"),
        );
    }
    if mode == "fp" {
        let mut s = Map::new();
        s.insert("x".into(), json!(px));
        s.insert("y".into(), json!(py));
        s.insert("z".into(), json!(pz + 64.0)); // примерно уровень глаз
        s.insert("pitch".into(), json!(pp));
        s.insert("yaw".into(), json!(py_));
        s.insert("fov".into(), arg_or(a, "fov", json!(75)));
        s.insert("drawviewer".into(), json!(false));
        s.insert("hide_hud".into(), json!(true));
        return gmod_screenshot(&s, Duration::from_secs(25));
    }

    // orbit: yaw=0 — камера перед лицом игрока
    let dist = get_f64(a, "dist", 110.0);
    let rel_yaw = (py_ + 180.0 + get_f64(a, "yaw", 0.0)).to_radians();
    let pitch = get_f64(a, "pitch", 15.0).to_radians();
    let th = get_f64(a, "target_height", 45.0);
    let cx = px + dist * rel_yaw.cos() * pitch.cos();
    let cy = py + dist * rel_yaw.sin() * pitch.cos();
    let cz = pz + th + dist * pitch.sin();
    // камера смотрит на точку (px, py, pz+th)
    let (dx, dy, dz) = (px - cx, py - cy, (pz + th) - cz);
    let cam_yaw = dy.atan2(dx).to_degrees();
    let cam_pitch = -(dz.atan2(dx.hypot(dy))).to_degrees();
    let mut s = Map::new();
    s.insert("x".into(), json!(cx));
    s.insert("y".into(), json!(cy));
    s.insert("z".into(), json!(cz));
    s.insert("pitch".into(), json!(cam_pitch));
    s.insert("yaw".into(), json!(cam_yaw));
    s.insert("fov".into(), arg_or(a, "fov", json!(60)));
    s.insert("drawviewer".into(), json!(true));
    s.insert("hide_hud".into(), json!(true));
    gmod_screenshot(&s, Duration::from_secs(25))
}

fn gmod_camera(a: &Map<String, Value>) -> String {
    let mut p = Map::new();
    p.insert("kind".into(), json!("cam"));
    p.insert("on".into(), json!(truthy(a.get("enabled"))));
    p.insert(
        "pos".into(),
        json!([
            arg_or(a, "x", json!(0)),
            arg_or(a, "y", json!(0)),
            arg_or(a, "z", json!(0)),
        ]),
    );
    p.insert(
        "ang".into(),
        json!([arg_or(a, "pitch", json!(0)), arg_or(a, "yaw", json!(0)), json!(0)]),
    );
    p.insert("fov".into(), arg_or(a, "fov", json!(75)));
    p.insert("drawviewer".into(), arg_or(a, "drawviewer", json!(true)));
    match write_cmd("client", &mut p) {
        Ok(cid) => read_out(&cid, Duration::from_secs(10)).unwrap_or_else(|| "no response".into()),
        Err(e) => e,
    }
}

fn gmod_errors() -> String {
    let path = dir().join("errors.txt");
    if !path.exists() {
        return "(errors.txt пуст — ошибок нет)".into();
    }
    let txt = match fs::read(&path) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) => return format!("IOError: {e}"),
    };
    let _ = fs::write(&path, b"");
    if txt.is_empty() {
        "(no errors)".into()
    } else {
        txt
    }
}

fn gmod_console(realm: &str, command: &str) -> String {
    if realm == "menu" {
        let mut p = Map::new();
        p.insert("kind".into(), json!("console"));
        p.insert("command".into(), json!(command));
        return match write_cmd("menu", &mut p) {
            Ok(cid) => read_out_menu(&cid, Duration::from_secs(15))
                .unwrap_or_else(|| "TIMEOUT: menu realm did not respond".into()),
            Err(e) => e,
        };
    }
    let esc = command.replace('\\', "\\\\").replace('"', "\\\"");
    let code = if realm == "server" {
        format!("game.ConsoleCommand(\"{esc}\")\nreturn \"sent: {esc}\"")
    } else {
        format!("RunConsoleCommand(\"{esc}\")\nreturn \"sent: {esc}\"")
    };
    gmod_lua(realm, &code, Duration::from_secs(15))
}

fn hl2_running() -> bool {
    match Command::new("tasklist")
        .arg("/NH")
        .stderr(Stdio::null())
        .output()
    {
        Ok(o) => {
            let out = String::from_utf8_lossy(&o.stdout);
            GMOD_PROCS.iter().any(|p| out.contains(*p))
        }
        Err(_) => false,
    }
}

fn realm_alive(realm: &str, timeout: Duration) -> bool {
    let mut p = Map::new();
    p.insert("kind".into(), json!("lua"));
    p.insert("code".into(), json!("return 'ok'"));
    match write_cmd(realm, &mut p) {
        Ok(cid) => {
            let out = if realm == "menu" {
                read_out_menu(&cid, timeout)
            } else {
                read_out(&cid, timeout)
            };
            out.map_or(false, |t| t.contains("ok"))
        }
        Err(_) => false,
    }
}

fn gmod_status() -> String {
    if !hl2_running() {
        return "gmod running=False bridge=n/a".into();
    }
    // три реалма пингуем параллельно — иначе 3 последовательных
    // round-trip'а утраивают задержку статуса
    let handles: Vec<_> = ["server", "client", "menu"]
        .iter()
        .map(|r| (*r, std::thread::spawn(move || realm_alive(r, Duration::from_secs(5)))))
        .collect();
    let mut realms = Map::new();
    for (r, h) in handles {
        let alive = h.join().unwrap_or(false);
        let state = if alive { "alive" } else { "no response" };
        realms.insert(r.into(), json!(state));
    }
    // состояние игры через server realm (единственный, кто живёт и в меню)
    let mut info = String::new();
    if realms["server"] == "alive" {
        let res = gmod_lua(
            "server",
            "local gm = GetConVar('gamemode') \
             return string.format('map=%s gamemode=%s', \
             game.GetMap() or '?', gm and gm:GetString() or '?')",
            Duration::from_secs(8),
        );
        if let Some(tail) = res.rsplit("=>").next() {
            info = format!(" | {}", tail.trim());
        }
    }
    let mut fullscreen = String::new();
    if realms["client"] == "alive" {
        let res = gmod_lua(
            "client",
            "local c=GetConVar('fullscreen') return c and tostring(c:GetInt()) or '?'",
            Duration::from_secs(8),
        );
        if let Some(tail) = res.rsplit("=>").next() {
            fullscreen = format!(" | fullscreen={}", tail.trim());
        }
    }
    format!(
        "gmod running=True server={} client={} menu={}{}{}",
        realms["server"].as_str().unwrap_or("?"),
        realms["client"].as_str().unwrap_or("?"),
        realms["menu"].as_str().unwrap_or("?"),
        info,
        fullscreen
    )
}

fn gmod_launch(mapname: &str, gamemode: &str, extra_args: &str) -> String {
    if hl2_running() {
        return "already running".into();
    }
    let mut args: Vec<String> = vec![
        "-game".into(),
        "garrysmod".into(),
        "-condebug".into(),
    ];
    if !mapname.is_empty() {
        args.extend(["+map".into(), mapname.into()]);
    }
    if !gamemode.is_empty() {
        args.extend(["+gamemode".into(), gamemode.into()]);
    }
    args.extend(["+sv_lan".into(), "1".into()]);
    if !extra_args.is_empty() {
        args.extend(extra_args.split_whitespace().map(String::from));
    }
    let cmdline = format!("{} {}", gmod_exe(), args.join(" "));
    match Command::new(gmod_exe())
        .args(&args)
        .stdin(Stdio::null())
        .spawn()
    {
        Ok(_) => format!("launching: {cmdline}"),
        Err(e) => format!("OSError: {e}"),
    }
}

fn gmod_restart(mapname: &str, gamemode: &str) -> String {
    if hl2_running() {
        for proc in GMOD_PROCS {
            let _ = Command::new("taskkill")
                .args(["/F", "/IM", proc])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        for _ in 0..50 {
            if !hl2_running() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    gmod_launch(mapname, gamemode, "")
}

fn gmod_map(mapname: &str, gamemode: &str) -> String {
    // В игре — changelevel (без дисконнекта); в меню — gamemode+map.
    // Геймод применяется только при загрузке карты (convar `gamemode`).
    let mapname = mapname.trim();
    if mapname.is_empty() {
        return "UsageError: 'map' must be a non-empty map name (e.g. 'gm_flatgrass')".into();
    }
    if realm_alive("client", Duration::from_secs(6)) {
        if !gamemode.is_empty() {
            gmod_lua(
                "server",
                &format!("game.ConsoleCommand(\"gamemode {gamemode}\")"),
                Duration::from_secs(15),
            );
        }
        return gmod_lua(
            "server",
            &format!("game.ConsoleCommand(\"changelevel {mapname}\")\nreturn \"changelevel {mapname}\""),
            Duration::from_secs(15),
        );
    }
    // главное меню: menu-realm (или server realm меню-сервера)
    let realm = if realm_alive("menu", Duration::from_secs(6)) {
        "menu"
    } else {
        "server"
    };
    if !gamemode.is_empty() {
        gmod_console(realm, &format!("gamemode {gamemode}"));
    }
    gmod_console(realm, &format!("map {mapname}"))
}

fn gmod_display(a: &Map<String, Value>) -> String {
    let mode = a.get("mode").and_then(|v| v.as_str()).unwrap_or("get");
    if mode == "get" {
        return gmod_lua(
            "client",
            "local fs = GetConVar('fullscreen') \
             local w = GetConVar('mat_setvideomode_width') \
             local h = GetConVar('mat_setvideomode_height') \
             return string.format('fullscreen=%s %sx%s sw=%dx%d', \
             tostring(fs and fs:GetInt()), \
             tostring(w and w:GetInt()), tostring(h and h:GetInt()), \
             ScrW(), ScrH())",
            Duration::from_secs(15),
        );
    }
    // set
    let mut parts: Vec<String> = Vec::new();
    if a.get("fullscreen").is_some_and(|v| !v.is_null()) {
        parts.push(format!("fullscreen {}", get_f64(a, "fullscreen", 0.0) as i64));
    }
    if truthy(a.get("width")) && truthy(a.get("height")) {
        parts.push(format!(
            "mat_setvideomode {} {} 0",
            get_f64(a, "width", 0.0) as i64,
            get_f64(a, "height", 0.0) as i64
        ));
    }
    if parts.is_empty() {
        return "UsageError: nothing to apply — pass 'fullscreen' and/or 'width'+'height'\n\
                Usage: gmod_display([fullscreen=<0|1>] [, width=<px>, height=<px>])".into();
    }
    parts
        .iter()
        .map(|c| gmod_console("client", c))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Проброс выражения числа в Lua-код (целые — без точки).
fn lua_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Обязательный конечный f64-аргумент.
fn req_num(a: &Map<String, Value>, k: &str, usage: &str) -> Result<f64, String> {
    match a.get(k).and_then(|v| v.as_f64()) {
        Some(v) if v.is_finite() => Ok(v),
        Some(_) => Err(uerr(usage, format!("argument '{k}' must be a finite number"))),
        None => Err(uerr(usage, format!("missing required numeric argument '{k}'"))),
    }
}

fn opt_num(a: &Map<String, Value>, k: &str) -> Option<f64> {
    a.get(k).and_then(|v| v.as_f64()).filter(|v| v.is_finite())
}

/// Префикс для server-realm сниппетов: p = локальный игрок или 'no player'.
const PLAYER_PREFIX: &str =
    "local p = player.GetAll()[1] if not IsValid(p) then return 'no player' end ";

/// Экранирование строки для Lua-литерала в двойных кавычках.
fn lua_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Валидный класс entity для give.
fn valid_class(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// Управление локальным игроком. См. описание инструмента gmod_player.
fn gmod_player(a: &Map<String, Value>) -> Result<String, String> {
    let action = req_str(a, "action", U_PLAYER)?;
    let t15 = Duration::from_secs(15);
    match action {
        "info" => Ok(gmod_lua(
            "server",
            "local p = player.GetAll()[1] \
             if not IsValid(p) then return 'no player' end \
             local pos, ang, vel = p:GetPos(), p:EyeAngles(), p:GetVelocity() \
             local w = p:GetActiveWeapon() \
             return string.format('pos=%.1f %.1f %.1f ang=%.1f %.1f vel=%.0f \
             hp=%d armor=%d ground=%s movetype=%d weapon=%s', \
             pos.x, pos.y, pos.z, ang.p, ang.y, vel:Length(), \
             p:Health(), p:Armor(), tostring(p:OnGround()), \
             p:GetMoveType(), IsValid(w) and w:GetClass() or 'none')",
            t15,
        )),
        "move" => {
            let mut dirs: Vec<String> = Vec::new();
            match a.get("dir") {
                Some(Value::String(s)) => dirs.push(s.clone()),
                Some(Value::Array(arr)) => {
                    dirs.extend(arr.iter().filter_map(|v| v.as_str().map(String::from)))
                }
                _ => {}
            }
            if dirs.is_empty() {
                return Err(uerr(
                    U_PLAYER,
                    "move: 'dir' is required (forward|back|left|right, string or array)",
                ));
            }
            let mut names: Vec<&str> = Vec::new();
            for d in &dirs {
                match d.as_str() {
                    "forward" | "back" => names.push(d.as_str()),
                    "left" => names.push("moveleft"),
                    "right" => names.push("moveright"),
                    _ => {
                        return Err(uerr(
                            U_PLAYER,
                            format!("move: unknown direction '{d}' (forward|back|left|right)"),
                        ))
                    }
                }
            }
            if truthy(a.get("run")) {
                names.push("speed");
            }
            let dur = opt_num(a, "dur").unwrap_or(1.0);
            let down: String = names
                .iter()
                .map(|n| format!("RunConsoleCommand(\"+{n}\") "))
                .collect();
            let up: String = names
                .iter()
                .map(|n| format!("RunConsoleCommand(\"-{n}\") "))
                .collect();
            let code = format!(
                "{down}timer.Simple({}, function() {up}end)\nreturn \"move {} {}s\"",
                lua_num(dur),
                dirs.join("+"),
                lua_num(dur)
            );
            Ok(gmod_lua("client", &code, t15))
        }
        "look" => {
            let body = if a.get("x").is_some() && a.get("y").is_some() && a.get("z").is_some() {
                // смотреть на точку мира (x,y,z)
                let (x, y, z) = (
                    req_num(a, "x", U_PLAYER)?,
                    req_num(a, "y", U_PLAYER)?,
                    req_num(a, "z", U_PLAYER)?,
                );
                format!(
                    "local d = Vector({},{},{}) - __P__:EyePos() \
                     __P__:SetEyeAngles(d:Angle()) \
                     local aa = __P__:EyeAngles() \
                     return string.format('look at %.0f %.0f %.0f -> %.1f %.1f', \
                     {},{},{}, aa.p, aa.y)",
                    lua_num(x), lua_num(y), lua_num(z), lua_num(x), lua_num(y), lua_num(z)
                )
            } else {
                // pitch/yaw: абсолютные, либо rel=true — прибавить к текущим
                let rel = truthy(a.get("rel"));
                let expr = |k: &str, c: &str| match (rel, opt_num(a, k)) {
                    (true, Some(d)) => format!("{c} + ({})", lua_num(d)),
                    (true, None) => c.into(),
                    (false, Some(v)) => lua_num(v),
                    (false, None) => c.into(),
                };
                format!(
                    "local c = __P__:EyeAngles() local np, ny = {}, {} \
                     __P__:SetEyeAngles(Angle(np, ny, 0)) \
                     return string.format('look %.1f %.1f', np, ny)",
                    expr("pitch", "c.p"),
                    expr("yaw", "c.y")
                )
            };
            // server — авторитетно. Результат (абсолютные углы) парсим и
            // применяем на клиент: rel-режим на клиенте считать нельзя —
            // клиент увидел бы уже повёрнутый сервером угол и добавил бы
            // дельту второй раз.
            let srv = format!("{}{}", PLAYER_PREFIX, body.replace("__P__", "p"));
            if !truthy(a.get("rel")) {
                // Абсолютные углы / точка мира: клиент может вычислить тот
                // же код сам — реалмы независимы, исполняем параллельно.
                let cl = format!(
                    "local p = LocalPlayer() if not IsValid(p) then return 'no player' end {}",
                    body.replace("__P__", "p")
                );
                let h = std::thread::spawn(move || gmod_lua("client", &cl, t15));
                let rs = gmod_lua("server", &srv, t15);
                let rc = h.join().unwrap_or_else(|_| "cl thread panicked".into());
                return Ok(format!("{rs} | cl: {rc}"));
            }
            let rs = gmod_lua("server", &srv, t15);
            let parsed = rs
                .rsplit("=>")
                .next()
                .and_then(|tail| {
                    let t: Vec<&str> = tail.split_whitespace().collect();
                    // "look <p> <y>" или "look at <x> <y> <z> -> <p> <y>"
                    t.iter()
                        .rev()
                        .take(2)
                        .collect::<Vec<_>>()
                        .iter()
                        .rev()
                        .map(|s| s.parse::<f64>())
                        .collect::<Result<Vec<_>, _>>()
                        .ok()
                });
            let rc = match parsed {
                Some(v) if v.len() == 2 => {
                    let cl = format!(
                        "local p = LocalPlayer() if IsValid(p) then \
                         p:SetEyeAngles(Angle({}, {}, 0)) end \
                         return 'cl look {} {}'",
                        lua_num(v[0]),
                        lua_num(v[1]),
                        lua_num(v[0]),
                        lua_num(v[1])
                    );
                    gmod_lua("client", &cl, t15)
                }
                _ => {
                    let cl = format!(
                        "local p = LocalPlayer() if not IsValid(p) then return 'no player' end {}",
                        body.replace("__P__", "p")
                    );
                    gmod_lua("client", &cl, t15)
                }
            };
            Ok(format!("{rs} | cl: {rc}"))
        }
        "teleport" => {
            let (x, y, z) = (
                req_num(a, "x", U_PLAYER)?,
                req_num(a, "y", U_PLAYER)?,
                req_num(a, "z", U_PLAYER)?,
            );
            let mut body = format!(
                "p:SetPos(Vector({},{},{})) p:SetLocalVelocity(Vector(0,0,0))",
                lua_num(x),
                lua_num(y),
                lua_num(z)
            );
            if let Some(yaw) = opt_num(a, "yaw") {
                body += &format!(
                    " local c = p:EyeAngles() p:SetEyeAngles(Angle(c.p, {}, 0))",
                    lua_num(yaw)
                );
            }
            body += &format!(
                " return 'teleport {} {} {}'",
                lua_num(x),
                lua_num(y),
                lua_num(z)
            );
            Ok(gmod_lua("server", &format!("{PLAYER_PREFIX}{body}"), t15))
        }
        "noclip" => {
            let want: &str = match a.get("on") {
                None | Some(Value::Null) => "nil",
                Some(v) => {
                    if truthy(Some(v)) {
                        "true"
                    } else {
                        "false"
                    }
                }
            };
            let body = format!(
                "local want = {want} local cur = p:GetMoveType() == MOVETYPE_NOCLIP \
                 if want == nil then want = not cur end \
                 p:SetMoveType(want and MOVETYPE_NOCLIP or MOVETYPE_WALK) \
                 return 'noclip ' .. tostring(want)"
            );
            Ok(gmod_lua("server", &format!("{PLAYER_PREFIX}{body}"), t15))
        }
        "press" => {
            let key = req_str(a, "key", U_PLAYER)?;
            const KEYS: [&str; 9] = [
                "jump", "use", "attack", "attack2", "reload", "duck", "walk", "speed", "zoom",
            ];
            if !KEYS.contains(&key) {
                return Err(uerr(
                    U_PLAYER,
                    format!("press: unknown key '{key}' (one of: {})", KEYS.join("|")),
                ));
            }
            let dur = opt_num(a, "dur").unwrap_or(0.2);
            let code = format!(
                "RunConsoleCommand(\"+{key}\") \
                 timer.Simple({}, function() RunConsoleCommand(\"-{key}\") end)\n\
                 return 'press {key}'",
                lua_num(dur)
            );
            Ok(gmod_lua("client", &code, t15))
        }
        "hold" => {
            let name: String = req_str(a, "cmd", U_PLAYER)?
                .trim_start_matches(['+', '-'])
                .to_string();
            if name.is_empty()
                || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return Err(uerr(
                    U_PLAYER,
                    "hold: 'cmd' must match [a-z0-9_] (e.g. 'forward')",
                ));
            }
            let on = a.get("on").map(|v| truthy(Some(v))).unwrap_or(true);
            let sign = if on { "+" } else { "-" };
            let code = format!("RunConsoleCommand(\"{sign}{name}\")\nreturn 'hold {sign}{name}'");
            Ok(gmod_lua("client", &code, t15))
        }
        "stop" => Ok(gmod_lua(
            "client",
            "for _, c in ipairs({'forward','back','moveleft','moveright','jump','duck',\
             'use','attack','attack2','reload','walk','speed','zoom','strafe'}) do \
             RunConsoleCommand('-' .. c) end \
             return 'released all keys'",
            t15,
        )),
        "speed" => {
            let mut sets = String::new();
            if let Some(w) = opt_num(a, "walk") {
                sets += &format!("p:SetWalkSpeed({}) ", lua_num(w));
            }
            if let Some(r) = opt_num(a, "run") {
                sets += &format!("p:SetRunSpeed({}) ", lua_num(r));
            }
            if let Some(j) = opt_num(a, "jump") {
                sets += &format!("p:SetJumpPower({}) ", lua_num(j));
            }
            if sets.is_empty() {
                return Err(uerr(
                    U_PLAYER,
                    "speed: at least one of 'walk'/'run'/'jump' is required",
                ));
            }
            let body = format!(
                "{sets} return string.format('walk=%.0f run=%.0f jump=%.0f', \
                 p:GetWalkSpeed(), p:GetRunSpeed(), p:GetJumpPower())"
            );
            Ok(gmod_lua("server", &format!("{PLAYER_PREFIX}{body}"), t15))
        }
        "health" => {
            let mut sets = String::new();
            if let Some(h) = opt_num(a, "hp") {
                sets += &format!("p:SetHealth({}) ", lua_num(h));
            }
            if let Some(ar) = opt_num(a, "armor") {
                sets += &format!("p:SetArmor({}) ", lua_num(ar));
            }
            if let Some(v) = a.get("god") {
                sets += if truthy(Some(v)) {
                    "p:GodEnable() "
                } else {
                    "p:GodDisable() "
                };
            }
            if sets.is_empty() {
                return Err(uerr(
                    U_PLAYER,
                    "health: at least one of 'hp'/'armor'/'god' is required",
                ));
            }
            let body = format!(
                "{sets} return string.format('hp=%d armor=%d god=%s', \
                 p:Health(), p:Armor(), tostring(p:HasGodMode()))"
            );
            Ok(gmod_lua("server", &format!("{PLAYER_PREFIX}{body}"), t15))
        }
        "give" => {
            let class = req_str(a, "class", U_PLAYER)?;
            if !valid_class(class) {
                return Err(uerr(
                    U_PLAYER,
                    "give: invalid 'class' (allowed chars: [a-z0-9_.-])",
                ));
            }
            let c = lua_escape(class);
            let sel = if truthy(a.get("select")) {
                format!(" p:SelectWeapon(\"{c}\")")
            } else {
                String::new()
            };
            let body = format!(
                "local w = p:Give(\"{c}\"){sel} \
                 return IsValid(w) and 'gave {c}' or 'give failed: {c}'"
            );
            Ok(gmod_lua("server", &format!("{PLAYER_PREFIX}{body}"), t15))
        }
        _ => Err(uerr(U_PLAYER, format!("unknown action '{action}'"))),
    }
}

fn text(t: impl Into<String>) -> Value {
    json!({"type": "text", "text": t.into()})
}

fn jpeg_item(data: &[u8]) -> Value {
    json!({"type": "image", "data": B64.encode(data), "mimeType": "image/jpeg"})
}

/// Единый формат ошибки использования: что не так + usage-строка
/// инструмента. Все сообщения — только на английском.
fn uerr(usage: &str, msg: impl std::fmt::Display) -> String {
    format!("UsageError: {msg}\n{usage}")
}

const U_LUA: &str =
    "Usage: gmod_lua(realm='server'|'client'|'menu', code='<lua code>')";
const U_BATCH: &str = "Usage: gmod_batch(realm='server'|'client'|'menu', \
    cmds=[{kind:'lua',code:'...'}|{kind:'console',command:'...'}]) — \
    multiple commands in one round trip; menu also accepts js/js_eval, \
    srv/cl accept shot/cam; replies are joined as '[i] <result>'";
const U_CONSOLE: &str =
    "Usage: gmod_console(realm='server'|'client'|'menu', command='<console command>')";
const U_SCREENSHOT: &str = "Usage: gmod_screenshot(target='menu') or \
    gmod_screenshot(target='world', x=<n>, y=<n>, z=<n> [, pitch, yaw, roll, fov, hide_hud, drawviewer])";
const U_VIEW: &str =
    "Usage: gmod_view([mode='orbit'|'fp'] [, dist, yaw, pitch, target_height, fov])";
const U_MENU: &str = "Usage: gmod_menu(action=...) — actions: \
    buttons | press name=<button> [times n] [interval_ms ms] | \
    navigate route=<route> | click selector=<css> | \
    js code=<js> | js_eval code=<js> | state | loading | \
    start_map map=<name> [gamemode gm] [maxplayers n] [lan] [hostname s] [cheats] | \
    connect address=<ip:port> [password pw] | disconnect | quit | \
    gamemode name=<gm> | list_maps [category s] | list_gamemodes | \
    gameui command=<cmd>";
const U_PLAYER: &str = "Usage: gmod_player(action=...) — actions: \
    info | move dir=<forward|back|left|right|array> [dur s] [run] | \
    look [pitch p] [yaw y] [rel] or look x=<n> y=<n> z=<n> | \
    teleport x=<n> y=<n> z=<n> [yaw] | noclip [on] | \
    press key=<jump|use|attack|attack2|reload|duck|walk|speed|zoom> [dur s] | \
    hold cmd=<name> [on] | stop | \
    speed [walk n] [run n] [jump n] | health [hp n] [armor n] [god] | \
    give class=<entity> [select]";

fn req_str<'a>(a: &'a Map<String, Value>, k: &str, usage: &str) -> Result<&'a str, String> {
    match a.get(k).and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => Ok(s),
        Some(_) => Err(uerr(usage, format!("argument '{k}' must be a non-empty string"))),
        None => Err(uerr(usage, format!("missing required argument '{k}'"))),
    }
}

fn call_tool(params: &Map<String, Value>) -> Result<Vec<Value>, String> {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let empty = Map::new();
    let args = params
        .get("arguments")
        .and_then(|v| v.as_object())
        .unwrap_or(&empty);
    match name {
        "gmod_lua" => Ok(vec![text(gmod_lua(
            req_str(args, "realm", U_LUA)?,
            req_str(args, "code", U_LUA)?,
            Duration::from_secs(15),
        ))]),
        "gmod_batch" => {
            let realm = req_str(args, "realm", U_BATCH)?;
            let cmds = args
                .get("cmds")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            Ok(vec![text(gmod_batch(realm, &cmds)?)])
        }
        "gmod_screenshot" => {
            let (data, txt) = gmod_screenshot(args, Duration::from_secs(25));
            let mut out = vec![text(txt)];
            if let Some(d) = data {
                out.push(jpeg_item(&d));
            }
            Ok(out)
        }
        "gmod_view" => {
            let (data, txt) = gmod_view(args);
            let mut out = vec![text(txt)];
            if let Some(d) = data {
                out.push(jpeg_item(&d));
            }
            Ok(out)
        }
        "gmod_camera" => Ok(vec![text(gmod_camera(args))]),
        "gmod_errors" => Ok(vec![text(gmod_errors())]),
        "gmod_console" => Ok(vec![text(gmod_console(
            req_str(args, "realm", U_CONSOLE)?,
            req_str(args, "command", U_CONSOLE)?,
        ))]),
        "gmod_status" => Ok(vec![text(gmod_status())]),
        "gmod_launch" => Ok(vec![text(gmod_launch(
            args.get("mapname").and_then(|v| v.as_str()).unwrap_or("gm_construct"),
            args.get("gamemode").and_then(|v| v.as_str()).unwrap_or(""),
            args.get("extra_args").and_then(|v| v.as_str()).unwrap_or(""),
        ))]),
        "gmod_restart" => Ok(vec![text(gmod_restart(
            args.get("mapname").and_then(|v| v.as_str()).unwrap_or("gm_construct"),
            args.get("gamemode").and_then(|v| v.as_str()).unwrap_or(""),
        ))]),
        "gmod_map" => Ok(vec![text(gmod_map(
            args.get("map").and_then(|v| v.as_str()).unwrap_or(""),
            args.get("gamemode").and_then(|v| v.as_str()).unwrap_or(""),
        ))]),
        "gmod_display" => Ok(vec![text(gmod_display(args))]),
        "gmod_player" => Ok(vec![text(gmod_player(args)?)]),
        "gmod_menu" => Ok(vec![text(gmod_menu(args)?)]),
        _ => Err(format!(
            "UsageError: unknown tool '{name}'. Available tools: gmod_lua, gmod_batch, \
             gmod_screenshot, \
             gmod_view, gmod_camera, gmod_errors, gmod_console, gmod_status, gmod_launch, \
             gmod_restart, gmod_map, gmod_display, gmod_player, gmod_menu"
        )),
    }
}

fn handle(msg: &Map<String, Value>) -> Option<Value> {
    let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let mid = msg.get("id").cloned();
    match method {
        "initialize" => Some(json!({
            "jsonrpc": "2.0", "id": mid,
            "result": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "gmod", "version": "0.1.0"},
            },
        })),
        "ping" => Some(json!({"jsonrpc": "2.0", "id": mid, "result": {}})),
        "tools/list" => Some(json!({
            "jsonrpc": "2.0", "id": mid,
            "result": {"tools": tools()},
        })),
        "tools/call" => {
            let empty = Map::new();
            let params = msg
                .get("params")
                .and_then(|v| v.as_object())
                .unwrap_or(&empty);
            match call_tool(params) {
                Ok(content) => Some(json!({
                    "jsonrpc": "2.0", "id": mid,
                    "result": {"content": content},
                })),
                Err(e) => Some(json!({
                    "jsonrpc": "2.0", "id": mid,
                    "result": {
                        "isError": true,
                        "content": [text(e)],
                    },
                })),
            }
        }
        _ => match mid {
            Some(id) if !id.is_null() => Some(json!({
                "jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": format!("method not found: {method}")},
            })),
            _ => None,
        },
    }
}

fn main() {
    let d = dir();
    let _ = fs::create_dir_all(&d);
    eprintln!("[gmod-mcp] watching {}", d.display());
    let stdin = io::stdin();
    let stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: Map<String, Value> = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(resp) = handle(&msg) {
            let mut out = stdout.lock();
            if let Ok(s) = serde_json::to_string(&resp) {
                let _ = out.write_all(s.as_bytes());
                let _ = out.write_all(b"\n");
                let _ = out.flush();
            }
        }
    }
}
