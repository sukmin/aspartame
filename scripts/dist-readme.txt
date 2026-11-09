Aspartame 이미지 뷰어 - 사용법
================================

이 폴더의 구성
  aspartame.exe                  뷰어 본체 (단일 실행 파일, 설치 불필요)
  register-default-viewer.bat    Windows 기본 이미지 뷰어로 등록
  unregister-default-viewer.bat  등록 해제 (/purge 를 붙이면 복사한 exe 도 삭제)
  README.md                      상세 설명 (조작법, 빌드 방법)
  개발가이드.html                 소스 구조, 동작 원리, 빌드·등록 스크립트 설명 (브라우저로 열기)

바로 쓰기
  aspartame.exe 를 실행한 뒤 이미지를 끌어다 놓거나 Ctrl+O 로 여세요.
  탐색기에서 이미지를 aspartame.exe 위로 끌어다 놓아도 열립니다.
  zip 파일을 열면 안의 이미지가 갤러리로 보이고, 클릭하면 한 장씩 볼 수 있습니다.

기본 이미지 뷰어로 만들기
  1. register-default-viewer.bat 를 더블클릭합니다. (관리자 권한 불필요)
     exe 가 %LOCALAPPDATA%\Programs\Aspartame 에 복사되고 레지스트리에 등록됩니다.
  2. 자동으로 열리는 "설정 > 기본 앱 > Aspartame" 페이지에서
     [기본값으로 설정] 단추를 누르거나, .png .jpg 등 확장자를 눌러 Aspartame 를 고릅니다.
     (Windows 정책상 이 마지막 선택은 사용자가 직접 해야 합니다.)
  3. 되돌리려면 unregister-default-viewer.bat 를 실행합니다.

조작법
  <- ->, PageUp/PageDown, 휠     같은 폴더의 이전/다음 이미지 (이름순, 끝에서 순환)
  Home / End                    첫 / 마지막 이미지
  F 또는 0                       창에 꽉 차게 보기 (기본)
  1                             실제 크기 (모니터 픽셀 1:1)
  + / -  또는 Alt+휠             확대 / 축소 (휠은 커서 기준)
  마우스 드래그                   이동
  F11 또는 더블클릭               전체 화면
  Ctrl+S                        CR3 의 내장 JPEG 저장
  Esc                           전체 화면 해제, zip 의 한 장 보기에서는 갤러리로 복귀, 그 외에는 종료
  I                             파일 정보 표시
  F5                            폴더 또는 zip 다시 읽기
  우클릭                         메뉴

Smart App Control 안내
  Windows 11 의 Smart App Control 이 켜져 있으면 서명되지 않은 aspartame.exe 실행이 차단됩니다.
  Windows 보안 > 앱 및 브라우저 컨트롤 > Smart App Control 설정 에서 끌 수 있습니다.
  (한 번 끄면 Windows 를 다시 설치하기 전까지 다시 켤 수 없습니다.)

aspartame.exe 가 이 폴더에 없다면
  소스 폴더에서 scripts\package.bat 를 실행하면 빌드 후 이 폴더와 zip 이 만들어집니다.
