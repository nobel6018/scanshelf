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
2. 꼭짓점: `Edits::apply(manifest)` 값(손으로 고친 값, 없으면 iPhone 감지값). 둘 다 없거나 믿을 만한 사각형이 아니면 원본 해상도에서 `VNDetectDocumentSegmentationRequest`로 다시 찾고, 그것도 못 찾거나 믿을 만하지 않으면 사진 전체를 씁니다. Mac이 찾은 꼭짓점은 캐시 상태 파일에 두고 화면에 "Mac 감지"로 보입니다(`src-tauri/src/pipeline/mod.rs` `choose_quad`)
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
- 글자가 적거나 1등이 2등의 1.5배에 못 미치면 정하지 않습니다. 이때 세로로 선 펼침, 접지 사진만 눕히고 나머지는 그대로 둡니다
- 글자가 적어 정하지 못한 페이지는 문서의 다수 방향을 따릅니다. 앞쪽 페이지는 아직 표가 모이지 않았을 수 있어, 한 바퀴 처리한 뒤 다수 방향과 다르게 처리된 페이지를 다시 처리합니다(`process_document`, 표 2개 이상, 60% 이상일 때)
- 틀리면 문서 화면의 "돌리기"로 90도씩 더 돌립니다(`edits.json` `rotations`)

## 한 쪽으로 찍힌 펼침

문서 사진의 3분의 1 이상이 펼침이나 접지이면, 사람이 배치를 고르지 않은 "한 쪽" 사진도 살핍니다. 가로세로 비 1.15 이상, 접힌 선 신뢰도 0.8 이상, 가운데에서 폭의 8% 안일 때만 나눕니다(`steps.rs` `effective_layout`). 화면에서 고른 배치(`edits.json` `layouts`)는 그대로 따릅니다.

## 다시 처리 판단

페이지마다 입력 지문(원본 파일 크기와 수정 시각, 꼭짓점, 배치, 옵션, `PIPELINE_VERSION`)을 SHA-256으로 만들어 캐시(`<앱 캐시>/state/<문서 id>.json`)에 둡니다. 아래 중 하나면 다시 처리합니다.

- 출력이 없거나, `pipelineVersion`이 낮거나, 옵션이 문서 기본값과 다름
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
