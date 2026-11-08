//! 문서 보관함 조회와 Mac 쪽 수정(edits.json, processing.json 옵션, summary.json 손 편집).
//! 형식은 scanshelf-core가 정한다.
//!
//! @see docs/document-format.md
//! @see docs/pipeline.md

use std::fs;
use std::path::{Path, PathBuf};

use scanshelf_core::overrides::Edits;
use scanshelf_core::processing::{ProcessOptions, Processing};
use scanshelf_core::summary::{PamphletSummary, SUMMARY_FILE};
use scanshelf_core::{DocumentKind, Manifest, PageLayout, Quad};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tauri::Manager;
use uuid::Uuid;

use crate::pipeline::{self, JobState, Pipeline};

pub fn library_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let base = app.path().app_data_dir().map_err(|e| format!("앱 데이터 폴더를 찾지 못했다: {e}"))?;
    Ok(base.join("library"))
}

/// 미리보기, 썸네일, 처리 상태처럼 지워도 다시 만들 수 있는 것
pub fn cache_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let base = app.path().app_cache_dir().map_err(|e| format!("캐시 폴더를 찾지 못했다: {e}"))?;
    Ok(base)
}

/// 화면에서 받은 id로 폴더를 찾는다. UUID가 아니면 거부해 보관함 밖 경로를 막는다
pub fn document_dir(app: &tauri::AppHandle, document_id: &str) -> Result<PathBuf, String> {
    let id = Uuid::parse_str(document_id).map_err(|_| format!("문서 id가 올바르지 않다: {document_id}"))?;
    let dir = library_dir(app)?.join(id.to_string());
    if !dir.join(scanshelf_core::MANIFEST_FILE).exists() {
        return Err(format!("문서를 찾지 못했다: {document_id}"));
    }
    Ok(dir)
}

/// scanshelf-core 공용 쓰기를 화면용 오류 문자열로 감싼다
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    scanshelf_core::fsio::write_json(path, value).map_err(|e| e.to_string())
}

/// 없으면 None, 깨졌으면 오류
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    scanshelf_core::fsio::read_json_optional(path).map_err(|e| e.to_string())
}

/// edits.json이 깨졌으면 없는 것으로 본다. 화면에서 다시 고치면 새로 쓴다
pub fn load_edits(dir: &Path) -> Edits {
    Edits::load(dir).unwrap_or_else(|e| {
        eprintln!("{e}");
        Edits::default()
    })
}

pub fn load_processing(dir: &Path) -> Processing {
    Processing::load(dir)
        .unwrap_or_else(|e| {
            eprintln!("{e}");
            None
        })
        .unwrap_or_default()
}

/// manifest에 Mac 수정을 덮어쓴 결과
pub fn load_applied(dir: &Path) -> Result<(Manifest, Edits), String> {
    let manifest = Manifest::load(dir).map_err(|e| e.to_string())?;
    let edits = load_edits(dir);
    Ok((edits.apply(&manifest), edits))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentSummary {
    pub id: String,
    pub name: String,
    pub kind: DocumentKind,
    pub session: Option<String>,
    pub page_count: usize,
    pub created_at: String,
    pub state: DocumentState,
    /// 첫 출력 페이지 썸네일 절대 경로. 처리 전이면 없음
    pub cover: Option<String>,
    pub has_summary: bool,
    /// 처리하며 사람이 볼 것이 나온 쪽 수(꼭짓점, 나누기, 방향)
    pub review_pages: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum DocumentState {
    /// 한 번도 처리하지 않음
    Unprocessed,
    /// 처리 방식이 바뀌었거나 꼭짓점, 옵션을 고쳐 다시 처리할 페이지가 있음
    Stale { pages: usize },
    Ready,
    Queued,
    Processing { done: usize, total: usize },
    Failed { message: String },
}

#[tauri::command]
pub fn list_documents(app: tauri::AppHandle, pipeline: tauri::State<'_, Pipeline>) -> Result<Vec<DocumentSummary>, String> {
    let dir = library_dir(&app)?;
    let cache = cache_dir(&app)?;
    Ok(scan_library(&dir, &cache, &pipeline))
}

/// 읽지 못한 문서는 건너뛴다. 전송 중인 문서 하나 때문에 보관함 전체가 안 보이면 안 된다
fn scan_library(dir: &Path, cache: &Path, pipeline: &Pipeline) -> Vec<DocumentSummary> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut documents: Vec<DocumentSummary> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| match summarize(&entry.path(), cache, pipeline) {
            Ok(summary) => Some(summary),
            Err(error) => {
                eprintln!("문서를 건너뜀: {error}");
                None
            }
        })
        .collect();
    documents.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    documents
}

pub fn summarize(dir: &Path, cache: &Path, pipeline: &Pipeline) -> Result<DocumentSummary, String> {
    let (manifest, _) = load_applied(dir)?;
    let processing = load_processing(dir);
    let id = manifest.id.to_string();
    let state = match pipeline.job_state(&id) {
        Some(JobState::Queued) => DocumentState::Queued,
        Some(JobState::Running { done, total }) => DocumentState::Processing { done, total },
        Some(JobState::Failed { message }) => DocumentState::Failed { message },
        None => {
            let stale = pipeline::stale_pages(dir, cache, &manifest, &processing);
            if processing.outputs.is_empty() && !manifest.pages.is_empty() {
                DocumentState::Unprocessed
            } else if stale > 0 {
                DocumentState::Stale { pages: stale }
            } else {
                DocumentState::Ready
            }
        }
    };
    let cover = processing
        .outputs
        .first()
        .map(|o| pipeline::thumb_path(cache, &id, &o.id))
        .filter(|p| p.exists())
        .map(|p| p.to_string_lossy().into_owned());
    let review_pages = pipeline::page_checks(cache, &id)
        .into_iter()
        .filter(|(page, checks)| manifest.pages.iter().any(|p| p.id == *page) && checks.iter().any(|c| c.needs_review()))
        .count();
    Ok(DocumentSummary {
        id,
        name: manifest.name.clone(),
        kind: manifest.kind,
        session: manifest.session.clone(),
        page_count: manifest.pages.len(),
        created_at: manifest.created_at.to_rfc3339(),
        state,
        cover,
        has_summary: dir.join(SUMMARY_FILE).exists(),
        review_pages,
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputView {
    pub id: String,
    pub part: scanshelf_core::processing::PagePart,
    pub image: String,
    pub thumb: Option<String>,
    pub has_ocr: bool,
    pub processed_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum QuadSource {
    /// Mac에서 손으로 고침
    Manual,
    /// iPhone 촬영 때 감지
    Phone,
    /// Mac이 원본 해상도에서 다시 감지
    Detected,
    /// 감지 실패. 사진 전체를 쓴다
    None,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageView {
    pub id: String,
    pub raw: String,
    pub captured_at: String,
    pub layout: PageLayout,
    pub quad: Option<Quad>,
    pub quad_source: QuadSource,
    pub outputs: Vec<OutputView>,
    /// 처리하며 확인한 것(pipeline::steps::PageCheck). 화면이 "확인 필요"를 그린다
    pub checks: Vec<pipeline::steps::PageCheck>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentDetail {
    pub summary: DocumentSummary,
    pub dir: String,
    pub pages: Vec<PageView>,
    pub deleted_pages: Vec<String>,
    pub options: ProcessOptions,
    pub edits: Edits,
}

#[tauri::command]
pub fn get_document(
    app: tauri::AppHandle,
    pipeline: tauri::State<'_, Pipeline>,
    document_id: String,
) -> Result<DocumentDetail, String> {
    let dir = document_dir(&app, &document_id)?;
    let cache = cache_dir(&app)?;
    let summary = summarize(&dir, &cache, &pipeline)?;
    let (manifest, edits) = load_applied(&dir)?;
    let processing = load_processing(&dir);
    let detected = pipeline::detected_quads(&cache, &document_id);
    let mut checks = pipeline::page_checks(&cache, &document_id);
    let pages = manifest
        .pages
        .iter()
        .map(|page| {
            let (quad, quad_source) = if let Some(q) = edits.quads.get(&page.id) {
                (Some(*q), QuadSource::Manual)
            } else if let Some(q) = page.quad {
                (Some(q), QuadSource::Phone)
            } else if let Some(q) = detected.get(&page.id) {
                (Some(*q), QuadSource::Detected)
            } else {
                (None, QuadSource::None)
            };
            let outputs = processing
                .outputs
                .iter()
                .filter(|o| o.source_page == page.id)
                .map(|o| {
                    let thumb = pipeline::thumb_path(&cache, &document_id, &o.id);
                    OutputView {
                        id: o.id.clone(),
                        part: o.part,
                        image: dir.join(&o.image).to_string_lossy().into_owned(),
                        thumb: thumb.exists().then(|| thumb.to_string_lossy().into_owned()),
                        has_ocr: o.ocr.is_some(),
                        processed_at: o.processed_at.to_rfc3339(),
                    }
                })
                .collect();
            PageView {
                id: page.id.to_string(),
                raw: dir.join(&page.raw).to_string_lossy().into_owned(),
                captured_at: page.captured_at.to_rfc3339(),
                layout: page.layout,
                quad,
                quad_source,
                outputs,
                checks: checks.remove(&page.id).unwrap_or_default(),
            }
        })
        .collect();
    Ok(DocumentDetail {
        summary,
        dir: dir.to_string_lossy().into_owned(),
        pages,
        deleted_pages: edits.deleted_pages.iter().map(|id| id.to_string()).collect(),
        options: processing.defaults,
        edits,
    })
}

/// 화면이 고친 edits 전체를 받아 저장하고, 바뀐 페이지만 다시 처리하도록 줄을 세운다
#[tauri::command]
pub fn save_edits(
    app: tauri::AppHandle,
    pipeline: tauri::State<'_, Pipeline>,
    document_id: String,
    mut edits: Edits,
) -> Result<(), String> {
    let dir = document_dir(&app, &document_id)?;
    let manifest = Manifest::load(&dir).map_err(|e| e.to_string())?;
    let merged = edits.apply(&manifest);
    // Edits는 꼭짓점 범위를 검사하지 않으므로 manifest 검증 규칙으로 한 번 더 본다
    merged.validate().map_err(|e| e.to_string())?;
    {
        // 태그는 태그 명령(tagger.rs)만 쓴다. 화면이 들고 있던 옛 edits로 덮으면 AI가 방금 붙인 태그가 사라진다.
        // 같은 잠금 안에서 디스크의 태그를 옮겨 담아 저장한다(docs/organize.md)
        let _lock = crate::tagger::EDITS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let current = Edits::load(&dir).map_err(|e| e.to_string())?;
        edits.tags = current.tags;
        edits.tags_by_user = current.tags_by_user;
        edits.save(&dir).map_err(|e| e.to_string())?;
    }
    pipeline.enqueue(&document_id, false);
    crate::search::refresh_document(&app, &document_id);
    Ok(())
}

#[tauri::command]
pub fn set_processing_options(
    app: tauri::AppHandle,
    pipeline: tauri::State<'_, Pipeline>,
    document_id: String,
    options: ProcessOptions,
) -> Result<(), String> {
    let dir = document_dir(&app, &document_id)?;
    // 처리 중인 작업이 processing.json을 쓰는 중일 수 있어 작업 줄의 잠금 안에서 바꾼다
    pipeline.with_document_lock(&document_id, || {
        let mut processing = load_processing(&dir);
        processing.defaults = options;
        processing.save(&dir).map_err(|e| e.to_string())
    })?;
    pipeline.enqueue(&document_id, false);
    Ok(())
}

#[tauri::command]
pub fn reprocess_document(
    app: tauri::AppHandle,
    pipeline: tauri::State<'_, Pipeline>,
    document_id: String,
    force: bool,
) -> Result<(), String> {
    document_dir(&app, &document_id)?;
    pipeline.enqueue(&document_id, force);
    Ok(())
}

/// 꼭짓점 편집기에 띄울 원본 미리보기(회전 적용, 긴 변 2000픽셀). HEIC를 웹뷰가 직접 그리면 회전 처리가 Rust와 다를 수 있어 직접 만든다
#[tauri::command]
pub async fn raw_preview(app: tauri::AppHandle, document_id: String, page_id: String) -> Result<String, String> {
    let dir = document_dir(&app, &document_id)?;
    let cache = cache_dir(&app)?;
    let page_uuid = Uuid::parse_str(&page_id).map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let manifest = Manifest::load(&dir).map_err(|e| e.to_string())?;
        let page = manifest
            .pages
            .iter()
            .find(|p| p.id == page_uuid)
            .ok_or_else(|| format!("페이지를 찾지 못했다: {page_id}"))?;
        let raw = dir.join(&page.raw);
        let target = pipeline::preview_path(&cache, &document_id, &page_id);
        let fresh = match (fs::metadata(&raw), fs::metadata(&target)) {
            (Ok(r), Ok(t)) => t.modified().ok() >= r.modified().ok(),
            _ => false,
        };
        if !fresh {
            let image = pipeline::platform::decode_oriented(&raw, Some(2000)).map_err(|e| e.to_string())?;
            pipeline::write_jpeg(&target, &image, 85)?;
        }
        Ok(target.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn get_ocr(app: tauri::AppHandle, document_id: String, output_id: String) -> Result<Option<scanshelf_core::ocr::OcrPage>, String> {
    let dir = document_dir(&app, &document_id)?;
    let processing = load_processing(&dir);
    let Some(path) = processing.outputs.iter().find(|o| o.id == output_id).and_then(|o| o.ocr.clone()) else {
        return Ok(None);
    };
    read_json(&dir.join(path))
}

#[tauri::command]
pub fn get_summary(app: tauri::AppHandle, document_id: String) -> Result<Option<PamphletSummary>, String> {
    let dir = document_dir(&app, &document_id)?;
    read_json(&dir.join(SUMMARY_FILE))
}

/// 사람이 고친 필드는 `editedFields`에 남겨 AI가 다시 정리해도 덮지 않게 한다
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryFieldEdit {
    pub field: String,
    pub value: serde_json::Value,
}

#[tauri::command]
pub fn save_summary_field(app: tauri::AppHandle, document_id: String, edit: SummaryFieldEdit) -> Result<PamphletSummary, String> {
    let dir = document_dir(&app, &document_id)?;
    let path = dir.join(SUMMARY_FILE);
    let summary: PamphletSummary = read_json(&path)?.ok_or("아직 AI 정리가 없다")?;
    let mut value = serde_json::to_value(&summary).map_err(|e| e.to_string())?;
    let object = value.as_object_mut().ok_or("summary 형식 오류")?;
    const LOCKED: [&str; 4] = ["generatedBy", "generatedAt", "editedFields", "sourceFile"];
    if LOCKED.contains(&edit.field.as_str()) || !object.contains_key(&edit.field) {
        return Err(format!("고칠 수 없는 필드: {}", edit.field));
    }
    object.insert(edit.field.clone(), edit.value);
    let mut updated: PamphletSummary = serde_json::from_value(value).map_err(|e| format!("값 형식 오류: {e}"))?;
    if !updated.edited_fields.contains(&edit.field) {
        updated.edited_fields.push(edit.field);
    }
    write_json(&path, &updated)?;
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 깨진_edits는_빈_수정으로_본다() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(scanshelf_core::overrides::EDITS_FILE), b"{not json").unwrap();
        assert_eq!(load_edits(dir.path()), Edits::default());
    }
}
