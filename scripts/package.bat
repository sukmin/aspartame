@echo off
chcp 65001 >nul
setlocal EnableExtensions

rem ============================================================
rem  릴리스 빌드 후 배포 폴더(dist\Aspartame)와 zip 을 만듭니다.
rem
rem  사용법: package.bat [/nobuild]
rem    /nobuild : cargo build 를 건너뛰고 target\release\aspartame.exe 를 그대로 사용
rem  결과:
rem    dist\Aspartame\aspartame.exe
rem    dist\Aspartame\register-default-viewer.bat
rem    dist\Aspartame\unregister-default-viewer.bat
rem    dist\Aspartame\사용법.txt, README.md, 개발가이드.html
rem    dist\Aspartame-<버전>-win64.zip
rem ============================================================

cd /d "%~dp0.."
set "DIST=dist\Aspartame"
set "EXE=target\release\aspartame.exe"

if /I not "%~1"=="/nobuild" (
    echo [1/3] cargo build --release
    cargo build --release
    if errorlevel 1 (
        echo [오류] 빌드에 실패했습니다.
        exit /b 1
    )
)

if not exist "%EXE%" (
    echo [오류] %EXE% 가 없습니다. 먼저 빌드하세요.
    exit /b 1
)

echo [2/3] 배포 폴더 구성: %DIST%
if exist "%DIST%" rd /s /q "%DIST%"
mkdir "%DIST%"
copy /Y "%EXE%" "%DIST%\" >nul
copy /Y "scripts\register-default-viewer.bat" "%DIST%\" >nul
copy /Y "scripts\unregister-default-viewer.bat" "%DIST%\" >nul
copy /Y "scripts\dist-readme.txt" "%DIST%\사용법.txt" >nul
copy /Y "README.md" "%DIST%\" >nul
copy /Y "docs\guide.html" "%DIST%\개발가이드.html" >nul

set "VER="
for /f "tokens=2 delims== " %%V in ('findstr /b /c:"version" Cargo.toml') do if not defined VER set "VER=%%~V"
if not defined VER set "VER=dev"
set "ZIP=dist\Aspartame-%VER%-win64.zip"

echo [3/3] 압축: %ZIP%
if exist "%ZIP%" del /q "%ZIP%"
powershell -NoProfile -ExecutionPolicy Bypass -Command "Compress-Archive -Path '%DIST%' -DestinationPath '%ZIP%' -Force"
if errorlevel 1 (
    echo [경고] zip 생성에 실패했습니다. %DIST% 폴더는 그대로 사용할 수 있습니다.
    exit /b 0
)

echo.
echo 완료: %DIST%  및  %ZIP%
echo 다른 PC 에서는 zip 을 풀고 register-default-viewer.bat 를 실행하면 됩니다.
endlocal
exit /b 0
