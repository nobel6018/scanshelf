# 단축키, scanshelf:// 링크, 빠른 찾기

앱 밖에서 들어오는 요청(scanshelf:// 링크, Finder의 "다음으로 열기", 파일 메뉴)은 모두 같은 길로 화면에 전달됩니다. 앱 안 단축키와 다른 화면이 보내는 명령은 `runCommand` 하나로 모입니다.

```
scanshelf:// 링크 ──┐
파일 열기 ─────┼─> actions::dispatch ──(app-action 이벤트)──> App.handleAction
파일 메뉴 ─────┘
앱 안 단축키 ──> matchShortcut ──┐
sendCommand ────────────────────┼─> App.runCommand ──> 기본 명령, 또는 registerCommand로 붙은 처리 함수
```

열린 문서와 화면 상태는 프론트가 들고 있습니다. 그래서 Rust는 할 일을 정해 넘기기만 하고, 실제 동작은 화면 단추와 같은 명령을 부릅니다.

| 위치 | 하는 일 |
|---|---|
| `src-tauri/src/actions.rs` `parse_url`, `dispatch`, `frontend_ready`, `register_global_shortcut`, `add_import_menu` | 링크 해석, 화면이 준비되기 전 요청 보관, 전역 단축키, 파일 메뉴의 "가져오기…" |
| `src-tauri/src/actions.rs` `quick_open_entries`, `document_markdown` | 빠른 찾기 목록(태그 포함), 마크다운 복사 |
| `src-tauri/src/lib.rs` `run`, `setup_deep_links` | 플러그인 등록, 파일 열기(`RunEvent::Opened`, 두 번째 실행 인자) |
| `src/shortcuts.ts` `SHORTCUTS`, `matchShortcut`, `availableOn`, `shortcutsFor`, `registerCommand`, `sendCommand` | 앱 안 단축키 목록(화면별 범위 포함)과 명령 통로 |
| `src/views/ShortcutOverlay.tsx`, `src/App.tsx`(⌘ 길게 누르기 `useEffect`) | 단축키 목록 오버레이 |
| `src-tauri/src/actions.rs` `add_import_menu`(`SHORTCUTS_MENU_ID`) | 도움말 메뉴의 "단축키 보기" |
| `src/App.tsx` `handleAction`, `runCommand` | 요청을 화면 동작으로 바꿈 |
| `src/views/QuickOpen.tsx` | 빠른 찾기 창 (목록, 미리 보기, 마크다운 복사) |

## 앱 안 단축키

목록의 원본은 `src/shortcuts.ts`의 `SHORTCUTS`입니다. 사이드바의 "빠른 찾기", "가져오기" 단추도 같은 명령을 부릅니다.

| 키 | 명령 | 동작 | 입력칸에서도 |
|---|---|---|---|
| ⌘K | `quickOpen` | 빠른 찾기 | 예 |
| ⌘F | `search` | 검색 화면 | 예 |
| ⌘1 | `library` | 보관함 | 예 |
| ⌘, | `settings` | 설정 | 예 |
| ⌘J | `openAsk` | 보관함에 물어보기 (질문 창이 붙임) | 예 |
| ⌘O | `import` | 그림, PDF 가져오기 | 예 |
| ⌘E | `exportPdf` | 문서 화면에서만. 이 문서를 PDF로 내보내기. 다 되면 위에 "Finder에서 보기"를 띄움 | 아니요 |
| ⌘R | `reprocess` | 문서 화면에서만. 이 문서만 다시 처리(강제) | 아니요 |
| ⌘⌫ | `delete` | 문서 화면에서만. 이 문서를 휴지통으로. 확인 창을 먼저 띄움 | 아니요 |
| ← → ↑ ↓ | | 문서 화면에서만. 이전, 다음 쪽. `DocumentView`가 직접 받고 표에는 `passive`로만 둠 | 아니요 |
| ? | `showShortcuts` | 단축키 목록. Esc나 바깥을 누르면 닫힘 | 아니요 |
| ⌘ 길게(0.8초) | | 단축키 목록. 떼면 닫힘 | |

- 글자 입력칸에 커서가 있으면 ⌘⌫, 화살표 같은 키는 입력칸의 원래 동작을 따릅니다
- 한글 조합 중에 누른 키는 무시합니다(`isComposing`, keyCode 229)
- 빠른 찾기 창이 열려 있으면 창이 키를 먼저 받고, 앱 단축키는 쉽니다
- ⌘R은 웹뷰 새로고침 대신 다시 처리입니다. `preventDefault`로 새로고침을 막습니다
- 문서 단위 명령(⌘E, ⌘R, ⌘⌫)은 `screens: ["document"]`입니다. 다른 화면에서 누르면 실행하지 않고 "⌘R은 문서 화면에서 씁니다: 이 문서만 다시 처리"를 알립니다. 보관함 화면에는 고른 문서가 없어 무엇을 다시 처리하는지 알 수 없기 때문입니다(2026-11-08 사용자 지적). 보관함 전체 다시 처리는 단축키로 두지 않습니다

### 단축키 목록 오버레이

iPadOS처럼 ⌘를 혼자 0.8초 누르고 있으면 지금 화면(보관함, 문서, 검색, 설정)에서 쓰는 단축키를 묶음(이동, 파일, 문서, 도움말)별로 띄웁니다. 떼면 닫힙니다. ⌘K처럼 다른 키와 같이 누르면 띄우지 않고, 떠 있다가 다른 키를 누르거나 창이 포커스를 잃어도 닫습니다. ? 키나 도움말 메뉴의 "단축키 보기"로 열면 Esc나 바깥을 누를 때까지 남습니다.

목록은 `SHORTCUTS`를 그대로 읽습니다(`shortcutsFor(screen)`). 새 단축키를 `SHORTCUTS`에 넣으면 오버레이에도 바로 보이므로 따로 고칠 곳이 없습니다. 특정 화면 전용이면 `screens`를, 키 표기를 직접 정하려면 `display`를 줍니다.

### 다른 화면이 명령 붙이기

⌘J는 질문 창(mac-organize 담당)을 위해 비워 두었습니다. 질문 창이 자기 컴포넌트에서 붙이면 됩니다. 붙인 처리 함수가 없으면 "아직 준비되지 않았습니다"를 알립니다.

```ts
import { registerCommand } from "../shortcuts";

useEffect(() => registerCommand("openAsk", () => setAskOpen(true)), []);
```

새 단축키가 필요하면 `SHORTCUTS`에 한 줄을 더하고 같은 이름으로 `registerCommand`를 부릅니다. 단추에서 같은 명령을 부를 때는 `sendCommand("openAsk")`를 씁니다.

## 전역 단축키

| 동작 | macOS |
|---|---|
| scanshelf 창 앞으로 | ⌥⇧⌘L (`actions::SHOW_WINDOW_SHORTCUT`) |

다른 앱이 이미 쓰는 조합이면 등록이 실패하고, 화면이 준비될 때 이유를 알립니다. 지금은 바꾸는 설정이 없습니다.

## scanshelf:// 링크

| 링크 | 동작 |
|---|---|
| `scanshelf://open/<문서 id>` | 문서를 엶 |
| `scanshelf://open/<문서 id>?page=3` | 문서의 3쪽을 엶. 쪽은 1부터, 화면 순서(원본 쪽 순서, 펼친 면은 왼쪽부터) |
| `scanshelf://open?id=<문서 id>&page=3` | 위와 같음 |
| `scanshelf://search?q=<검색어>` | 빠른 찾기 창을 그 검색어로 엶 |
| `scanshelf://show` | 창 앞으로 |

모든 링크는 창을 앞으로 가져옵니다. MCP 서버, Raycast, 단축어(Shortcuts)가 `open "scanshelf://open/<id>?page=2"`로 씁니다.

- 문서 id는 UUID만 받습니다. 아니면 링크를 버립니다. `page`가 0이거나 숫자가 아니면 첫 쪽을 엽니다
- 화면을 보여 주기만 하고 데이터를 바꾸지 않아 토큰을 받지 않습니다. 웹 페이지도 `scanshelf://` 링크를 열 수 있으므로, 지우기, 가져오기, 내보내기처럼 데이터를 바꾸거나 이 컴퓨터의 파일을 읽는 링크는 만들지 않았습니다. 필요해지면 데이터 폴더의 600 권한 파일에 둔 토큰을 링크에 요구하는 방식을 먼저 붙입니다
- 링크로 앱을 처음 띄우면 화면이 준비되기 전에 링크가 도착합니다. Rust가 요청을 보관했다가, 화면이 `app-action` 구독을 마치고 `frontend_ready`를 부를 때 넘깁니다
- macOS에서는 설치된 앱(.app)만 링크를 받습니다. `pnpm tauri dev`로는 시험할 수 없습니다. `pnpm tauri build --debug --bundles app`으로 묶은 뒤 `open "scanshelf://search?q=로봇"`으로 시험합니다

플러그인은 검증한 버전으로 고정합니다(`tauri-plugin-deep-link =2.6.2`, `tauri-plugin-single-instance =2.5.4`, `tauri-plugin-global-shortcut =2.4.1`). 두 번째 실행은 single-instance가 가로채 첫 창에 넘깁니다.

## 빠른 찾기 (⌘K)

검색어가 없으면 모든 문서를 최신순으로 보여 줍니다. 검색어를 넣으면 아래 순서로 보여 줍니다.

1. 이름, 묶음(session), 태그가 맞은 문서(최신순). 초성으로도 맞춥니다(`ㅎㅂㄹ`, `한ㅂ`). 규칙은 `search.rs` `matches_with_initials`와 같고 화면(`QuickOpen.tsx` `matchesWithInitials`)에서 맞춥니다. 태그는 검색 색인에 없어 `quick_open_entries`가 edits.json에서 읽어 줍니다
2. 본문(OCR)만 맞은 문서. `search_library`를 그대로 씁니다. 문서마다 찾은 쪽을 5개까지 펼치고 나머지는 개수만

| 키 | 동작 |
|---|---|
| ↑ ↓ | 고르기 |
| ↩ | 열기. 본문에서 찾은 쪽을 골랐으면 그 쪽을 열고 찾은 글자를 칠함 |
| ⇧⌘C | 고른 문서의 글자를 마크다운으로 복사 |
| Esc, ⌘K | 닫기 |

오른쪽 미리 보기는 고른 쪽(문서 줄이면 첫 쪽)의 썸네일과 OCR 글자를 보여 줍니다. 찾은 글자 앞뒤 400자만 그립니다.

복사하는 마크다운(`actions::build_markdown`)은 제목, 묶음, 태그, 만든 날, 그다음 `## n쪽`마다 OCR 글자입니다. 쪽 번호는 `processing.json` 출력 순서이며 텍스트 내보내기(`[n쪽]`)와 같습니다.
