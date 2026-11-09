//! 처리 파이프라인: 원본(raw/) → 보정한 페이지(pages/) → OCR(ocr/) → processing.json.
//!
//! 페이지마다 입력 지문(원본 파일, 꼭짓점, 배치, 옵션, PIPELINE_VERSION)을 캐시에 남기고,
//! 지문이 같은 페이지는 건너뛴다. 꼭짓점 하나를 고치면 그 페이지만 다시 처리한다.
//!
//! @see docs/pipeline.md

pub mod platform;
pub mod steps;
pub mod worker;
#[cfg(all(test, target_os = "macos"))]
mod e2e_tests;

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use image::RgbImage;
use scanshelf_core::processing::{OutputPage, PIPELINE_VERSION, PagePart, ProcessOptions, Processing};
use scanshelf_core::overrides::{Edits, PageHints};
use scanshelf_core::{Manifest, Page, PageLayout, Quad};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub use worker::{JobState, Pipeline};

use scanshelf_core::fsio::write_atomic;

use crate::library::{load_processing, read_json, write_json};

/// 썸네일 너비. 보관함 표지와 문서 화면 페이지 목록이 같이 쓴다
pub const THUMB_WIDTH: u32 = 360;
const JPEG_QUALITY: u8 = 90;

/// 문서 다수 방향을 믿는 데 필요한 최소 표 수
const DOCUMENT_ROTATION_MIN_VOTES: usize = 2;

pub fn thumb_path(cache: &Path, document_id: &str, output_id: &str) -> PathBuf {
    cache.join("thumbs").join(document_id).join(format!("{output_id}.jpg"))
}

pub fn preview_path(cache: &Path, document_id: &str, page_id: &str) -> PathBuf {
    cache.join("previews").join(document_id).join(format!("{page_id}.jpg"))
}

fn state_path(cache: &Path, document_id: &str) -> PathBuf {
    cache.join("state").join(format!("{document_id}.json"))
}

/// 캐시에 두는 처리 상태. 계약(processing.json)에 입력 지문 칸이 없어 Mac 캐시에 둔다(docs/pipeline.md 참고)
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PipelineState {
    #[serde(default)]
    pages: BTreeMap<Uuid, PageState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageState {
    fingerprint: String,
    #[serde(default)]
    detected_quad: Option<Quad>,
    /// 글자로 확실히 판단한 회전. 글자가 적으면 없음
    #[serde(default)]
    rotation: Option<u16>,
    /// 실제 쓴 자동 회전(문서 다수 방향을 따랐을 수 있다)
    #[serde(default)]
    applied_rotation: Option<u16>,
    /// 처리하며 확인한 것(steps::PageCheck)
    #[serde(default)]
    checks: Vec<steps::PageCheck>,
    /// 나누기 전 그림의 폭/높이(steps::Corrected::flat_aspect). 반쪽짜리 표지를 가를 때 문서 펼침과 비교한다
    #[serde(default)]
    flat_aspect: Option<f32>,
}

/// 앞뒤 표지 자리의 펼침 사진을 한 쪽으로 볼 폭/높이 기준. 같은 문서에서 두 쪽으로 나뉜 펼침 비율의 가운데값에
/// steps::COVER_ASPECT_RATIO를 곱한다. 나뉜 펼침이 둘 미만이면 비교하지 않는다
fn cover_limit(state: &PipelineState, outputs: &HashMap<Uuid, Vec<OutputPage>>, except: Uuid) -> Option<f32> {
    let mut aspects: Vec<f32> = outputs
        .iter()
        .filter(|(id, parts)| **id != except && parts.iter().any(|o| o.part == PagePart::Left))
        .filter_map(|(id, _)| state.pages.get(id)?.flat_aspect)
        .collect();
    if aspects.len() < 2 {
        return None;
    }
    aspects.sort_by(f32::total_cmp);
    Some(aspects[aspects.len() / 2] * steps::COVER_ASPECT_RATIO)
}

/// 페이지마다 확인 결과. 화면이 "확인 필요"를 그린다
pub fn page_checks(cache: &Path, document_id: &str) -> HashMap<Uuid, Vec<steps::PageCheck>> {
    load_state(cache, document_id).pages.into_iter().map(|(id, s)| (id, s.checks)).collect()
}

/// 테두리 바로 옆(왼쪽이나 오른쪽)에 다른 페이지가 보이는지. 펼침인데 한 쪽만 잡힌 장(어두운 표지 옆 흰 면)과
/// 펼침 배치로 찍은 낱장 전단을 가른다. 사진 가장자리 띠의 색을 책상으로 보고, 테두리 옆 띠가 책상과 다른 색이면 페이지가 있다.
/// 2026-11-08: 로보월드_4 낱장 전단 6장이 "한 쪽만 잡힘"으로 잘못 표시됐다
fn page_beside(image: &RgbImage, quad: &Quad) -> bool {
    let small = image::imageops::thumbnail(image, 600, (600 * image.height() / image.width().max(1)).max(1));
    let (w, h) = (small.width() as f64, small.height() as f64);
    let px = |x: f64, y: f64| small.get_pixel((x.clamp(0.0, w - 1.0)) as u32, (y.clamp(0.0, h - 1.0)) as u32).0;
    // 책상: 사진 바깥 3% 띠의 채널별 중앙값
    let mut ring: [Vec<u8>; 3] = Default::default();
    let band = (w.min(h) * 0.03).max(2.0);
    for y in (0..h as u32).step_by(2) {
        for x in (0..w as u32).step_by(2) {
            let (fx, fy) = (x as f64, y as f64);
            if fx < band || fy < band || fx > w - band || fy > h - band {
                let p = px(fx, fy);
                (0..3).for_each(|c| ring[c].push(p[c]));
            }
        }
    }
    let desk: [f64; 3] = std::array::from_fn(|c| {
        let v = &mut ring[c];
        v.sort_unstable();
        v.get(v.len() / 2).copied().unwrap_or(0) as f64
    });
    let left = (quad.top_left.x.min(quad.bottom_left.x)) * w;
    let right = (quad.top_right.x.max(quad.bottom_right.x)) * w;
    let top = quad.top_left.y.max(quad.top_right.y) * h;
    let bottom = quad.bottom_left.y.min(quad.bottom_right.y) * h;
    let width = right - left;
    let (y0, y1) = (top + (bottom - top) * 0.15, bottom - (bottom - top) * 0.15);
    // 띠 하나: 테두리에서 폭의 5%~35% 떨어진 곳. 사진 밖으로 나가면 볼 수 없다
    let strip = |from: f64, to: f64| -> Option<f64> {
        let (a, b) = (from.min(to).max(0.0), from.max(to).min(w - 1.0));
        if b - a < width * 0.1 || y1 <= y0 {
            return None;
        }
        // 줄마다 책상과 다른 색이 절반 넘는지. 페이지는 위아래 전체에 걸치고, 손이나 그림자는 일부 줄에만 걸친다
        let (mut page_rows, mut rows) = (0usize, 0usize);
        let mut y = y0;
        while y < y1 {
            let (mut differ, mut total) = (0usize, 0usize);
            let mut x = a;
            while x < b {
                let p = px(x, y);
                let d: f64 = (0..3).map(|c| (p[c] as f64 - desk[c]).abs()).sum();
                differ += usize::from(d > 60.0);
                total += 1;
                x += 2.0;
            }
            page_rows += usize::from(differ * 2 > total);
            rows += 1;
            y += 2.0;
        }
        Some(page_rows as f64 / rows.max(1) as f64)
    };
    let sides = [strip(left - width * 0.35, left - width * 0.05), strip(right + width * 0.05, right + width * 0.35)];
    sides.into_iter().flatten().any(|f| f > 0.85)
}

/// 이 페이지에 쓸 처리 옵션. 사람이 페이지마다 고른 필터가 문서 설정보다 먼저다
fn page_options(defaults: &ProcessOptions, edits: &Edits, page: Uuid) -> ProcessOptions {
    let mut options = *defaults;
    if let Some(filter) = edits.filters.get(&page) {
        options.filter = *filter;
    }
    options
}

/// 문서의 다수 방향. 거치대에서 찍은 문서는 보통 한 방향이다. 글자가 적어 판단하지 못한 페이지가 따른다.
/// 표가 적거나 방향이 갈리면 정하지 않는다
fn document_rotation(votes: &[u16]) -> Option<u16> {
    let best = [0u16, 90, 180, 270].into_iter().max_by_key(|d| votes.iter().filter(|v| *v == d).count())?;
    let count = votes.iter().filter(|v| **v == best).count();
    (count >= DOCUMENT_ROTATION_MIN_VOTES && count * 10 >= votes.len() * 6).then_some(best)
}

/// 문서의 펼침, 접지 사진 비율이 충분한지. 그때만 한 쪽으로 찍힌 장의 접은 자국을 살핀다
fn mostly_spread(manifest: &Manifest) -> bool {
    let wide = manifest.pages.iter().filter(|p| p.layout != PageLayout::Single).count();
    wide * 3 >= manifest.pages.len() && wide > 0
}

fn load_state(cache: &Path, document_id: &str) -> PipelineState {
    read_json(&state_path(cache, document_id)).ok().flatten().unwrap_or_default()
}

/// 펼침, 3단 접지가 세로/가로가 이보다 크게 남으면 눕혀 본다. 2026-11-08 실제 보관함에서 세로형 표지 20장은 0.69~0.73,
/// 누운 3단 접지(로보월드_6 RBM 바깥 면)는 0.46이었다
const LAY_DOWN_RATIO: f32 = 1.6;

/// AI가 보고 고칠 수 있게 처리 단계 중간 그림을 만든다(control.rs). 긴 변 이 값으로 줄인다
pub const VIEW_SIDE: u32 = 2000;

/// 처리가 실제로 쓴 꼭짓점. Mac이 다시 찾았으면 그 값, 테두리를 못 찾았으면 None(사진 전체)
pub fn used_quad(cache: &Path, document_id: &str, page: &Page) -> Option<Quad> {
    let state = load_state(cache, document_id);
    match state.pages.get(&page.id) {
        Some(s) if s.checks.contains(&steps::PageCheck::QuadMissing) => None,
        Some(s) => s.detected_quad.or(page.quad),
        None => page.quad,
    }
}

/// 원본 사진(회전 메타데이터 적용). 꼭짓점 좌표(0~1)가 이 그림 기준이다
pub fn raw_view(dir: &Path, page: &Page) -> Result<RgbImage, String> {
    platform::decode_oriented(&dir.join(&page.raw), Some(VIEW_SIDE)).map_err(|e| e.to_string())
}

/// 원근 보정과 회전까지 한, 나누기 직전 그림. 접힌 선 좌표(0~1)가 이 그림 기준이다(steps::correct_page와 같은 순서)
pub fn flat_view(dir: &Path, cache: &Path, document_id: &str, page: &Page) -> Result<RgbImage, String> {
    let image = platform::decode_oriented(&dir.join(&page.raw), Some(VIEW_SIDE * 2)).map_err(|e| e.to_string())?;
    let flat = steps::perspective_step(image, used_quad(cache, document_id, page))?;
    let applied = load_state(cache, document_id).pages.get(&page.id).and_then(|s| s.applied_rotation);
    let decided = move |_: &RgbImage| applied;
    let flat = steps::rotate_step(flat, page.layout, &decided, page.extra_rotation);
    let (w, h) = (flat.width(), flat.height());
    let scale = (VIEW_SIDE as f32 / w.max(h) as f32).min(1.0);
    Ok(if scale < 1.0 {
        image::imageops::resize(&flat, (w as f32 * scale).round() as u32, (h as f32 * scale).round() as u32, image::imageops::FilterType::Triangle)
    } else {
        flat
    })
}

/// JPEG 바이트. 화면 대신 MCP로 보낼 때 쓴다
pub fn jpeg_bytes(image: &RgbImage, quality: u8) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, quality)
        .encode_image(image)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// Mac이 다시 감지한 꼭짓점. 화면이 iPhone 감지값이 없는 페이지에 보여 준다
pub fn detected_quads(cache: &Path, document_id: &str) -> HashMap<Uuid, Quad> {
    load_state(cache, document_id)
        .pages
        .into_iter()
        .filter_map(|(id, s)| s.detected_quad.map(|q| (id, q)))
        .collect()
}

/// iPhone 사진의 높이/폭(세로 4:3)
const IPHONE_PHOTO_ASPECT: f64 = 4.0 / 3.0;

/// 처리에 쓸 테두리와, Mac이 새로 감지했으면 그 값.
/// iPhone이 준 테두리가 믿을 만하지 않으면(흰 면, 어두운 표지에서 꼭짓점을 접힌 선에 붙인 삼각형 등) Mac이 원본 해상도에서 다시 찾고,
/// 그것도 믿을 만하지 않으면 테두리 없이 사진 전체를 쓴다. 찢긴 페이지보다 배경이 조금 남은 페이지가 낫다.
/// @see docs/pipeline.md, docs/capture.md
fn choose_quad(
    manifest_quad: Option<Quad>,
    image: &RgbImage,
    detect: impl FnOnce(&RgbImage) -> Option<Quad>,
) -> (Option<Quad>, Option<Quad>) {
    let aspect = image.height() as f64 / image.width().max(1) as f64;
    if let Some(quad) = manifest_quad.filter(|q| q.is_plausible(aspect)) {
        return (Some(quad), None);
    }
    let detected = detect(image).filter(|q| q.is_plausible(aspect));
    (detected, detected)
}

/// 다시 찾을 때 줄여서 한 번 더 보는 크기(긴 변). Vision 문서 영역 감지는 같은 사진도 크기에 따라 결과가 바뀐다
const REDETECT_SIDE: u32 = 2000;

/// Mac이 테두리를 다시 찾는다. 믿을 만한 테두리가 나올 때까지 차례로 시도한다.
/// 1. 원본 해상도에서 문서 영역 감지
/// 2. 2000px로 줄여 문서 영역 감지(밝은 책상 위 흰 펼침에서 원본은 틀리고 줄인 쪽이 맞는 경우가 있었다)
/// 3. 사각형 감지(테두리 직선). 펼침은 한 쪽씩 잡힌 사각형 둘을 합친다
/// 4. 꼭짓점 하나만 틀린 테두리(문서 영역 감지 두 번과 iPhone 값)를 나머지 셋으로 고친다(`repair_one_corner`)
///
/// 펼침인데 세로로 긴 테두리(한 쪽만 잡힘, 왼쪽이 검은 표지일 때 흔하다)는 버린다. 한 쪽을 잃느니 사진 전체가 낫다.
///
/// @see docs/pipeline.md
fn find_page_quad(image: &RgbImage, layout: PageLayout, phone: Option<Quad>) -> Option<Quad> {
    let aspect = image.height() as f64 / image.width().max(1) as f64;
    let fits = |q: &Quad| q.is_plausible(aspect) && (layout != PageLayout::Spread || quad_width_ratio(q, aspect) >= 1.0);
    let full = platform::detect_document_quad(image);
    if let Some(quad) = full.filter(fits) {
        return Some(quad);
    }
    let small = image::imageops::thumbnail(
        image,
        REDETECT_SIDE.min(image.width()),
        (REDETECT_SIDE as f64 * aspect).round().min(image.height() as f64) as u32,
    );
    let reduced = platform::detect_document_quad(&small);
    if let Some(quad) = reduced.filter(fits) {
        return Some(quad);
    }
    // 크기마다 잡히는 변이 달라 두 크기의 후보를 모은다
    let mut rects = platform::detect_rectangle_quads(&small);
    rects.extend(platform::detect_rectangle_quads(image));
    if let Some(quad) = pick_rectangle(&rects, aspect, layout) {
        return Some(quad);
    }
    // 꼭짓점 하나만 접힌 선에 붙은 테두리(Vision과 iPhone이 같은 모양으로 틀린다)를 고쳐 쓴다
    let corners: Vec<scanshelf_core::Point> = rects.iter().flat_map(|q| q.points()).collect();
    [full, reduced, phone].into_iter().flatten().find_map(|q| repair_one_corner(&q, &corners, aspect, &fits))
}

/// 꼭짓점 하나가 틀린 테두리를 고친다. 나머지 셋으로 평행사변형을 완성해 틀린 꼭짓점 자리를 정하고,
/// 그 근처(사진 폭의 4%)에 사각형 감지 꼭짓점이 있으면 그 자리로 맞춘다(원근 때문에 평행사변형은 조금 어긋난다).
/// 고친 결과가 믿을 만한 꼭짓점이 하나뿐일 때만 쓴다. 둘 이상이면 어느 꼭짓점이 틀렸는지 모른다.
/// 사진 넓이의 10%보다 작으면 버린다. 직각만 맞는 가느다란 띠(화면 밖으로 나간 3단 접지)가 나온 적이 있다
fn repair_one_corner(
    quad: &Quad,
    corners: &[scanshelf_core::Point],
    aspect: f64,
    fits: &dyn Fn(&Quad) -> bool,
) -> Option<Quad> {
    const SNAP: f64 = 0.04;
    const MIN_AREA: f64 = 0.1;
    let fits = |q: &Quad| fits(q) && quad_area(q, aspect) / aspect >= MIN_AREA;
    let points = quad.points();
    let mut repaired = (0..4).filter_map(|i| {
        let (prev, next, opposite) = (points[(i + 3) % 4], points[(i + 1) % 4], points[(i + 2) % 4]);
        let guess = scanshelf_core::Point { x: prev.x + next.x - opposite.x, y: prev.y + next.y - opposite.y };
        let distance = |p: &scanshelf_core::Point| (p.x - guess.x).hypot((p.y - guess.y) * aspect);
        let snapped = corners.iter().filter(|p| distance(p) < SNAP).min_by(|a, b| distance(a).total_cmp(&distance(b)));
        let with = |corner: scanshelf_core::Point| {
            let mut p = points;
            p[i] = corner;
            Quad { top_left: p[0], top_right: p[1], bottom_right: p[2], bottom_left: p[3] }
        };
        let in_frame = (0.0..=1.0).contains(&guess.x) && (0.0..=1.0).contains(&guess.y);
        snapped.map(|c| with(*c)).filter(|q| fits(q)).or_else(|| in_frame.then(|| with(guess)).filter(|q| fits(q)))
    });
    let first = repaired.next()?;
    repaired.next().is_none().then_some(first)
}

/// 테두리의 폭/높이(사진 비율을 맞춘 길이)
fn quad_width_ratio(q: &Quad, aspect: f64) -> f64 {
    let len = |a: scanshelf_core::Point, b: scanshelf_core::Point| (a.x - b.x).hypot((a.y - b.y) * aspect);
    let width = (len(q.top_left, q.top_right) + len(q.bottom_left, q.bottom_right)) / 2.0;
    let height = (len(q.top_left, q.bottom_left) + len(q.top_right, q.bottom_right)) / 2.0;
    width / height.max(1e-9)
}

/// 테두리 넓이(사진 폭을 1로 둔 단위)
fn quad_area(q: &Quad, aspect: f64) -> f64 {
    let p = q.points().map(|p| (p.x, p.y * aspect));
    let twice: f64 = (0..4).map(|i| p[i].0 * p[(i + 1) % 4].1 - p[(i + 1) % 4].0 * p[i].1).sum();
    twice.abs() / 2.0
}

/// 사각형 감지 후보에서 페이지 테두리를 고른다. 가장 넓은 믿을 만한 사각형을 쓴다.
/// 펼침은 나란히 붙은 두 쪽(왼쪽 사각형 오른 변이 오른쪽 사각형 왼 변과 겹침)을 합친 펼침 전체도 후보로 넣고,
/// 세로로 긴 후보(한 쪽만)는 버린다
fn pick_rectangle(rects: &[Quad], aspect: f64, layout: PageLayout) -> Option<Quad> {
    /// 두 쪽의 맞닿은 변이 이만큼(사진 폭 대비) 안에 있으면 붙어 있다고 본다
    const SEAM_GAP: f64 = 0.04;
    let mut candidates: Vec<Quad> = rects.iter().copied().filter(|q| q.is_plausible(aspect)).collect();
    if layout == PageLayout::Spread {
        for a in &candidates.clone() {
            for b in &candidates.clone() {
                let touching = (a.top_right.x - b.top_left.x).abs() < SEAM_GAP
                    && (a.bottom_right.x - b.bottom_left.x).abs() < SEAM_GAP
                    && (a.top_right.y - b.top_left.y).abs() < SEAM_GAP
                    && (a.bottom_right.y - b.bottom_left.y).abs() < SEAM_GAP;
                let joined = Quad { top_left: a.top_left, top_right: b.top_right, bottom_right: b.bottom_right, bottom_left: a.bottom_left };
                if touching && joined.is_plausible(aspect) {
                    candidates.push(joined);
                }
            }
        }
        candidates.retain(|q| quad_width_ratio(q, aspect) >= 1.0);
    }
    candidates.into_iter().max_by(|a, b| quad_area(a, aspect).total_cmp(&quad_area(b, aspect)))
}

/// 이 값이 바뀌면 페이지를 다시 처리한다
fn fingerprint(dir: &Path, page: &Page, options: &ProcessOptions, hints: &PageHints) -> String {
    let raw = dir.join(&page.raw);
    let (len, modified) = fs::metadata(&raw)
        .map(|m| {
            let modified = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis());
            (m.len(), modified)
        })
        .unwrap_or((0, 0));
    let mut input = serde_json::json!({
        "pipeline": PIPELINE_VERSION,
        "raw": page.raw,
        "len": len,
        "modified": modified.to_string(),
        "quad": page.quad,
        "layout": page.layout,
        "extraRotation": page.extra_rotation,
        "options": options,
    });
    // 손으로 고른 접힌 선과 면 순서. 없을 때 지문이 그대로여야 기존 페이지를 다시 처리하지 않는다
    if !hints.folds.is_empty() {
        input["folds"] = serde_json::json!(hints.folds);
        if hints.exact {
            input["exactFolds"] = serde_json::json!(true);
        }
    }
    if !hints.panel_order.is_empty() {
        input["panelOrder"] = serde_json::json!(hints.panel_order);
    }
    // 믿지 못할 테두리로 이미 처리한 장(v0.3.1까지)을 다시 처리하게 한다. 정상 장은 지문이 그대로라 다시 하지 않는다.
    // 원본을 열지 않으려고 iPhone 사진 비율(세로 4:3)로 판단한다. 실제 선택은 choose_quad가 원본 비율로 한다
    if page.quad.is_some_and(|q| !q.is_plausible(IPHONE_PHOTO_ASPECT)) {
        input["quadRejected"] = serde_json::Value::Bool(true);
    }
    hex::encode(Sha256::digest(input.to_string().as_bytes()))
}

fn page_is_current(
    dir: &Path,
    page: &Page,
    options: &ProcessOptions,
    hints: &PageHints,
    processing: &Processing,
    state: &PipelineState,
) -> bool {
    let outputs: Vec<&OutputPage> = processing.outputs.iter().filter(|o| o.source_page == page.id).collect();
    if outputs.is_empty() {
        return false;
    }
    let versions_ok = outputs
        .iter()
        .all(|o| o.pipeline_version >= PIPELINE_VERSION && o.options == *options && dir.join(&o.image).exists());
    let fingerprint_ok = state
        .pages
        .get(&page.id)
        .is_some_and(|s| s.fingerprint == fingerprint(dir, page, options, hints));
    versions_ok && fingerprint_ok
}

/// 다시 처리할 페이지 수. 보관함 목록에서 부르므로 이미지는 열지 않고 파일 정보만 본다
pub fn stale_pages(dir: &Path, cache: &Path, manifest: &Manifest, processing: &Processing) -> usize {
    let state = load_state(cache, &manifest.id.to_string());
    let defaults = processing.defaults;
    let edits = scanshelf_core::overrides::Edits::load(dir).unwrap_or_default();
    let current_ids: Vec<Uuid> = manifest.pages.iter().map(|p| p.id).collect();
    let orphan = processing.outputs.iter().any(|o| !current_ids.contains(&o.source_page));
    let stale = manifest
        .pages
        .iter()
        .filter(|page| !page_is_current(dir, page, &page_options(&defaults, &edits, page.id), &edits.hints(page.id), processing, &state))
        .count();
    // 지운 페이지의 출력이 남아 있으면 정리하러 한 번 더 돌아야 한다
    stale + usize::from(orphan)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub document_id: String,
    pub done: usize,
    pub total: usize,
    /// 지금 하는 일: `decode`, `correct`, `ocr`, `done`
    pub stage: &'static str,
}

pub fn write_jpeg(path: &Path, image: &RgbImage, quality: u8) -> Result<(), String> {
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, quality)
        .encode_image(image)
        .map_err(|e| format!("JPEG 인코딩 실패: {e}"))?;
    write_atomic(path, &bytes).map_err(|e| format!("{} 저장 실패: {e}", path.display()))
}

pub struct ProcessContext<'a> {
    pub dir: &'a Path,
    pub cache: &'a Path,
    /// 문서 단위로 processing.json 읽고 쓰기를 묶는다(화면의 옵션 저장과 겹치지 않게)
    pub io_lock: &'a std::sync::Mutex<()>,
    pub force: bool,
    pub models: &'a scanshelf_vision::Models,
    pub on_progress: &'a dyn Fn(Progress),
}

/// 문서 하나를 처리한다. 바뀐 페이지만 다시 하고, 페이지마다 processing.json을 저장해 중간에 꺼져도 한 일은 남는다
pub fn process_document(ctx: &ProcessContext<'_>) -> Result<Processing, String> {
    let dir = ctx.dir;
    let (manifest, edits) = crate::library::load_applied(dir)?;
    let document_id = manifest.id.to_string();
    let defaults = load_processing(dir).defaults;
    let mut state = load_state(ctx.cache, &document_id);
    let total = manifest.pages.len();
    let report = |done: usize, stage: &'static str| {
        (ctx.on_progress)(Progress {
            document_id: document_id.clone(),
            done,
            total,
            stage,
        })
    };

    let mut outputs_by_page: HashMap<Uuid, Vec<OutputPage>> = HashMap::new();
    for output in load_processing(dir).outputs {
        outputs_by_page.entry(output.source_page).or_default().push(output);
    }

    let spread_document = mostly_spread(&manifest);
    let mut redo: Vec<Uuid> = Vec::new();
    // 둘째 바퀴: 앞쪽에서 글자가 없어 방향을 못 정한 페이지를 문서 다수 방향으로 다시 처리한다
    for pass in 0..2 {
        for (index, page) in manifest.pages.iter().enumerate() {
            let options = page_options(&defaults, &edits, page.id);
            let current = {
                let processing = load_processing(dir);
                page_is_current(dir, page, &options, &edits.hints(page.id), &processing, &state)
            };
            let skip = if pass == 0 { current && !ctx.force } else { !redo.contains(&page.id) };
            if skip {
                continue;
            }
            report(index, "decode");
            let raw_path = dir.join(&page.raw);
            let image = platform::decode_oriented(&raw_path, None).map_err(|e| e.to_string())?;
            let (quad, detected_quad) = choose_quad(page.quad, &image, |image| find_page_quad(image, page.layout, page.quad));
            let mut checks = Vec::new();
            if quad.is_none() {
                checks.push(steps::PageCheck::QuadMissing);
            } else if page.quad.is_some() && detected_quad.is_some() {
                checks.push(steps::PageCheck::QuadRedetected);
            }
            report(index, "correct");
            let votes: Vec<u16> = manifest.pages.iter().filter_map(|p| state.pages.get(&p.id)?.rotation).collect();
            let document_turn = document_rotation(&votes);
            let decided = std::cell::Cell::new(None);
            let doubtful = std::cell::Cell::new(false);
            // 펼침, 3단 접지는 가로로 길다. 사람이 회전을 고르지 않았는데 세로로 남게 되면 눕혀서 글자가 확실히 서는 쪽을 고른다.
            // 2026-11-08 로보월드_6 RBM 바깥 면: 방향을 확신하지 못해 세로로 누운 채 남았다
            let lay_down = page.layout != PageLayout::Single && !edits.rotations.contains_key(&page.id);
            let detect = |image: &RgbImage| {
                let reading = crate::ocr::detect_rotation_checked(image);
                decided.set(reading.rotation);
                doubtful.set(reading.doubtful);
                // 글자 방향이 의심스러우면 문서 다수 방향도 따르지 않는다. 그 페이지 글자가 바로 섰다고 읽혔기 때문이다
                let turn = if reading.doubtful { Some(0) } else { reading.rotation.or(document_turn) };
                let (w, h) = (image.width() as f32, image.height() as f32);
                // 세로형 표지(0.7)는 원래 세로다. 펼친 3단 접지가 누운 정도(0.46)로 길쭉할 때만 눕혀 본다
                let tall_after = |t: u16| if t.is_multiple_of(180) { h > w * LAY_DOWN_RATIO } else { w > h * LAY_DOWN_RATIO };
                if lay_down && turn.is_none_or(tall_after) {
                    for t in [90, 270] {
                        let check = crate::ocr::detect_rotation_checked(&crate::ocr::rotate(image, t));
                        if check.rotation == Some(0) && !check.doubtful {
                            decided.set(Some(t));
                            doubtful.set(false);
                            return Some(t);
                        }
                    }
                }
                turn
            };
            let beside = quad.is_some_and(|q| page_beside(&image, &q));
            let cover = index == 0 || index + 1 == manifest.pages.len();
            // 사람이 배치를 고르지 않은 앞뒤 표지 자리 펼침만 반쪽 표지인지 본다
            let single_below = (cover && page.layout == PageLayout::Spread && !edits.layouts.contains_key(&page.id))
                .then(|| cover_limit(&state, &outputs_by_page, page.id))
                .flatten();
            let corrected = steps::correct_page(steps::CorrectInput {
                image,
                quad,
                layout: page.layout,
                options: &options,
                models: ctx.models,
                detect_rotation: &detect,
                extra_rotation: page.extra_rotation,
                // 사람이 배치를 고른 장은 그대로 둔다
                auto_spread: spread_document && !edits.layouts.contains_key(&page.id),
                hints: &edits.hints(page.id),
                single_below,
            })?;
            // 사람이 회전을 고른 장은 방향을 다시 묻지 않는다
            if doubtful.get() && !edits.rotations.contains_key(&page.id) {
                checks.push(steps::PageCheck::RotationDoubtful);
            }
            // 펼침 배치로 찍은 앞뒤 표지는 원래 한 쪽이다. 한 쪽만 잡혔다, 나누지 못했다는 경고를 붙이지 않는다.
            // 3단 접지는 표지 자리여도 세 면이라 나누지 못하면 알린다
            let cover_skip = cover && page.layout == PageLayout::Spread;
            checks.extend(
                corrected
                    .checks
                    .into_iter()
                    .filter(|c| !(cover_skip && matches!(c, steps::PageCheck::QuadOnePage | steps::PageCheck::NotSplit)))
                    // 옆에 다른 페이지가 안 보이면 펼침 배치로 찍은 낱장이다
                    .filter(|c| beside || *c != steps::PageCheck::QuadOnePage),
            );
            let mut page_outputs = Vec::new();
            let mut cut_text = false;
            for ((part, image), edges) in corrected.parts.into_iter().zip(&corrected.cut_edges) {
                let (output, ocr) = write_output(ctx, &document_id, page.id, part, &image, &options, || report(index, "ocr"))?;
                cut_text |= ocr.is_some_and(|ocr| steps::text_at_cut(&ocr, *edges));
                page_outputs.push(output);
            }
            if cut_text {
                checks.push(steps::PageCheck::TextAtCut);
            }
            let old = outputs_by_page.insert(page.id, page_outputs.clone()).unwrap_or_default();
            // 펼침 ↔ 낱장을 바꾸면 출력 id가 달라진다. 남은 옛 파일을 지운다
            for stale in old.iter().filter(|o| !page_outputs.iter().any(|n| n.id == o.id)) {
                remove_output_files(dir, ctx.cache, &document_id, stale);
            }
            state.pages.insert(
                page.id,
                PageState {
                    fingerprint: fingerprint(dir, page, &options, &edits.hints(page.id)),
                    detected_quad,
                    rotation: decided.get(),
                    applied_rotation: if doubtful.get() { Some(0) } else { decided.get().or(document_turn) },
                    checks,
                    flat_aspect: Some(corrected.flat_aspect),
                },
            );
            write_json(&state_path(ctx.cache, &document_id), &state)?;
            save_outputs(ctx, &manifest, &outputs_by_page)?;
        }

        let votes: Vec<u16> = manifest.pages.iter().filter_map(|p| state.pages.get(&p.id)?.rotation).collect();
        redo = match document_rotation(&votes) {
            Some(turn) => manifest
                .pages
                .iter()
                .filter(|p| {
                    state.pages.get(&p.id).is_some_and(|s| {
                        s.rotation.is_none() && s.applied_rotation != Some(turn) && !s.checks.contains(&steps::PageCheck::RotationDoubtful)
                    })
                })
                .map(|p| p.id)
                .collect(),
            None => Vec::new(),
        };
        // 처음 처리할 때는 표지를 다른 펼침보다 먼저 처리해 비교할 값이 없다. 다 처리한 뒤 반쪽짜리 표지를 다시 한다
        let last = manifest.pages.len().saturating_sub(1);
        for (index, page) in manifest.pages.iter().enumerate() {
            let candidate = (index == 0 || index == last) && page.layout == PageLayout::Spread && !edits.layouts.contains_key(&page.id);
            let split = outputs_by_page.get(&page.id).is_some_and(|o| o.iter().any(|o| o.part == PagePart::Left));
            let aspect = state.pages.get(&page.id).and_then(|s| s.flat_aspect);
            let narrow = match (aspect, cover_limit(&state, &outputs_by_page, page.id)) {
                (Some(a), Some(limit)) => a < limit,
                _ => false,
            };
            if candidate && split && narrow && !redo.contains(&page.id) {
                redo.push(page.id);
            }
        }
        if pass == 1 || redo.is_empty() {
            break;
        }
    }

    // 지운 페이지의 출력도 정리한다(원본은 그대로 둔다)
    let live: Vec<Uuid> = manifest.pages.iter().map(|p| p.id).collect();
    let removed: Vec<Uuid> = outputs_by_page.keys().filter(|id| !live.contains(id)).copied().collect();
    for id in removed {
        for output in outputs_by_page.remove(&id).unwrap_or_default() {
            remove_output_files(dir, ctx.cache, &document_id, &output);
        }
        state.pages.remove(&id);
    }
    write_json(&state_path(ctx.cache, &document_id), &state)?;
    let processing = save_outputs(ctx, &manifest, &outputs_by_page)?;
    report(total, "done");
    Ok(processing)
}

fn write_output(
    ctx: &ProcessContext<'_>,
    document_id: &str,
    source: Uuid,
    part: PagePart,
    image: &RgbImage,
    options: &ProcessOptions,
    on_ocr: impl Fn(),
) -> Result<(OutputPage, Option<scanshelf_core::ocr::OcrPage>), String> {
    let id = part.output_id(source);
    let image_rel = format!("pages/{id}.jpg");
    write_jpeg(&ctx.dir.join(&image_rel), image, JPEG_QUALITY)?;
    let thumb_height = (image.height() as f64 * THUMB_WIDTH as f64 / image.width().max(1) as f64).round().max(1.0) as u32;
    let thumb = image::imageops::thumbnail(image, THUMB_WIDTH, thumb_height);
    write_jpeg(&thumb_path(ctx.cache, document_id, &id), &thumb, 80)?;
    on_ocr();
    // 글자 인식이 실패해도 페이지 이미지는 남긴다. ocr이 없으면 화면이 "글자 없음"으로 보여 준다
    let ocr_rel = format!("ocr/{id}.json");
    let (ocr, page) = match crate::ocr::recognize(image) {
        Ok(page) => {
            write_json(&ctx.dir.join(&ocr_rel), &page)?;
            (Some(ocr_rel), Some(page))
        }
        Err(error) => {
            eprintln!("OCR 실패 ({id}): {error}");
            (None, None)
        }
    };
    let output = OutputPage {
        id,
        source_page: source,
        part,
        image: image_rel,
        ocr,
        options: *options,
        pipeline_version: PIPELINE_VERSION,
        processed_at: Utc::now(),
    };
    Ok((output, page))
}

fn remove_output_files(dir: &Path, cache: &Path, document_id: &str, output: &OutputPage) {
    let _ = fs::remove_file(dir.join(&output.image));
    if let Some(ocr) = &output.ocr {
        let _ = fs::remove_file(dir.join(ocr));
    }
    let _ = fs::remove_file(thumb_path(cache, document_id, &output.id));
}

/// 문서 페이지 순서대로 저장한다. 옵션 기본값은 저장 순간의 파일 값을 따른다(처리 중에 화면이 바꿨을 수 있다)
fn save_outputs(
    ctx: &ProcessContext<'_>,
    manifest: &Manifest,
    outputs_by_page: &HashMap<Uuid, Vec<OutputPage>>,
) -> Result<Processing, String> {
    let _guard = ctx.io_lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut processing = load_processing(ctx.dir);
    processing.outputs = manifest
        .pages
        .iter()
        .flat_map(|page| outputs_by_page.get(&page.id).cloned().unwrap_or_default())
        .collect();
    processing.save(ctx.dir).map_err(|e| e.to_string())?;
    Ok(processing)
}

#[cfg(test)]
mod rotation_tests {
    use super::document_rotation;

    #[test]
    fn 문서_다수_방향을_고른다() {
        assert_eq!(document_rotation(&[90, 90, 90, 0]), Some(90));
    }

    #[test]
    fn 표가_적거나_갈리면_정하지_않는다() {
        assert_eq!(document_rotation(&[90]), None);
        assert_eq!(document_rotation(&[90, 90, 180, 180]), None);
        assert_eq!(document_rotation(&[]), None);
    }
}

#[cfg(test)]
mod quad_choice_tests {
    use super::{choose_quad, pick_rectangle, repair_one_corner};
    use scanshelf_core::PageLayout;
    use image::RgbImage;
    use scanshelf_core::{Point, Quad};

    fn quad(points: [(f64, f64); 4]) -> Quad {
        let p = points.map(|(x, y)| Point { x, y });
        Quad { top_left: p[0], top_right: p[1], bottom_right: p[2], bottom_left: p[3] }
    }

    fn good() -> Quad {
        quad([(0.07, 0.25), (0.88, 0.27), (0.87, 0.70), (0.03, 0.68)])
    }

    /// 2026-11-08 로보월드_3 8번: 위쪽 두 꼭짓점이 접힌 선에 겹친 삼각형
    fn triangle() -> Quad {
        quad([(0.444, 0.164), (0.486, 0.164), (0.924, 0.629), (0.028, 0.633)])
    }

    fn photo() -> RgbImage {
        RgbImage::new(300, 400)
    }

    #[test]
    fn 믿을_만한_iphone_테두리는_그대로_쓰고_다시_찾지_않는다() {
        let chosen = choose_quad(Some(good()), &photo(), |_| panic!("다시 찾으면 안 된다"));
        assert_eq!(chosen, (Some(good()), None));
    }

    #[test]
    fn 찌그러진_iphone_테두리는_버리고_mac이_찾은_테두리를_쓴다() {
        assert_eq!(choose_quad(Some(triangle()), &photo(), |_| Some(good())), (Some(good()), Some(good())));
    }

    #[test]
    fn mac이_찾은_테두리도_믿지_못하면_사진_전체를_쓴다() {
        assert_eq!(choose_quad(Some(triangle()), &photo(), |_| Some(triangle())), (None, None));
        assert_eq!(choose_quad(None, &photo(), |_| None), (None, None));
    }

    /// 2026-11-08 로보월드_3 8번(밝은 책상 위 흰 펼침) 사각형 감지 결과. 사진 높이/폭 4:3
    const ASPECT: f64 = 4.0 / 3.0;
    fn spread() -> Quad {
        quad([(0.02, 0.19), (0.94, 0.15), (0.93, 0.64), (0.02, 0.64)])
    }
    fn left_page() -> Quad {
        quad([(0.03, 0.20), (0.48, 0.17), (0.47, 0.62), (0.04, 0.63)])
    }
    fn right_page() -> Quad {
        quad([(0.49, 0.16), (0.92, 0.19), (0.92, 0.62), (0.48, 0.63)])
    }

    #[test]
    fn 사각형_후보에서_가장_넓은_펼침_전체를_고른다() {
        let picked = pick_rectangle(&[right_page(), spread(), left_page()], ASPECT, PageLayout::Spread);
        assert_eq!(picked, Some(spread()));
    }

    #[test]
    fn 펼침_전체가_없으면_나란한_두_쪽을_합친다() {
        let picked = pick_rectangle(&[right_page(), left_page()], ASPECT, PageLayout::Spread).unwrap();
        assert_eq!(picked.top_left, left_page().top_left);
        assert_eq!(picked.top_right, right_page().top_right);
        assert_eq!(picked.bottom_right, right_page().bottom_right);
        assert_eq!(picked.bottom_left, left_page().bottom_left);
    }

    #[test]
    fn 펼침인데_한_쪽만_잡혔으면_쓰지_않는다() {
        assert_eq!(pick_rectangle(&[right_page()], ASPECT, PageLayout::Spread), None);
    }

    #[test]
    fn 한_쪽으로_찍은_장은_세로_사각형도_쓴다() {
        assert_eq!(pick_rectangle(&[right_page()], ASPECT, PageLayout::Single), Some(right_page()));
    }

    /// 2026-11-08 로보월드_9 8번: 왼쪽 위 꼭짓점만 가운데 접힌 선에 붙었다(Vision과 iPhone 모두)
    fn corner_on_gutter() -> Quad {
        quad([(0.49, 0.18), (0.93, 0.18), (0.94, 0.66), (0.03, 0.68)])
    }
    fn spread_fits(q: &Quad) -> bool {
        q.is_plausible(ASPECT) && super::quad_width_ratio(q, ASPECT) >= 1.0
    }

    #[test]
    fn 꼭짓점_하나가_틀린_테두리는_나머지_셋으로_고친다() {
        let fixed = repair_one_corner(&corner_on_gutter(), &[], ASPECT, &spread_fits).unwrap();
        // 평행사변형으로 채운 자리: (0.93 + 0.03 - 0.94, 0.18 + 0.68 - 0.66)
        assert!((fixed.top_left.x - 0.02).abs() < 1e-9 && (fixed.top_left.y - 0.20).abs() < 1e-9);
        assert_eq!(fixed.bottom_right, corner_on_gutter().bottom_right);
    }

    #[test]
    fn 고친_꼭짓점_근처에_사각형_감지_꼭짓점이_있으면_그_자리로_맞춘다() {
        let detected = scanshelf_core::Point { x: 0.02, y: 0.19 };
        let fixed = repair_one_corner(&corner_on_gutter(), &[detected], ASPECT, &spread_fits).unwrap();
        assert_eq!(fixed.top_left, detected);
    }

    /// 2026-11-08 로보월드_6 2번(화면 밖으로 나간 3단 접지) iPhone 값. 오른쪽 아래를 고치면 가느다란 띠가 된다
    #[test]
    fn 고친_테두리가_가느다란_띠면_쓰지_않는다() {
        let cut_off = quad([(0.375, 0.551), (0.847, 0.25), (1.0, 0.996), (0.382, 0.559)]);
        assert_eq!(repair_one_corner(&cut_off, &[], ASPECT, &|q: &Quad| q.is_plausible(ASPECT)), None);
    }

    #[test]
    fn 꼭짓점_둘이_틀리면_고치지_않는다() {
        assert_eq!(repair_one_corner(&triangle(), &[], ASPECT, &spread_fits), None);
    }

    #[test]
    fn 찌그러진_사각형은_고르지_않는다() {
        assert_eq!(pick_rectangle(&[triangle()], ASPECT, PageLayout::Single), None);
    }
}


#[cfg(test)]
mod beside_tests {
    use super::page_beside;
    use image::{Rgb, RgbImage};
    use scanshelf_core::{Point, Quad};

    fn quad(l: f64, t: f64, r: f64, b: f64) -> Quad {
        Quad {
            top_left: Point { x: l, y: t },
            top_right: Point { x: r, y: t },
            bottom_right: Point { x: r, y: b },
            bottom_left: Point { x: l, y: b },
        }
    }

    /// 베이지 책상 위 흰 종이(오른쪽 면). 왼쪽 면은 어두운 표지이거나 없다
    fn desk(dark_left_page: bool, hand: bool) -> RgbImage {
        RgbImage::from_fn(600, 800, |x, y| {
            let right_page = (300..520).contains(&x) && (200..600).contains(&y);
            let left_page = dark_left_page && (80..300).contains(&x) && (200..600).contains(&y);
            let glove = hand && (230..300).contains(&x) && (380..450).contains(&y);
            if right_page || glove {
                Rgb([245, 245, 245])
            } else if left_page {
                Rgb([30, 30, 32])
            } else {
                Rgb([220, 205, 185])
            }
        })
    }

    #[test]
    fn 옆에_어두운_표지가_있으면_한_쪽만_잡힌_펼침이다() {
        assert!(page_beside(&desk(true, false), &quad(0.5, 0.25, 0.867, 0.75)));
    }

    #[test]
    fn 옆이_책상이면_낱장이다_손이_걸쳐_있어도() {
        assert!(!page_beside(&desk(false, false), &quad(0.5, 0.25, 0.867, 0.75)));
        assert!(!page_beside(&desk(false, true), &quad(0.5, 0.25, 0.867, 0.75)));
    }
}
