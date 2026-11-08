# scanshelf MCP 서버

Claude Code, Claude 데스크톱 같은 MCP 클라이언트에서 scanshelf로 스캔한 문서(책, 문서, 팜플렛)를 찾고 읽습니다. 읽기 도구는 Mac 앱이 꺼져 있어도 되고, 쓰기 도구(돌리기, 확인함, 태그)는 켜진 앱에 부탁합니다. 자세한 규칙과 한계는 [docs/mcp.md](../docs/mcp.md)에 있습니다.

| 도구 | 하는 일 |
|---|---|
| `list_documents` | 최신순 문서 목록. 종류, 묶음, 태그(상위 태그는 하위 포함), 날짜 범위, 팜플렛 관련성으로 거른다 |
| `list_sessions` | 묶음(박람회처럼 함께 찍은 모음)마다 문서 수 |
| `list_tags` | 태그 계층과 문서 수 |
| `search_documents` | 이름, 묶음, 태그, OCR 본문, AI 정리에서 찾기. 띄어쓰기와 대소문자 무시, 이름과 묶음과 태그는 초성(ㅎㅂㄹ)도. `synonyms`(OR), `must_include`(AND), 날짜 범위. 쪽 번호와 조각을 준다 |
| `get_document` | 정보, AI 정리, 쪽마다 OCR 본문. `pages`("12-20")로 일부만, `max_chars`로 길이 제한. `layout`이면 줄마다 위치 |
| `get_page_image` | 처리된 페이지 그림. 긴 변을 1568px(기본)로 줄인다. `region`으로 일부만 원본 해상도로 |
| `list_review_pages` | 확인이 필요한 쪽(화면의 "!")과 이유. 앱 필요 |
| `rotate_page` | 쪽 돌리기. 앱이 다시 처리한다. 앱 필요 |
| `set_page_reviewed` | 확인함 두기, 되돌리기. 앱 필요 |
| `update_document_tags` | 태그 더하기, 빼기. 앱 필요 |
| `get_page_view` | 처리 중간 그림(원본과 꼭짓점, 나누기 직전과 접힌 선). 앱 필요 |
| `processing_status` | 다시 처리가 끝났는지. 기다릴 수 있다. 앱 필요 |
| `set_page_layout`, `set_page_folds`, `set_panel_order`, `set_page_quad` | 배치, 접힌 선, 3단 접지 면 순서, 꼭짓점 고치기. 앱 필요 |
| `move_page`, `remove_page`, `restore_page`, `reset_page` | 원본 사진 옮기기, 빼기, 되살리기, 손 지정 지우기. 앱 필요 |

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

보관함은 `~/Library/Application Support/com.nobel6018.scanshelf/library/`를 읽습니다. 다른 곳을 읽게 하려면 `SCANSHELF_LIBRARY_DIR` 환경 변수에 보관함 폴더를 줍니다.

## 개발

```sh
npm test    # 빌드 뒤 합성 보관함으로 테스트
```
