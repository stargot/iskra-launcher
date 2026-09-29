@echo off
rem Build wrapper: sets up MSVC x64 env (vcvars64) then runs cargo build.
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" >nul 2>&1
cd /d "%~dp0"
cargo build --release %*
