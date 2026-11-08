//! MCP 서버가 쓰는 로컬 쓰기 통로. 127.0.0.1에만 열고, 앱을 켤 때마다 새로 만든 토큰을 `control.json`에 남긴다.
//!
//! MCP가 edits.json에 직접 쓰지 않는 이유: 화면이 문서의 edits 전체를 들고 있다가 `save_edits`로 통째로 덮으므로,
//! 바깥에서 쓴 칸은 다음 편집 때 사라진다. 앱이 같은 잠금(`tagger::EDITS_LOCK`) 안에서 쓰고 `edits-changed`로
//! 화면을 다시 읽게 해야 겹치지 않는다. 처리 줄 세우기도 앱만 할 수 있다.
//!
//! 로직이 기본값을 만들고, 틀린 장은 AI가 그림을 보고 고친다. 그래서 처리 중간 그림(원본, 나누기 직전)을 보여 주고
//! 배치, 접힌 선, 면 순서, 꼭짓점, 돌리기, 순서, 빼기를 받는다. 모두 edits.json에만 남아 `reset`이나 되살리기로 되돌린다.
//! Wi-Fi 전송 서버(transfer/server.rs)와 따로 두는 이유는 그쪽이 같은 네트워크에 열리고 iPhone 토큰을 쓰기 때문이다.
//!
//! @see docs/mcp.md "쓰기 통로"

use std::fs;
use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::body::Body;
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use image::RgbImage;
use scanshelf_core::overrides::Edits;
use scanshelf_core::{Manifest, Page, PageLayout, Quad};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::library::load_applied;
use crate::pipeline::page_checks;

/// 앱 데이터 폴더(보관함 library/의 부모)에 두는 접속 정보. MCP는 보관함 폴더의 부모에서 찾는다
pub const CONTROL_FILE: &str = "control.json";
/// 화면이 받는 이벤트. 바깥(MCP)이 edits.json을 고친 문서 id. 열린 문서 화면이 다시 읽는다
pub const EVENT_EDITS_CHANGED: &str = "edits-changed";

/// 앱에 맡기는 일. 앱은 처리 줄, Tauri 이벤트, 원본 해독(macOS ImageIO)으로, 테스트는 기록과 합성 그림으로 받는다
pub trait ControlHost: Send + Sync {
    /// edits.json이 바뀌었다. reprocess면 처리 줄에 넣는다
    fn edits_changed(&self, document_id: Uuid, reprocess: bool);
    fn tags_changed(&self, document_id: Uuid);
    /// 처리 줄에서의 상태. 줄에 없으면 None
    fn job(&self, document_id: Uuid) -> Option<Job>;
    /// 원본 사진(회전 메타데이터 적용). 꼭짓점 좌표 기준
    fn raw_view(&self, dir: &Path, page: &Page) -> Result<RgbImage, String>;
    /// 원근 보정과 회전 뒤, 나누기 직전 그림. 접힌 선 좌표 기준
    fn flat_view(&self, dir: &Path, document_id: Uuid, page: &Page) -> Result<RgbImage, String>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Job {
    Queued,
    Running { done: usize, total: usize },
    Failed { message: String },
}

pub struct ControlService {
    pub library: PathBuf,
    /// 처리 상태 캐시(확인 필요 표시가 여기 있다)
    pub cache: PathBuf,
    pub token: [u8; 32],
    pub events: Arc<dyn ControlHost>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct ControlInfo {
    pub port: u16,
    /// 32바이트 hex. `Authorization: Bearer <token>`
    pub token: String,
    pub pid: u32,
}

pub fn router(service: Arc<ControlService>) -> Router {
    Router::new()
        .route("/v1/review", get(review))
        .route("/v1/documents/{id}/pages/{page}/rotate", post(rotate))
        .route("/v1/documents/{id}/pages/{page}/reviewed", put(reviewed))
        .route("/v1/documents/{id}/tags", patch(tags))
        .route("/v1/documents/{id}/status", get(status))
        .route("/v1/documents/{id}/pages/{page}/raw", get(raw_image))
        .route("/v1/documents/{id}/pages/{page}/flat", get(flat_image))
        .route("/v1/documents/{id}/pages/{page}/layout", put(set_layout))
        .route("/v1/documents/{id}/pages/{page}/folds", put(set_folds))
        .route("/v1/documents/{id}/pages/{page}/panel-order", put(set_panel_order))
        .route("/v1/documents/{id}/pages/{page}/quad", put(set_quad))
        .route("/v1/documents/{id}/pages/{page}/move", post(move_page))
        .route("/v1/documents/{id}/pages/{page}/remove", post(remove_page))
        .route("/v1/documents/{id}/pages/{page}/restore", post(restore_page))
        .route("/v1/documents/{id}/pages/{page}/reset", post(reset_page))
        .with_state(service)
}

/// 127.0.0.1 빈 포트에 묶는다. 같은 네트워크의 다른 기기는 닿지 않는다
pub fn bind_local() -> io::Result<std::net::TcpListener> {
    let listener = std::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

pub async fn serve(listener: tokio::net::TcpListener, service: Arc<ControlService>) -> io::Result<()> {
    axum::serve(listener, router(service)).await
}

/// 접속 정보를 나만 읽을 수 있게(0600) 쓴다. 다른 사용자 계정이 토큰을 읽지 못하게 한다
pub fn write_info(path: &Path, info: &ControlInfo) -> io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let _ = fs::remove_file(&tmp);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(&serde_json::to_vec_pretty(info).expect("접속 정보 직렬화는 실패하지 않는다"))?;
    file.sync_all()?;
    fs::rename(&tmp, path)
}

/// 앱이 끝날 때 지운다. 다른 앱 인스턴스가 새로 쓴 파일이면(pid가 다르면) 두고 간다
pub fn remove_info(path: &Path) {
    let mine = fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<ControlInfo>(&b).ok())
        .is_some_and(|info| info.pid == std::process::id());
    if mine {
        let _ = fs::remove_file(path);
    }
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
    }
    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
    fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }
    fn internal(message: impl Into<String>) -> Self {
        let message = message.into();
        eprintln!("MCP 쓰기 통로 오류: {message}");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

type Shared = State<Arc<ControlService>>;
type ApiResult<T> = Result<Json<T>, ApiError>;

/// 토큰을 맞춰 본다. 브라우저가 보낸 요청(Origin 머리)은 토큰이 맞아도 받지 않는다(웹 페이지가 localhost를 두드리는 경우)
fn authorize(service: &ControlService, headers: &HeaderMap) -> Result<(), ApiError> {
    if headers.contains_key(header::ORIGIN) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "브라우저 요청은 받지 않는다"));
    }
    let given = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .and_then(|v| hex::decode(v.trim()).ok());
    match given {
        Some(given) if constant_time_eq(&given, &service.token) => Ok(()),
        _ => Err(ApiError::new(StatusCode::UNAUTHORIZED, "토큰이 맞지 않는다")),
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 보관함 안의 문서 폴더. UUID가 아니거나 manifest가 없으면 없는 문서다
fn document_dir(library: &Path, id: &str) -> Result<(Uuid, PathBuf), ApiError> {
    let id = Uuid::parse_str(id).map_err(|_| ApiError::bad_request(format!("문서 id가 올바르지 않다: {id}")))?;
    let dir = library.join(id.to_string());
    if !dir.join(scanshelf_core::MANIFEST_FILE).exists() {
        return Err(ApiError::not_found(format!("문서를 찾지 못했다: {id}")));
    }
    Ok((id, dir))
}

/// 지우지 않은 원본 페이지인지 확인한다
fn existing_page(dir: &Path, page: &str) -> Result<Uuid, ApiError> {
    let page = Uuid::parse_str(page).map_err(|_| ApiError::bad_request(format!("페이지 id가 올바르지 않다: {page}")))?;
    let (manifest, _) = load_applied(dir).map_err(ApiError::internal)?;
    if !manifest.pages.iter().any(|p| p.id == page) {
        return Err(ApiError::not_found(format!("페이지를 찾지 못했다(지웠을 수 있다): {page}")));
    }
    Ok(page)
}

/// edits.json을 화면 저장, 태그 쓰기와 같은 잠금 안에서 고친다
fn edit(dir: &Path, change: impl FnOnce(&mut Edits)) -> Result<Edits, ApiError> {
    let _lock = crate::tagger::EDITS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut edits = Edits::load(dir).map_err(|e| ApiError::internal(e.to_string()))?;
    change(&mut edits);
    edits.save(dir).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(edits)
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, ApiError> + Send + 'static) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f).await.map_err(|e| ApiError::internal(e.to_string()))?
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewPage {
    pub page_id: Uuid,
    /// 사람이 볼 확인만(pipeline/steps.rs PageCheck::needs_review)
    pub checks: Vec<crate::pipeline::steps::PageCheck>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDocument {
    pub document_id: Uuid,
    pub pages: Vec<ReviewPage>,
}

/// 화면의 "!"와 같은 목록. 확인함을 누른 쪽과 지운 쪽은 뺀다(library.rs summarize와 같은 규칙)
pub fn review_documents(library: &Path, cache: &Path) -> Vec<ReviewDocument> {
    let Ok(entries) = fs::read_dir(library) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Ok(id) = Uuid::parse_str(&entry.file_name().to_string_lossy()) else {
            continue;
        };
        let Ok((manifest, edits)) = load_applied(&entry.path()) else {
            continue;
        };
        let checks = page_checks(cache, &id.to_string());
        let pages: Vec<ReviewPage> = manifest
            .pages
            .iter()
            .filter(|p| !edits.reviewed.contains(&p.id))
            .filter_map(|p| {
                let checks: Vec<_> = checks.get(&p.id)?.iter().copied().filter(|c| c.needs_review()).collect();
                (!checks.is_empty()).then_some(ReviewPage { page_id: p.id, checks })
            })
            .collect();
        if !pages.is_empty() {
            out.push(ReviewDocument { document_id: id, pages });
        }
    }
    out.sort_by_key(|d| d.document_id);
    out
}

async fn review(State(service): Shared, headers: HeaderMap) -> ApiResult<serde_json::Value> {
    authorize(&service, &headers)?;
    let (library, cache) = (service.library.clone(), service.cache.clone());
    let documents = blocking(move || Ok(review_documents(&library, &cache))).await?;
    Ok(Json(json!({ "documents": documents })))
}

#[derive(Deserialize)]
struct RotateBody {
    degrees: u16,
}

async fn rotate(
    State(service): Shared,
    headers: HeaderMap,
    UrlPath((id, page)): UrlPath<(String, String)>,
    Json(body): Json<RotateBody>,
) -> ApiResult<serde_json::Value> {
    authorize(&service, &headers)?;
    if !matches!(body.degrees, 90 | 180 | 270) {
        return Err(ApiError::bad_request("degrees는 90, 180, 270 중 하나다(시계 방향)"));
    }
    let library = service.library.clone();
    let (id, rotation) = blocking(move || {
        let (id, dir) = document_dir(&library, &id)?;
        let page = existing_page(&dir, &page)?;
        let edits = edit(&dir, |edits| {
            let total = (edits.rotations.get(&page).copied().unwrap_or(0) + body.degrees) % 360;
            if total == 0 {
                edits.rotations.remove(&page);
            } else {
                edits.rotations.insert(page, total);
            }
        })?;
        Ok((id, edits.rotations.get(&page).copied().unwrap_or(0)))
    })
    .await?;
    service.events.edits_changed(id, true);
    Ok(Json(json!({ "rotation": rotation })))
}

#[derive(Deserialize)]
struct ReviewedBody {
    reviewed: bool,
}

async fn reviewed(
    State(service): Shared,
    headers: HeaderMap,
    UrlPath((id, page)): UrlPath<(String, String)>,
    Json(body): Json<ReviewedBody>,
) -> ApiResult<serde_json::Value> {
    authorize(&service, &headers)?;
    let library = service.library.clone();
    let id = blocking(move || {
        let (id, dir) = document_dir(&library, &id)?;
        let page = existing_page(&dir, &page)?;
        edit(&dir, |edits| {
            edits.reviewed.retain(|p| *p != page);
            if body.reviewed {
                edits.reviewed.push(page);
            }
        })?;
        Ok(id)
    })
    .await?;
    service.events.edits_changed(id, false);
    Ok(Json(json!({ "reviewed": body.reviewed })))
}

#[derive(Deserialize)]
struct TagsBody {
    #[serde(default)]
    add: Vec<String>,
    #[serde(default)]
    remove: Vec<String>,
}

/// 태그 더하기, 빼기. 사람 손으로 고친 것과 같게 본다(tagsByUser). MCP 쓰기는 사람이 부탁해서 하는 일이라서다
async fn tags(
    State(service): Shared,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<TagsBody>,
) -> ApiResult<serde_json::Value> {
    authorize(&service, &headers)?;
    if body.add.is_empty() && body.remove.is_empty() {
        return Err(ApiError::bad_request("add나 remove에 태그를 하나 이상 준다"));
    }
    let library = service.library.clone();
    let (id, tags) = blocking(move || {
        let (id, dir) = document_dir(&library, &id)?;
        let current = Edits::load(&dir).map_err(|e| ApiError::internal(e.to_string()))?.tags;
        let removed: Vec<String> = body.remove.iter().filter_map(|t| scanshelf_core::tags::normalize_tag(t)).map(|t| t.to_lowercase()).collect();
        let mut next: Vec<String> = current.into_iter().filter(|t| !removed.contains(&t.to_lowercase())).collect();
        next.extend(body.add);
        let tags = crate::tagger::set_tags_in_dir(&dir, &next).map_err(ApiError::internal)?;
        Ok((id, tags))
    })
    .await?;
    service.events.tags_changed(id);
    Ok(Json(json!({ "tags": tags })))
}

/// 지운 페이지까지 포함한 원본 페이지 id. 되살리기에 쓴다
fn page_uuid(page: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(page).map_err(|_| ApiError::bad_request(format!("페이지 id가 올바르지 않다: {page}")))
}

/// 지우지 않은 페이지 하나를 edits 적용 뒤 값으로. 배치, 회전이 edits를 따른다
fn applied_page(dir: &Path, page: &str) -> Result<(Manifest, Page), ApiError> {
    let id = page_uuid(page)?;
    let (manifest, _) = load_applied(dir).map_err(ApiError::internal)?;
    let found = manifest.pages.iter().find(|p| p.id == id).cloned();
    let page = found.ok_or_else(|| ApiError::not_found(format!("페이지를 찾지 못했다(지웠을 수 있다): {id}")))?;
    Ok((manifest, page))
}

/// 페이지 하나의 edits를 고치고 다시 처리하게 한다. change가 오류를 내면 쓰지 않는다
async fn change_page(
    service: Arc<ControlService>,
    id: String,
    page: String,
    reprocess: bool,
    change: impl FnOnce(&mut Edits, &Manifest, &Page) -> Result<serde_json::Value, ApiError> + Send + 'static,
) -> ApiResult<serde_json::Value> {
    let library = service.library.clone();
    let (doc, result) = blocking(move || {
        let (doc, dir) = document_dir(&library, &id)?;
        let (manifest, page) = applied_page(&dir, &page)?;
        let _lock = crate::tagger::EDITS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut edits = Edits::load(&dir).map_err(|e| ApiError::internal(e.to_string()))?;
        let result = change(&mut edits, &manifest, &page)?;
        // 화면 저장(library.rs save_edits)과 같은 검사. 꼭짓점 범위 같은 것이 틀리면 쓰지 않는다
        let raw = Manifest::load(&dir).map_err(|e| ApiError::internal(e.to_string()))?;
        edits.apply(&raw).validate().map_err(|e| ApiError::bad_request(e.to_string()))?;
        edits.save(&dir).map_err(|e| ApiError::internal(e.to_string()))?;
        Ok((doc, result))
    })
    .await?;
    service.events.edits_changed(doc, reprocess);
    Ok(Json(result))
}

#[derive(Deserialize)]
struct LayoutBody {
    layout: PageLayout,
}

/// 배치를 바꾸면 그 배치에 맞춰 고른 접힌 선과 면 순서는 버린다
async fn set_layout(State(service): Shared, headers: HeaderMap, UrlPath((id, page)): UrlPath<(String, String)>, Json(body): Json<LayoutBody>) -> ApiResult<serde_json::Value> {
    authorize(&service, &headers)?;
    change_page(service, id, page, true, move |edits, _, page| {
        if page.layout != body.layout {
            edits.folds.remove(&page.id);
            edits.exact_folds.retain(|p| *p != page.id);
            edits.panel_order.remove(&page.id);
        }
        edits.layouts.insert(page.id, body.layout);
        Ok(json!({ "layout": body.layout }))
    })
    .await
}

#[derive(Deserialize)]
struct FoldsBody {
    folds: Vec<f32>,
    /// 근처 자국에 붙이지 않고 준 위치 그대로
    #[serde(default)]
    exact: bool,
}

/// 접힌 선 사이가 이보다 좁으면 받지 않는다(면 하나가 폭의 10%도 안 된다)
const MIN_FOLD_GAP: f32 = 0.1;

pub fn check_folds(layout: PageLayout, folds: &[f32]) -> Result<(), String> {
    let expected = match layout {
        PageLayout::Single => return Err("한 장 배치에는 접힌 선이 없다. 먼저 배치를 spread나 trifold로 바꾼다".into()),
        PageLayout::Spread => 1,
        PageLayout::Trifold => 2,
    };
    if folds.len() != expected {
        return Err(format!("이 배치는 접힌 선이 {expected}개다(받은 것 {}개)", folds.len()));
    }
    let mut edges = vec![0.0];
    edges.extend_from_slice(folds);
    edges.push(1.0);
    if folds.iter().any(|f| !f.is_finite()) || edges.windows(2).any(|w| w[1] - w[0] < MIN_FOLD_GAP) {
        return Err("접힌 선은 왼쪽부터 0~1 사이에 두고, 서로와 가장자리에서 0.1 이상 떨어져야 한다".into());
    }
    Ok(())
}

async fn set_folds(State(service): Shared, headers: HeaderMap, UrlPath((id, page)): UrlPath<(String, String)>, Json(body): Json<FoldsBody>) -> ApiResult<serde_json::Value> {
    authorize(&service, &headers)?;
    change_page(service, id, page, true, move |edits, _, page| {
        edits.exact_folds.retain(|p| *p != page.id);
        if body.folds.is_empty() {
            edits.folds.remove(&page.id);
            return Ok(json!({ "folds": [], "exact": false }));
        }
        check_folds(page.layout, &body.folds).map_err(ApiError::bad_request)?;
        edits.folds.insert(page.id, body.folds.clone());
        if body.exact {
            edits.exact_folds.push(page.id);
        }
        Ok(json!({ "folds": body.folds, "exact": body.exact }))
    })
    .await
}

#[derive(Deserialize)]
struct PanelOrderBody {
    order: Vec<u8>,
}

async fn set_panel_order(State(service): Shared, headers: HeaderMap, UrlPath((id, page)): UrlPath<(String, String)>, Json(body): Json<PanelOrderBody>) -> ApiResult<serde_json::Value> {
    authorize(&service, &headers)?;
    change_page(service, id, page, true, move |edits, _, page| {
        if page.layout != PageLayout::Trifold {
            return Err(ApiError::bad_request("면 순서는 3단 접지(trifold)만 바꾼다"));
        }
        let mut sorted = body.order.clone();
        sorted.sort_unstable();
        if sorted != [0, 1, 2] {
            return Err(ApiError::bad_request("order는 0, 1, 2를 한 번씩 쓴 순서다(왼쪽 면이 0). 예: [2, 0, 1]"));
        }
        if body.order == [0, 1, 2] {
            edits.panel_order.remove(&page.id);
        } else {
            edits.panel_order.insert(page.id, body.order.clone());
        }
        Ok(json!({ "order": body.order }))
    })
    .await
}

#[derive(Deserialize)]
struct QuadBody {
    /// null이면 손으로 고친 꼭짓점을 지우고 iPhone, Mac 감지로 돌아간다
    quad: Option<Quad>,
}

/// pipeline/mod.rs IPHONE_PHOTO_ASPECT와 같다. 처리가 사각형으로 보지 않는 꼭짓점은 무시되므로 미리 거른다
const PHOTO_ASPECT: f64 = 4.0 / 3.0;

async fn set_quad(State(service): Shared, headers: HeaderMap, UrlPath((id, page)): UrlPath<(String, String)>, Json(body): Json<QuadBody>) -> ApiResult<serde_json::Value> {
    authorize(&service, &headers)?;
    change_page(service, id, page, true, move |edits, _, page| {
        match body.quad {
            None => {
                edits.quads.remove(&page.id);
            }
            Some(quad) => {
                if !quad.is_plausible(PHOTO_ASPECT) {
                    return Err(ApiError::bad_request(
                        "사각형으로 보기 어려운 꼭짓점이다(마주 보는 변 길이 비 0.8 이상, 모서리 65~115도). topLeft부터 시계 방향인지 본다",
                    ));
                }
                edits.quads.insert(page.id, quad);
            }
        }
        Ok(json!({ "quad": body.quad }))
    })
    .await
}

#[derive(Deserialize)]
struct MoveBody {
    /// 이 원본 페이지 앞으로. null이면 맨 뒤로
    before: Option<Uuid>,
}

async fn move_page(State(service): Shared, headers: HeaderMap, UrlPath((id, page)): UrlPath<(String, String)>, Json(body): Json<MoveBody>) -> ApiResult<serde_json::Value> {
    authorize(&service, &headers)?;
    change_page(service, id, page, false, move |edits, manifest, page| {
        let mut order: Vec<Uuid> = manifest.pages.iter().map(|p| p.id).filter(|p| *p != page.id).collect();
        let at = match body.before {
            Some(before) if before == page.id => return Ok(json!({ "order": manifest.pages.iter().map(|p| p.id).collect::<Vec<_>>() })),
            Some(before) => order
                .iter()
                .position(|p| *p == before)
                .ok_or_else(|| ApiError::not_found(format!("앞에 둘 페이지를 찾지 못했다: {before}")))?,
            None => order.len(),
        };
        order.insert(at, page.id);
        edits.page_order = Some(order.clone());
        Ok(json!({ "order": order }))
    })
    .await
}

async fn remove_page(State(service): Shared, headers: HeaderMap, UrlPath((id, page)): UrlPath<(String, String)>) -> ApiResult<serde_json::Value> {
    authorize(&service, &headers)?;
    change_page(service, id, page, true, move |edits, manifest, page| {
        if manifest.pages.len() <= 1 {
            return Err(ApiError::bad_request("문서의 마지막 원본 페이지는 뺄 수 없다"));
        }
        if !edits.deleted_pages.contains(&page.id) {
            edits.deleted_pages.push(page.id);
        }
        Ok(json!({ "removed": page.id }))
    })
    .await
}

/// 뺀 원본 페이지를 되살린다. 순서 목록에 없으면 원래 촬영 순서 자리로 간다
async fn restore_page(State(service): Shared, headers: HeaderMap, UrlPath((id, page)): UrlPath<(String, String)>) -> ApiResult<serde_json::Value> {
    authorize(&service, &headers)?;
    let library = service.library.clone();
    let (doc, page) = blocking(move || {
        let (doc, dir) = document_dir(&library, &id)?;
        let page = page_uuid(&page)?;
        edit_checked(&dir, |edits| {
            if !edits.deleted_pages.contains(&page) {
                return Err(ApiError::not_found(format!("뺀 페이지가 아니다: {page}")));
            }
            edits.deleted_pages.retain(|p| *p != page);
            Ok(())
        })?;
        Ok((doc, page))
    })
    .await?;
    service.events.edits_changed(doc, true);
    Ok(Json(json!({ "restored": page })))
}

/// edit와 같지만 change가 오류를 내면 쓰지 않는다
fn edit_checked(dir: &Path, change: impl FnOnce(&mut Edits) -> Result<(), ApiError>) -> Result<Edits, ApiError> {
    let _lock = crate::tagger::EDITS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut edits = Edits::load(dir).map_err(|e| ApiError::internal(e.to_string()))?;
    change(&mut edits)?;
    edits.save(dir).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(edits)
}

/// 한 페이지에 손으로 고른 배치, 접힌 선, 면 순서, 꼭짓점, 돌리기, 확인함을 모두 지워 자동 처리로 돌린다
async fn reset_page(State(service): Shared, headers: HeaderMap, UrlPath((id, page)): UrlPath<(String, String)>) -> ApiResult<serde_json::Value> {
    authorize(&service, &headers)?;
    change_page(service, id, page, true, move |edits, _, page| {
        let id = page.id;
        edits.layouts.remove(&id);
        edits.folds.remove(&id);
        edits.exact_folds.retain(|p| *p != id);
        edits.panel_order.remove(&id);
        edits.quads.remove(&id);
        edits.rotations.remove(&id);
        edits.filters.remove(&id);
        edits.reviewed.retain(|p| *p != id);
        Ok(json!({ "reset": id }))
    })
    .await
}

async fn status(State(service): Shared, headers: HeaderMap, UrlPath(id): UrlPath<String>) -> ApiResult<serde_json::Value> {
    authorize(&service, &headers)?;
    let (library, cache) = (service.library.clone(), service.cache.clone());
    let (doc, stale) = blocking(move || {
        let (doc, dir) = document_dir(&library, &id)?;
        let (manifest, _) = load_applied(&dir).map_err(ApiError::internal)?;
        let processing = crate::library::load_processing(&dir);
        Ok((doc, crate::pipeline::stale_pages(&dir, &cache, &manifest, &processing)))
    })
    .await?;
    let job = service.events.job(doc);
    Ok(Json(json!({ "job": job, "stalePages": stale })))
}

/// 그림 위 눈금. 0.1마다 긴 점선, 0.05마다 가장자리에 짧은 눈금(청록). AI가 좌표를 어림하는 기준이다
pub fn draw_grid(image: &mut RgbImage) {
    let (w, h) = (image.width(), image.height());
    let ink = image::Rgb([0, 190, 210]);
    let tick = (w.min(h) / 40).max(6);
    for i in 1..20u32 {
        let major = i % 2 == 0;
        let x = (w as f32 * i as f32 / 20.0) as u32;
        let y = (h as f32 * i as f32 / 20.0) as u32;
        for yy in 0..h {
            if (major && (yy / 6) % 2 == 0) || yy < tick || yy >= h - tick {
                image.put_pixel(x.min(w - 1), yy, ink);
            }
        }
        for xx in 0..w {
            if (major && (xx / 6) % 2 == 0) || xx < tick || xx >= w - tick {
                image.put_pixel(xx, y.min(h - 1), ink);
            }
        }
    }
}

/// 두께 있는 선. 꼭짓점 테두리(자홍)와 고른 접힌 선(빨강)에 쓴다
pub fn draw_line(image: &mut RgbImage, from: (f32, f32), to: (f32, f32), color: image::Rgb<u8>) {
    let (w, h) = (image.width() as i64, image.height() as i64);
    let steps = ((to.0 - from.0).abs().max((to.1 - from.1).abs()).ceil() as usize).max(1);
    let r = (image.width().min(image.height()) / 300).max(2) as i64;
    for s in 0..=steps {
        let t = s as f32 / steps as f32;
        let (cx, cy) = ((from.0 + (to.0 - from.0) * t) as i64, (from.1 + (to.1 - from.1) * t) as i64);
        for dy in -r..=r {
            for dx in -r..=r {
                let (x, y) = (cx + dx, cy + dy);
                if x >= 0 && y >= 0 && x < w && y < h {
                    image.put_pixel(x as u32, y as u32, color);
                }
            }
        }
    }
}

fn jpeg_response(image: &RgbImage, info: serde_json::Value) -> Result<Response, ApiError> {
    let bytes = crate::pipeline::jpeg_bytes(image, 85).map_err(ApiError::internal)?;
    Response::builder()
        .header(header::CONTENT_TYPE, "image/jpeg")
        // 그림과 함께 좌표 정보를 준다. 숫자와 영문 값만이라 머리에 그대로 실린다
        .header("x-scanshelf-view", info.to_string())
        .body(Body::from(bytes))
        .map_err(|e| ApiError::internal(e.to_string()))
}

/// 원본 사진 위에 처리가 쓴 꼭짓점(자홍)과 눈금. 꼭짓점 좌표는 이 그림 기준 0~1이다
async fn raw_image(State(service): Shared, headers: HeaderMap, UrlPath((id, page)): UrlPath<(String, String)>) -> Result<Response, ApiError> {
    authorize(&service, &headers)?;
    let svc = service.clone();
    let (image, info) = blocking(move || {
        let (doc, dir) = document_dir(&svc.library, &id)?;
        let (_, page) = applied_page(&dir, &page)?;
        let mut image = svc.events.raw_view(&dir, &page).map_err(ApiError::internal)?;
        let quad = crate::pipeline::used_quad(&svc.cache, &doc.to_string(), &page);
        draw_grid(&mut image);
        if let Some(q) = quad {
            let (w, h) = (image.width() as f32, image.height() as f32);
            let pts: Vec<(f32, f32)> = q.points().iter().map(|p| (p.x as f32 * w, p.y as f32 * h)).collect();
            for i in 0..4 {
                draw_line(&mut image, pts[i], pts[(i + 1) % 4], image::Rgb([230, 0, 160]));
            }
        }
        let edited = Edits::load(&dir).map(|e| e.quads.contains_key(&page.id)).unwrap_or(false);
        let source = match (quad.is_some(), edited) {
            (false, _) => "none",
            (true, true) => "edited",
            (true, false) => "detected",
        };
        let info = json!({ "width": image.width(), "height": image.height(), "quad": quad, "quadSource": source });
        Ok((image, info))
    })
    .await?;
    jpeg_response(&image, info)
}

/// 원근 보정과 회전 뒤, 나누기 직전 그림 위에 고른 접힌 선(빨강)과 눈금. 접힌 선 좌표는 이 그림 폭 기준 0~1이다
async fn flat_image(State(service): Shared, headers: HeaderMap, UrlPath((id, page)): UrlPath<(String, String)>) -> Result<Response, ApiError> {
    authorize(&service, &headers)?;
    let svc = service.clone();
    let (image, info) = blocking(move || {
        let (doc, dir) = document_dir(&svc.library, &id)?;
        let (_, page) = applied_page(&dir, &page)?;
        let mut image = svc.events.flat_view(&dir, doc, &page).map_err(ApiError::internal)?;
        let hints = Edits::load(&dir).map(|e| e.hints(page.id)).unwrap_or_default();
        draw_grid(&mut image);
        let (w, h) = (image.width() as f32, image.height() as f32);
        for x in &hints.folds {
            draw_line(&mut image, (x * w, 0.0), (x * w, h - 1.0), image::Rgb([230, 30, 30]));
        }
        let info = json!({
            "width": image.width(),
            "height": image.height(),
            "layout": page.layout,
            "folds": hints.folds,
            "exactFolds": hints.exact,
            "panelOrder": hints.panel_order,
            "extraRotation": page.extra_rotation,
        });
        Ok((image, info))
    })
    .await?;
    jpeg_response(&image, info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 접힌_선은_배치와_수가_맞고_떨어져_있어야_한다() {
        assert!(check_folds(PageLayout::Trifold, &[0.33, 0.66]).is_ok());
        assert!(check_folds(PageLayout::Spread, &[0.5]).is_ok());
        assert!(check_folds(PageLayout::Trifold, &[0.5]).is_err());
        assert!(check_folds(PageLayout::Single, &[0.5]).is_err());
        assert!(check_folds(PageLayout::Trifold, &[0.66, 0.33]).is_err(), "왼쪽부터");
        assert!(check_folds(PageLayout::Trifold, &[0.33, 0.38]).is_err(), "너무 붙음");
        assert!(check_folds(PageLayout::Spread, &[0.97]).is_err(), "가장자리");
        assert!(check_folds(PageLayout::Spread, &[f32::NAN]).is_err());
    }

    #[test]
    fn 토큰은_길이까지_같아야_맞다() {
        assert!(constant_time_eq(b"abcd", b"abcd"));
        assert!(!constant_time_eq(b"abcd", b"abce"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }

    #[test]
    fn 접속_정보는_나만_읽는다() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONTROL_FILE);
        let info = ControlInfo { port: 4242, token: "ab".into(), pid: std::process::id() };
        write_info(&path, &info).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let read: ControlInfo = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(read, info);
        remove_info(&path);
        assert!(!path.exists());
    }

    #[test]
    fn 다른_인스턴스가_쓴_접속_정보는_지우지_않는다() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONTROL_FILE);
        write_info(&path, &ControlInfo { port: 1, token: "ab".into(), pid: std::process::id() + 1 }).unwrap();
        remove_info(&path);
        assert!(path.exists());
    }
}
