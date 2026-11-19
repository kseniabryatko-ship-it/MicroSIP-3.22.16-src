@echo off
rem Builds MicroSIP-Rust for Windows in Docker: checks, tests and the
rem programs, without Rust installed here. Needs Docker Desktop running.
rem The result goes to target\docker-out next to this file.
setlocal
cd /d "%~dp0"
docker build -t microsip-rs-build docker || goto :failed
if not exist target\docker-out mkdir target\docker-out
docker run --rm ^
  -v "%cd%":/src ^
  -v microsip-rs-cargo:/usr/local/cargo/registry ^
  -v microsip-rs-target:/target ^
  -v "%cd%\target\docker-out":/out ^
  microsip-rs-build || goto :failed
echo.
echo Done. The programs are in target\docker-out
exit /b 0
:failed
echo.
echo Docker build failed.
exit /b 1
