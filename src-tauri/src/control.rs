//! MCP 서버가 쓰는 로컬 쓰기 통로. 127.0.0.1에만 열고, 앱을 켤 때마다 새로 만든 토큰을 `control.json`에 남긴다.
//!
//! MCP가 edits.json에 직접 쓰지 않는 이유: 화면이 문서의 edits 전체를 들고 있다가 `save_edits`로 통째로 덮으므로,
//! 바깥에서 쓴 칸은 다음 편집 때 사라진다. 앱이 같은 잠금(`tagger::EDITS_LOCK`) 안에서 쓰고 `edits-changed`로
//! 화면을 다시 읽게 해야 겹치지 않는다. 처리 줄 세우기도 앱만 할 수 있다.
//!
//! 할 수 있는 일은 되돌릴 수 있는 것만 둔다: 확인 필요 쪽 목록, 돌리기, 확인함, 태그. 꼭짓점은 받지 않는다.
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
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use scanshelf_core::overrides::Edits;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::library::load_applied;
use crate::pipeline::page_checks;

/// 앱 데이터 폴더(보관함 library/의 부모)에 두는 접속 정보. MCP는 보관함 폴더의 부모에서 찾는다
pub const CONTROL_FILE: &str = "control.json";
/// 화면이 받는 이벤트. 바깥(MCP)이 edits.json을 고친 문서 id. 열린 문서 화면이 다시 읽는다
pub const EVENT_EDITS_CHANGED: &str = "edits-changed";

/// 쓰기가 끝난 뒤 앱에 알리는 일. 앱은 처리 줄과 Tauri 이벤트로, 테스트는 기록으로 받는다
pub trait ControlEvents: Send + Sync {
    /// edits.json이 바뀌었다. reprocess면 처리 줄에 넣는다(돌리기)
    fn edits_changed(&self, document_id: Uuid, reprocess: bool);
    fn tags_changed(&self, document_id: Uuid);
}

pub struct ControlService {
    pub library: PathBuf,
    /// 처리 상태 캐시(확인 필요 표시가 여기 있다)
    pub cache: PathBuf,
    pub token: [u8; 32],
    pub events: Arc<dyn ControlEvents>,
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

#[cfg(test)]
mod tests {
    use super::*;

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
