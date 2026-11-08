//! Wi-Fi 직접 전송 HTTP 서버. iPhone이 원본을 먼저, manifest를 마지막에 보낸다.
//!
//! Tauri와 떼어 두어 통합 테스트가 같은 라우터를 localhost에 띄워 확인한다.
//!
//! @see docs/transfer.md

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::Utc;
use futures_util::StreamExt;
use scanshelf_core::transfer::{MAX_CLOCK_SKEW_SECONDS, sha256_hex, verify};
use scanshelf_core::{Manifest, ManifestError};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use super::devices::{Devices, PairedDevice};
use super::pairing::{PairError, Pairing, random_token};
use super::sessions::UploadSessions;
use crate::store::{ImportMode, Store, StoreError, is_raw_path};

/// 원본 한 장 상한. 4800만 화소 HEIC가 10MB 안팎이고, ProRAW DNG도 100MB를 넘지 않는다
pub const MAX_FILE_BYTES: u64 = 200 * 1024 * 1024;
/// manifest 상한. 페이지 1000장도 수백 KB다
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_DEVICE_NAME_CHARS: usize = 100;

pub const HEADER_DEVICE: &str = "x-scanshelf-device";
pub const HEADER_TIMESTAMP: &str = "x-scanshelf-timestamp";
pub const HEADER_BODY_SHA256: &str = "x-scanshelf-body-sha256";
pub const HEADER_SIGNATURE: &str = "x-scanshelf-signature";

/// 서버가 화면 쪽에 알리는 일. 앱은 Tauri 이벤트로, 테스트는 기록으로 받는다
pub trait TransferEvents: Send + Sync {
    fn library_changed(&self, document_id: Uuid);
    fn pairing_completed(&self, device: &PairedDevice);
}

pub struct TransferService {
    pub store: Arc<Store>,
    pub devices: Arc<Devices>,
    pub pairing: Arc<Pairing>,
    pub mac_name: String,
    pub events: Arc<dyn TransferEvents>,
    /// 받는 중인 문서. 앱 안 업데이트가 전송 도중에 앱을 다시 켜지 않게 busy 등록부에 올린다
    pub uploads: UploadSessions,
}

pub fn router(service: Arc<TransferService>) -> Router {
    Router::new()
        .route("/v1/info", get(info))
        .route("/v1/pair", post(pair))
        .route("/v1/documents/{id}/status", get(status))
        .route("/v1/documents/{id}/files/{*path}", put(put_file))
        .route("/v1/documents/{id}/manifest", put(put_manifest))
        .with_state(service)
}

pub async fn serve(listener: tokio::net::TcpListener, service: Arc<TransferService>) -> io::Result<()> {
    let sweeper = service.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            tick.tick().await;
            sweeper.uploads.sweep(std::time::Instant::now());
        }
    });
    axum::serve(listener, router(service)).await
}

type Shared = State<Arc<TransferService>>;

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    missing: Vec<String>,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> ApiError {
        ApiError {
            status,
            code,
            message: message.into(),
            missing: Vec::new(),
        }
    }

    fn unauthorized(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", message)
    }

    fn bad_request(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    fn internal(message: impl Into<String>) -> ApiError {
        let message = message.into();
        eprintln!("전송 서버 오류: {message}");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
    }

    fn too_large() -> ApiError {
        ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "본문이 너무 크다")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({ "error": self.code, "message": self.message });
        if !self.missing.is_empty() {
            body["missing"] = json!(self.missing);
        }
        (self.status, Json(body)).into_response()
    }
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> ApiError {
        match error {
            StoreError::MissingFiles(missing) => ApiError {
                status: StatusCode::CONFLICT,
                code: "missing_files",
                message: "manifest의 원본이 아직 다 오지 않았다".to_string(),
                missing,
            },
            StoreError::InvalidRawPath(_) | StoreError::Manifest(ManifestError::InvalidPage { .. }) => {
                ApiError::bad_request(error.to_string())
            }
            StoreError::Manifest(ManifestError::UnsupportedVersion { .. }) => {
                ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "unsupported_version", error.to_string())
            }
            other => ApiError::internal(other.to_string()),
        }
    }
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| ApiError::internal(format!("작업 스레드 실패: {e}")))
}

async fn info(State(service): Shared) -> Json<Value> {
    Json(json!({ "name": service.mac_name, "version": env!("CARGO_PKG_VERSION") }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairRequest {
    code: String,
    device_id: String,
    device_name: String,
}

async fn pair(State(service): Shared, body: Bytes) -> Result<Json<Value>, ApiError> {
    let request: PairRequest =
        serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(format!("페어링 요청 형식 오류: {e}")))?;
    let device_id = Uuid::parse_str(&request.device_id)
        .map_err(|_| ApiError::bad_request("deviceId는 UUID여야 한다"))?
        .to_string();
    let device_name: String = request.device_name.trim().chars().take(MAX_DEVICE_NAME_CHARS).collect();

    match service.pairing.attempt(request.code.trim(), Utc::now()) {
        Ok(()) => {}
        Err(PairError::Closed) => {
            return Err(ApiError::new(StatusCode::FORBIDDEN, "pairing_closed", "페어링 창이 열려 있지 않다"));
        }
        Err(PairError::WrongCode) => {
            return Err(ApiError::new(StatusCode::UNAUTHORIZED, "wrong_code", "코드가 맞지 않는다"));
        }
    }

    let token = random_token();
    let devices = service.devices.clone();
    let name = device_name.clone();
    let device = blocking(move || devices.add(&device_id, &name, &token))
        .await?
        .map_err(ApiError::internal)?;
    eprintln!("iPhone 페어링하음: {} ({})", device.device_name, device.device_id);
    service.events.pairing_completed(&device);
    Ok(Json(json!({ "token": hex::encode(token), "macName": service.mac_name })))
}

struct Signed {
    device_id: String,
    body_sha256: String,
}

/// 서명 헤더를 확인한다. 본문 해시는 헤더 값으로 서명을 확인하고, 실제 본문과의 비교는 부른 쪽이 한다
/// (파일은 디스크에 흘려 쓰면서 해시를 구하므로 서명을 먼저 확인해야 무단 업로드를 받지 않는다)
async fn authenticate(service: &Arc<TransferService>, method: &Method, path: &str, headers: &HeaderMap) -> Result<Signed, ApiError> {
    let header = |name: &str| -> Result<String, ApiError> {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim().to_string())
            .ok_or_else(|| ApiError::unauthorized(format!("{name} 헤더가 없다")))
    };
    let device_id = header(HEADER_DEVICE)?;
    let timestamp: i64 = header(HEADER_TIMESTAMP)?
        .parse()
        .map_err(|_| ApiError::unauthorized("시각 형식 오류"))?;
    let body_sha256 = header(HEADER_BODY_SHA256)?;
    let signature = header(HEADER_SIGNATURE)?;

    if (Utc::now().timestamp() - timestamp).abs() > MAX_CLOCK_SKEW_SECONDS {
        return Err(ApiError::unauthorized("시각 차이가 너무 크다"));
    }

    let devices = service.devices.clone();
    let lookup_id = device_id.clone();
    let token = blocking(move || devices.token(&lookup_id))
        .await?
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::unauthorized("페어링한 기기가 아니다"))?;

    if !verify(&token, method.as_str(), path, timestamp, &body_sha256, &signature) {
        return Err(ApiError::unauthorized("서명이 맞지 않는다"));
    }
    Ok(Signed {
        device_id,
        body_sha256: body_sha256.to_ascii_lowercase(),
    })
}

/// busy 등록부에 보일 이름. 문서 이름은 manifest가 와야 알 수 있어 기기 이름과 문서 id를 쓴다
fn upload_label(service: &TransferService, device_id: &str, document_id: Uuid) -> String {
    let device = service
        .devices
        .list()
        .into_iter()
        .find(|d| d.device_id == device_id)
        .map(|d| d.device_name)
        .unwrap_or_else(|| "iPhone".to_string());
    format!("iPhone에서 받는 중: {device} ({document_id})")
}

fn parse_document_id(raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw).map_err(|_| ApiError::bad_request("문서 id는 UUID여야 한다"))
}

async fn status(State(service): Shared, Path(id): Path<String>, request: Request) -> Result<Json<Value>, ApiError> {
    let signed = authenticate(&service, request.method(), request.uri().path(), request.headers()).await?;
    if signed.body_sha256 != sha256_hex(b"") {
        return Err(ApiError::unauthorized("본문 해시가 맞지 않는다"));
    }
    let id = parse_document_id(&id)?;
    let store = service.store.clone();
    let files = blocking(move || store.received_files(id)).await??;
    Ok(Json(json!({ "files": files })))
}

async fn put_file(
    State(service): Shared,
    Path((id, rel)): Path<(String, String)>,
    request: Request,
) -> Result<Json<Value>, ApiError> {
    let signed = authenticate(&service, request.method(), request.uri().path(), request.headers()).await?;
    let id = parse_document_id(&id)?;
    if !is_raw_path(&rel) {
        return Err(ApiError::bad_request("원본은 raw/ 아래 상대 경로로만 받는다"));
    }
    let declared = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|len| len > MAX_FILE_BYTES) {
        return Err(ApiError::too_large());
    }
    let _request = service
        .uploads
        .begin(id, || upload_label(&service, &signed.device_id, id));

    let store = service.store.clone();
    let part = blocking(move || store.new_part_path(id)).await??;
    let received = stream_to_file(request.into_body(), part.clone()).await;
    let actual = match received {
        Ok(hash) => hash,
        Err(error) => {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(error);
        }
    };
    if actual != signed.body_sha256 {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(ApiError::unauthorized("본문 해시가 맞지 않는다"));
    }

    let store = service.store.clone();
    let target = rel.clone();
    let finalize_part = part.clone();
    if let Err(error) = blocking(move || store.finalize_upload(id, &target, &finalize_part)).await? {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(error.into());
    }
    Ok(Json(json!({ "path": rel, "sha256": actual })))
}

/// 본문을 디스크에 흘려 쓰며 SHA-256을 구한다. 사진을 통째로 메모리에 올리지 않는다
async fn stream_to_file(body: Body, part: PathBuf) -> Result<String, ApiError> {
    let mut file = tokio::fs::File::create(&part)
        .await
        .map_err(|e| ApiError::internal(format!("임시 파일을 만들지 못했다: {e}")))?;
    let mut hasher = Sha256::new();
    let mut total: u64 = 0;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ApiError::bad_request(format!("본문을 읽다 끊겼다: {e}")))?;
        total += chunk.len() as u64;
        if total > MAX_FILE_BYTES {
            return Err(ApiError::too_large());
        }
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|e| ApiError::internal(format!("임시 파일 쓰기 실패: {e}")))?;
    }
    // iPhone은 받았다는 응답을 보고 원본을 지울 수 있으므로 디스크에 내린 뒤 답한다
    file.sync_all()
        .await
        .map_err(|e| ApiError::internal(format!("임시 파일 쓰기 실패: {e}")))?;
    Ok(hex::encode(hasher.finalize()))
}

async fn put_manifest(State(service): Shared, Path(id): Path<String>, request: Request) -> Result<Json<Value>, ApiError> {
    let signed = authenticate(&service, request.method(), request.uri().path(), request.headers()).await?;
    let id = parse_document_id(&id)?;
    let body = axum::body::to_bytes(request.into_body(), MAX_MANIFEST_BYTES)
        .await
        .map_err(|_| ApiError::too_large())?;
    if sha256_hex(&body) != signed.body_sha256 {
        return Err(ApiError::unauthorized("본문 해시가 맞지 않는다"));
    }
    let manifest: Manifest =
        serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(format!("manifest 형식 오류: {e}")))?;
    if manifest.id != id {
        return Err(ApiError::bad_request("manifest id가 경로의 문서 id와 다르다"));
    }
    let _request = service
        .uploads
        .begin(id, || upload_label(&service, &signed.device_id, id));

    let store = service.store.clone();
    let outcome = blocking(move || {
        let staging = store.staging_dir(id);
        store.import(&manifest, &staging, ImportMode::Move)
    })
    .await??;
    service.uploads.finish(id);
    eprintln!("문서 받음 (Wi-Fi): {} (새 원본 {}개)", outcome.document_id, outcome.added_raw.len());
    if !outcome.stale {
        // iCloud 보관(archive)은 inbox 감시가 1분 안에 보관함에서 복사한다. 응답을 늦추지 않는다
        crate::archive::record_arrival(&service.store.document_dir(id), crate::archive::Route::Wifi, outcome.added_raw.len());
    }
    service.events.library_changed(outcome.document_id);
    Ok(Json(json!({
        "documentId": outcome.document_id,
        "created": outcome.created,
        "added": outcome.added_raw,
    })))
}
