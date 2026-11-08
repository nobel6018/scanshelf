//! MCP 쓰기 통로(control.rs)를 localhost에 띄우고 토큰, 확인 필요 목록, 돌리기, 확인함, 태그를 확인한다.
//! manifest는 fixtures/manifest-v1.json만 쓰고, 처리 상태 캐시는 합성한다.
//!
//! @see docs/mcp.md

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use reqwest::StatusCode;
use scanshelf_core::overrides::Edits;
use image::RgbImage;
use scanshelf_core::Page;
use scanshelf_lib::control::{ControlHost, ControlService, Job, serve};
use serde_json::{Value, json};
use uuid::Uuid;

const DOC: &str = "7d1c2a4e-0f3b-4a8e-9c61-2b5d8e3f1a90";
const PAGE1: &str = "1e9f4b7a-3c2d-4e5f-8a6b-7c8d9e0f1a2b";
const PAGE2: &str = "5a6b7c8d-9e0f-4a1b-8c2d-3e4f5a6b7c8d";
const TOKEN: [u8; 32] = [7; 32];

#[derive(Default)]
struct Recorder {
    edits: Mutex<Vec<(Uuid, bool)>>,
    tags: Mutex<Vec<Uuid>>,
}

impl ControlHost for Recorder {
    fn edits_changed(&self, document_id: Uuid, reprocess: bool) {
        self.edits.lock().unwrap().push((document_id, reprocess));
    }
    fn tags_changed(&self, document_id: Uuid) {
        self.tags.lock().unwrap().push(document_id);
    }
    fn job(&self, _: Uuid) -> Option<Job> {
        Some(Job::Running { done: 1, total: 2 })
    }
    /// 합성 회색 그림. 실제 사진은 쓰지 않는다
    fn raw_view(&self, _: &Path, _: &Page) -> Result<RgbImage, String> {
        Ok(RgbImage::from_pixel(300, 400, image::Rgb([128; 3])))
    }
    fn flat_view(&self, _: &Path, _: Uuid, _: &Page) -> Result<RgbImage, String> {
        Ok(RgbImage::from_pixel(600, 300, image::Rgb([128; 3])))
    }
}

struct Harness {
    base: String,
    events: Arc<Recorder>,
    data: tempfile::TempDir,
    client: reqwest::Client,
}

impl Harness {
    fn doc_dir(&self) -> std::path::PathBuf {
        self.data.path().join("library").join(DOC)
    }

    fn edits(&self) -> Edits {
        Edits::load(&self.doc_dir()).unwrap()
    }

    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.base))
            .header("authorization", format!("Bearer {}", hex::encode(TOKEN)));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        (status, response.json().await.unwrap_or(Value::Null))
    }
}

async fn start() -> Harness {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let data = tempfile::tempdir().unwrap();
    let doc = data.path().join("library").join(DOC);
    fs::create_dir_all(&doc).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/manifest-v1.json");
    fs::copy(fixture, doc.join("manifest.json")).unwrap();
    // 처리 상태 캐시: 1쪽은 사람이 볼 확인(방향 애매), 2쪽은 기록만 하는 확인(이음매에서 나눔)
    let state = data.path().join("cache/state");
    fs::create_dir_all(&state).unwrap();
    let pages = json!({
        PAGE1: { "fingerprint": "a", "checks": ["rotationDoubtful", "quadRedetected"] },
        PAGE2: { "fingerprint": "b", "checks": ["splitAtSeam"] },
    });
    fs::write(state.join(format!("{DOC}.json")), json!({ "pages": pages }).to_string()).unwrap();

    let events = Arc::new(Recorder::default());
    let service = Arc::new(ControlService {
        library: data.path().join("library"),
        cache: data.path().join("cache"),
        token: TOKEN,
        events: events.clone(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(serve(listener, service));
    Harness { base, events, data, client: reqwest::Client::new() }
}

#[tokio::test]
async fn 토큰이_없거나_틀리면_거부한다() {
    let h = start().await;
    let none = h.client.get(format!("{}/v1/review", h.base)).send().await.unwrap();
    assert_eq!(none.status(), StatusCode::UNAUTHORIZED);
    let wrong = h
        .client
        .get(format!("{}/v1/review", h.base))
        .header("authorization", format!("Bearer {}", hex::encode([8u8; 32])))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn 브라우저_요청은_토큰이_맞아도_거부한다() {
    let h = start().await;
    let response = h
        .client
        .get(format!("{}/v1/review", h.base))
        .header("authorization", format!("Bearer {}", hex::encode(TOKEN)))
        .header("origin", "https://example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn 확인_필요_목록은_사람이_볼_확인만_보여_준다() {
    let h = start().await;
    let (status, body) = h.call(reqwest::Method::GET, "/v1/review", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "documents": [{ "documentId": DOC, "pages": [{ "pageId": PAGE1, "checks": ["rotationDoubtful"] }] }] })
    );
}

#[tokio::test]
async fn 확인함을_누른_쪽은_목록에서_빠지고_취소하면_돌아온다() {
    let h = start().await;
    let path = format!("/v1/documents/{DOC}/pages/{PAGE1}/reviewed");
    let (status, _) = h.call(reqwest::Method::PUT, &path, Some(json!({ "reviewed": true }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h.edits().reviewed, vec![Uuid::parse_str(PAGE1).unwrap()]);
    let (_, body) = h.call(reqwest::Method::GET, "/v1/review", None).await;
    assert_eq!(body, json!({ "documents": [] }));
    // 두 번 눌러도 한 번만 남는다
    h.call(reqwest::Method::PUT, &path, Some(json!({ "reviewed": true }))).await;
    assert_eq!(h.edits().reviewed.len(), 1);
    h.call(reqwest::Method::PUT, &path, Some(json!({ "reviewed": false }))).await;
    assert!(h.edits().reviewed.is_empty());
    // 확인함은 다시 처리하지 않는다
    assert!(h.events.edits.lock().unwrap().iter().all(|(_, reprocess)| !reprocess));
}

#[tokio::test]
async fn 돌리기는_더해지고_한_바퀴면_지운다() {
    let h = start().await;
    let path = format!("/v1/documents/{DOC}/pages/{PAGE2}/rotate");
    let page = Uuid::parse_str(PAGE2).unwrap();
    let (status, body) = h.call(reqwest::Method::POST, &path, Some(json!({ "degrees": 270 }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["rotation"], 270);
    let (_, body) = h.call(reqwest::Method::POST, &path, Some(json!({ "degrees": 180 }))).await;
    assert_eq!(body["rotation"], 90);
    assert_eq!(h.edits().rotations.get(&page), Some(&90));
    h.call(reqwest::Method::POST, &path, Some(json!({ "degrees": 270 }))).await;
    assert!(h.edits().rotations.is_empty());
    let doc = Uuid::parse_str(DOC).unwrap();
    assert_eq!(*h.events.edits.lock().unwrap(), vec![(doc, true), (doc, true), (doc, true)]);
}

#[tokio::test]
async fn 돌리기는_90도_단위만_받는다() {
    let h = start().await;
    let path = format!("/v1/documents/{DOC}/pages/{PAGE2}/rotate");
    let (status, _) = h.call(reqwest::Method::POST, &path, Some(json!({ "degrees": 45 }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(h.edits().rotations.is_empty());
}

#[tokio::test]
async fn 지운_쪽이나_없는_문서는_고치지_않는다() {
    let h = start().await;
    let mut edits = Edits::default();
    edits.deleted_pages.push(Uuid::parse_str(PAGE2).unwrap());
    edits.save(&h.doc_dir()).unwrap();
    let (status, _) = h
        .call(reqwest::Method::POST, &format!("/v1/documents/{DOC}/pages/{PAGE2}/rotate"), Some(json!({ "degrees": 90 })))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let other = Uuid::new_v4();
    let (status, _) = h
        .call(reqwest::Method::PUT, &format!("/v1/documents/{other}/pages/{PAGE1}/reviewed"), Some(json!({ "reviewed": true })))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = h
        .call(reqwest::Method::PATCH, "/v1/documents/..%2F..%2Fetc/tags", Some(json!({ "add": ["a"] })))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn 태그를_더하고_빼면_사람이_고친_태그로_남는다() {
    let h = start().await;
    let edits = Edits { tags: vec!["분야/냉각".into(), "박람회".into()], ..Edits::default() };
    edits.save(&h.doc_dir()).unwrap();
    let (status, body) = h
        .call(
            reqwest::Method::PATCH,
            &format!("/v1/documents/{DOC}/tags"),
            Some(json!({ "add": ["#분야/전력 ", "분야/냉각"], "remove": ["박람회"] })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tags"], json!(["분야/냉각", "분야/전력"]));
    let saved = h.edits();
    assert_eq!(saved.tags, vec!["분야/냉각", "분야/전력"]);
    assert!(saved.tags_by_user);
    assert_eq!(*h.events.tags.lock().unwrap(), vec![Uuid::parse_str(DOC).unwrap()]);
}

async fn put(h: &Harness, path: &str, body: Value) -> (StatusCode, Value) {
    h.call(reqwest::Method::PUT, &format!("/v1/documents/{DOC}/pages/{path}"), Some(body)).await
}

async fn post(h: &Harness, path: &str, body: Value) -> (StatusCode, Value) {
    h.call(reqwest::Method::POST, &format!("/v1/documents/{DOC}/pages/{path}"), Some(body)).await
}

fn uuid(s: &str) -> Uuid {
    Uuid::parse_str(s).unwrap()
}

#[tokio::test]
async fn 배치를_바꾸면_그_배치용_접힌_선과_면_순서를_버린다() {
    let h = start().await;
    let page = uuid(PAGE1);
    assert_eq!(put(&h, &format!("{PAGE1}/layout"), json!({ "layout": "trifold" })).await.0, StatusCode::OK);
    assert_eq!(put(&h, &format!("{PAGE1}/folds"), json!({ "folds": [0.33, 0.67] })).await.0, StatusCode::OK);
    assert_eq!(put(&h, &format!("{PAGE1}/panel-order"), json!({ "order": [2, 0, 1] })).await.0, StatusCode::OK);
    let edits = h.edits();
    assert_eq!(edits.folds.get(&page), Some(&vec![0.33, 0.67]));
    assert_eq!(edits.panel_order.get(&page), Some(&vec![2, 0, 1]));
    put(&h, &format!("{PAGE1}/layout"), json!({ "layout": "spread" })).await;
    let edits = h.edits();
    assert!(edits.folds.is_empty() && edits.panel_order.is_empty());
    assert_eq!(edits.layouts.get(&page), Some(&scanshelf_core::PageLayout::Spread));
    assert!(h.events.edits.lock().unwrap().iter().all(|(_, reprocess)| *reprocess));
}

#[tokio::test]
async fn 배치와_맞지_않는_접힌_선이나_면_순서는_받지_않는다() {
    let h = start().await;
    // fixtures 1쪽은 한 장(single)이다
    assert_eq!(put(&h, &format!("{PAGE1}/folds"), json!({ "folds": [0.5] })).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(put(&h, &format!("{PAGE1}/panel-order"), json!({ "order": [2, 0, 1] })).await.0, StatusCode::BAD_REQUEST);
    put(&h, &format!("{PAGE1}/layout"), json!({ "layout": "trifold" })).await;
    assert_eq!(put(&h, &format!("{PAGE1}/folds"), json!({ "folds": [0.5] })).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(put(&h, &format!("{PAGE1}/panel-order"), json!({ "order": [0, 0, 1] })).await.0, StatusCode::BAD_REQUEST);
    // 제자리 순서와 빈 선은 지운다
    put(&h, &format!("{PAGE1}/folds"), json!({ "folds": [0.3, 0.6] })).await;
    put(&h, &format!("{PAGE1}/folds"), json!({ "folds": [] })).await;
    put(&h, &format!("{PAGE1}/panel-order"), json!({ "order": [0, 1, 2] })).await;
    let edits = h.edits();
    assert!(edits.folds.is_empty() && edits.panel_order.is_empty());
}

#[tokio::test]
async fn 꼭짓점은_사각형일_때만_받고_null이면_지운다() {
    let h = start().await;
    let quad = json!({ "topLeft": { "x": 0.1, "y": 0.1 }, "topRight": { "x": 0.9, "y": 0.1 }, "bottomRight": { "x": 0.9, "y": 0.9 }, "bottomLeft": { "x": 0.1, "y": 0.9 } });
    assert_eq!(put(&h, &format!("{PAGE1}/quad"), json!({ "quad": quad })).await.0, StatusCode::OK);
    assert!(h.edits().quads.contains_key(&uuid(PAGE1)));
    let twisted = json!({ "topLeft": { "x": 0.9, "y": 0.1 }, "topRight": { "x": 0.1, "y": 0.1 }, "bottomRight": { "x": 0.9, "y": 0.9 }, "bottomLeft": { "x": 0.1, "y": 0.9 } });
    assert_eq!(put(&h, &format!("{PAGE1}/quad"), json!({ "quad": twisted })).await.0, StatusCode::BAD_REQUEST);
    let outside = json!({ "topLeft": { "x": -0.1, "y": 0.1 }, "topRight": { "x": 0.9, "y": 0.1 }, "bottomRight": { "x": 0.9, "y": 0.9 }, "bottomLeft": { "x": -0.1, "y": 0.9 } });
    assert_eq!(put(&h, &format!("{PAGE1}/quad"), json!({ "quad": outside })).await.0, StatusCode::BAD_REQUEST);
    put(&h, &format!("{PAGE1}/quad"), json!({ "quad": null })).await;
    assert!(h.edits().quads.is_empty());
}

#[tokio::test]
async fn 원본_페이지를_옮기고_빼고_되살린다() {
    let h = start().await;
    let (p1, p2) = (uuid(PAGE1), uuid(PAGE2));
    let (status, body) = post(&h, &format!("{PAGE2}/move"), json!({ "before": PAGE1 })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["order"], json!([PAGE2, PAGE1]));
    assert_eq!(h.edits().page_order, Some(vec![p2, p1]));
    post(&h, &format!("{PAGE2}/move"), json!({ "before": null })).await;
    assert_eq!(h.edits().page_order, Some(vec![p1, p2]));
    assert_eq!(post(&h, &format!("{PAGE2}/remove"), json!({})).await.0, StatusCode::OK);
    assert_eq!(h.edits().deleted_pages, vec![p2]);
    // 마지막 한 장은 빼지 않는다
    assert_eq!(post(&h, &format!("{PAGE1}/remove"), json!({})).await.0, StatusCode::BAD_REQUEST);
    // 뺀 페이지는 옮기거나 고치지 못하고 되살릴 수만 있다
    assert_eq!(post(&h, &format!("{PAGE2}/move"), json!({ "before": null })).await.0, StatusCode::NOT_FOUND);
    assert_eq!(post(&h, &format!("{PAGE2}/restore"), json!({})).await.0, StatusCode::OK);
    assert!(h.edits().deleted_pages.is_empty());
    assert_eq!(post(&h, &format!("{PAGE2}/restore"), json!({})).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn 되돌리기는_그_페이지의_손_지정만_지운다() {
    let h = start().await;
    for page in [PAGE1, PAGE2] {
        put(&h, &format!("{page}/layout"), json!({ "layout": "trifold" })).await;
        put(&h, &format!("{page}/folds"), json!({ "folds": [0.3, 0.6] })).await;
        post(&h, &format!("{page}/rotate"), json!({ "degrees": 90 })).await;
        put(&h, &format!("{page}/reviewed"), json!({ "reviewed": true })).await;
    }
    assert_eq!(post(&h, &format!("{PAGE1}/reset"), json!({})).await.0, StatusCode::OK);
    let edits = h.edits();
    let (p1, p2) = (uuid(PAGE1), uuid(PAGE2));
    assert!(!edits.layouts.contains_key(&p1) && !edits.folds.contains_key(&p1) && !edits.rotations.contains_key(&p1));
    assert!(!edits.reviewed.contains(&p1));
    assert!(edits.layouts.contains_key(&p2) && edits.folds.contains_key(&p2) && edits.rotations.contains_key(&p2));
    assert!(edits.reviewed.contains(&p2));
}

#[tokio::test]
async fn 처리_상태를_알려_준다() {
    let h = start().await;
    let (status, body) = h.call(reqwest::Method::GET, &format!("/v1/documents/{DOC}/status"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["job"], json!({ "state": "running", "done": 1, "total": 2 }));
    // 처리 결과가 없어 원본 두 장 모두 다시 처리할 대상이다
    assert_eq!(body["stalePages"], 2);
}

#[tokio::test]
async fn 중간_그림은_눈금과_좌표_정보를_함께_준다() {
    let h = start().await;
    put(&h, &format!("{PAGE1}/layout"), json!({ "layout": "spread" })).await;
    put(&h, &format!("{PAGE1}/folds"), json!({ "folds": [0.5] })).await;
    for (kind, size) in [("raw", (300, 400)), ("flat", (600, 300))] {
        let response = h
            .client
            .get(format!("{}/v1/documents/{DOC}/pages/{PAGE1}/{kind}", h.base))
            .header("authorization", format!("Bearer {}", hex::encode(TOKEN)))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{kind}");
        assert_eq!(response.headers()["content-type"], "image/jpeg");
        let info: Value = serde_json::from_str(response.headers()["x-scanshelf-view"].to_str().unwrap()).unwrap();
        assert_eq!((info["width"].as_u64().unwrap(), info["height"].as_u64().unwrap()), (size.0, size.1));
        let image = image::load_from_memory(&response.bytes().await.unwrap()).unwrap().to_rgb8();
        assert_eq!(image.dimensions(), (size.0 as u32, size.1 as u32));
        if kind == "flat" {
            assert_eq!(info["folds"], json!([0.5]));
            assert_eq!(info["layout"], "spread");
            // 고른 접힌 선(빨강)이 가운데에 그려졌다
            let p = image.get_pixel(300, 150);
            assert!(p[0] > 180 && p[1] < 90, "{p:?}");
        } else {
            assert_eq!(info["quadSource"], "detected");
        }
    }
}
