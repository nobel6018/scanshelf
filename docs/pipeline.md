# 처리 파이프라인 (Mac)

받은 원본(`raw/`)을 보정한 페이지(`pages/`), OCR(`ocr/`), `processing.json`으로 만듭니다. 원본은 지우지 않습니다.

코드: `src-tauri/src/pipeline/`

| 파일 | 하는 일 |
|---|---|
| `mod.rs` | `process_document`: 문서 하나 처리, 바뀐 페이지만 다시. `stale_pages`: 다시 처리할 페이지 수 |
| `steps.rs` | 페이지 보정 단계(`correct_page`). 단계마다 함수 하나 |
| `worker.rs` | 작업 줄(`Pipeline`). 작업 스레드 하나, 이벤트, busy 등록 |
| `platform/macos.rs` | ImageIO로 HEIC, JPEG 읽기(EXIF 회전 적용), Vision 문서 영역 감지, RGB와 CGImage 변환 |
| `e2e_tests.rs` | 합성 HEIC로 처음부터 PDF 텍스트까지 확인 |

## 페이지 하나의 흐름

1. 원본 읽기: `CGImageSourceCreateThumbnailAtIndex`에 크기 제한 없이 `CreateThumbnailWithTransform`을 줘서 EXIF 회전을 적용합니다. image 크레이트는 HEIC를 못 읽습니다
2. 꼭짓점: `Edits::apply(manifest)` 값(손으로 고친 값, 없으면 iPhone 감지값). 둘 다 없거나 믿을 만한 사각형이 아니면 Mac이 다시 찾습니다(`find_page_quad`). 원본 해상도 문서 영역 감지(`VNDetectDocumentSegmentationRequest`), 2000px로 줄인 문서 영역 감지, 사각형 감지(`VNDetectRectanglesRequest`, 두 크기 후보를 모음) 차례입니다. 문서 영역 감지는 같은 사진도 크기에 따라 맞고 틀리기를 오가서 두 크기를 봅니다. 사각형 감지는 펼침의 한 쪽씩도 잡으므로 나란히 붙은 두 쪽을 합친 펼침 전체도 후보에 넣고 가장 넓은 것을 고릅니다(`pick_rectangle`). 펼침 배치인데 세로로 긴 테두리(왼쪽이 검은 표지일 때 흰 오른쪽만 잡히는 경우)는 버립니다. 그래도 없으면 꼭짓점 하나만 틀린 테두리를 고쳐 씁니다(`repair_one_corner`). 밝은 책상 위 흰 펼침에서 Vision과 iPhone이 똑같이 왼쪽 위 꼭짓점만 가운데 접힌 선에 붙였고, 나머지 셋은 정확했습니다. 맞는 세 꼭짓점으로 평행사변형을 완성해 자리를 정하고, 4% 안에 사각형 감지 꼭짓점이 있으면 그 자리로 맞춥니다. 고칠 수 있는 꼭짓점이 하나뿐이고 고친 테두리가 사진 넓이의 10% 이상일 때만 씁니다. 모두 못 찾으면 사진 전체를 씁니다. Mac이 찾은 꼭짓점은 캐시 상태 파일에 두고 화면에 "Mac 감지"로 보입니다(`src-tauri/src/pipeline/mod.rs` `choose_quad`)
   - 믿을 만한 사각형: 볼록하고, 마주 보는 변 길이의 비가 0.8 이상이고, 내각이 65~115도(`scanshelf_core::Quad::is_plausible`, 기준과 근거는 docs/capture.md "테두리를 믿을 수 있는가"). 2026-11-08 촬영에서 iPhone이 흰 면이나 어두운 표지 쪽 꼭짓점을 접힌 선에 붙인 삼각형을 넘겼고, 그대로 펴서 페이지가 찢긴 그림이 됐습니다(6장)
   - 이렇게 버린 iPhone 꼭짓점으로 이미 처리한 장은 지문에 `quadRejected`가 붙어 한 번 다시 처리됩니다. 원본을 열지 않으려고 이 판단만 iPhone 사진 비율(세로 4:3)로 합니다(`fingerprint`)
3. 보정 단계(`steps.rs`): 원근 보정 → 두 페이지 분리 → 곡면 보정 → 손가락 지우기 → 필기 지우기 → 조명과 필터
4. `pages/<출력 id>.jpg`(품질 90, 긴 변 최대 4000픽셀)와 캐시 썸네일(너비 360)
5. OCR(`docs/ocr.md`) → `ocr/<출력 id>.json`. 실패해도 페이지는 남기고 `ocr`을 비웁니다
6. 페이지마다 `processing.json`을 저장해, 중간에 앱이 꺼져도 끝낸 페이지는 남습니다

원근 보정은 `crates/scanshelf-vision/src/perspective.rs`(호모그래피 + 쌍삼차 보간, 출력 크기는 마주 보는 변 중 긴 쪽)입니다. Core Image를 쓰지 않아 Windows에서도 같은 결과가 나옵니다.

## 단계 연결 자리 (vision 팀)

`steps.rs`의 아래 함수 본문만 바꾸면 됩니다. 입력과 출력은 `image::RgbImage`입니다.

| 함수 | 지금 | 연결할 것 |
|---|---|---|
| `perspective_step(image, quad)` | scanshelf-vision 원근 보정 | 그대로 |
| `split_step(image, layout)` | 가운데에서 자르는 자리표시 | 접힌 선 찾기 |
| `dewarp_step(image, Dewarp)` | 그대로 돌려줌 | UVDoc 계열 ONNX |
| `remove_fingers_step(image)` | 그대로 돌려줌 | 손 검출 + 인페인팅 |
| `remove_handwriting_step(image)` | 그대로 돌려줌 | 필기 분할 + 인페인팅 |
| `filter_step(image, Filter)` | 그대로 돌려줌 | 조명 보정, 회색, 흑백 |

순서를 원근 보정 다음에 분리로 둔 이유: iPhone이 찍는 꼭짓점은 펼친 책 전체의 바깥 테두리라, 먼저 펴야 접힌 선이 세로 직선에 가까워집니다. 필터를 맨 끝에 둔 이유: 흑백으로 바꾼 뒤 인페인팅하면 지운 자국이 도드라집니다. 순서를 바꾸려면 `correct_page`만 고칩니다.

## 자동 회전

거치대 촬영은 기기 방향 정보를 믿을 수 없어 글자로 판단합니다(`src-tauri/src/ocr/mod.rs` `detect_rotation`). 원근 보정 직후, 두 쪽 나누기 전에 돌립니다. 접힌 선은 세로로 찾으므로 누운 채로는 나누지 못합니다.

- 페이지를 긴 변 1400px로 줄여 한 번 읽고, 글줄 사각형의 왼쪽 위에서 오른쪽 위로 가는 읽는 방향을 글자 수와 신뢰도로 투표합니다
- 네 방향으로 돌려 "잘 읽히는 쪽"을 고르는 방식은 쓰지 않습니다. Vision은 돌아간 글자도 읽어 점수가 비슷하게 나오고, 실제 문서에서 많은 페이지를 뒤집었습니다
- 돌려야 한다고 나오면 돌린 축소본을 한 번 더 읽어 확인합니다(`detect_rotation_checked`, `rotation_confirmed`). 돌린 뒤에 바로 선 글줄이 1등이고, 읽힌 글자량이 돌리기 전의 0.8배 이상일 때만 돌립니다. Vision이 바로 선 페이지의 글줄 방향을 거꾸로 알려 주는 일이 있어서입니다(2026-11-08 데이터센터 박람회_9 3쪽과 로보월드_6 3단 접지가 180도로 뒤집혔고, 돌리면 0.66배, 0.76배로 덜 읽혔습니다. 제대로 돌린 책 14장은 0.98배 이상, 합성 영문 시험 페이지는 0.86배). 기준 사이 여유가 크지 않아, 확인에 실패한 페이지는 돌리지 않고 문서 다수 방향도 따르지 않으며 "방향 확인"으로 남깁니다
- 글자가 적거나 1등이 2등의 1.5배에 못 미치면 정하지 않습니다. 이때 세로로 선 펼침, 접지 사진만 눕히고 나머지는 그대로 둡니다
- 글자가 적어 정하지 못한 페이지는 문서의 다수 방향을 따릅니다. 앞쪽 페이지는 아직 표가 모이지 않았을 수 있어, 한 바퀴 처리한 뒤 다수 방향과 다르게 처리된 페이지를 다시 처리합니다(`process_document`, 표 2개 이상, 60% 이상일 때)
- 틀리면 문서 화면의 "돌리기"로 90도씩 더 돌립니다(`edits.json` `rotations`)

## 한 쪽으로 찍힌 펼침

문서 사진의 3분의 1 이상이 펼침이나 접지이면, 사람이 배치를 고르지 않은 "한 쪽" 사진도 살핍니다. 가로세로 비 1.15 이상, 접힌 선 신뢰도 0.8 이상, 가운데에서 폭의 8% 안일 때만 나눕니다(`steps.rs` `effective_layout`). 화면에서 고른 배치(`edits.json` `layouts`)는 그대로 따릅니다.

## 그림자 없는 펼침

코팅지 팜플렛을 눌러 찍으면 접힌 선 그림자도 여백 빈 띠도 없습니다. 가장 깊은 골이 사진 경계처럼 가운데 밖에 있으면 접힌 선을 찾지 못합니다(2026-11-08 데이터센터 박람회_10 2쪽: 접힌 선 0.50, 왼쪽 사진 경계 0.38). 이때 가로로 긴 사진(폭/높이 1.2 이상)만 가운데 ±3% 안에서 위아래로 이어지는 세로 경계(두 페이지 바탕색이 바뀌는 단, 가는 선)를 찾아 나눕니다(`scanshelf-vision` `split.rs` `centered_seam`). 경계가 안 보이면 나누지 않습니다. 단서 없이 가운데를 자르면 가로로 긴 한 쪽 그림을 망칩니다. 가운데 밖으로 찾지 않는 것도 사진 경계를 접힌 선으로 잘못 고르지 않기 위해서입니다.

## 사진 페이지 필터

"선명하게"는 칸마다 밝은 화소를 종이로 보고 나눠 그림자와 색을 걷어 냅니다. 종이 바탕이 거의 없는 페이지(사진이 꽉 찬 표지)는 사진 속 밝은 곳이 종이로 잡혀 색이 깨집니다(2026-11-08 데이터센터 박람회_9 표지). 종이 바탕으로 인정된 칸이 30% 미만이면 페이지 전체에 한 번만 검은 점과 흰 점을 맞추고, 끌어올리는 폭을 1.25배로 묶습니다(`filter.rs` `is_photo_page`, `enhance_photo`). 실제 팜플렛 60여 쪽에서 사진 표지는 0.00~0.19, 글과 표가 있는 쪽은 0.38 이상이었습니다.

자동 판단이 마음에 들지 않으면 문서 화면 도구 막대의 "필터"에서 이 쪽만 다른 필터를 고릅니다(`edits.json` `filters`, `pipeline/mod.rs` `page_options`). 고르지 않은 쪽은 문서 처리 옵션을 따릅니다.

## 페이지 확인

처리하며 확인한 것을 페이지마다 캐시 상태에 남기고(`steps.rs` `PageCheck`, `PageState.checks`), 문서 화면 아래 줄과 썸네일의 `!`, 보관함 카드의 "확인 N쪽"으로 보여 줍니다. 보관함 머리줄의 "확인 필요 N개"를 누르면 그런 문서만 보이고, 문서 화면 아래 줄의 "다음 확인 쪽"이 볼 쪽으로 차례로 옮겨 갑니다(`LibraryView.tsx`, `DocumentView.tsx` `goNextReview`). 자동으로 고친 것은 기록만, 사람이 볼 것은 경고로 띄웁니다. 문장마다 무엇을 보고 어떻게 고치는지 적습니다(`CHECK_TEXT`). 보고 나서 고칠 게 없으면 아래 줄의 "확인함"을 누릅니다. `edits.json` `reviewed`에 남아 `!`와 "확인 필요" 수에서 빠집니다(`library.rs` `summarize`).

| 확인 | 뜻 | 사람이 볼 것 |
|---|---|---|
| `quadRedetected` | iPhone 꼭짓점이 사각형이 아니어서 Mac이 다시 찾음 | 아니오 |
| `quadMissing` | 믿을 만한 꼭짓점이 없어 사진 전체를 씀 | 예 |
| `quadOnePage` | 펼침인데 편 결과가 세로로 김. 한 쪽만 잡혔을 수 있음 | 예 |
| `notSplit` | 펼침인데 접힌 선을 못 찾아 한 장으로 남김 | 예 |
| `splitAtSeam` | 그림자 없는 펼침을 가운데 이음매에서 나눔. 실제 보관함 4개 문서에서 모두 맞게 나뉘어 기록만 함 | 아니오 |
| `rotationDoubtful` | 돌리라고 나왔지만 돌려 보니 덜 읽혀 그대로 둠 | 예 |
| `photoPage` | 사진 페이지라 선명하게 대신 밝기만 맞춤 | 아니오 |

펼침 배치로 찍은 앞뒤 표지는 원래 한 쪽이라 문서의 첫 장과 마지막 장에는 `quadOnePage`, `notSplit`을 붙이지 않습니다.

`quadOnePage`는 테두리 바로 옆에 다른 페이지가 실제로 보일 때만 붙입니다(`pipeline/mod.rs` `page_beside`). 사진 가장자리 띠의 색을 책상으로 보고, 테두리 옆 띠(폭의 5~35%)에서 책상과 다른 색이 절반 넘는 줄이 85% 이상이면 페이지가 있다고 봅니다. 줄 단위로 보는 이유는 페이지는 위아래 전체에 걸치고 손은 일부 줄에만 걸치기 때문입니다. 2026-11-08 펼침 배치로 찍은 낱장 전단 6장이 잘못 표시돼 넣었습니다.

## 손이나 AI가 고른 나누기

자동 나누기가 틀린 장은 `edits.json`의 `folds`(접힌 선 위치, 원근 보정과 회전 뒤 그림 폭 기준 0~1, 펼침은 하나, 3단 접지는 둘)로 고칩니다. 처리(`steps.rs` `manual_split`)는 배치와 선 수가 맞을 때만 이 값을 쓰고, 준 위치 ±0.03 안에서 실제 접은 자국에 붙입니다(`fold.rs` `snap_folds`). 자국이 약하면 준 위치를 세로선으로 자릅니다. 배치와 수가 맞지 않으면 무시하고 자동으로 찾습니다.

3단 접지는 `panelOrder`로 면 순서를 바꿉니다(`order_panels`). `[2, 0, 1]`이면 오른쪽 면이 `-P1`, 왼쪽 면이 `-P2`가 됩니다. 출력 id 자체를 순서대로 다시 매기므로 화면, 내보내기, 검색, MCP의 쪽 순서 규칙(통째, 왼쪽, 오른쪽, `-P1`~`-P3`)은 그대로입니다. 바깥 면 사진에서 표지(오른쪽 면)를 맨 앞에 둘 때 씁니다.

두 값은 지금 MCP(`set_page_folds`, `set_panel_order`)로만 고칩니다. 화면은 값을 보여 주지 않고 저장할 때 그대로 돌려줍니다. 배치를 바꾸면 그 배치에 맞춰 고른 값은 지웁니다(`control.rs` `set_layout`). 도구 목록은 `docs/mcp.md`에 있습니다.

## 다시 처리 판단

페이지마다 입력 지문(원본 파일 크기와 수정 시각, 꼭짓점, 배치, 돌리기, 옵션, `PIPELINE_VERSION`, 고른 접힌 선과 면 순서가 있으면 그것도)을 SHA-256으로 만들어 캐시(`<앱 캐시>/state/<문서 id>.json`)에 둡니다. 아래 중 하나면 다시 처리합니다.

- 출력이 없거나, `pipelineVersion`이 낮거나, 옵션이 그 페이지 옵션(문서 기본값에 페이지별 필터를 덮은 값)과 다름
- 지문이 다름(꼭짓점을 고쳤거나 iPhone이 원본을 바꿔 보냄)

지운 페이지의 출력 파일은 정리하고, 펼침과 낱장을 바꿔 출력 id가 달라지면 옛 파일을 지웁니다.

계약 요청: 지문은 `processing.json`의 `OutputPage`에 `inputFingerprint`로 두는 편이 맞습니다. 지금은 계약을 바꾸지 않으려고 Mac 캐시에 둡니다. 캐시를 지우면 한 번 모두 다시 처리할 뿐 결과는 같습니다.

## 작업 줄

- 문서를 하나씩 처리하는 스레드 하나(4800만 화소 사진 한 장이 수백 MB). 같은 문서가 처리 중에 다시 들어오면 끝난 뒤 한 번 더 돕니다
- 앱을 켤 때 다시 처리할 문서를 모두 줄에 세우고, 전송이 `library-changed`를 보내면 그 문서를 줄에 세웁니다
- 처리 중에는 `busy::start("처리: 문서 이름")`으로 등록해 앱 안 업데이트가 끊지 않게 합니다
- 화면 이벤트: `processing-progress { documentId, done, total, stage }`, `document-processed { documentId, error }`
- 화면이 옵션을 바꾸는 저장과 작업의 `processing.json` 저장은 같은 잠금(`io_lock`)을 씁니다

## 명령

| 명령 | 설명 |
|---|---|
| `list_documents` | 보관함. 이름, 종류, 세션은 edits 적용 값. 처리 상태와 표지 썸네일 |
| `get_document(documentId)` | 페이지, 출력, 꼭짓점과 그 출처, 옵션, edits |
| `save_edits(documentId, edits)` | edits.json 전체 저장 후 다시 처리 줄에 세움 |
| `set_processing_options(documentId, options)` | processing.json 기본 옵션 |
| `reprocess_document(documentId, force)` | `force`면 지문이 같아도 모두 |
| `raw_preview(documentId, pageId)` | 꼭짓점 편집기용 원본 미리보기 경로(회전 적용, 긴 변 2000) |
| `get_ocr`, `get_summary`, `save_summary_field` | 화면 표시와 AI 정리 손 편집(`editedFields`에 키를 더함) |
| `processing_states` | 작업 줄 상태 |

## 앱 띄워 보기

합성 문서를 보관함에 만들고 앱을 켭니다.

```sh
SCANSHELF_SAMPLE_LIBRARY="$HOME/Library/Application Support/com.nobel6018.scanshelf/library" \
  cargo test -p scanshelf -- --ignored 합성_예시
pnpm tauri dev
```
