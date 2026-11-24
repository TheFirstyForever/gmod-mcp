@echo off
rem Пересборка gmod-mcp и копирование бинаря рядом с исходником.
rem CARGO_TARGET_DIR обязан быть ASCII-путём: mingw-линкер падает
rem на кириллице/пробелах в пути репозитория.
set CARGO_TARGET_DIR=C:/Users/1/.cargo-target/gmod-mcp
"%USERPROFILE%\.cargo\bin\cargo.exe" build --release || exit /b 1
copy /y "%CARGO_TARGET_DIR%\release\gmod-mcp.exe" "%~dp0gmod-mcp.exe" >nul
echo OK: %~dp0gmod-mcp.exe
