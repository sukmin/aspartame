# Aspartame

Windows 11용 경량 이미지 뷰어. Rust + egui/eframe로 작성했습니다.

- 지원 포맷: PNG, JPEG, BMP, GIF(첫 프레임), WebP, TIFF, ICO, TGA, CR3(캐논 RAW, 파일에 내장된 JPEG로 표시, HDR PQ 촬영 파일 제외)
- 창에 꽉 차게 보기(기본), 실제 크기, 커서 기준 확대/축소, 드래그 이동
- 좌우 방향키로 같은 폴더 안의 이미지를 **이름순(탐색기와 같은 자연 정렬)**으로 이동
- 백그라운드 디코딩, 앞뒤 이미지 프리로드, EXIF 회전 자동 적용
- 전체 화면, 드래그 앤 드롭, 명령줄 인자로 열기
- zip 파일을 열면 안의 이미지를 **갤러리**로 보여 주고, 클릭하면 한 장씩 보기 (암호가 걸린 zip은 암호를 묻습니다)
- **압축 풀기**: zip, 7z, rar, tar, tar.gz / tar.bz2 / tar.xz / tar.zst 와 gz·bz2·xz·zst 단일 파일. 암호가 걸려 있으면 암호를 입력받습니다
- CR3의 내장 JPEG를 촬영 정보와 함께 JPEG 파일로 저장

## 빌드

사전 준비: Rust 툴체인(MSVC), Visual Studio 2022(C++ 데스크톱 워크로드).

```powershell
# 개발 실행 (콘솔에 로그 출력)
cargo run -- "C:\Pictures\sample.jpg"

# 단위 테스트 (정렬, 폴더 탐색, 줌/맞춤 계산, 압축 풀기, 화면 흐름)
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
| `Ctrl+O` | 이미지 또는 압축 파일 열기 |
| `Ctrl+E` | 열어 둔 압축 파일을 그 파일이 있는 폴더에 풀기 |
| `Ctrl+Shift+E` | 폴더를 골라서 압축 풀기 |
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
| `F5` | 폴더 또는 압축 파일 다시 읽기 |
| 우클릭 | 컨텍스트 메뉴 |

zip 갤러리에서는 클릭이나 `Enter`로 이미지를 열고, 방향키와 `Home` / `End`로 선택을 옮기며, 마우스 휠로 스크롤합니다.

### 압축 풀기

이미지가 없는 zip이나 7z·rar·tar 같은 다른 압축 파일을 열면 내용 목록과 `[압축 풀기]`, `[다른 폴더에 풀기...]` 단추가 있는
화면이 나옵니다. 이미지가 있는 zip은 갤러리로 열리며, 우클릭 메뉴나 `Ctrl+E`로 풀 수 있습니다.
gz·bz2·xz·zst는 풀어 봐야 내용을 알 수 있어 목록 없이 크기만 보입니다.

- 최상위 항목이 하나뿐이면 그 폴더(또는 파일)를 그대로 꺼내고, 여럿이면 압축 파일 이름의 폴더를 만들어 담습니다.
- 같은 이름이 이미 있으면 덮어쓰지 않고 `이름 (2)`처럼 번호를 붙입니다.
- 임시 폴더(`이름.푸는 중`)에 먼저 풀고 끝까지 성공했을 때만 제 이름으로 옮깁니다. 실패하거나 취소하면 아무것도 남지 않습니다.
- 암호가 걸려 있으면 암호를 묻고, 틀리면 다시 묻습니다. 목록까지 암호화한 7z·rar는 열 때 묻습니다.
- `../`나 드라이브 문자가 들어 있는 이름도 풀 폴더 밖으로 나가지 않습니다. 심볼릭 링크, 하드 링크, 장치 파일은 만들지 않고
  건너뛴 개수를 완료 창에 알립니다.
- 대소문자만 다른 이름처럼 Windows에서 같은 파일이 되는 항목은 덮어쓰지 않고 `이름 (2)`로 풉니다.
- 옛 zip 암호, 7z, RAR은 틀린 암호와 손상된 파일을 구분하지 못합니다. 맞는 암호인데도 계속 틀렸다고 하면 파일 손상을 의심하세요.
- RAR은 큰 파일 하나를 푸는 동안에는 취소가 그 파일이 끝난 뒤에 적용됩니다.
- gz·bz2·xz·zst는 안에 tar가 들어 있으면 tar까지 풀고, 아니면 파일 하나로 풉니다.

RAR은 [unrar](https://crates.io/crates/unrar) 크레이트에 들어 있는 UnRAR 소스로 풉니다. UnRAR 라이선스는 압축 풀기 용도의
사용을 허용하지만 RAR 압축 알고리즘을 다시 만드는 데 쓰는 것은 금지합니다.

## 구조

```
src/
├── main.rs           진입점, 창 옵션, 아이콘
├── app.rs            앱 상태, 입력 처리, 그리기, 텍스처 캐시
├── image_loader.rs   백그라운드 디코딩 스레드 (마지막 요청 우선, 프리로드)
├── navigator.rs      폴더 스캔, 이름순 자연 정렬, 이전/다음 이동
├── cr3.rs            CR3에서 내장 JPEG 추출, EXIF를 넣어 저장
├── archive.rs        zip 안의 이미지 목록과 내용 읽기
├── extract.rs        압축 형식 판별, 목록 읽기, 폴더에 풀기 (zip·7z·rar·tar·gz·bz2·xz·zst)
├── archive_view.rs   압축 파일 화면, 암호 입력 창, 풀기 진행 표시
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
파일 형식 `Aspartame.Image`(이미지), `Aspartame.Archive`(압축 파일)와 기본 프로그램 정보를 기록한 뒤 **설정 > 기본 앱 > Aspartame** 페이지를 엽니다.
Windows 10/11 은 스크립트가 기본 앱을 직접 바꾸는 것을 막으므로, 열린 페이지에서 `[기본값으로 설정]` 단추를
누르거나 확장자를 하나씩 눌러 Aspartame 를 고르면 됩니다. 이후 탐색기에서 이미지를 더블클릭하면 Aspartame 로 열립니다.

압축 파일(`.zip .7z .rar .tar .gz .tgz .bz2 .tbz .tbz2 .xz .txz .zst .tzst`)은 `[기본값으로 설정]` 대상에서 빼고
"연결 프로그램" 후보로만 추가합니다. 압축 파일을 우클릭한 뒤 **연결 프로그램 > 다른 앱 선택 > Aspartame > 항상** 을 고르면
Aspartame 로 열립니다. 압축 파일은 이미지와 다른 파일 형식(`Aspartame.Archive`)으로 등록하므로 탐색기에서 Aspartame 아이콘이
아니라 **Windows 의 압축 폴더(zip) 아이콘**으로 보입니다.

> 0.3.0 이하에서 zip 을 이미 Aspartame 로 지정해 두었다면 아이콘이 Aspartame 모양으로 남아 있습니다.
> 새 등록 스크립트를 실행한 뒤 zip 파일에서 **연결 프로그램 > 다른 앱 선택 > Aspartame > 항상** 을 한 번만 다시 골라 주세요.
> (Windows 는 프로그램이 기본 앱 지정을 직접 바꾸는 것을 막기 때문에 자동으로 옮길 수 없습니다.)

exe 경로를 직접 지정하려면 `scripts\register-default-viewer.bat "C:\경로\aspartame.exe"` 처럼 인자로 넘기세요.
