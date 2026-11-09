# Aspartame 이미지 뷰어 개발 계획

> Windows 11용 경량 이미지 뷰어를 Rust로 직접 만들기 위한 개발 계획서
> 작성일: 2026-09-13

---

## 0. 구현 현황 (2026-09-13)

| 항목 | 상태 |
|------|------|
| Phase 0 ~ 5 소스 코드 | 작성 완료 (`src/` 6개 모듈, `Cargo.toml`, `build.rs`, `assets/icon.*`) |
| 채택 버전 | eframe/egui 0.36.2, image 0.25.10, rfd 0.17.2, winresource 0.1.31 |
| 로컬 빌드 | **완료**. Smart App Control 해제 후 `cargo build --release` 성공 (경고 0). 이전에는 SAC가 `cargo.exe`를 차단했음 |
| 단위 테스트 | `cargo test` 14개 통과 (navigator 8, viewport 6) |
| 실행 검증 | 릴리스 exe로 이름순 이동·순환, 실제 크기, 창 맞춤, Home/End, 전체 화면, 손상 파일 오류 표시, 정보 오버레이, 한글 메뉴를 창 제목과 화면 캡처로 확인 |
| O-08 파일 연결 | `scripts\register-default-viewer.bat` / `unregister-default-viewer.bat` 작성, 등록·해제 검증 완료 |
| 패키징 | `scripts\package.bat` → `dist\Aspartame\` (exe, bat 2개, 사용법.txt, README.md) 및 `dist\Aspartame-0.1.0-win64.zip` 생성 완료 |
| 사용법 | `README.md` 참고 |

> eframe 0.36부터 `App::update`가 `App::ui(&mut Ui, &mut Frame)` + `App::logic(&Context, &mut Frame)`으로 바뀌었고,
> `CentralPanel::show`가 `&Context` 대신 `&mut Ui`를 받습니다. 4장 설계의 `update()` 표기는 이 두 메서드로 읽으면 됩니다.

---

## 1. 프로젝트 개요

### 1.1 목표

- 대중적인 이미지 포맷(PNG, JPEG, BMP, GIF, WebP, TIFF 등)을 빠르게 열어 볼 수 있는 Windows 11 데스크톱 뷰어를 만든다.
- 핵심 기능 두 가지를 반드시 제공한다.
  1. **창 크기에 꽉 차게 보기(Fit to Window)**
  2. **좌우 방향키로 같은 폴더 안의 이미지를 이름순으로 이동**
- 실행 파일 하나로 배포되는 가볍고 빠른 프로그램을 지향한다.
- 개인 사용이 1차 목표이므로 기능은 최소한으로 시작하고, 필요할 때 점진적으로 확장한다.

### 1.2 비목표 (이번 버전에서 하지 않는 것)

- 이미지 편집(자르기, 색 보정, 필터 등)
- RAW 카메라 포맷 지원
- 클라우드 연동, 썸네일 데이터베이스, 태그/앨범 관리
- macOS / Linux 지원 (Rust 특성상 나중에 쉽게 확장 가능하지만 이번엔 Windows만 검증)

---

## 2. 요구사항

### 2.1 필수 기능 (MVP)

| 번호 | 기능 | 설명 |
|------|------|------|
| F-01 | 이미지 열기 | 파일 열기 대화상자, 드래그 앤 드롭, 명령줄 인자(`viewer.exe image.png`)로 열기 |
| F-02 | 포맷 지원 | PNG, JPEG, BMP, GIF(첫 프레임), WebP, TIFF, ICO, TGA |
| F-03 | **창에 꽉 차게 보기** | 이미지 비율을 유지하면서 창 안에 최대 크기로 표시. 창 크기가 바뀌면 자동으로 다시 맞춤 |
| F-04 | 실제 크기 보기 | 100% 배율로 표시 (`1` 키) |
| F-05 | 확대/축소 | 마우스 휠, `+`/`-` 키. 마우스 커서 위치를 기준으로 확대 |
| F-06 | 이동(Pan) | 확대 상태에서 마우스 드래그로 이미지 이동 |
| F-07 | **좌우 방향키로 폴더 내 이동** | `←`/`→` 키로 같은 폴더의 이전/다음 이미지로 이동. 순서는 **파일 이름순 정렬**. `PageUp`/`PageDown`도 동일 동작 |
| F-08 | 전체 화면 | `F11` 또는 더블클릭으로 전체 화면 전환 |
| F-09 | 정보 표시 | 제목 표시줄에 파일명, 해상도, 배율(%) 표시 |

### 2.2 선택 기능 (MVP 이후)

| 번호 | 기능 | 우선순위 |
|------|------|----------|
| O-01 | 회전(90°) / 좌우 반전 | 높음 |
| O-02 | GIF/WebP 애니메이션 재생 | 중간 |
| O-03 | 파일 삭제(휴지통으로 이동), 이름 바꾸기 | 중간 |
| O-04 | 슬라이드쇼 | 낮음 |
| O-05 | EXIF 정보 패널, EXIF 방향 자동 적용 | 중간 |
| O-06 | 배경색 선택(검정/회색/체커보드) | 낮음 |
| O-07 | 다크/라이트 테마 | 낮음 |
| O-08 | Windows 파일 연결(기본 앱으로 등록) | 높음 |
| O-09 | 설정 저장(마지막 창 위치, 기본 배경색 등) | 중간 |

### 2.3 비기능 요구사항

- **시작 속도**: 실행 후 첫 이미지가 화면에 나오기까지 0.5초 이내 (일반 수천만 화소 JPEG 기준)
- **메모리**: 열린 이미지 한 장 + 앞뒤 프리로드 캐시 최대 2장 이내로 유지
- **HiDPI**: Windows 배율(125%, 150% 등)에서 흐릿하지 않게 표시
- **단일 실행 파일**: 외부 DLL 없이 `.exe` 하나로 실행
- **콘솔 창 없음**: 실행 시 검은 콘솔 창이 뜨지 않도록 GUI 서브시스템 사용

---

## 3. 기술 스택

### 3.1 언어 및 도구

- **언어**: Rust (stable, edition 2021 이상)
- **툴체인**: `x86_64-pc-windows-msvc` (Visual Studio Build Tools 필요)
- **빌드**: Cargo
- **에디터**: VS Code + rust-analyzer 권장

### 3.2 GUI 프레임워크 선정

| 후보 | 장점 | 단점 | 판단 |
|------|------|------|------|
| **egui + eframe** | 즉시 모드로 코드가 단순, 이미지 텍스처 표시가 쉬움, 문서와 예제가 풍부, wgpu/glow 백엔드로 GPU 렌더링 | 네이티브 룩앤필은 아님 | **채택** |
| iced | Elm 스타일로 구조가 깔끔, 성숙한 위젯 | 학습 곡선이 상대적으로 높고 커스텀 이미지 뷰포트 구현이 다소 번거로움 | 보류 |
| slint | 네이티브에 가까운 UI, 디자이너 친화적 | 별도 마크업 언어 학습 필요 | 보류 |
| winit + softbuffer/pixels | 의존성 최소, 완전한 제어 | UI 요소(메뉴, 대화상자)를 전부 직접 구현해야 함 | 보류 |
| Tauri | 웹 기술로 UI 구성 | WebView 의존, 대용량 이미지 전달 비효율 | 제외 |

**선정 이유**: 이미지 뷰어는 UI가 단순하고 "이미지 한 장을 원하는 배율과 위치로 그리는 것"이 핵심입니다. egui는 이미지를 텍스처로 올려 원하는 사각형에 그리는 API가 간단하고, 키보드/마우스 입력 처리와 창 제어(전체 화면, 제목 변경)를 eframe이 제공하므로 가장 빠르게 결과를 볼 수 있습니다.

### 3.3 주요 크레이트

| 크레이트 | 용도 | 비고 |
|----------|------|------|
| `eframe` / `egui` | 창 생성, 렌더링, 입력 처리 | wgpu 백엔드 권장 |
| `image` | 이미지 디코딩 | PNG, JPEG, BMP, GIF, WebP, TIFF, ICO, TGA 등 기본 지원 |
| `rfd` | 네이티브 파일 열기 대화상자 | Windows 공용 대화상자 사용 |
| `anyhow` | 에러 처리 단순화 | |
| `log` + `env_logger` | 로깅 | 디버깅용 |
| `trash` | 파일을 휴지통으로 이동 (선택 기능) | |
| `kamadak-exif` | EXIF 읽기 (선택 기능) | |
| `winresource` 또는 `winres` | 실행 파일 아이콘/버전 정보 삽입 | build.rs에서 사용 |

> 버전은 착수 시점에 crates.io에서 최신 안정 버전을 확인해 고정합니다.

### 3.4 Cargo.toml 초안

```toml
[package]
name = "aspartame"
version = "0.1.0"
edition = "2021"

[dependencies]
eframe = { version = "*", default-features = false, features = ["wgpu", "default_fonts"] }
egui = "*"
image = "*"
rfd = "*"
anyhow = "*"
log = "*"
env_logger = "*"

[build-dependencies]
winresource = "*"

[profile.release]
opt-level = 3
lto = true
codegen-units = 1
strip = true
```

> `"*"` 자리는 착수 시 실제 버전으로 교체합니다.

---

## 4. 아키텍처 설계

### 4.1 모듈 구조

```
aspartame/
├── Cargo.toml
├── build.rs              # 아이콘, 버전 리소스 삽입
├── assets/
│   └── icon.ico
└── src/
    ├── main.rs           # 진입점, 명령줄 인자 처리, eframe 실행
    ├── app.rs            # ViewerApp: egui 상태 및 update 루프
    ├── image_loader.rs   # 파일 → RGBA 버퍼 디코딩 (백그라운드 스레드)
    ├── navigator.rs      # 폴더 스캔, 이전/다음 파일 탐색
    ├── viewport.rs       # 배율/오프셋 계산, Fit/실제 크기/줌 로직
    ├── input.rs          # 키보드/마우스 입력 → 액션 매핑
    └── config.rs         # 설정 저장/불러오기 (선택)
```

### 4.2 핵심 데이터 구조

```rust
/// 디코딩된 이미지
pub struct LoadedImage {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub texture: egui::TextureHandle,   // GPU에 올라간 텍스처
}

/// 화면 표시 상태
pub enum ZoomMode {
    FitToWindow,     // 창에 꽉 차게 (기본값)
    ActualSize,      // 100%
    Custom(f32),     // 사용자가 휠/키로 조정한 배율
}

pub struct Viewport {
    pub mode: ZoomMode,
    pub scale: f32,          // 실제 적용 중인 배율
    pub offset: egui::Vec2,  // 팬 오프셋 (화면 픽셀 단위)
}

/// 앱 전체 상태
pub struct ViewerApp {
    pub current: Option<LoadedImage>,
    pub navigator: Navigator,        // 폴더 내 파일 목록과 현재 인덱스
    pub viewport: Viewport,
    pub fullscreen: bool,
    pub loader_rx: Receiver<LoadResult>,  // 백그라운드 로더 결과 수신
    pub error: Option<String>,
}
```

### 4.3 데이터 흐름

```
[명령줄 인자 / 드롭 / 열기 대화상자]
            │
            ▼
   Navigator::open(path)  ── 폴더 스캔 → 정렬된 이미지 파일 목록
            │
            ▼
   ImageLoader (별도 스레드) ── image 크레이트로 디코딩 → RGBA8
            │  채널(mpsc)로 전달
            ▼
   ViewerApp::update()
            ├─ 텍스처 업로드 (ctx.load_texture)
            ├─ Viewport::compute(창 크기, 이미지 크기, 모드) → 그릴 사각형
            ├─ 입력 처리 (휠/키/드래그) → 모드·배율·오프셋 갱신
            └─ egui Painter로 이미지 그리기 + 제목 표시줄 갱신
```

### 4.4 "창에 꽉 차게 보기" 계산식

창 내부 영역이 `W × H`, 이미지가 `w × h`일 때:

```
scale = min(W / w, H / h)
draw_w = w * scale
draw_h = h * scale
x = (W - draw_w) / 2
y = (H - draw_h) / 2
```

- 이미지가 창보다 작아도 확대해서 꽉 채운다. (원하면 "확대 금지" 옵션으로 `scale = min(scale, 1.0)` 적용)
- `FitToWindow` 모드에서는 창 크기가 바뀔 때마다 매 프레임 재계산하므로 자동으로 따라간다.
- 사용자가 휠로 줌하면 `Custom(scale)` 모드로 전환되고, `F` 키 또는 메뉴로 다시 `FitToWindow`로 복귀한다.

### 4.5 커서 기준 확대 계산식

마우스 위치 `m`(화면 좌표)를 기준으로 배율을 `s0 → s1`으로 바꿀 때, 커서 아래 이미지 픽셀이 그대로 있으려면:

```
offset1 = m - (m - offset0) * (s1 / s0)
```

### 4.6 HiDPI 처리

- eframe은 `ctx.pixels_per_point()`로 DPI 배율을 제공한다.
- "실제 크기(100%)"는 **물리 픽셀 기준**으로 정의한다. 즉 논리 좌표에서는 `1 / pixels_per_point` 배율로 그려야 이미지 픽셀 1개 = 모니터 픽셀 1개가 된다.
- 텍스처 필터: 확대 시 `Linear`, 100% 또는 정수 배율일 때는 `Nearest`도 선택 가능하게 한다.

### 4.7 좌우 방향키 폴더 탐색 설계

`navigator.rs`가 담당하며, 동작 규칙은 다음과 같다.

**목록 구성**

1. 이미지를 열면 그 파일이 속한 폴더를 한 번 스캔한다. (하위 폴더는 포함하지 않음)
2. 지원 확장자(`png, jpg, jpeg, bmp, gif, webp, tif, tiff, ico, tga`)만 남긴다. 확장자 비교는 대소문자 무시.
3. **파일 이름순으로 정렬**한다.
   - 대소문자를 구분하지 않는다. (`a.png`와 `B.png`는 `a`, `B` 순)
   - 이름에 포함된 숫자는 자연 정렬(natural sort)로 비교해 `img2.png` → `img10.png` 순서가 되도록 한다. Windows 탐색기의 기본 이름순과 동일한 결과를 목표로 한다.
   - 한글 파일명은 유니코드 코드포인트 순으로 비교하면 가나다순이 된다.
4. 현재 열린 파일의 인덱스를 찾아 `current_index`로 저장한다.

**이동 규칙**

| 입력 | 동작 |
|------|------|
| `→`, `PageDown` | `current_index + 1` 파일로 이동 |
| `←`, `PageUp` | `current_index - 1` 파일로 이동 |
| `Home` / `End` | 첫 / 마지막 파일로 이동 |
| 마지막에서 `→` | 첫 파일로 순환 (설정으로 순환 끄기 가능, 기본은 순환) |
| 첫 파일에서 `←` | 마지막 파일로 순환 |

- 이동 직후 줌 모드는 `FitToWindow`로 초기화한다. 즉, 방향키만 눌러도 매번 창에 꽉 찬 상태로 다음 이미지를 본다.
- 이동 중 방향키를 연타하면 이전 로딩 요청은 무시하고 마지막 요청만 반영한다. (로더 결과에 요청 세대 번호를 붙여 오래된 결과를 버림)
- 폴더에 이미지가 1개뿐이면 방향키는 아무 동작도 하지 않는다.

**목록 갱신**

- 이동할 파일이 삭제되어 없으면 목록을 다시 스캔하고 가장 가까운 인덱스로 이동한다.
- 폴더에 파일이 새로 추가된 경우는 `F5`로 수동 재스캔한다. (파일 시스템 감시는 선택 기능으로 미룸)

**정렬 예시**

```
입력 폴더:  photo10.jpg, Photo2.jpg, photo1.jpg, 가.png, readme.txt, b.PNG
정렬 결과:  b.PNG, photo1.jpg, Photo2.jpg, photo10.jpg, 가.png
            (readme.txt는 제외)
```

---

## 5. 단계별 개발 계획

### Phase 0. 환경 준비 (0.5일)

- [ ] Rust 툴체인 설치 (`rustup`, MSVC 타깃)
- [ ] Visual Studio Build Tools 설치 (C++ 데스크톱 워크로드)
- [ ] `cargo new aspartame` 후 eframe 예제 창 띄우기
- [ ] `#![windows_subsystem = "windows"]` 적용해 콘솔 창 제거 확인

### Phase 1. 이미지 한 장 띄우기 (1일)

- [ ] `image` 크레이트로 PNG/JPEG 디코딩 → `ColorImage` → 텍스처 업로드
- [ ] 명령줄 인자로 받은 파일을 시작 시 열기
- [ ] **Fit to Window** 계산식 구현, 창 리사이즈 시 자동 재맞춤 확인
- [ ] 제목 표시줄에 `파일명 — 1920×1080 — 67%` 형식 표시

**완료 기준**: `aspartame.exe photo.jpg` 실행 시 창에 꽉 찬 사진이 나오고, 창 크기를 바꿔도 비율을 유지하며 따라온다.

### Phase 2. 줌 / 팬 / 실제 크기 (1일)

- [ ] 마우스 휠 줌 (커서 기준), `+`/`-` 키 줌
- [ ] 드래그 팬, 이미지가 창보다 작을 땐 중앙 고정
- [ ] `1` 키: 실제 크기, `F` 키: 창에 맞춤 토글
- [ ] HiDPI 환경에서 100% 배율 검증 (150% 배율 모니터에서 테스트)
- [ ] 최소/최대 배율 제한 (예: 1% ~ 3200%)

### Phase 3. 좌우 방향키 폴더 탐색과 파일 열기 (1일)

- [ ] 열린 파일의 폴더를 스캔해 지원 확장자만 **이름순(대소문자 무시, 자연 정렬)**으로 목록화
- [ ] `←`/`→`로 이전/다음 이미지 이동, `PageUp`/`PageDown`, `Home`/`End` 지원
- [ ] 끝에서 순환 이동, 이미지 1개일 때 무동작 처리
- [ ] 방향키 연타 시 마지막 요청만 반영 (요청 세대 번호)
- [ ] 이동 후 줌 모드를 `FitToWindow`로 초기화
- [ ] 이동 대상 파일이 사라졌을 때 재스캔 후 인접 파일로 이동, `F5` 수동 재스캔
- [ ] `Ctrl+O` 파일 열기 대화상자 (`rfd`)
- [ ] 드래그 앤 드롭으로 열기 (`ctx.input(|i| i.raw.dropped_files)`)

**완료 기준**: 사진 폴더에서 `→` 키를 계속 누르면 Windows 탐색기 이름순과 같은 순서로 이미지가 넘어가고, 각 이미지는 창에 꽉 찬 상태로 표시된다.

### Phase 4. 백그라운드 로딩과 안정성 (1일)

- [ ] 디코딩을 별도 스레드로 분리해 UI 프리징 제거
- [ ] 앞/뒤 이미지 1장씩 프리로드 (LRU 캐시, 최대 3장)
- [ ] 손상 파일, 미지원 포맷, 권한 오류 시 화면에 에러 메시지 표시 후 계속 동작
- [ ] 매우 큰 이미지(예: 20000×20000) 처리: GPU 최대 텍스처 크기 초과 시 다운스케일 후 업로드
- [ ] 로딩 중 표시(스피너 또는 상태 텍스트)

### Phase 5. 전체 화면과 마무리 (0.5일)

- [ ] `F11`, 더블클릭으로 전체 화면 토글, `Esc`로 복귀
- [ ] 전체 화면에서 커서 자동 숨김 (선택)
- [ ] 우클릭 컨텍스트 메뉴: 열기, 창에 맞춤, 실제 크기, 전체 화면, 종료
- [ ] 아이콘 리소스 삽입 (`build.rs` + `winresource`)
- [ ] `cargo build --release`로 단일 exe 생성 및 크기 확인

**MVP 완료 기준**: 2.1절 필수 기능 F-01 ~ F-09 전부 동작.

### Phase 6. 선택 기능 (이후 점진적으로)

- [ ] 회전 / 반전 (O-01)
- [ ] Windows 파일 연결 등록 스크립트 또는 안내 (O-08)
- [ ] 휴지통 삭제 (O-03)
- [ ] EXIF 방향 자동 적용 (O-05)
- [ ] GIF/WebP 애니메이션 (O-02)
- [ ] 설정 저장 (O-09)

---

## 6. 일정 요약

| 단계 | 내용 | 예상 소요 | 누적 |
|------|------|-----------|------|
| Phase 0 | 환경 준비 | 0.5일 | 0.5일 |
| Phase 1 | 이미지 한 장 + Fit to Window | 1일 | 1.5일 |
| Phase 2 | 줌 / 팬 / 실제 크기 | 1일 | 2.5일 |
| Phase 3 | 폴더 탐색, 파일 열기 | 1일 | 3.5일 |
| Phase 4 | 백그라운드 로딩, 안정성 | 1일 | 4.5일 |
| Phase 5 | 전체 화면, 마무리 | 0.5일 | **5일 (MVP)** |
| Phase 6 | 선택 기능 | 필요 시 | — |

> 하루 2~3시간 작업 기준이면 약 2주, 집중 작업이면 1주 안에 MVP 완성이 목표입니다.

---

## 7. 키보드 / 마우스 조작 정의

| 입력 | 동작 |
|------|------|
| `Ctrl+O` | 파일 열기 |
| `←` / `→`, `PageUp` / `PageDown` | 이전 / 다음 이미지 |
| `Home` / `End` | 첫 / 마지막 이미지 |
| `F` | 창에 꽉 차게 보기 |
| `1` | 실제 크기 (100%) |
| `+` / `-`, 마우스 휠 | 확대 / 축소 |
| 마우스 드래그 | 이동(Pan) |
| `F11`, 더블클릭 | 전체 화면 토글 |
| `Esc` | 전체 화면 해제 (일반 모드에서는 종료) |
| `R` / `Shift+R` | 시계 / 반시계 회전 (선택 기능) |
| `Delete` | 휴지통으로 이동 (선택 기능) |
| `I` | 이미지 정보 표시 토글 (선택 기능) |

---

## 8. 테스트 계획

### 8.1 단위 테스트

- `viewport.rs`: Fit 계산, 커서 기준 줌 계산, 배율 상/하한 클램프
- `navigator.rs`: 확장자 필터(대소문자 무시), 이름순 자연 정렬(`img2.png` < `img10.png`, `a` < `B`), 한글 파일명 정렬, 끝에서 순환, 파일 1개일 때 무동작, 삭제된 파일 건너뛰기

### 8.2 수동 테스트 체크리스트

- [ ] 포맷별 샘플 파일 8종 열기 (PNG, JPEG, BMP, GIF, WebP, TIFF, ICO, TGA)
- [ ] 알파 채널 PNG가 배경색 위에 올바르게 합성되는지
- [ ] 가로/세로/정사각 이미지 각각 Fit 시 여백이 대칭인지
- [ ] 창 크기 변경, 최대화, 복원 시 Fit 유지
- [ ] 100%, 125%, 150%, 200% 디스플레이 배율에서 실제 크기 표시 정확도
- [ ] 폴더에 파일 1개만 있을 때 / 0개일 때 탐색 키 동작
- [ ] 손상 파일 열었을 때 크래시 없이 에러 표시
- [ ] 100MB 이상 대형 이미지 로딩 중 UI 응답성

### 8.3 성능 측정

- 시작부터 첫 프레임 표시까지 시간 (`Instant` 로깅)
- 이미지 전환 지연 시간
- 릴리스 빌드 exe 크기 및 메모리 사용량 (작업 관리자)

---

## 9. 빌드와 배포

```powershell
# 개발 실행
cargo run -- "C:\Pictures\sample.jpg"

# 릴리스 빌드
cargo build --release
# 결과물: target\release\aspartame.exe
```

- `Cargo.toml`의 `[profile.release]`에 `lto`, `strip` 적용으로 exe 크기 최소화
- `build.rs`에서 `winresource`로 아이콘과 버전 정보 삽입
- 파일 연결: Windows 설정 > 기본 앱에서 수동 등록하거나, 레지스트리 `.reg` 스크립트 제공 (선택 기능 O-08)
- 배포는 exe 단일 파일 복사로 충분. 설치 프로그램은 만들지 않음

---

## 10. 리스크와 대응

| 리스크 | 영향 | 대응 |
|--------|------|------|
| GPU 최대 텍스처 크기(보통 8192 또는 16384px) 초과 이미지 | 업로드 실패 또는 크래시 | 로드 시 한계 초과면 비율 유지 다운스케일. 원본 픽셀 확인이 필요하면 타일링은 향후 과제 |
| 대형 이미지 디코딩 중 UI 정지 | 사용성 저하 | Phase 4에서 백그라운드 스레드 분리 |
| HiDPI에서 "실제 크기"가 흐릿함 | 요구사항 위반 | 물리 픽셀 기준으로 배율 정의, Nearest 필터 옵션 |
| eframe 버전 업그레이드 시 API 변경 | 빌드 실패 | Cargo.lock으로 버전 고정, 업그레이드는 별도 시간에 |
| WebP/TIFF 일부 변형 미지원 | 특정 파일 열기 실패 | 에러 메시지 표시, 필요 시 `image` 크레이트 feature 추가 검토 |
| 한글 파일명/경로 | 열기 실패 | Rust `PathBuf`는 UTF-16 경로를 정상 처리. 테스트 케이스에 한글 경로 포함 |

---

## 11. 참고 자료

- egui / eframe 공식 저장소 및 예제: https://github.com/emilk/egui
- image 크레이트 문서: https://docs.rs/image
- rfd 크레이트 문서: https://docs.rs/rfd
- Rust Windows 툴체인 안내: https://rust-lang.github.io/rustup/installation/windows-msvc.html
