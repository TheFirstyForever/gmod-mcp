-- =============================================
-- gmod_mcp menu-realm bridge (dbg only)
-- Подключается из garrysmod/lua/includes/init_menu.lua
-- (menu state не поддерживает аддоны — нужен include engine-файла).
-- Читает data/gmod_mcp/cmd_menu_*.json -> выполняет ->
-- пишет ответ.
--
-- ОТВЕТНЫЙ КАНАЛ (важно!): с обновления ~20.11.2025 file.Write в
-- menu-state СЛОМАН: обёртка file.lua делает file.Open("wb") — файл
-- создаётся, — но у хэндла нет метода :Write → ошибка убивает
-- вызывающий таймер (в т.ч. GmodMcpMenuPoll!). Файлы-маркеры с hex
-- в имени работают, но упираются в MAX_PATH/"dangerous path" на
-- длинных ответах и текут хэндлами.
-- РАБОЧИЙ КАНАЛ: MsgN() из menu-state пишет напрямую в
-- garrysmod/console.log (игра запущена с -condebug). Ответ идёт
-- строками:
--   GMCPO|<id>|<seq>/<total>|<hex>
-- MCP-сервер читает хвост console.log и собирает hex-чанки.
--
-- kind'ы команд:
--   "lua"      — RunString code, результат через hex-канал
--   "console"  — RunConsoleCommand(unpack(parts))
--   "shot"     — engine screenshot -> имя TGA из screenshots/
--   "js"       — QueueJavascript на странице меню (без результата)
--   "js_eval"  — eval JS + ответ через lua.Run -> GMCP_outh -> hex-канал
-- ВНИМАНИЕ: удалённое исполнение команд. Только для локальной отладки.
-- =============================================

if not MENU_DLL then return end
if GMOD_MCP_MENU_LOADED then return end
GMOD_MCP_MENU_LOADED = true

GMOD_MCP = GMOD_MCP or {}
GMOD_MCP.Dir = "gmod_mcp"

file.CreateDir(GMOD_MCP.Dir)

-- ---------- ответный канал: hex в именах файлов ----------

local OUT_CHUNK = 1800 -- hex-символов на строку console.log

-- Ответ уходит построчно в console.log через MsgN:
--   GMCPO|<id>|<seq>/<total>|<hex>
local function mwrite_out(id, text)
    text = tostring(text or "")
    local hex = (text:gsub(".", function(c)
        return string.format("%02x", c:byte())
    end))
    local total = math.max(1, math.ceil(#hex / OUT_CHUNK))
    for seq = 1, total do
        MsgN("GMCPO|" .. id .. "|" .. seq .. "/" .. total .. "|"
            .. hex:sub((seq - 1) * OUT_CHUNK + 1, seq * OUT_CHUNK))
    end
end

-- GMCP_outh — приёмник результатов из JS (вызывается через lua.Run
-- со страницы меню; hex т.к. lua.Run делает format() и % ломал бы код)
local GMCP_answered = {}
function GMCP_outh(id, hexstr)
    GMCP_answered[id] = true
    local s = (tostring(hexstr or ""):gsub("%x%x", function(cc)
        return string.char(tonumber(cc, 16))
    end))
    mwrite_out(id, s)
end

-- ---------- исполнение Lua ----------

-- В menu state НЕТ CompileString/loadstring — только RunString.
-- RunString не возвращает значения и НЕ поглощает ошибки: ошибка
-- в чанке убивает весь mtick. Поэтому ошибки ловим ДВАЖДЫ:
-- pcall внутри обёртки (runtime) + pcall(RunString) снаружи (синтаксис).
local function mrun(code)
    local out = {}
    local oldprint = print
    print = function(...)
        local t = {}
        for i = 1, select("#", ...) do t[i] = tostring(select(i, ...)) end
        out[#out + 1] = table.concat(t, "\t")
    end
    local wrapped = "local __o = { pcall(function() " .. (code or "") .. " end) }\n"
        .. "if not __o[1] then print('ERROR: ' .. tostring(__o[2])) "
        .. "elseif __o[2] ~= nil then print('=> ' .. tostring(__o[2])) end"
    local ok, err = pcall(RunString, wrapped)
    print = oldprint
    if not ok then
        out[#out + 1] = "SYNTAX: " .. tostring(err)
    end
    return table.concat(out, "\n")
end

-- ---------- JS-канал (страница меню) ----------

local function mhtml()
    if IsValid(pnlMainMenu) and IsValid(pnlMainMenu.HTML) then
        return pnlMainMenu.HTML
    end
end

-- kind=js: fire-and-forget
local function mjs(code)
    local h = mhtml()
    if not h then return "ERROR: menu page not ready (pnlMainMenu.HTML invalid)" end
    local jf = h.QueueJavascript or h.RunJavascript
    if not jf then return "ERROR: no JS queue method on menu DHTML" end
    jf(h, code or "")
    return "ok (queued; no result — use js_eval)"
end

-- экранирование кода в JS-строковый литерал
local function jstr(s)
    return (tostring(s or ""):gsub('[%z\1-\31\\"]', function(c)
        if c == '\\' then return '\\\\' end
        if c == '"' then return '\\"' end
        if c == '\n' then return '\\n' end
        if c == '\r' then return '\\r' end
        if c == '\t' then return '\\t' end
        return string.format('\\u%04x', c:byte())
    end))
end

-- kind=js_eval: выполняем JS на странице, результат JSON возвращаем
-- через lua.Run("GMCP_outh('<id>','<hex>')") — зарегистрированный
-- AddFunction-биндинг страницы меню (см. mainmenu.lua).
local function mjs_eval(id, code)
    local h = mhtml()
    if not h then
        mwrite_out(id, "ERROR: menu page not ready (pnlMainMenu.HTML invalid)")
        return
    end
    local jf = h.QueueJavascript or h.RunJavascript
    if not jf then
        mwrite_out(id, "ERROR: no JS queue method on menu DHTML")
        return
    end
    local js = table.concat({
        '(function(){var id="', id, '";',
        'function hx(s){s=unescape(encodeURIComponent(s));var h="";for(var i=0;i<s.length;i++){var c=s.charCodeAt(i);h+=(c<16?"0":"")+c.toString(16)}return h}',
        'function ret(o){lua.Run("GMCP_outh(\'"+id+"\',\'"+hx(JSON.stringify(o))+"\')")}',
        'try{var r=eval("', jstr(code), '");ret({ok:true,result:(r===undefined?null:r)})}',
        'catch(e){ret({ok:false,error:String(e&&(e.message||e))})}})();'
    })
    jf(h, js)
    -- Страховка от тишины: если lua.Run/GMCP_outh не сработал,
    -- через 8с пишем таймаут, чтобы мост не висел.
    timer.Simple(8, function()
        if not GMCP_answered[id] then
            mwrite_out(id, "ERROR: js_eval timeout (lua.Run/GMCP_outh channel dead?)")
        end
        GMCP_answered[id] = nil
    end)
end

-- ---------- скриншоты ----------

-- Снимок меню: в menu state нет render.Capture, зато движковая
-- команда `screenshot` пишет Snapshot*.tga в garrysmod/screenshots/
-- (захватывает кадр целиком, включая VGUI). Отдаём имя файла —
-- MCP-сервер читает его сам.
GMOD_MCP.ShotReq = nil

local function snap_seen()
    local seen = {}
    for _, f in ipairs(file.Find("screenshots/Snapshot*", "GAME") or {}) do
        seen[f] = true
    end
    return seen
end

local function shot_poll()
    local req = GMOD_MCP.ShotReq
    if not req then return end
    for _, f in ipairs(file.Find("screenshots/Snapshot*", "GAME") or {}) do
        if not req.seen[f] then
            mwrite_out(req.id, "ok " .. f)
            GMOD_MCP.ShotReq = nil
            return
        end
    end
    if (CurTime() or SysTime()) - req.t > 5 then
        mwrite_out(req.id, "screenshot timeout")
        GMOD_MCP.ShotReq = nil
    end
end

-- ---------- опрос ----------

-- Выполнение одной команды (без чтения файла) — возвращает ответ
-- или nil, если ответ уйдёт асинхронно (js_eval, shot).
local function mexec(cmd)
    if cmd.kind == "console" then
        -- RunConsoleCommand принимает пары (name, arg)
        local parts = {}
        for p in string.gmatch(cmd.command or "", "%S+") do parts[#parts + 1] = p end
        local cok, cerr = pcall(RunConsoleCommand, unpack(parts))
        return cok and ("menu console: " .. tostring(cmd.command))
            or ("ERROR: " .. tostring(cerr))
    elseif cmd.kind == "shot" then
        RunConsoleCommand("screenshot")
        GMOD_MCP.ShotReq = { id = cmd.id, seen = snap_seen(),
                             t = CurTime() or SysTime() }
        return "shot requested"
    elseif cmd.kind == "js" then
        return mjs(cmd.code)
    elseif cmd.kind == "js_eval" then
        -- ответ пишет GMCP_outh асинхронно под cmd.id (в батче — id_<i>)
        mjs_eval(cmd.id, cmd.code)
        return nil
    elseif cmd.kind == "batch" and type(cmd.cmds) == "table" then
        -- мультиплекс: много команд за один round-trip
        local parts = {}
        for i, sub in ipairs(cmd.cmds) do
            sub.id = sub.id or (cmd.id .. "_" .. i)
            local r = mexec(sub)
            if r ~= nil then parts[#parts + 1] = "[" .. i .. "] " .. tostring(r) end
        end
        return table.concat(parts, "\n")
    end
    return mrun(cmd.code or "")
end

local function mdispatch(f)
    local path = GMOD_MCP.Dir .. "/" .. f
    local raw = file.Read(path, "DATA")
    file.Delete(path)
    -- pcall: кривой JSON не должен убивать тик и съедать команду
    local ok, cmd = pcall(util.JSONToTable, raw or "")
    if not (ok and cmd and cmd.id) then return end
    local res = mexec(cmd)
    if res ~= nil then mwrite_out(cmd.id, res) end
end

local function mtick()
    pcall(shot_poll)
    local files = file.Find(GMOD_MCP.Dir .. "/cmd_menu_*.json", "DATA")
    if not files then return end
    for _, f in ipairs(files) do
        -- mdispatch целиком в pcall: любая ошибка обработчика
        -- не должна убивать poll-таймер (иначе мост умирает
        -- на первой же проблемной команде).
        local ok, err = pcall(mdispatch, f)
        if not ok then
            -- id из имени файла: cmd_menu_<id>.json
            local id = f:match("^cmd_menu_(.+)%.json$")
            if id then
                pcall(mwrite_out, id, "ERROR: dispatch: " .. tostring(err))
            end
        end
    end
end

if timer then
    timer.Create("GmodMcpMenuPoll", 0.02, 0, mtick)
else
    hook.Add("Think", "GmodMcpMenuPoll", mtick)
end

MsgN("[gmod_mcp] menu-realm bridge loaded")
