//! scanshelf Mac 앱. 문서 보관함은 앱 데이터 폴더의 `library/`이고, 문서 하나가 폴더 하나다.
//!
//! @see docs/architecture.md

mod actions;
mod ai;
pub mod archive;
mod ask;
mod book;
pub mod busy;
mod calendar;
pub mod control;
mod download;
mod export;
mod import;
pub mod inbox;
mod library;
mod migrate;
mod models;
mod ocr;
mod pipeline;
mod search;
pub mod store;
mod tagger;
pub mod transfer;
mod update;

use std::sync::Arc;

use tauri::{Emitter, Listener, Manager};

pub fn run() {
    tauri::Builder::default()
        // 다른 플러그인보다 먼저 등록해야 두 번째 실행을 가장 먼저 가로챈다. 링크는 deep-link 플러그인이 받는다
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if args.iter().any(|a| a.starts_with("scanshelf:")) {
                return;
            }
            let files = actions::opened_files(args.iter().skip(1).map(std::path::PathBuf::from));
            if files.is_empty() {
                actions::show_main(app);
            } else {
                actions::dispatch(app, actions::Action::Import { paths: files });
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        actions::show_main(app);
                    }
                })
                .build(),
        )
        .manage(actions::Bus::new())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(pipeline::Pipeline::default())
        .setup(|app| {
            update::add_update_menu(app)?;
            actions::add_import_menu(app)?;
            actions::register_global_shortcut(app.handle());
            setup_deep_links(app)?;
            let app_data_dir = app.path().app_data_dir()?;
            // 보관함을 처음 읽기 전에 옮긴다
            let moved = migrate::migrate_legacy_data(&app_data_dir);
            if moved > 0 {
                eprintln!("이름을 바꾸기 전 데이터 {moved}개를 옮겼다");
            }
            let store = Arc::new(store::Store::new(&app_data_dir));
            store.clean_stale_parts();
            transfer::setup(app.handle(), store.clone(), &app_data_dir);
            inbox::setup(app.handle(), store);

            let handle = app.handle().clone();
            match search::SearchIndex::open(&app_data_dir.join("search.sqlite3")) {
                Ok(index) => {
                    app.manage(index);
                }
                Err(error) => eprintln!("{error}"),
            }
            let pipeline = app.state::<pipeline::Pipeline>().inner().clone();
            pipeline.start(handle.clone());
            // 전송(Wi-Fi, iCloud)이 새 문서나 페이지를 받으면 알린다. 받은 문서를 바로 처리한다
            let on_change = pipeline.clone();
            app.listen(transfer::EVENT_LIBRARY_CHANGED, move |event| {
                #[derive(serde::Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Changed {
                    document_id: String,
                }
                if let Ok(changed) = serde_json::from_str::<Changed>(event.payload()) {
                    on_change.enqueue(&changed.document_id, false);
                }
            });
            start_control(app.handle(), &app_data_dir);
            std::thread::spawn(move || {
                search::refresh_all(&handle);
                pipeline.enqueue_stale(&handle);
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            busy::busy_tasks,
            transfer::transfer_status,
            transfer::start_pairing,
            transfer::cancel_pairing,
            transfer::remove_paired_device,
            inbox::icloud_inbox_status,
            inbox::refresh_inbox,
            archive::document_origin,
            library::list_documents,
            library::get_document,
            library::save_edits,
            library::set_processing_options,
            library::reprocess_document,
            library::raw_preview,
            library::get_ocr,
            library::get_summary,
            library::save_summary_field,
            pipeline::worker::processing_states,
            search::search_library,
            export::export_document,
            export::list_exports,
            export::reveal_export,
            export::share_export,
            export::save_export_as,
            ai::summarize_pamphlet,
            ai::suggest_document_name,
            ai::export_docx,
            ai::export_ai_bundle,
            ai::export_notion_csv,
            ask::ask_document,
            ask::ask_library,
            tagger::document_tags,
            tagger::set_document_tags,
            tagger::suggest_tags,
            calendar::calendar_access,
            calendar::suggest_session,
            models::models_status,
            models::install_models,
            models::cancel_model_install,
            actions::frontend_ready,
            actions::reveal_document,
            actions::quick_open_entries,
            actions::document_markdown,
            actions::delete_document,
            import::import_files,
            import::pick_import_files,
            book::book_page_report,
            book::sort_by_page_numbers,
        ])
        .build(tauri::generate_context!())
        .expect("scanshelf 앱을 시작하지 못했다")
        .run(|app, event| {
            // Finder에서 "다음으로 열기 > scanshelf"를 고르거나 Dock 아이콘에 파일을 끌어다 놓은 경우
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Opened { urls } = &event {
                let files = actions::opened_files(urls.iter().filter_map(|u| u.to_file_path().ok()));
                if !files.is_empty() {
                    actions::dispatch(app, actions::Action::Import { paths: files });
                }
            }
            if let tauri::RunEvent::Exit = &event
                && let Ok(dir) = app.path().app_data_dir()
            {
                control::remove_info(&dir.join(control::CONTROL_FILE));
            }
            let _ = (app, event);
        });
}

/// MCP 쓰기 통로(control.rs)를 127.0.0.1에 띄우고 접속 정보를 남긴다. 실패해도 앱은 그대로 쓴다(MCP 쓰기만 안 된다)
fn start_control(app: &tauri::AppHandle, app_data_dir: &std::path::Path) {
    struct Host {
        app: tauri::AppHandle,
        cache: std::path::PathBuf,
    }
    impl control::ControlHost for Host {
        fn edits_changed(&self, document_id: uuid::Uuid, reprocess: bool) {
            if reprocess {
                self.app.state::<pipeline::Pipeline>().enqueue(&document_id.to_string(), false);
            }
            let _ = self.app.emit(control::EVENT_EDITS_CHANGED, serde_json::json!({ "documentId": document_id }));
        }
        fn tags_changed(&self, document_id: uuid::Uuid) {
            let payload = serde_json::json!({ "documentId": document_id });
            let _ = self.app.emit(tagger::EVENT_TAGS_CHANGED, payload.clone());
            let _ = self.app.emit(control::EVENT_EDITS_CHANGED, payload);
        }
        fn job(&self, document_id: uuid::Uuid) -> Option<control::Job> {
            let states = self.app.state::<pipeline::Pipeline>().states();
            Some(match states.get(&document_id.to_string())? {
                pipeline::JobState::Queued => control::Job::Queued,
                pipeline::JobState::Running { done, total } => control::Job::Running { done: *done, total: *total },
                pipeline::JobState::Failed { message } => control::Job::Failed { message: message.clone() },
            })
        }
        fn raw_view(&self, dir: &std::path::Path, page: &scanshelf_core::Page) -> Result<image::RgbImage, String> {
            pipeline::raw_view(dir, page)
        }
        fn flat_view(&self, dir: &std::path::Path, document_id: uuid::Uuid, page: &scanshelf_core::Page) -> Result<image::RgbImage, String> {
            pipeline::flat_view(dir, &self.cache, &document_id.to_string(), page)
        }
    }
    let (Ok(library), Ok(cache)) = (library::library_dir(app), library::cache_dir(app)) else {
        return;
    };
    let listener = match control::bind_local() {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("MCP 쓰기 통로를 열지 못했다: {error}");
            return;
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let token = transfer::pairing::random_token();
    let info = control::ControlInfo { port, token: hex::encode(token), pid: std::process::id() };
    if let Err(error) = control::write_info(&app_data_dir.join(control::CONTROL_FILE), &info) {
        eprintln!("MCP 쓰기 통로 정보를 쓰지 못했다: {error}");
        return;
    }
    let host = Host { app: app.clone(), cache: cache.clone() };
    let service = std::sync::Arc::new(control::ControlService { library, cache, token, events: std::sync::Arc::new(host) });
    tauri::async_runtime::spawn(async move {
        let result = match tokio::net::TcpListener::from_std(listener) {
            Ok(listener) => control::serve(listener, service).await,
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            eprintln!("MCP 쓰기 통로가 멈췄다: {error}");
        }
    });
}

/// scanshelf:// 링크. 앱이 떠 있으면 on_open_url로, 링크로 앱을 처음 띄웠으면 get_current로 온다.
/// 화면이 준비되기 전 요청은 actions::Bus가 보관했다가 frontend_ready로 넘긴다
fn setup_deep_links(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    use tauri_plugin_deep_link::DeepLinkExt;
    // 설치한 앱(.app)이 스킴을 등록한다. 개발 중인 Windows와 Linux만 직접 등록한다
    #[cfg(all(debug_assertions, any(windows, target_os = "linux")))]
    app.deep_link().register_all()?;

    let handle = app.handle().clone();
    app.deep_link().on_open_url(move |event| {
        for url in event.urls() {
            actions::handle_url(&handle, &url);
        }
    });
    if let Some(urls) = app.deep_link().get_current()? {
        for url in urls {
            actions::handle_url(app.handle(), &url);
        }
    }
    Ok(())
}
