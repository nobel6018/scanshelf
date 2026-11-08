# MCP 서버

코드: `mcp/src/`(`library.ts` 보관함 읽기와 edits 적용, `search.ts` 검색 규칙, `image.ts` 그림 줄이기와 잘라 내기, `control.ts` 앱 쓰기 통로 클라이언트, `server.ts` 도구), Mac 쪽 쓰기 통로 `src-tauri/src/control.rs`. 테스트: `mcp/test/`, `src-tauri/tests/control.rs`

Claude Code, Claude 데스크톱 같은 MCP 클라이언트가 scanshelf로 스캔한 문서를 직접 찾고 읽게 합니다. 스캔한 책, 문서, 팜플렛을 나중에 조사할 때 참고 자료로 쓰려는 것입니다. 예를 들어 "작년 박람회에서 받은 냉각 업체 중 협력 후보 보여 줘", "이 책에서 액침 냉각을 다룬 곳 찾아 줘" 같은 요청입니다.

읽기 도구는 보관함 파일을 직접 읽어 Mac 앱이 꺼져 있어도 됩니다. 쓰기 도구(쪽 돌리기, 확인함, 태그)와 확인 필요 목록은 Mac 앱에 부탁하므로 앱이 켜져 있어야 합니다. MCP는 보관함 파일에 직접 쓰지 않습니다(아래 "쓰기 통로").

## 도구

| 도구 | 하는 일 | 입력 |
|---|---|---|
| `list_documents` | 최신순 문서 목록. 이름, id, 종류, 묶음, 날짜, 쪽수, 처리 상태, OCR 쪽수, 태그, AI 정리 여부 | `kind`, `session`, `tag`, `since`, `until`(YYYY-MM-DD), `relevance`, `limit`(기본 50) |
| `list_sessions` | 묶음(촬영 세션, 박람회명)마다 문서 수, 종류별 수, 최근 날짜 | 없음 |
| `list_tags` | 태그 계층과 문서 수. 상위 태그는 하위 태그 문서까지 센다 | 없음 |
| `search_documents` | 이름, 묶음, 태그, OCR 본문, AI 정리에서 찾기. 문서마다 맞은 곳, 쪽 번호, 앞뒤 36글자 조각(맞은 글자는 `«»`) | `query`, `synonyms`(OR), `must_include`(AND), `kind`, `session`, `tag`, `since`, `until`, `document_id`, `limit`(기본 20), `pages_per_document`(기본 5) |
| `get_document` | edits를 적용한 정보, AI 정리, 쪽마다 OCR 본문(`[3쪽 시작, 출력 id …]` ~ `[3쪽 끝]`). `layout`이면 본문 대신 블록 종류와 줄마다 위치 | `id`, `pages`("3", "3-5", "10-"), `text`, `summary`, `max_chars`(기본 40000), `layout` |
| `get_page_image` | 처리된 페이지 그림(`pages/<출력 id>.jpg`)을 이미지로. OCR이 놓친 표, 그림, 도면을 볼 때. `region`이면 그 부분만 원본 해상도로 | `id`, `page`(1부터), `max_edge`(기본 1568), `region`(`{x,y,width,height}`, 0~1) |
| `list_review_pages` | Mac 처리가 확인을 부탁한 쪽(화면의 "!")과 이유, 앱에서 여는 링크(`scanshelf://open/<id>?page=N`). 앱 필요 | `document_id` |
| `rotate_page` | 쪽을 시계 방향으로 더 돌리고 다시 처리하게 한다(`edits.rotations`). 앱 필요 | `id`, `page`, `degrees`(90, 180, 270) |
| `set_page_reviewed` | 확인함으로 두거나 되돌린다(`edits.reviewed`). 앱 필요 | `id`, `page`, `reviewed`(기본 true) |
| `update_document_tags` | 태그 더하기, 빼기. 사람이 고친 태그(`tagsByUser`)로 남는다. 앱 필요 | `id`, `add`, `remove` |
| `get_page_view` | 처리 중간 그림. `raw` 원본과 처리가 쓴 꼭짓점(자홍), `flat` 원근 보정과 회전 뒤 나누기 직전과 고른 접힌 선(빨강). 0.1 간격 청록 눈금. 앱 필요 | `id`, `page`, `view` |
| `processing_status` | 처리 줄 상태와 다시 처리할 원본 수. `wait_seconds`면 끝날 때까지 기다린다 | `id`, `wait_seconds`(최대 300) |
| `set_page_layout` | 배치(한 장, 펼침, 3단 접지). 전에 고른 접힌 선과 면 순서는 지운다. 앱 필요 | `id`, `page`, `layout` |
| `set_page_folds` | 접힌 선 위치(flat 그림 폭 기준 0~1). 근처 자국에 붙이고, `exact`면 그 위치 그대로 자른다. 빈 목록이면 자동 | `id`, `page`, `folds`, `exact` |
| `set_panel_order` | 3단 접지 세 면의 순서(왼쪽 면 0). 예: `[2, 0, 1]` | `id`, `page`, `order` |
| `set_page_quad` | 꼭짓점(raw 그림 기준 0~1). null이면 자동 | `id`, `page`, `quad` |
| `move_page` | 원본 사진을 다른 사진 앞으로, 비우면 맨 뒤로 | `id`, `page`, `before_page` |
| `remove_page`, `restore_page` | 원본 사진 빼기(원본 파일은 남음), 되살리기 | `id`, `page` / `id`, `photo_id` |
| `reset_page` | 그 사진의 배치, 접힌 선, 면 순서, 꼭짓점, 돌리기, 필터, 확인함을 지우고 자동 처리로 | `id`, `page` |

읽기 도구에는 `readOnlyHint: true`, 쓰기 도구에는 `readOnlyHint: false, destructiveHint: false`를 붙입니다. 클라이언트가 읽기는 자동으로, 쓰기는 물어보고 실행하도록 도구마다 다르게 승인할 수 있습니다.

`page`는 늘 출력 쪽 번호이고, 고치기 도구는 그 쪽이 나온 원본 사진 전체에 적용합니다. 펼침과 3단 접지는 같은 사진의 쪽이 함께 바뀌고, 결과 문장이 그 쪽 번호를 알려 줍니다. `get_document`에 `photos: true`를 주면 원본 사진마다 배치와 쪽 번호를, 뺀 사진이 있으면 그 id를 보여 줍니다.

### AI가 고치는 흐름

로직이 기본값을 만들고, 틀린 장은 AI가 그림을 보고 고칩니다. 모든 경우를 규칙으로 맞추려 기준값을 계속 바꾸면 다른 장이 틀어지기 때문입니다. 흐름은 이렇습니다.

1. 틀린 쪽 찾기: `list_review_pages`(화면의 "!") 또는 사용자가 짚은 쪽
2. 보기: `get_page_image`로 결과, `get_page_view`로 중간 그림(`raw` 꼭짓점, `flat` 나누기 직전)
3. 고치기: `set_page_layout`, `set_page_folds`, `set_panel_order`, `set_page_quad`, `rotate_page`, `move_page`, `remove_page`
4. 확인: `processing_status`(`wait_seconds`)로 다시 처리가 끝나기를 기다리고 `get_page_image`로 다시 봅니다. 나빠졌으면 `reset_page`
5. 정리: 문제가 없으면 `set_page_reviewed`

AI가 눈으로 고른 좌표는 어긋날 수 있어, 접힌 선은 기본으로 준 위치 ±0.03 안의 실제 접은 자국에 붙입니다(`fold.rs` `snap_folds`). 기운 선도 따라갑니다. 다만 접힌 선 바로 옆에 사진 가장자리 같은 센 세로선이 있으면 그쪽에 끌려갑니다. 데이터센터 박람회_1의 3단 접지가 그랬습니다. 접힌 선은 0.672인데 0.683의 사진 경계에 붙어, 가운데 면이 오른쪽 면 제목 첫 글자를 먹었습니다. 이럴 때는 `exact: true`로 준 위치에서 세로로 자릅니다(`edits.json` `exactFolds`). 꼭짓점은 붙이지 않으므로 4번 확인이 특히 중요합니다.

예: 3단 접지 바깥 면을 다시 찍어 옛 사진 둘을 대체할 때
`get_document(photos=true)` → 옛 사진 `remove_page` → 새 사진 `set_page_layout(trifold)` → 누웠으면 `rotate_page` → `move_page`로 앞으로 → 표지가 오른쪽이면 `set_panel_order([2, 0, 1])` → 나누지 못했으면 `get_page_view(flat)`를 보고 `set_page_folds` → `processing_status` → `get_page_image`

### 검색 조건

- `synonyms`: `query`와 같은 뜻의 다른 표기입니다. 하나만 맞아도 찾고, 조각에는 맞은 표기를 모두 `«»`로 표시합니다. 형태소 분석이나 임베딩 검색을 넣지 않고, 같은 뜻의 표기를 고르는 일은 호출하는 AI에 맡깁니다
- `must_include`: 문서 어딘가(이름, 묶음, 태그, AI 정리, 본문)에 모두 있어야 하는 낱말입니다. 문서 단위로 거르기만 하고 조각에는 표시하지 않습니다
- `since`, `until`: `list_documents`와 같이 문서를 만든(찍은) 날로 거릅니다

### 작은 글자 확대

`get_document`에 `layout: true`를 주면 쪽마다 OCR 블록(제목, 문단, 목록, 표)과 줄마다 위치 `[x,y,w,h]`를 줍니다. 단위는 `ocr.rs` `Rect`와 같은 그림 기준 0~1, 원점 왼쪽 위입니다. 이 값을 `get_page_image`의 `region`에 그대로 넣으면 그 부분을 원본 해상도로 잘라 받습니다(`image.ts` `cropJpeg`). 잘라 낸 것도 긴 변이 `max_edge`를 넘으면 줄입니다. 쪽 전체를 1568px로 받고 작은 표만 확대하면, 쪽 전체를 4096px로 받는 것보다 토큰이 적게 듭니다.

문서 id는 UUID 전체나, 하나로 정해지면 앞 8글자 이상을 받습니다. 사용자 입력을 경로에 바로 붙이지 않고 보관함 목록에서 찾습니다.

## 설치

```sh
cd mcp
npm ci
npm run build
```

Claude Code에 등록합니다.

```sh
claude mcp add scanshelf --scope user -- node "$PWD/dist/index.js"
```

Claude 데스크톱은 설정 파일(`claude_desktop_config.json`)의 `mcpServers`에 넣습니다.

```json
{
  "mcpServers": {
    "scanshelf": { "command": "node", "args": ["/절대/경로/scanshelf/mcp/dist/index.js"] }
  }
}
```

보관함 위치는 `~/Library/Application Support/com.nobel6018.scanshelf/library/`(Mac 앱의 `app_data_dir()/library`, `src-tauri/src/library.rs` `library_dir`)입니다. 다른 폴더를 읽게 하려면 `SCANSHELF_LIBRARY_DIR`에 보관함 폴더(문서 id 폴더들이 바로 들어 있는 곳)를 줍니다.

```sh
claude mcp add scanshelf --scope user -e SCANSHELF_LIBRARY_DIR=/다른/보관함 -- node "$PWD/dist/index.js"
```

## 쓰는 예

- "scanshelf에서 작년 박람회 팜플렛 중 냉각 업체이면서 협력 후보인 곳 정리해 줘"
- "scanshelf 2026 데이터센터코리아 묶음 회사들을 표로 만들어 줘"
- "scanshelf에 있는 데이터센터 설계 책에서 UPS 이야기하는 쪽 찾아서 요약해 줘"
- "한빛로보틱스 팜플렛 3쪽 스펙 표를 그림으로 보고 정리해 줘"

## 읽는 규칙

앱과 같은 결과를 보이도록 `crates/scanshelf-core`의 규칙을 TypeScript로 옮겼습니다. 형식이 바뀌면 이쪽도 같이 고칩니다.

- manifest: `schemaVersion`이 1보다 크면 읽지 않습니다(`SCHEMA_VERSION`). manifest가 없거나 깨진 폴더는 건너뜁니다
- edits.json: `Edits::apply`(`overrides.rs`)와 같습니다. 이름, 종류, 묶음을 덮고, `deletedPages`를 빼고, `layouts`와 `quads`를 덮고, `pageOrder`로 안정 정렬합니다(순서에 없는 새 페이지는 촬영 순서대로 뒤). 깨진 edits.json은 앱처럼 없는 것으로 봅니다. id는 대소문자를 무시하고 비교합니다
- 태그: `edits.tags`를 `normalize_tags`(`tags.rs`) 규칙으로 정리합니다. 필터는 `matches`와 같이 상위 태그가 하위 태그를 포함하고, 필터 값도 `normalize_tag`로 정리합니다. 대소문자는 무시합니다(`normalize_tags`가 대소문자만 다른 태그를 하나로 보기 때문)
- 쪽 순서: edits를 적용한 원본 페이지 순서대로 그 원본의 출력 페이지를 놓습니다. 한 원본 안에서는 통째, `-L`, `-R`, `-P1`~`-P3` 순서입니다. 지운 원본의 출력은 다시 처리하기 전까지 `processing.json`에 남아 있어도 뺍니다. 쪽 번호는 이 순서의 1부터이고 `get_document`, `search_documents`, `get_page_image`가 같은 번호를 씁니다
- 처리 상태: 출력이 하나도 없으면 "처리 전", 출력이 없는 원본, 지운 원본의 출력, `pipelineVersion`이 낮은 출력이 있으면 "다시 처리할 페이지 있음", 아니면 "처리 끝"입니다
- 검색: `src-tauri/src/search.rs`와 같습니다. 공백을 빼고 소문자로 바꿔 부분 문자열로 찾고, 이름, 묶음, 태그는 초성(`ㅎㅂㄹ`, `한ㅂ`)으로도 찾습니다. 본문은 초성 검색에서 뺍니다. 이름, 묶음, 태그가 맞은 문서를 먼저, 그다음 최신순입니다. 앱의 SQLite 색인(`search.sqlite3`)은 쓰지 않고 파일을 직접 훑습니다. 색인이 없거나 오래돼도 결과가 늘 지금 파일과 같고, OCR 글자는 수정 시각으로 메모리에 캐시합니다
- 그림: 긴 변이 `max_edge`보다 크면 상자 평균으로 줄여 JPEG(품질 85)로 다시 인코딩합니다. 순수 JS(`jpeg-js`)라 네이티브 모듈이 없습니다. 작으면 파일을 그대로 보냅니다
- 경로: `processing.json`의 `image`, `ocr` 경로가 문서 폴더 밖을 가리키면 읽지 않습니다(`lib.rs` `validate_page`와 같은 뜻)

## 쓰기 통로

MCP는 edits.json에 직접 쓰지 않고 Mac 앱에 부탁합니다. 이유는 셋입니다.

- 문서 화면이 edits 전체를 들고 있다가 `save_edits`(`src-tauri/src/library.rs`)로 통째로 덮어씁니다. 바깥에서 쓴 칸은 사용자가 그 문서에서 다음에 무엇을 고치든 사라집니다. 태그만 저장 직전에 디스크 값을 다시 읽어 지킵니다
- 그 잠금(`tagger::EDITS_LOCK`)은 앱 안의 Mutex라 다른 프로세스의 쓰기를 막지 못합니다
- 돌리기는 다시 처리해야 그림이 바뀌는데, 처리 줄은 앱 메모리에 있습니다

그래서 앱이 켜질 때 `src-tauri/src/control.rs`가 127.0.0.1의 빈 포트에 작은 HTTP 서버를 띄우고, 앱 데이터 폴더(보관함 `library/`의 부모)에 `control.json`(`port`, `token`, `pid`)을 권한 0600으로 남깁니다. 토큰은 켤 때마다 새로 만듭니다. MCP(`mcp/src/control.ts`)는 이 파일을 읽어 `Authorization: Bearer <token>`으로 부릅니다. 파일 위치는 `SCANSHELF_CONTROL_FILE`로 바꿀 수 있습니다.

| 경로 | 하는 일 |
|---|---|
| `GET /v1/review` | 확인 필요 쪽. 사람이 볼 확인(`PageCheck::needs_review`)만, 확인함과 지운 쪽은 뺀다(`library.rs` `summarize`와 같은 규칙) |
| `POST /v1/documents/{id}/pages/{page}/rotate` | `{"degrees": 90 \| 180 \| 270}`. 각도를 더해 360이면 지우고, 처리 줄에 넣는다 |
| `PUT /v1/documents/{id}/pages/{page}/reviewed` | `{"reviewed": bool}`. 같은 쪽은 한 번만 남는다 |
| `PATCH /v1/documents/{id}/tags` | `{"add": [...], "remove": [...]}`. `tagger::set_tags_in_dir`로 써 `tagsByUser`가 켜진다 |
| `GET /v1/documents/{id}/status` | `{"job": {"state": "queued" \| "running" \| "failed", ...} \| null, "stalePages": n}` |
| `GET /v1/documents/{id}/pages/{page}/raw`, `/flat` | JPEG. 좌표 정보는 `x-scanshelf-view` 머리(JSON). 그림은 `pipeline::raw_view`, `flat_view`가 처리와 같은 꼭짓점(`used_quad`)과 회전으로 만든다 |
| `PUT .../layout`, `.../folds`, `.../panel-order`, `.../quad` | 배치, 접힌 선(`check_folds`: 배치와 수, 0.1 이상 간격), 면 순서(0, 1, 2 순열, 3단 접지만), 꼭짓점(`Quad::is_plausible`). 처리 줄에 넣는다 |
| `POST .../move`, `.../remove`, `.../restore`, `.../reset` | 원본 사진 순서(`pageOrder`), 빼기(마지막 한 장은 못 뺌), 되살리기, 손 지정 지우기 |

- 쓰기는 모두 화면 저장, AI 태그와 같은 `EDITS_LOCK` 안에서 합니다. 쓴 뒤 `edits-changed` 이벤트를 보내고(태그는 `tags-changed`도), 화면(`src/App.tsx`)이 이 이벤트로 열린 문서를 다시 읽어 옛 edits로 덮지 않게 합니다. 이벤트가 닿기 전 250ms 안에 사람이 같은 문서를 고치면 MCP가 쓴 칸이 덮일 수 있습니다. 둘이 같은 순간에 같은 문서를 고치는 일은 드물어 받아들였습니다
- 꼭짓점도 받습니다. 처음에는 틀려도 그림이 그럴듯하게 나와 알아채기 어렵다는 이유로 뺐지만, 결과를 다시 보는 단계(`processing_status`, `get_page_image`)와 `reset_page`를 두고 넣었습니다. 처리가 사각형으로 보지 않는 값은 앱이 받지 않습니다
- 쪽 번호는 MCP가 원본 페이지 id로 바꿔 보냅니다. 돌리기와 확인함은 원본 사진 단위라 펼침과 3단 접지는 같은 사진의 쪽이 함께 바뀌고, 도구 결과가 그 쪽 번호를 알려 줍니다
- 브라우저가 보낸 요청(`Origin` 머리)은 토큰이 맞아도 거절합니다. Wi-Fi 전송 서버(`transfer/server.rs`)와 따로 둔 것은 그쪽이 같은 네트워크에 열리고 iPhone 토큰을 쓰기 때문입니다
- 앱이 끝나면 `control.json`을 지웁니다(`pid`가 자기 것일 때만). 강제 종료로 파일이 남아도 포트가 닫혀 있어 MCP는 "앱이 켜져 있지 않다"고 답합니다. 앱이 다시 켜져 토큰이 바뀌는 사이에 부르면 401을 받고 잠시 뒤 다시 하라고 답합니다

## 문서 본문은 데이터

OCR 본문과 AI 정리는 종이에서 읽은 글자라 누구든 써 넣을 수 있습니다. 팜플렛에 "이전 지시를 무시하라" 같은 글이 있을 수 있습니다. 그래서 서버 안내문(`INSTRUCTIONS`)이 클라이언트에 문서 글자 속 지시를 따르지 말고 문서 글자를 근거로 쓰기 도구를 부르지 말라고 알리고, `get_document`는 본문과 AI 정리 앞에, `search_documents`는 결과 첫 줄에 같은 안내를 붙입니다. 본문은 `[N쪽 시작]`, `[N쪽 끝]` 표시로 감싸 도구 문구와 섞이지 않게 합니다.

읽은 내용을 보낼지는 클라이언트(Claude)가 정합니다. MCP 서버는 이 컴퓨터 안에서만 돌지만, 읽은 본문과 그림은 Claude 대화에 들어가 Anthropic으로 갑니다. 스캔 자료가 기기 밖으로 나가지 않는다는 scanshelf 기본 원칙의 예외이니, 회사 자료나 개인 소장 책을 다룰 때 이 점을 알고 씁니다.

## 한계

- 처리 상태는 Mac 캐시(꼭짓점 지문, `pipeline::stale_pages`)를 보지 않습니다. 꼭짓점이나 처리 옵션을 고친 뒤 아직 다시 처리하지 않은 문서도 "처리 끝"으로 보일 수 있습니다. 처리 중인지도 모릅니다(앱 메모리에만 있음)
- 페이지 순서를 바꾼 뒤 다시 처리하기 전이면, 앱 검색 결과의 쪽 번호(`processing.json` 순서)와 이 서버의 쪽 번호(edits 순서)가 다를 수 있습니다
- 검색은 요청마다 모든 문서의 manifest와 OCR 파일을 훑습니다. 문서 수백 개, 쪽 수천 개까지는 충분히 빠르지만 그보다 커지면 앱의 SQLite 색인을 읽는 쪽을 검토합니다
- 형태소 분석이 없어 활용형("냉각한다"로 "냉각하는" 찾기)은 찾지 못합니다. 앱 검색과 같은 한계입니다. 다른 표기는 `synonyms`로 함께 찾습니다
- 화면은 `folds`, `panelOrder`를 보여 주지 않습니다. 화면에서 배치를 바꾸면 처리는 수가 맞지 않는 접힌 선을 무시하지만 값은 남습니다
- `layout`은 OCR 블록이 있는 쪽만 위치를 줍니다. 블록 없이 본문만 있는 옛 OCR은 "OCR 글자가 없습니다"로 보입니다
- 메모 칸은 없습니다. 앱에 메모 칸이 없어서이고, 필요하면 앱에 먼저 만듭니다
- 원본 사진(`raw/*.heic`)과 내보낸 파일(`exports/`)은 돌려주지 않습니다

## 테스트

```sh
cd mcp
npm test
```

실제 스캔은 쓰지 않습니다. `fixtures/`의 manifest 예시에 합성 edits, 처리 결과, OCR, AI 정리, 단색 JPEG를 붙인 임시 보관함(`mcp/test/library-fixture.mjs`)을 만들고, edits 적용(`crates/scanshelf-core/tests/edits.rs`와 같은 경우), 태그 규칙, 검색 규칙, 그림 줄이기와 잘라 내기, 도구 20개를 MCP 클라이언트로 불러 확인합니다. 쓰기 도구는 가짜 앱으로 원본 페이지 id가 맞게 가는지, 틀린 값은 앱에 보내지 않는지 보고, 도구를 모두 부른 뒤 보관함 파일 내용과 수정 시각이 그대로인지도 봅니다(MCP가 직접 쓰지 않음). `control.test.mjs`는 쓰기 통로 클라이언트를 진짜 HTTP로 확인하고, `PIPELINE_VERSION`이 `processing.rs`와 같은지 봅니다. 앱 쪽 통로는 `src-tauri/tests/control.rs`가 localhost에 띄워 토큰, 브라우저 거절, 확인 필요 목록, 돌리기, 확인함, 태그, 배치, 접힌 선, 면 순서, 꼭짓점, 옮기기, 빼기, 되살리기, 되돌리기, 처리 상태, 중간 그림을 확인합니다. 손으로 고른 나누기와 면 순서는 `pipeline/steps.rs` `hint_tests`와 `fold.rs` 테스트가 봅니다. CI는 `.github/workflows/build.yml`의 `mcp` 작업입니다.
