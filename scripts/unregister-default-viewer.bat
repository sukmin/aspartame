@echo off
chcp 65001 >nul
setlocal EnableExtensions

rem ============================================================
rem  register-default-viewer.bat 가 등록한 내용을 모두 제거합니다.
rem
rem  사용법: unregister-default-viewer.bat [/purge]
rem    /purge : %LOCALAPPDATA%\Programs\Aspartame 에 복사한 exe 도 삭제
rem ============================================================

set "APP_NAME=Aspartame"
set "PROG_ID=Aspartame.Image"
set "INSTALL_DIR=%LOCALAPPDATA%\Programs\Aspartame"
set "EXTS=.png .jpg .jpeg .jpe .jfif .bmp .gif .webp .tif .tiff .ico .tga .cr3"
set "ARCHIVE_PROG_ID=Aspartame.Archive"
set "ARCHIVE_EXTS=.zip .7z .rar .tar .gz .tgz .bz2 .tbz .tbz2 .xz .txz .zst .tzst"

reg delete "HKCU\Software\RegisteredApplications" /v "%APP_NAME%" /f >nul 2>&1
reg delete "HKCU\Software\%APP_NAME%" /f >nul 2>&1
reg delete "HKCU\Software\Classes\%PROG_ID%" /f >nul 2>&1
for %%E in (%EXTS%) do (
    reg delete "HKCU\Software\Classes\%%E\OpenWithProgids" /v "%PROG_ID%" /f >nul 2>&1
)

reg delete "HKCU\Software\Classes\%ARCHIVE_PROG_ID%" /f >nul 2>&1
for %%E in (%ARCHIVE_EXTS%) do (
    reg delete "HKCU\Software\Classes\%%E\OpenWithProgids" /v "%ARCHIVE_PROG_ID%" /f >nul 2>&1
)
rem 0.3.0 까지는 zip 을 이미지 형식으로 등록했습니다. 그 흔적도 지웁니다.
reg delete "HKCU\Software\Classes\.zip\OpenWithProgids" /v "%PROG_ID%" /f >nul 2>&1

rem 탐색기에 연결 정보 변경 알림 (SHChangeNotify SHCNE_ASSOCCHANGED)
powershell -NoProfile -ExecutionPolicy Bypass -EncodedCommand JABzAGkAZwAgAD0AIAAnAFsARABsAGwASQBtAHAAbwByAHQAKAAiAHMAaABlAGwAbAAzADIALgBkAGwAbAAiACkAXQAgAHAAdQBiAGwAaQBjACAAcwB0AGEAdABpAGMAIABlAHgAdABlAHIAbgAgAHYAbwBpAGQAIABTAEgAQwBoAGEAbgBnAGUATgBvAHQAaQBmAHkAKABpAG4AdAAgAGUAdgBlAG4AdABJAGQALAAgAGkAbgB0ACAAZgBsAGEAZwBzACwAIABJAG4AdABQAHQAcgAgAGkAdABlAG0AMQAsACAASQBuAHQAUAB0AHIAIABpAHQAZQBtADIAKQA7ACcACgAkAHQAIAA9ACAAQQBkAGQALQBUAHkAcABlACAALQBNAGUAbQBiAGUAcgBEAGUAZgBpAG4AaQB0AGkAbwBuACAAJABzAGkAZwAgAC0ATgBhAG0AZQAgAE4AbwB0AGkAZgB5ACAALQBOAGEAbQBlAHMAcABhAGMAZQAgAEEAcwBwAGEAcgB0AGEAbQBlAFMAZQB0AHUAcAAgAC0AUABhAHMAcwBUAGgAcgB1AAoAJAB0ADoAOgBTAEgAQwBoAGEAbgBnAGUATgBvAHQAaQBmAHkAKAAwAHgAMAA4ADAAMAAwADAAMAAwACwAIAAwACwAIABbAEkAbgB0AFAAdAByAF0AOgA6AFoAZQByAG8ALAAgAFsASQBuAHQAUAB0AHIAXQA6ADoAWgBlAHIAbwApAA== >nul 2>&1

if /I "%~1"=="/purge" (
    if exist "%INSTALL_DIR%" rd /s /q "%INSTALL_DIR%"
    echo 설치 폴더를 삭제했습니다: %INSTALL_DIR%
)

echo 등록을 해제했습니다.
echo Aspartame 가 기본 앱이었던 확장자(압축 파일 포함)는 다음에 열 때 Windows 가 사용할 앱을 다시 물어봅니다.
endlocal
exit /b 0
