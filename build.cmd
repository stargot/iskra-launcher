@echo off
rem Iskra build wrapper (Фаза 1, шаг 1). Порядок важен: cargo build встраивает ui/dist
rem (tauri.conf.json frontendDist), поэтому UI собирается раньше cargo (риск 8).
rem Использование: build.cmd [аргументы cargo build --release]
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" >nul 2>&1
cd /d "%~dp0"
if not exist "ui\dist\" (
  echo [iskra] ui\dist не найден - собираю UI ^(npm^)...
  pushd ui
  call npm install
  if errorlevel 1 goto :fail
  call npm run build
  if errorlevel 1 goto :fail
  popd
)
cargo build --release --features custom-protocol %*
exit /b %errorlevel%
:fail
popd
echo [iskra] UI build FAILED
exit /b 1
