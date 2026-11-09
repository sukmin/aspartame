# Aspartame

Windows 11용 경량 이미지 뷰어. Rust + egui/eframe로 작성했습니다.

- 지원 포맷: PNG, JPEG, BMP, GIF(첫 프레임), WebP, TIFF, ICO, TGA, CR3(캐논 RAW, 파일에 내장된 JPEG로 표시, HDR PQ 촬영 파일 제외)
- 창에 꽉 차게 보기(기본), 실제 크기, 커서 기준 확대/축소, 드래그 이동
- 좌우 방향키로 같은 폴더 안의 이미지를 **이름순(탐색기와 같은 자연 정렬)**으로 이동
- 백그라운드 디코딩, 앞뒤 이미지 프리로드, EXIF 회전 자동 적용
- 전체 화면, 드래그 앤 드롭, 명령줄 인자로 열기
- zip 파일을 열면 안의 이미지를 **갤러리**로 보여 주고, 클릭하면 한 장씩 보기
- CR3의 내장 JPEG를 촬영 정보와 함께 JPEG 파일로 저장

## 빌드

사전 준비: Rust 툴체인(MSVC), Visual Studio 2022(C++ 데스크톱 워크로드).

```powershell
# 개발 실행 (콘솔에 로그 출력)
cargo run -- "C:\Pictures\sample.jpg"

# 단위 테스트 (정렬, 폴더 탐색, 줌/맞춤 계산)
cargo test

# 릴리스 빌드 -> target\release\aspartame.exe
cargo build --release
```

> **Smart App Control이 켜져 있으면** cargo.exe와 직접 빌드한 exe가 모두 차단됩니다.
> Windows 보안 > 앱 및 브라우저 컨트롤 > Smart App Control 설정에서 끈 뒤 빌드하세요.
> (한 번 끄면 Windows를 다시 설치하기 전까지 다시 켤 수 없습니다.)

## 조작

| 입력 | 동작 |
|------|------|
| `Ctrl+O` | 파일 또는 zip 열기 |
| `Ctrl+S` | CR3의 내장 JPEG 저장 |
| `←` / `→`, `PageUp` / `PageDown`, 마우스 휠 | 이전 / 다음 이미지 (끝에서 순환) |
| `Home` / `End` | 첫 / 마지막 이미지 |
| `F` 또는 `0` | 창에 꽉 차게 보기 |
| `1` | 실제 크기 (모니터 픽셀 1:1) |
| `+` / `-`, `Alt`+마우스 휠 | 확대 / 축소 (휠은 커서 기준) |
| 마우스 드래그 | 이동 |
| `F11`, 더블클릭 | 전체 화면 토글 (2초간 움직임이 없으면 커서 숨김) |
| `Esc` | 전체 화면 해제, zip의 한 장 보기에서는 갤러리로 복귀, 그 외에는 종료 |
| `I` | 파일 정보 표시 토글 |
| `F5` | 폴더 또는 zip 다시 읽기 |
| 우클릭 | 컨텍스트 메뉴 |

zip 갤러리에서는 클릭이나 `Enter`로 이미지를 열고, 방향키와 `Home` / `End`로 선택을 옮기며, 마우스 휠로 스크롤합니다.

## 구조

```
src/
├── main.rs           진입점, 창 옵션, 아이콘
├── app.rs            앱 상태, 입력 처리, 그리기, 텍스처 캐시
├── image_loader.rs   백그라운드 디코딩 스레드 (마지막 요청 우선, 프리로드)
├── navigator.rs      폴더 스캔, 이름순 자연 정렬, 이전/다음 이동
├── cr3.rs            CR3에서 내장 JPEG 추출, EXIF를 넣어 저장
├── archive.rs        zip 안의 이미지 목록과 내용 읽기
├── gallery.rs        zip 갤러리 화면
├── thumbnails.rs     썸네일을 만드는 작업 스레드
├── viewport.rs       창에 맞춤 / 실제 크기 / 커서 기준 줌 계산
└── input.rs          키·휠·드롭 입력 -> 액션 매핑
```

## 패키징

```powershell
scripts\package.bat            # cargo build --release 후 dist\Aspartame 폴더와 dist\Aspartame-<버전>-win64.zip 생성
scripts\package.bat /nobuild   # 이미 빌드된 target\release\aspartame.exe 로 배포 폴더만 다시 구성
```

`dist\Aspartame` 에는 exe, 등록/해제 bat, `사용법.txt`, `README.md`, `개발가이드.html`(소스 구조와 동작 원리 설명,
원본은 `docs\guide.html`) 이 들어갑니다. 다른 PC 에서는 zip 을 풀고 `register-default-viewer.bat` 를 실행하면 됩니다.

## 기본 이미지 뷰어로 등록

```powershell
cargo build --release
scripts\register-default-viewer.bat          # 등록 (관리자 권한 불필요)
scripts\unregister-default-viewer.bat        # 해제 (/purge 를 붙이면 복사한 exe 도 삭제)
```

등록 스크립트는 exe 를 `%LOCALAPPDATA%\Programs\Aspartame` 에 복사하고, 현재 사용자 레지스트리(HKCU)에
파일 형식 `Aspartame.Image` 와 기본 프로그램 정보를 기록한 뒤 **설정 > 기본 앱 > Aspartame** 페이지를 엽니다.
Windows 10/11 은 스크립트가 기본 앱을 직접 바꾸는 것을 막으므로, 열린 페이지에서 `[기본값으로 설정]` 단추를
누르거나 확장자를 하나씩 눌러 Aspartame 를 고르면 됩니다. 이후 탐색기에서 이미지를 더블클릭하면 Aspartame 로 열립니다.

`.zip` 은 이미지가 없는 압축 파일에도 쓰이므로 `[기본값으로 설정]` 대상에서 빼고 "연결 프로그램" 후보로만 추가합니다.
zip 을 우클릭한 뒤 **연결 프로그램 > Aspartame** 으로 열면 갤러리로 보입니다.

exe 경로를 직접 지정하려면 `scripts\register-default-viewer.bat "C:\경로\aspartame.exe"` 처럼 인자로 넘기세요.
