//! 앱 밖에서 들어오는 요청(전역 단축키, scanshelf:// 링크, Finder의 "다음으로 열기", 메뉴의 가져오기)을 화면에 전달한다.
//! 열린 문서와 화면 상태는 프론트가 들고 있으므로, 여기서는 할 일을 정해 `app-action` 이벤트로 넘기기만 한다.
//! 우클릭 메뉴와 빠른 찾기가 쓰는 문서 단위 명령(지우기, Finder에서 보기, 마크다운)도 여기에 둔다.
//!
//! @see docs/shortcuts-and-deep-links.md
//! @see docs/context-menu.md

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Url};

use crate::library::{document_dir, load_applied, load_processing, read_json};
use crate::pipeline::{JobState, Pipeline};

pub const EVENT: &str = "app-action";
/// 문서를 지웠다. library-changed는 받은 문서를 처리 줄에 넣으므로(lib.rs) 지운 문서에는 따로 알린다
pub const EVENT_DOCUMENT_DELETED: &str = "document-deleted";
/// macOS ⌥⇧⌘L. 세 수식키를 같이 누르는 조합이라 다른 앱과 잘 겹치지 않는다
pub const SHOW_WINDOW_SHORTCUT: &str = "CommandOrControl+Alt+Shift+L";
const IMPORT_MENU_ID: &str = "scanshelf-import";
const SHORTCUTS_MENU_ID: &str = "scanshelf-shortcuts";

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Action {
    ShowWindow,
    /// 문서를 열고, page가 있으면 그 쪽(1부터, 화면 순서)을 보여 준다
    Open { id: String, page: Option<u32> },
    /// 빠른 찾기 창을 그 검색어로 연다
    Search { query: String },
    /// Finder에서 연 파일. 화면이 import_files로 가져온다(끌어다 놓기와 같은 길)
    Import { paths: Vec<PathBuf> },
    /// 메뉴의 "가져오기…". 화면이 파일 고르기 창을 띄운다
    PickImport,
    /// 도움말 메뉴의 "단축키 보기". 화면이 단축키 목록을 띄운다
    ShowShortcuts,
}

#[derive(Default)]
pub struct Bus {
    /// 화면이 이벤트를 받을 준비가 되기 전에 들어온 요청. 링크나 파일로 앱을 처음 띄운 경우다
    pending: Mutex<Option<Vec<Action>>>,
    shortcut_error: Mutex<Option<String>>,
}

impl Bus {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(Some(Vec::new())),
            shortcut_error: Mutex::new(None),
        }
    }
}

pub fn dispatch(app: &AppHandle, action: Action) {
    show_main(app);
    let bus = app.state::<Bus>();
    let mut pending = bus.pending.lock().unwrap_or_else(|e| e.into_inner());
    match pending.as_mut() {
        Some(queue) => queue.push(action),
        None => {
            let _ = app.emit(EVENT, action);
        }
    }
}

pub fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadyInfo {
    pending: Vec<Action>,
    shortcut_error: Option<String>,
}

/// 화면이 app-action을 구독한 뒤 부른다. 그 전에 들어온 요청을 넘기고, 이후로는 바로 이벤트로 보낸다
#[tauri::command]
pub fn frontend_ready(app: AppHandle) -> ReadyInfo {
    let bus = app.state::<Bus>();
    let pending = bus.pending.lock().unwrap_or_else(|e| e.into_inner()).take().unwrap_or_default();
    let shortcut_error = bus.shortcut_error.lock().unwrap_or_else(|e| e.into_inner()).clone();
    ReadyInfo { pending, shortcut_error }
}

pub fn handle_url(app: &AppHandle, url: &Url) {
    match parse_url(url) {
        Some(action) => dispatch(app, action),
        None => eprintln!("알 수 없는 scanshelf 링크: {url}"),
    }
}

/// scanshelf://open/<문서 id>?page=3, scanshelf://open?id=<문서 id>&page=3, scanshelf://search?q=검색어, scanshelf://show.
/// 화면을 보여 주기만 하고 데이터를 바꾸지 않아 토큰을 받지 않는다. 지우기나 가져오기 링크는 만들지 않는다
pub fn parse_url(url: &Url) -> Option<Action> {
    if url.scheme() != "scanshelf" {
        return None;
    }
    let query = |key: &str| url.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned());
    let path = url.path().trim_matches('/');
    match url.host_str().unwrap_or("") {
        "show" if path.is_empty() => Some(Action::ShowWindow),
        "open" => {
            let id = if path.is_empty() { query("id")? } else { path.to_string() };
            let id = uuid::Uuid::parse_str(&id).ok()?.to_string();
            let page = query("page").and_then(|p| p.parse::<u32>().ok()).filter(|p| *p >= 1);
            Some(Action::Open { id, page })
        }
        "search" if path.is_empty() => Some(Action::Search {
            query: query("q").unwrap_or_default(),
        }),
        _ => None,
    }
}

/// 창을 앞으로 가져오는 전역 단축키. 다른 앱이 이미 쓰면 등록이 실패하고, 화면이 준비되면 이유를 알린다
pub fn register_global_shortcut(app: &AppHandle) {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};
    let result = SHOW_WINDOW_SHORTCUT
        .parse::<Shortcut>()
        .map_err(|e| e.to_string())
        .and_then(|s| app.global_shortcut().register(s).map_err(|e| e.to_string()));
    if let Err(error) = result {
        *app.state::<Bus>().shortcut_error.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(format!("scanshelf 창 앞으로 단축키(⌥⇧⌘L)를 등록하지 못했습니다. 다른 앱이 쓰고 있을 수 있습니다: {error}"));
    }
}

/// 파일 메뉴 맨 위에 "가져오기…"(⌘O)를 넣는다. 앱 메뉴는 update.rs가 먼저 만든다
pub fn add_import_menu(app: &tauri::App) -> tauri::Result<()> {
    use tauri::menu::{MenuItem, MenuItemKind};
    let Some(menu) = app.menu() else { return Ok(()) };
    let item = MenuItem::with_id(app, IMPORT_MENU_ID, "가져오기…", true, Some("CmdOrCtrl+O"))?;
    // macOS 기본 메뉴는 [앱, 파일, 편집, 보기, 창, 도움말] 순서다. 파일 메뉴가 없으면 앱 메뉴 다음에 넣는다
    let file = menu.items()?.into_iter().nth(1);
    match file {
        Some(MenuItemKind::Submenu(file)) => file.insert(&item, 0)?,
        _ => menu.insert(&item, 1)?,
    }
    // 도움말 메뉴(기본 메뉴의 마지막 하위 메뉴)에 "단축키 보기". 단축키는 ⌘를 길게 누르는 것과 ? 키라 메뉴에는 붙이지 않는다
    let shortcuts = MenuItem::with_id(app, SHORTCUTS_MENU_ID, "단축키 보기", true, None::<&str>)?;
    match menu.items()?.into_iter().last() {
        Some(MenuItemKind::Submenu(help)) => help.append(&shortcuts)?,
        _ => menu.append(&shortcuts)?,
    }
    app.on_menu_event(|app, event| {
        if event.id() == IMPORT_MENU_ID {
            dispatch(app, Action::PickImport);
        } else if event.id() == SHORTCUTS_MENU_ID {
            dispatch(app, Action::ShowShortcuts);
        }
    });
    Ok(())
}

/// Finder에서 보기
#[tauri::command]
pub fn reveal_document(app: AppHandle, document_id: String) -> Result<(), String> {
    let dir = document_dir(&app, &document_id)?;
    tauri_plugin_opener::reveal_item_in_dir(dir).map_err(|e| e.to_string())
}

/// 빠른 찾기 목록 한 줄. 태그는 edits.json에만 있어 검색 색인에 없으므로 여기서 같이 준다
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuickOpenEntry {
    pub id: String,
    pub name: String,
    pub kind: scanshelf_core::DocumentKind,
    pub session: Option<String>,
    pub tags: Vec<String>,
    pub page_count: usize,
    pub created_at: String,
}

/// 보관함 전체를 최신순으로. 검색어가 없을 때 목록이고, 이름, 묶음, 태그 맞추기는 화면이 초성까지 한다
#[tauri::command]
pub fn quick_open_entries(app: AppHandle) -> Result<Vec<QuickOpenEntry>, String> {
    let library = crate::library::library_dir(&app)?;
    Ok(scan_entries(&library))
}

fn scan_entries(library: &Path) -> Vec<QuickOpenEntry> {
    let Ok(entries) = std::fs::read_dir(library) else {
        return Vec::new();
    };
    let mut out: Vec<QuickOpenEntry> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|dir| dir.join(scanshelf_core::MANIFEST_FILE).is_file())
        .filter_map(|dir| {
            let (manifest, edits) = load_applied(&dir).ok()?;
            Some(QuickOpenEntry {
                id: manifest.id.to_string(),
                name: manifest.name,
                kind: manifest.kind,
                session: manifest.session,
                tags: edits.tags,
                page_count: manifest.pages.len(),
                created_at: manifest.created_at.to_rfc3339(),
            })
        })
        .collect();
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    out
}

/// OCR 글자를 마크다운 한 문서로. 빠른 찾기와 우클릭 메뉴의 "마크다운으로 복사"가 쓴다
#[tauri::command]
pub fn document_markdown(app: AppHandle, document_id: String) -> Result<String, String> {
    let dir = document_dir(&app, &document_id)?;
    build_markdown(&dir)
}

pub fn build_markdown(dir: &Path) -> Result<String, String> {
    let (manifest, edits) = load_applied(dir)?;
    let processing = load_processing(dir);
    let mut out = format!("# {}\n\n", manifest.name.trim());
    let mut meta = Vec::new();
    if let Some(session) = manifest.session.as_deref().filter(|s| !s.trim().is_empty()) {
        meta.push(format!("- 묶음: {}", session.trim()));
    }
    if !edits.tags.is_empty() {
        meta.push(format!("- 태그: {}", edits.tags.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join(" ")));
    }
    meta.push(format!("- 만든 날: {}", manifest.created_at.format("%Y-%m-%d")));
    out.push_str(&meta.join("\n"));
    out.push('\n');
    for (index, output) in processing.outputs.iter().enumerate() {
        let text = output
            .ocr
            .as_ref()
            .and_then(|p| read_json::<scanshelf_core::ocr::OcrPage>(&dir.join(p)).ok().flatten())
            .map(|ocr| ocr.text.trim().to_string())
            .unwrap_or_default();
        out.push_str(&format!("\n## {}쪽\n\n", index + 1));
        out.push_str(if text.is_empty() { "(글자 없음)" } else { &text });
        out.push('\n');
    }
    if processing.outputs.is_empty() {
        out.push_str("\n아직 처리하지 않은 문서입니다.\n");
    }
    Ok(out)
}

/// 문서 폴더를 휴지통으로 옮긴다(완전히 지우지 않는다). 처리 중이거나 처리 대기인 문서는 거부한다.
/// 다시 만들 수 있는 캐시(썸네일, 미리보기)는 바로 지우고, 검색 색인에서도 뺀다
#[tauri::command]
pub async fn delete_document(app: AppHandle, document_id: String) -> Result<(), String> {
    let dir = document_dir(&app, &document_id)?;
    if let Some(state) = app.state::<Pipeline>().job_state(&document_id)
        && matches!(state, JobState::Queued | JobState::Running { .. })
    {
        return Err("처리 중인 문서는 지울 수 없습니다. 처리가 끝난 뒤 다시 지워 주세요".into());
    }
    let cache = crate::library::cache_dir(&app)?;
    let id = document_id.clone();
    let name = load_applied(&dir).map(|(m, _)| m.name).unwrap_or_else(|_| id.clone());
    tauri::async_runtime::spawn_blocking(move || {
        let _busy = crate::busy::start(format!("휴지통으로 옮기기: {name}"));
        remove_document_dir(&dir, &cache, &id, move_to_trash)
    })
    .await
    .map_err(|e| e.to_string())??;
    crate::search::refresh_document(&app, &document_id);
    let _ = app.emit(EVENT_DOCUMENT_DELETED, serde_json::json!({ "documentId": document_id }));
    Ok(())
}

fn remove_document_dir(
    dir: &Path,
    cache: &Path,
    document_id: &str,
    trash: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    trash(dir)?;
    for sub in ["thumbs", "previews"] {
        let _ = std::fs::remove_dir_all(cache.join(sub).join(document_id));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn move_to_trash(path: &Path) -> Result<(), String> {
    use objc2_foundation::{NSFileManager, NSURL};
    let url = NSURL::from_file_path(path).ok_or("휴지통으로 옮길 경로가 올바르지 않다")?;
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, None)
        .map_err(|e| format!("휴지통으로 옮기지 못했다: {}", e.localizedDescription()))
}

#[cfg(not(target_os = "macos"))]
fn move_to_trash(_path: &Path) -> Result<(), String> {
    Err("휴지통으로 옮기기는 아직 macOS에서만 된다".into())
}

/// Finder가 넘긴 경로 중 가져올 수 있는 것만. 끌어다 놓기와 같은 규칙(import.rs)
pub fn opened_files(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    crate::import::importable_files(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0b8e7c52-3f7a-4c3e-9d55-2a7f0b9a1c11";

    fn parse(s: &str) -> Option<Action> {
        parse_url(&Url::parse(s).unwrap())
    }

    #[test]
    fn 열기_링크는_경로의_id와_쪽을_읽는다() {
        assert_eq!(
            parse(&format!("scanshelf://open/{ID}?page=3")),
            Some(Action::Open { id: ID.into(), page: Some(3) })
        );
        assert_eq!(parse(&format!("scanshelf://open/{ID}")), Some(Action::Open { id: ID.into(), page: None }));
        assert_eq!(
            parse(&format!("scanshelf://open?id={ID}&page=2")),
            Some(Action::Open { id: ID.into(), page: Some(2) })
        );
    }

    #[test]
    fn 열기_링크의_잘못된_id와_쪽은_버린다() {
        assert_eq!(parse("scanshelf://open/../../etc"), None);
        assert_eq!(parse("scanshelf://open?id=nope"), None);
        assert_eq!(parse(&format!("scanshelf://open/{ID}?page=0")), Some(Action::Open { id: ID.into(), page: None }));
        assert_eq!(parse(&format!("scanshelf://open/{ID}?page=x")), Some(Action::Open { id: ID.into(), page: None }));
    }

    #[test]
    fn 검색_링크는_한국어_검색어를_읽는다() {
        assert_eq!(
            parse("scanshelf://search?q=%EB%8D%B0%EC%9D%B4%ED%84%B0%EC%84%BC%ED%84%B0"),
            Some(Action::Search { query: "데이터센터".into() })
        );
        assert_eq!(parse("scanshelf://search"), Some(Action::Search { query: String::new() }));
    }

    #[test]
    fn 다른_스킴과_모르는_경로는_무시한다() {
        assert_eq!(parse(&format!("https://open/{ID}")), None);
        assert_eq!(parse(&format!("scanshelf://delete/{ID}")), None);
        assert_eq!(parse("scanshelf://show/x"), None);
        assert_eq!(parse("scanshelf://show"), Some(Action::ShowWindow));
    }

    #[test]
    fn 전역_단축키는_읽을_수_있다() {
        assert!(SHOW_WINDOW_SHORTCUT.parse::<tauri_plugin_global_shortcut::Shortcut>().is_ok());
    }

    #[test]
    fn 지우면_문서_폴더와_캐시가_사라진다() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("library").join(ID);
        let cache = root.path().join("cache");
        std::fs::create_dir_all(dir.join("raw")).unwrap();
        std::fs::write(dir.join("manifest.json"), b"{}").unwrap();
        std::fs::create_dir_all(cache.join("thumbs").join(ID)).unwrap();
        std::fs::create_dir_all(cache.join("previews").join(ID)).unwrap();
        let trash = root.path().join("trash");

        remove_document_dir(&dir, &cache, ID, |p| {
            std::fs::create_dir_all(&trash).unwrap();
            std::fs::rename(p, trash.join(ID)).map_err(|e| e.to_string())
        })
        .unwrap();

        assert!(!dir.exists());
        assert!(trash.join(ID).join("manifest.json").is_file(), "휴지통에 그대로 있다");
        assert!(!cache.join("thumbs").join(ID).exists());
        assert!(!cache.join("previews").join(ID).exists());
    }

    #[test]
    fn 휴지통으로_못_옮기면_캐시도_그대로_둔다() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(ID);
        let cache = root.path().join("cache");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(cache.join("thumbs").join(ID)).unwrap();

        let result = remove_document_dir(&dir, &cache, ID, |_| Err("권한 없음".into()));

        assert!(result.is_err());
        assert!(dir.exists());
        assert!(cache.join("thumbs").join(ID).exists());
    }

    #[test]
    fn 마크다운은_이름_묶음_태그와_쪽별_글자를_담는다() {
        let root = tempfile::tempdir().unwrap();
        let dir = crate::import::tests::write_sample_document(root.path(), "예시 문서", &["첫 쪽 글자", ""]);
        let edits = scanshelf_core::overrides::Edits {
            session: Some("2026 예시박람회".into()),
            tags: vec!["분야/냉각".into()],
            ..Default::default()
        };
        edits.save(&dir).unwrap();

        let md = build_markdown(&dir).unwrap();

        assert!(md.starts_with("# 예시 문서\n"));
        assert!(md.contains("- 묶음: 2026 예시박람회"));
        assert!(md.contains("- 태그: #분야/냉각"));
        assert!(md.contains("## 1쪽\n\n첫 쪽 글자"));
        assert!(md.contains("## 2쪽\n\n(글자 없음)"));
    }

    #[test]
    fn 빠른_찾기_목록은_태그를_담아_최신순이다() {
        let root = tempfile::tempdir().unwrap();
        let older = crate::import::tests::write_sample_document(root.path(), "옛 문서", &[]);
        let newer = crate::import::tests::write_sample_document(root.path(), "새 문서", &[]);
        let mut manifest = scanshelf_core::Manifest::load(&newer).unwrap();
        manifest.created_at += chrono::Duration::days(1);
        manifest.save(&newer).unwrap();
        scanshelf_core::overrides::Edits {
            tags: vec!["책/경제".into()],
            ..Default::default()
        }
        .save(&older)
        .unwrap();

        let entries = scan_entries(root.path());

        assert_eq!(entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), vec!["새 문서", "옛 문서"]);
        assert_eq!(entries[1].tags, vec!["책/경제".to_string()]);
    }
}
