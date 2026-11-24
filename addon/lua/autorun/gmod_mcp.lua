-- =============================================
-- gmod_mcp: debug bridge for the gmod-mcp MCP server
-- Читает data/gmod_mcp/cmd_<realm>_<id>.json -> выполняет ->
-- пишет data/gmod_mcp/out_<id>.txt (или shot_<id>.png для скриншотов)
-- ВНИМАНИЕ: удалённое исполнение Lua. Только для локальной отладки.
-- =============================================

GMOD_MCP = GMOD_MCP or {}
GMOD_MCP.Dir = "gmod_mcp"

file.CreateDir(GMOD_MCP.Dir)

local realm = SERVER and "srv" or "cl"

-- Выполнить код, перехватить print, вернуть текст результата
local function gmcp_run(code)
    local out = {}
    local oldprint = print
    print = function(...)
        local t = {}
        for i = 1, select("#", ...) do t[i] = tostring(select(i, ...)) end
        out[#out + 1] = table.concat(t, "\t")
    end
    -- handleError=false: на синтаксической ошибке вернёт строку ошибки
    local fn = CompileString(code, "gmod_mcp_cmd", false)
    local ok, err
    if isfunction(fn) then
        ok, err = pcall(fn)
    else
        ok, err = false, fn
    end
    print = oldprint
    if not ok then
        out[#out + 1] = "ERROR: " .. tostring(err)
    elseif err ~= nil then
        out[#out + 1] = "=> " .. tostring(err)
    end
    return table.concat(out, "\n")
end

local function gmcp_write_out(id, text)
    file.Write(GMOD_MCP.Dir .. "/out_" .. id .. ".txt", text or "")
end

local function gmcp_handle(cmd)
    if cmd.kind == "batch" and type(cmd.cmds) == "table" then
        -- мультиплекс: много команд за один round-trip;
        -- sub-ответы помечаются [i], асинхронные (shot) — под id_<i>
        local parts = {}
        for i, sub in ipairs(cmd.cmds) do
            sub.id = sub.id or (cmd.id .. "_" .. i)
            local r = gmcp_handle(sub)
            if r ~= nil then parts[#parts + 1] = "[" .. i .. "] " .. tostring(r) end
        end
        return table.concat(parts, "\n")
    end
    if cmd.kind == "console" then
        -- server: game.ConsoleCommand(полная строка);
        -- client: RunConsoleCommand(пары имя, арг)
        local okc, errc
        if SERVER then
            okc, errc = pcall(game.ConsoleCommand, cmd.command or "")
        else
            local parts = {}
            for p in string.gmatch(cmd.command or "", "%S+") do parts[#parts + 1] = p end
            okc, errc = pcall(RunConsoleCommand, unpack(parts))
        end
        return okc and ("console: " .. tostring(cmd.command))
            or ("ERROR: " .. tostring(errc))
    end
    if CLIENT and cmd.kind == "shot" then
        -- Камера ставится немедленно; кадр снимется в PostRender
        GMOD_MCP.Cam = {
            pos = Vector(cmd.pos[1], cmd.pos[2], cmd.pos[3]),
            ang = Angle(cmd.ang[1], cmd.ang[2], cmd.ang[3]),
            fov = cmd.fov or 75,
            drawviewer = cmd.drawviewer == true,
        }
        if cmd.hud ~= false then RunConsoleCommand("cl_drawhud", "0") end
        GMOD_MCP.ShotId = cmd.id
        return nil -- результат запишет PostRender
    elseif CLIENT and cmd.kind == "cam" then
        -- Постоянная отладочная камера (вкл/выкл)
        if cmd.on then
            GMOD_MCP.Cam = {
                pos = Vector(cmd.pos[1], cmd.pos[2], cmd.pos[3]),
                ang = Angle(cmd.ang[1], cmd.ang[2], cmd.ang[3]),
                fov = cmd.fov or 75,
                drawviewer = cmd.drawviewer == true,
            }
            return "cam ON " .. tostring(GMOD_MCP.Cam.pos)
        else
            GMOD_MCP.Cam = nil
            return "cam OFF"
        end
    end
    return gmcp_run(cmd.code or "")
end

-- Поллер команд: каждый реалм забирает только свои cmd_<realm>_*.json
timer.Create("GmodMcpPoll_" .. realm, 0.02, 0, function()
    local files = file.Find(GMOD_MCP.Dir .. "/cmd_" .. realm .. "_*.json", "DATA")
    if not files then return end
    for _, f in ipairs(files) do
        local path = GMOD_MCP.Dir .. "/" .. f
        local raw = file.Read(path, "DATA")
        file.Delete(path)
        local cmd = raw and util.JSONToTable(raw)
        if cmd and cmd.id then
            local res = gmcp_handle(cmd)
            if res ~= nil then gmcp_write_out(cmd.id, res) end
        end
    end
end)

-- Сбор Lua-ошибок обоих реалмов в общий файл
hook.Add("OnLuaError", "GmodMcpErr", function(err, rlm, stack, name, id)
    file.Append(
        GMOD_MCP.Dir .. "/errors.txt",
        os.date("%H:%M:%S") .. " [" .. tostring(rlm) .. "] " .. tostring(err) .. "\n"
    )
end)

if CLIENT then
    -- Отладочная камера: подменяет вид, пока GMOD_MCP.Cam задан
    hook.Add("CalcView", "GmodMcpCam", function(ply, pos, ang, fov)
        local c = GMOD_MCP.Cam
        if not c then return end
        return { origin = c.pos, angles = c.ang, fov = c.fov, drawviewer = c.drawviewer }
    end)

    -- Снимок кадра строго в render-хуке (иначе Capture отдаёт чёрный кадр)
    hook.Add("PostRender", "GmodMcpShot", function()
        local id = GMOD_MCP.ShotId
        if not id then return end
        GMOD_MCP.ShotId = nil
        local png = render.Capture({ format = "jpeg", quality = 70, x = 0, y = 0, w = ScrW(), h = ScrH() })
        if png then
            file.Write(GMOD_MCP.Dir .. "/shot_" .. id .. ".jpg", png)
            gmcp_write_out(id, "ok " .. #png .. " bytes")
        else
            gmcp_write_out(id, "render.Capture returned nil")
        end
        GMOD_MCP.Cam = nil
        RunConsoleCommand("cl_drawhud", "1")
    end)
end

concommand.Add("gmod_mcp_ping", function()
    print("gmod_mcp alive on realm:", realm)
end)

MsgN("[gmod_mcp] bridge loaded on realm " .. realm)
