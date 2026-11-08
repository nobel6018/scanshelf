# iPhone 앱

거치대 촬영, 문서 묶기, Mac으로 보내기를 합니다. 보정과 OCR은 Mac이 합니다(docs/architecture.md).

## 빌드

```bash
cd ios
xcodegen generate
export DEVELOPER_DIR=/Applications/Xcode-27.1.0.app/Contents/Developer
xcodebuild test -project Scanshelf.xcodeproj -scheme Scanshelf \
  -destination 'platform=iOS Simulator,name=iPhone 18 Pro' \
  -derivedDataPath build CODE_SIGNING_ALLOWED=NO
```

- `Scanshelf.xcodeproj`는 `project.yml`에서 만들어지므로 커밋하지 않습니다
- 타깃은 앱(`Scanshelf`), 제어 센터 확장(`ScanshelfControls`, 앱 안 `PlugIns/`로 들어감), 테스트(`ScanshelfTests`)입니다. `Shared/`는 앱과 확장이 함께 컴파일합니다
- `xcode-select -p`가 CommandLineTools이면 `DEVELOPER_DIR`를 줘야 합니다
- 최소 iOS 18, Swift 6(엄격한 동시성 검사)

## 앱 아이콘

`ios/Scanshelf/Assets.xcassets/AppIcon.appiconset`에 1024 정사각형 한 장을 둡니다. 원본은 `design/logo-ios-1024.png`이고, 로고를 바꾸면 이 파일도 같이 바꿉니다. 둥근 모서리와 투명도는 없어야 합니다. 빠져도 빌드는 성공하므로 release.yml이 `Assets.car`와 `CFBundleIcons`를 검사합니다.

## 설치

AltStore로 본인 기기에만 설치합니다. 빌드는 서명 없이 하고 AltStore가 무료 Apple ID로 다시 서명합니다. 무료 Apple ID는 7일마다 다시 서명해야 하고 AltStore가 자동으로 갱신합니다.

무료 Apple ID라 entitlements가 비어 있습니다. CloudKit, 앱 전용 iCloud 컨테이너, 백그라운드 모드를 쓰지 않습니다. 토큰은 앱 기본 키체인 그룹에 둡니다.

## 화면

| 화면 | 코드 | 내용 |
|---|---|---|
| 문서 목록 | `Views/DocumentsView.swift` | 새 버전 알림, iCloud 폴더 안내, 묶음별 문서, 상태(아래 "iCloud 백업"), 새 문서, 모두 보내기 |
| 새 문서 | `Views/NewDocumentView.swift` | 종류(문서, 책, 팜플렛), 묶음(박람회명, 책 시리즈 등), 이름(기본 `묶음_번호`). 책은 "펼친 두 페이지", 팜플렛은 "펼친 3단 접지" 스위치. 종류, 묶음, 종류별 배치는 바꾸기 전까지 기억 |
| 촬영 | `Views/CaptureView.swift`, `Capture/CaptureModel.swift` | iCloud 폴더 안내(한 줄), 4:3 미리보기, 테두리, 자동 촬영 안내, 자동과 수동 전환, 배치 전환(한 장, 두 쪽, 팜플렛은 3단까지), 페이지 띠(누르면 다시 찍기, 지우기) |
| 문서 | `Views/DocumentDetailView.swift` | 이름, 묶음, 종류 고치기, 찍은 장(두 쪽, 3단 표시), 이어서 찍기, Wi-Fi로 보내기, iCloud 백업 다시 시도, 문서 지우기 |
| 설정 | `Views/SettingsView.swift` | Mac 연결과 해제, iCloud 백업 폴더, 원본 지우기 기간, 자동 촬영 민감도 |

문서 이름 기본값은 같은 묶음에서 쓴 가장 큰 번호 + 1입니다(`Library/SendRecords.swift` `DocumentNaming`). 묶음이 없으면 종류 이름을 씁니다(`문서_1`, `책_1`, `팜플렛_1`). 팜플렛은 나중에 Mac AI가 표지에서 회사명을 추천합니다.

배치는 페이지마다 manifest `layout`에 들어갑니다(docs/document-format.md). 종류마다 고를 수 있는 배치는 `Library/KindLayouts.swift`에 있습니다.

| 종류 | 배치 | 처음 값 |
|---|---|---|
| 문서 | 한 장, 두 쪽 | 한 장 |
| 책 | 한 장, 두 쪽 | 두 쪽 |
| 팜플렛 | 한 장, 두 쪽(반 접은 리플릿), 3단(펼친 3단 접지) | 한 장 |

새 문서 화면이나 촬영 화면에서 배치를 바꾸면 그 종류의 다음 새 문서도 그 배치로 시작합니다(`AppSettings.lastLayout(for:)`). 이어 찍기는 그 문서 마지막 장의 배치를 이어 갑니다.

다시 찍기를 고르면 다음 촬영이 그 장 자리에 들어가고 예전 원본은 지웁니다(`DocumentStore.addPage(replacing:)`).

## 저장

`Documents/library/<문서 id>/manifest.json`과 `raw/<페이지 id>.heic`. 형식은 docs/document-format.md 그대로입니다(`Library/DocumentStore.swift`).

- 원본을 먼저 쓰고 manifest를 나중에 씁니다. manifest는 `.manifest.json.tmp`에 쓴 뒤 `rename(2)`로 바꿉니다
- id는 소문자로 씁니다. Swift `UUID`는 대문자로 인코딩하므로 `Manifest`, `Page`가 직접 인코딩합니다
- 날짜는 밀리초로 맞춘 값을 메모리에도 둡니다. 저장했다 읽은 값과 같아야 보낸 뒤 바뀌었는지(지문) 판단이 맞습니다
- 저장 전에 Rust와 같은 규칙으로 검사합니다(`Manifest.validate`)
- 저장할 때마다 `revision`을 올립니다(`DocumentStore.save`). 손에 든 값이 옛것이어도 디스크보다 커지게 디스크 값과 견줍니다

보낸 기록은 문서 폴더 밖 `Application Support/send-records.json`에 둡니다. 문서마다 보낸 방법, 시각, 그때의 manifest 지문(SHA-256), Mac이 받은 것이 확인됐는지를 적습니다. iCloud 백업 기록은 `Application Support/backup-state.json`입니다.

## 보내기

docs/transfer.md를 따릅니다. 코드는 `ios/Scanshelf/Transfer/`, 흐름은 `App/AppModel.swift`.

| 경로 | 언제 | 확인 |
|---|---|---|
| Wi-Fi | 페어링한 Mac을 Bonjour 이름으로 4초 안에 찾으면. 못 찾으면 마지막 주소로 시도 | manifest `PUT` 200 |
| iCloud 폴더 | 폴더를 골라 두었으면 찍을 때마다(아래 "iCloud 백업") | Mac의 확인 파일 `received/<문서 id>.json`에 지금 판의 지문이 적혔을 때 |

- 촬영 화면을 닫을 때, 앱이 앞으로 올 때, "모두 보내기"를 누를 때 페어링한 Mac이 있으면 아직 Mac에 없는 문서를 Wi-Fi로 보냅니다. iCloud에 올라간 문서도 보냅니다. 같은 Wi-Fi면 그쪽이 빨리 확인되고, 받으면 iCloud 사본을 치웁니다. Mac에 닿지 않으면 그 차례의 나머지 문서는 건너뜁니다
- 이어 보내기: `status`로 Mac에 같은 해시로 있는 원본을 건너뜁니다. manifest가 409 `missing_files`이면 빠진 원본만 다시 보내고 manifest를 다시 보냅니다(2번까지)
- 서명 경로는 보낼 URL에서 꺼낸 퍼센트 인코딩된 경로 그대로입니다. 경로에는 UUID와 `raw/<uuid>.heic`만 들어가 인코딩할 글자가 없습니다
- 페어링 오류는 코드 틀림(401 `wrong_code`, 다시 넣으면 됨)과 창 닫힘(403 `pairing_closed`, Mac에서 새 코드)을 다르게 안내합니다
- Bonjour TXT `api`가 1이 아닌 Mac은 목록에 보이지 않습니다. 주소는 IPv4로 풉니다(Mac 서버가 IPv4만 엽니다)
- iCloud 폴더는 고른 폴더에서 `scanshelf` 루트를 찾습니다. `scanshelf` 안의 폴더를 골라도 위로 올라가고, iCloud Drive 자체를 고르면 그 안의 `scanshelf`를 씁니다. iCloud Drive 밖은 거절합니다(`ICloudFolderLayout.scanshelfRoot`)

### iCloud 백업

iOS는 앱을 지우면 앱의 파일을 전부 지웁니다. 그래서 찍은 장을 곧바로 iCloud 폴더에 복사하고, Mac이 받으면 거기서 지웁니다. 흐름과 함정은 docs/transfer.md "iCloud 백업"에 있습니다. 코드는 `Transfer/ICloudBackup.swift`(파일 일과 일꾼), `Library/BackupQueue.swift`(판단), `Views/BackupNoticeView.swift`(안내)입니다.

- 셔터는 복사를 기다리지 않습니다. `AppModel.replace`가 일꾼을 깨우고 바로 돌아옵니다. 복사, 지문 계산, 기록 저장은 메인 스레드 밖에서 합니다
- 폴더를 고르지 않았거나 폴더에 닿지 않으면 문서 목록과 촬영 화면에 "폴더 고르기" 버튼이 있는 안내가 계속 보입니다. 촬영은 막지 않습니다
- 할 일은 저장하지 않고 문서와 기록에서 계산합니다(`BackupQueue.plan`). 앱을 언제 끄든 다음에 켜면 이어 갑니다. 앱이 앞에 있을 때만 돌고, 앱을 내리면 복사하던 파일까지만 마칩니다

문서 상태(`SendStatus.resolve`)는 앱을 지워도 남는지를 먼저 보여 줍니다.

| 표시 | 뜻 | 앱을 지우면 |
|---|---|---|
| 이 iPhone에만 | 폴더를 고르지 않았고 Mac도 받지 않았다 | 사라진다 |
| iCloud 백업 대기 | 복사 전이거나 iCloud가 아직 다 올리지 않았다. 복사가 끝난 파일은 iCloud Drive 쪽 저장소에 있어 앱을 지워도 남는다 | 복사 안 된 장은 사라진다 |
| iCloud에 있음 | iCloud가 올렸다고 알려 줬고 Mac이 가져가기를 기다린다 | 남는다 |
| Mac에 있음 | Mac이 이 판을 받았다고 확인했다 | 남는다 |
| 바뀜 | Mac이 받은 뒤 더 찍었고 폴더를 고르지 않았다 | 새 장은 사라진다 |
| 실패 | Wi-Fi로 못 보냈고 폴더를 고르지 않았다. 폴더가 있으면 iCloud 상태를 보여 주고 이유는 문서 화면에 적는다 | 사라진다 |

### 알려진 한계

- 보내기는 앱이 앞에 있을 때만 합니다. 앱을 내리면 `beginBackgroundTask`로 얻는 30초 안에 보내던 파일까지만 보내고, 나머지는 다음에 앱을 열 때 이어 보냅니다. 백그라운드 `URLSession`은 업로드 파일마다 서명 헤더를 미리 정해야 하고 시각 차이 300초 제한에 걸려 쓰지 않았습니다
- iCloud에 올라간 것은 `ubiquitousItemIsUploaded` 값으로 판단합니다. "iCloud에 있음"이어도 원본 자동 삭제 대상이 아닙니다. Mac의 확인 파일만 근거가 됩니다
- 폴더 선택기는 고른 폴더 아래만 열어 줍니다. `scanshelf/inbox`처럼 안쪽 폴더를 고르면 `received/`를 읽지 못해 "Mac에 있음"이 되지 못할 수 있습니다. `scanshelf`나 iCloud Drive를 고릅니다

## 원본 자동 삭제

설정의 기간(기본 7일, 0이면 지우지 않음)이 지나고, Mac이 받은 것이 확인됐고, 보낸 뒤 바뀌지 않은 문서를 앱이 앞으로 올 때 지웁니다(`RetentionPolicy`, `AppModel.applyRetention`). Mac에는 원본이 남습니다.

## 스캔 시작 동작

앱을 열지 않은 상태에서 바로 새 문서 촬영으로 들어가는 동작입니다. 마지막에 고른 종류, 마지막 묶음, 그 종류의 마지막 배치로 새 문서를 만들고 촬영 화면을 엽니다. 종류를 정해 두면(`종류` 매개변수) 그 종류로 엽니다.

| 코드 | 내용 |
|---|---|
| `Shared/StartScanIntent.swift` | `StartScanIntent`(`openAppWhenRun`), 종류 `ScanKindOption`. 앱과 확장이 함께 컴파일하고 확장에서는 `SCANSHELF_CONTROLS` 조건으로 앱 코드 호출을 뺀다 |
| `Scanshelf/Intents/ScanshelfShortcuts.swift` | App Shortcut("scanshelf 스캔 시작", "scanshelf로 책 스캔" 등). 단축어를 만들지 않아도 목록에 보인다 |
| `Scanshelf/App/ScanLauncher.swift` | 인텐트가 남긴 요청과 시작 값(`ScanStartPlan`). 앱이 꺼져 있다 켜지면 요청이 화면보다 먼저 와서, 문서 목록이 나타날 때 가져간다 |
| `ScanshelfControls/ScanshelfControls.swift` | 제어 센터, 잠금 화면 컨트롤(iOS 18 `ControlWidget`) |

이미 촬영 화면이 떠 있으면 요청을 버리고 그 화면을 둡니다. 새 문서나 설정 시트가 떠 있으면 내린 뒤 촬영 화면을 엽니다.

무료 Apple ID 서명에서 되는 것과 안 되는 것:

| 쓰는 곳 | 무료 Apple ID | 비고 |
|---|---|---|
| 단축어 앱, Spotlight, Siri 문구 | 됨 | App Intents는 entitlement가 필요 없다. Siri 문구는 App Shortcut이라 SiriKit 권한이 필요 없다 |
| 동작 버튼(iPhone 15 Pro 이상) | 됨 | 설정 > 동작 버튼 > 단축어 > scanshelf "스캔 시작", 또는 제어 > scanshelf 스캔 시작 |
| 제어 센터, 잠금 화면 컨트롤 | 될 것으로 봄(실기기 미확인) | 위젯 확장이라 entitlement는 없지만 App ID를 하나 더 쓴다. 무료 Apple ID는 7일에 App ID 10개까지라 scanshelf가 2개를 쓴다. 앱 그룹은 쓰지 않는다(확장은 버튼만 그리고 실행은 앱에서 한다) |
| 앱을 열지 않고 백그라운드에서 촬영 | 안 됨 | 카메라는 앱이 앞에 있어야 한다. 그래서 늘 앱을 연다 |
| 위젯(홈 화면) | 만들지 않음 | |

확장의 버전은 앱과 같아야 설치되므로 프로젝트 설정의 `MARKETING_VERSION`을 같이 씁니다. AltStore 소스 스크립트는 `PlugIns/*.appex`의 권한 설명도 모으므로 고칠 것이 없습니다.

## 업데이트 알림

앱이 앞으로 올 때 하루에 한 번 GitHub `releases/latest`를 묻고, `scanshelf-ios.ipa`가 붙은 더 새 버전이면 문서 목록 위에 "새 버전 x.y.z"를 띄웁니다(`App/UpdateCheck.swift`, `AppModel.checkForUpdate`).

- 버전 비교는 점으로 나눈 숫자를 앞에서부터 봅니다(0.10.0 > 0.9.1). 앞의 `v`와 뒤표(`-beta.1`)는 무시합니다(`UpdateCheck.isNewer`, `UpdateCheckTests`)
- 확인 결과는 저장해 두어 묻지 않는 날에도 알림이 남습니다. 닫으면 그 버전은 다시 알리지 않고, 더 새 버전이 나오면 다시 알립니다
- GitHub에 묻지 못하면 기록하지 않고 다음에 앞으로 올 때 다시 묻습니다. 로그인 없이 IP당 시간당 60번까지라 하루 한 번이면 넉넉합니다
- "업데이트"는 `altstore://source?url=<apps.json 주소>`로 AltStore의 scanshelf 소스를 엽니다. AltStore가 주소를 받지 않으면 Release 페이지를 엽니다. 설치는 AltStore의 Update 버튼으로 합니다(사이드로딩 앱은 스스로 설치하지 못한다)
- 단위 테스트가 앱을 띄울 때는 묻지 않습니다

## 시뮬레이터에서 화면 보기

카메라 대신 합성 화면이 나옵니다(docs/capture.md "시뮬레이터"). DEBUG 빌드는 실행 인자로 화면을 바로 엽니다.

```bash
xcrun simctl launch <udid> com.nobel6018.scanshelf -scanshelfScreen capture   # 촬영(세션 "2026 예시박람회" 문서를 만든다)
xcrun simctl launch <udid> com.nobel6018.scanshelf -scanshelfScreen new       # 새 문서
xcrun simctl launch <udid> com.nobel6018.scanshelf -scanshelfScreen settings  # 설정
xcrun simctl launch <udid> com.nobel6018.scanshelf -scanshelfScreen storage   # 보관 현황
xcrun simctl launch <udid> com.nobel6018.scanshelf -scanshelfScreen scan -scanshelfScanKind book  # "스캔 시작" 동작과 같은 길
xcrun simctl launch <udid> com.nobel6018.scanshelf -scanshelfScreen update    # 가짜 새 버전 알림(9.9.9)
xcrun simctl io <udid> screenshot --display=1 shot.png
```

## 실기기에서만 확인되는 것

- 카메라 세션, 최대 해상도 HEIC, 사진 EXIF 방향과 quad가 실제 사진 위에서 맞는지
- Vision 문서 감지, 손 감지, feature print 거리와 기준값(거치대, 실제 문서, 책, 팜플렛으로 맞춘다)
- 볼륨 버튼과 블루투스 리모컨 셔터
- 로컬 네트워크 권한, Bonjour 찾기, Mac 서버와의 실제 전송
- 키체인(무료 Apple ID 서명에서), iCloud 폴더 선택과 보안 범위 북마크
- iCloud 백업의 실제 동작: 앱을 다시 켠 뒤 북마크로 쓰기, `ubiquitousItemIsUploaded`와 내려받기 상태 값이 파일 앱 폴더에서 오는지, Mac이 쓴 확인 파일이 iPhone에 내려오는 시간, Mac의 삭제와 iPhone의 쓰기가 겹칠 때 iCloud가 푸는 방식, 저장 공간이 찼을 때. 시험은 임시 폴더로 흐름만 확인했다
- 앱을 지운 뒤 Mac이 iCloud에 남은 스캔을 받아 가는지
- 동작 버튼, 제어 센터, 잠금 화면 컨트롤에서 "스캔 시작"이 앱을 열고 촬영 화면으로 들어가는지. 시뮬레이터에서는 `-scanshelfScreen scan`으로 같은 화면 흐름만 확인했다
- `altstore://source?url=` 주소를 AltStore가 받아 scanshelf 소스를 여는지. 받지 않으면 Release 페이지로 간다
- 펼친 3단 접지를 거치대에서 찍을 때 Vision 문서 감지가 가로로 긴 종이 전체를 잡는지
