@echo off
chcp 65001 >nul
setlocal EnableExtensions

rem ============================================================
rem  Aspartame 를 Windows 기본 이미지 뷰어 후보로 등록합니다.
rem
rem  - 관리자 권한 불필요 (현재 사용자 레지스트리 HKCU 에만 기록)
rem  - exe 를 %LOCALAPPDATA%\Programs\Aspartame 에 복사한 뒤 그 경로를 등록
rem  - 마지막에 설정 > 기본 앱 > Aspartame 페이지를 열어 줍니다.
rem    Windows 10/11 은 스크립트가 기본 앱을 직접 바꾸는 것을 막으므로,
rem    열린 페이지에서 확장자를 눌러 Aspartame 를 고르면 끝납니다.
rem
rem  사용법: register-default-viewer.bat [aspartame.exe 경로]
rem  해제:   unregister-default-viewer.bat [/purge]
rem ============================================================

set "APP_NAME=Aspartame"
set "PROG_ID=Aspartame.Image"
set "INSTALL_DIR=%LOCALAPPDATA%\Programs\Aspartame"
set "EXE=%INSTALL_DIR%\aspartame.exe"
set "EXTS=.png .jpg .jpeg .jpe .jfif .bmp .gif .webp .tif .tiff .ico .tga"

rem --- 1) 실행 파일 찾기: 인자 > 스크립트 옆 > ..\target\release ---
set "SRC="
if not "%~1"=="" if exist "%~1" set "SRC=%~f1"
if not defined SRC if exist "%~dp0aspartame.exe" set "SRC=%~dp0aspartame.exe"
if not defined SRC if exist "%~dp0..\target\release\aspartame.exe" set "SRC=%~dp0..\target\release\aspartame.exe"
if not defined SRC (
    echo [오류] aspartame.exe 를 찾을 수 없습니다.
    echo        먼저 cargo build --release 를 실행하거나, exe 경로를 인자로 주세요.
    echo        예: %~nx0 "C:\경로\aspartame.exe"
    exit /b 1
)

rem --- 2) 설치 폴더로 복사 (빌드 폴더를 지워도 연결이 깨지지 않도록) ---
if not exist "%INSTALL_DIR%" mkdir "%INSTALL_DIR%"
if /I not "%SRC%"=="%EXE%" (
    copy /Y "%SRC%" "%EXE%" >nul
    if errorlevel 1 (
        echo [오류] 복사에 실패했습니다. Aspartame 가 실행 중이면 닫고 다시 시도하세요.
        exit /b 1
    )
)
echo 설치 위치: %EXE%

rem --- 3) 파일 형식(ProgID) 등록 ---
reg add "HKCU\Software\Classes\%PROG_ID%" /ve /d "이미지 파일 (%APP_NAME%)" /f >nul
reg add "HKCU\Software\Classes\%PROG_ID%" /v FriendlyTypeName /d "이미지 파일 (%APP_NAME%)" /f >nul
reg add "HKCU\Software\Classes\%PROG_ID%\DefaultIcon" /ve /d "\"%EXE%\",0" /f >nul
reg add "HKCU\Software\Classes\%PROG_ID%\shell\open" /ve /d "%APP_NAME%으로 열기" /f >nul
reg add "HKCU\Software\Classes\%PROG_ID%\shell\open\command" /ve /d "\"%EXE%\" \"%%1\"" /f >nul

rem --- 4) 기본 프로그램(Capabilities) 등록 -> 설정 > 기본 앱 목록에 표시 ---
set "CAP=HKCU\Software\%APP_NAME%\Capabilities"
reg add "%CAP%" /v ApplicationName /d "%APP_NAME%" /f >nul
reg add "%CAP%" /v ApplicationDescription /d "가볍고 빠른 이미지 뷰어" /f >nul
reg add "%CAP%" /v ApplicationIcon /d "\"%EXE%\",0" /f >nul
reg add "HKCU\Software\RegisteredApplications" /v "%APP_NAME%" /d "Software\%APP_NAME%\Capabilities" /f >nul

rem --- 5) 확장자별로 연결 후보에 추가 ---
for %%E in (%EXTS%) do (
    reg add "%CAP%\FileAssociations" /v "%%E" /d "%PROG_ID%" /f >nul
    reg add "HKCU\Software\Classes\%%E\OpenWithProgids" /v "%PROG_ID%" /t REG_NONE /f >nul
)

rem --- 6) 탐색기에 연결 정보 변경 알림 (SHChangeNotify SHCNE_ASSOCCHANGED) ---
powershell -NoProfile -ExecutionPolicy Bypass -EncodedCommand JABzAGkAZwAgAD0AIAAnAFsARABsAGwASQBtAHAAbwByAHQAKAAiAHMAaABlAGwAbAAzADIALgBkAGwAbAAiACkAXQAgAHAAdQBiAGwAaQBjACAAcwB0AGEAdABpAGMAIABlAHgAdABlAHIAbgAgAHYAbwBpAGQAIABTAEgAQwBoAGEAbgBnAGUATgBvAHQAaQBmAHkAKABpAG4AdAAgAGUAdgBlAG4AdABJAGQALAAgAGkAbgB0ACAAZgBsAGEAZwBzACwAIABJAG4AdABQAHQAcgAgAGkAdABlAG0AMQAsACAASQBuAHQAUAB0AHIAIABpAHQAZQBtADIAKQA7ACcACgAkAHQAIAA9ACAAQQBkAGQALQBUAHkAcABlACAALQBNAGUAbQBiAGUAcgBEAGUAZgBpAG4AaQB0AGkAbwBuACAAJABzAGkAZwAgAC0ATgBhAG0AZQAgAE4AbwB0AGkAZgB5ACAALQBOAGEAbQBlAHMAcABhAGMAZQAgAEEAcwBwAGEAcgB0AGEAbQBlAFMAZQB0AHUAcAAgAC0AUABhAHMAcwBUAGgAcgB1AAoAJAB0ADoAOgBTAEgAQwBoAGEAbgBnAGUATgBvAHQAaQBmAHkAKAAwAHgAMAA4ADAAMAAwADAAMAAwACwAIAAwACwAIABbAEkAbgB0AFAAdAByAF0AOgA6AFoAZQByAG8ALAAgAFsASQBuAHQAUAB0AHIAXQA6ADoAWgBlAHIAbwApAA== >nul 2>&1

echo.
echo 등록이 끝났습니다. 이제 열리는 설정 창에서 기본 앱으로 지정하세요.
echo   - "Aspartame" 페이지에 [기본값으로 설정] 단추가 있으면 한 번만 누르면 됩니다.
echo   - 없으면 .png, .jpg 등 확장자를 하나씩 눌러 Aspartame 를 선택하세요.
echo   - 또는 탐색기에서 이미지를 우클릭 ^> 연결 프로그램 ^> 다른 앱 선택 ^> Aspartame ^> 항상.
echo.
start "" "ms-settings:defaultapps?registeredAppUser=%APP_NAME%"
endlocal
exit /b 0
