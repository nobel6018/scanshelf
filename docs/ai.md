# 팜플렛 AI 정리와 내보내기

코드: `crates/scanshelf-ai/src/`. 함수는 모두 동기 함수이고 문서 폴더 경로를 받습니다. Tauri 명령에서는 `spawn_blocking`으로 감싸 부릅니다.

| 기능 | 함수 | 결과 |
|---|---|---|
| 팜플렛 AI 정리 | `summarize::summarize_pamphlet(document_dir, &LlmConfig)` | `summary.json`을 쓰고 `PamphletSummary`를 돌려줌 |
| 문서 이름 추천 | `naming::suggest_document_name(document_dir, &LlmConfig)` | `Option<NameSuggestion>`. 파일은 쓰지 않음 |
| 노션 CSV | `notion::export_notion_csv(&[document_dir], out_path)` | `NotionCsvReport`(줄 수, 건너뛴 문서) |
| 세션 문서 찾기 | `notion::session_document_dirs(library_dir, session)` | 세션이 같은 문서 폴더 목록 |
| Word | `docx::export_docx(document_dir, &DocxOptions)` | `exports/<문서 이름>.docx` |
| AI 묶음 | `bundle::export_ai_bundle(document_dir, &BundleOptions)` | `exports/<문서 이름>-ai/` 폴더 |

문서 읽기는 `document::LoadedDocument::load`가 맡습니다. manifest에 `edits.json`을 덮고, `processing.json`의 출력 페이지를 Mac에서 정한 페이지 순서로 다시 정렬해 OCR과 짝짓습니다. 지운 페이지의 출력은 뺍니다.

## AI 호출

API 키 없이 구독 로그인된 CLI를 하위 프로세스로 부릅니다(`cli.rs`). 공개 저장소라 비밀 값을 두지 않기 위해서입니다.

| 제공자 | 명령 |
|---|---|
| Claude(기본) | `claude -p --tools "" --no-session-persistence --strict-mcp-config --output-format text` |
| Codex | `codex exec --sandbox read-only --skip-git-repo-check --ephemeral --color never -o <파일> -` |

- 프롬프트는 표준 입력으로 넘깁니다. 도구를 모두 끄고 빈 임시 폴더에서 돌려, OCR 글자에 지시문이 섞여도 모델이 파일을 읽거나 명령을 실행하지 못합니다
- 실행 파일 찾는 순서: `LlmConfig.program`, 환경 변수 `SCANSHELF_CLAUDE_BIN`(Codex는 `SCANSHELF_CODEX_BIN`), 대화형 로그인 셸의 PATH와 흔한 설치 위치. Finder에서 연 앱은 PATH가 짧아 셸에 PATH를 묻습니다
- 시간 상한 기본 300초(`DEFAULT_TIMEOUT`). 넘으면 프로세스 그룹째 끝냅니다
- 실패하면 stderr 뒤쪽 800자를 오류에 담습니다. 로그인 안내 문구(`/login`, `Not logged in` 등)가 보이면 `AiError::NotLoggedIn`으로 구분합니다
- 응답에서 첫 번째 완결된 JSON 객체를 꺼냅니다(`json.rs`). 코드 블록이나 앞뒤 설명이 붙어도 읽습니다. 못 읽으면 이유와 직전 응답을 붙여 한 번 더 묻고, 그래도 실패하면 `AiError::InvalidResponse`입니다

## 팜플렛 정리 (`summarize.rs`)

프롬프트에 문서 이름, 세션(박람회명), 촬영일, 출력 페이지 OCR 글자를 순서대로 넣습니다.

- OCR 글자는 `<ocr_무작위값>` 태그로 감쌉니다. 무작위값이 매번 바뀌어 인쇄물 글자가 태그를 닫을 수 없습니다. 태그 안 지시는 내용으로만 다루라고 적습니다
- OCR 글자는 4만 자(`OCR_CHAR_BUDGET`)까지 넣습니다. 넘으면 짧은 페이지는 온전히 두고 긴 페이지를 고르게 자르며, 자른 자리에 생략 표시를 남깁니다
- 글자에 없는 사실은 빈 문자열로 두게 합니다. 고유명사는 원문, 나머지는 한국어입니다
- 가운뎃점(U+00B7)과 줄표(U+2014)는 쓰지 말라고 지시하고, 모델이 어겨도 쉼표로 바꿉니다

모델이 아니라 문서 정보로 채우는 필드가 있습니다.

| 필드 | 값 |
|---|---|
| `event` | 세션이 있으면 세션, 없으면 모델이 팜플렛에서 읽은 박람회명 |
| `date` | manifest `createdAt`을 이 Mac 시간대 날짜로(박람회 방문일) |
| `sourceFile` | 문서 이름(`edits.json` 반영) |
| `generatedBy` | `claude -p` 또는 `codex exec` |

### 관련성

NULL SPACE(데이터센터 구축 로봇) 입장에서 고릅니다. 여러 개를 고를 수 있고, `unrelated`는 다른 값과 같이 두지 않습니다. 판단 근거는 `relevanceNote`에 한두 문장으로 남습니다.

| 값 | CSV 표기 | 기준 |
|---|---|---|
| `competitor` | 경쟁사 | 데이터센터나 건설 현장용 로봇, 자동화 장비 |
| `partnerCandidate` | 협력 후보 | 시공사, 설비사, 운영사, 통합 업체, 소프트웨어 |
| `supplier` | 공급사 | 부품, 센서, 구동기, 감속기, 배터리, 제어기, 소재, 제조 서비스 |
| `unrelated` | 무관 | 위 어디에도 해당 없음 |

### 사람이 고친 필드

`summary.json`의 `editedFields`에 적힌 필드는 다시 만들 때 이전 값을 그대로 둡니다. 이름은 `summary.json` 키(camelCase)입니다. 예: `["company", "relevance", "keySpecs"]`. 화면에서 필드를 고치면 이 목록에 키를 더해 저장합니다.

## 문서 이름 추천 (`naming.rs`)

OCR 글자가 있는 첫 출력 페이지(표지) 4천 자만 보내 회사명을 묻습니다. 법인 표기(Co., Ltd., 주식회사 등)는 빼게 합니다. 세션이 있으면 `세션_회사명`, 없으면 회사명만 돌려줍니다. 파일 이름에 못 쓰는 글자는 `_`로 바꿉니다. `edits.json`에 쓸지는 화면이 정합니다.

## 노션 CSV (`notion.rs`)

열 순서는 `NOTION_COLUMNS`입니다: 회사명, 국가, 박람회명, 날짜, 주력 제품/기술, 주요 스펙, 타깃 시장/고객, 레퍼런스, 우리 회사와의 관련성, 관련성 근거, 연락처, 원본 파일명. "관련성 근거"와 "원본 파일명"에 PDF 이름을 넣는 방식은 2026-11-05 사용자 결정입니다. 앱 보관함 화면 묶음 머리줄의 "정리표와 PDF 내보내기"(2026-11-09까지 "노션 CSV 내보내기")는 PDF가 없거나 오래된 문서의 PDF를 새로 만들어 CSV와 한 폴더에 둡니다(`src-tauri/src/ai.rs`).

- RFC 4180 따옴표 규칙(쉼표, 따옴표, 줄바꿈이 든 칸만 따옴표), 줄 끝 CRLF
- UTF-8 BOM을 붙입니다. 없으면 Excel이 한글을 깨뜨립니다
- 칸 안 줄바꿈은 LF로 맞춰 따옴표 안에 둡니다
- 관련성은 `경쟁사, 협력 후보`처럼 쉼표로 잇습니다. 노션이 다중 선택으로 나눠 가져갑니다
- `summary.json`이 없거나 깨진 문서는 건너뛰고 `skipped`에 이유와 함께 남깁니다

## Word (`docx.rs`)

출력 페이지 순서대로 OCR 블록을 옮기고 페이지 사이에 쪽 나눔을 넣습니다. A4, 여백 1인치입니다.

| 블록 | Word |
|---|---|
| 제목 | 15pt 굵게 |
| 단락 | OCR 줄을 한 단락으로 이음. 영어 하이픈 줄바꿈은 붙임 |
| 목록 | 글머리 표시가 있는 줄에서 새 항목, 없는 줄은 앞 항목에 이음 |
| 표 | 탭이 있으면 탭으로 칸을 나눔. 없으면 줄 bbox 세로 중심으로 행, 왼쪽부터 열 |

- 글꼴은 macOS `Apple SD Gothic Neo`, Windows `Malgun Gothic`입니다. Word는 대체 글꼴 목록을 받지 않아 하나만 정하고, 없는 글꼴이면 Word가 비슷한 한글 글꼴로 보여 줍니다
- `include_images`를 켜면 페이지마다 글자 앞에 처리된 이미지를 넣습니다. docx-rs가 PNG만 넣어서 긴 변을 1600px로 줄입니다

## AI 묶음 (`bundle.rs`)

채팅 창에 그대로 올리는 폴더입니다. 다 만든 뒤 이름을 바꿔, 만드는 도중의 폴더가 보이지 않습니다.

```
exports/<문서 이름>-ai/
  document.md        메타데이터 표, AI 정리 표(있으면), 페이지별 OCR 글자
  images/page-01.jpg 처리된 페이지. 긴 변 2000px 이하 JPEG(품질 85)
```

`document.md`의 페이지마다 `<!-- page N -->` 표시와 `### 페이지 N` 제목, 이미지 링크가 붙습니다.

## 테스트

`crates/scanshelf-ai/tests/`. 합성 문서 폴더와 가짜 CLI(받은 프롬프트를 파일에 남기고 준비한 응답을 찍는 셸 스크립트)를 씁니다. 실제 회사 정보는 쓰지 않습니다.

실제 claude로 확인하는 테스트는 기본 실행에서 뺐습니다.

```
cargo test -p scanshelf-ai --test smoke -- --ignored --nocapture
```
