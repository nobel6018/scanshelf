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

/// 이 값이 바뀌면 페이지를 다시 처리한다
fn fingerprint(dir: &Path, page: &Page, options: &ProcessOptions) -> String {
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
        .is_some_and(|s| s.fingerprint == fingerprint(dir, page, options));
    versions_ok && fingerprint_ok
}

/// 다시 처리할 페이지 수. 보관함 목록에서 부르므로 이미지는 열지 않고 파일 정보만 본다
pub fn stale_pages(dir: &Path, cache: &Path, manifest: &Manifest, processing: &Processing) -> usize {
    let state = load_state(cache, &manifest.id.to_string());
    let options = processing.defaults;
    let current_ids: Vec<Uuid> = manifest.pages.iter().map(|p| p.id).collect();
    let orphan = processing.outputs.iter().any(|o| !current_ids.contains(&o.source_page));
    let stale = manifest
        .pages
        .iter()
        .filter(|page| !page_is_current(dir, page, &options, processing, &state))
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
    let options = load_processing(dir).defaults;
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
            let current = {
                let processing = load_processing(dir);
                page_is_current(dir, page, &options, &processing, &state)
            };
            let skip = if pass == 0 { current && !ctx.force } else { !redo.contains(&page.id) };
            if skip {
                continue;
            }
            report(index, "decode");
            let raw_path = dir.join(&page.raw);
            let image = platform::decode_oriented(&raw_path, None).map_err(|e| e.to_string())?;
            let (quad, detected_quad) = choose_quad(page.quad, &image, platform::detect_document_quad);
            report(index, "correct");
            let votes: Vec<u16> = manifest.pages.iter().filter_map(|p| state.pages.get(&p.id)?.rotation).collect();
            let document_turn = document_rotation(&votes);
            let decided = std::cell::Cell::new(None);
            let detect = |image: &RgbImage| {
                let found = crate::ocr::detect_rotation(image);
                decided.set(found);
                found.or(document_turn)
            };
            let parts = steps::correct_page(steps::CorrectInput {
                image,
                quad,
                layout: page.layout,
                options: &options,
                models: ctx.models,
                detect_rotation: &detect,
                extra_rotation: page.extra_rotation,
                // 사람이 배치를 고른 장은 그대로 둔다
                auto_spread: spread_document && !edits.layouts.contains_key(&page.id),
            })?;
            let mut page_outputs = Vec::new();
            for (part, image) in parts {
                page_outputs.push(write_output(ctx, &document_id, page.id, part, &image, &options, || {
                    report(index, "ocr")
                })?);
            }
            let old = outputs_by_page.insert(page.id, page_outputs.clone()).unwrap_or_default();
            // 펼침 ↔ 낱장을 바꾸면 출력 id가 달라진다. 남은 옛 파일을 지운다
            for stale in old.iter().filter(|o| !page_outputs.iter().any(|n| n.id == o.id)) {
                remove_output_files(dir, ctx.cache, &document_id, stale);
            }
            state.pages.insert(
                page.id,
                PageState {
                    fingerprint: fingerprint(dir, page, &options),
                    detected_quad,
                    rotation: decided.get(),
                    applied_rotation: decided.get().or(document_turn),
                },
            );
            write_json(&state_path(ctx.cache, &document_id), &state)?;
            save_outputs(ctx, &manifest, &outputs_by_page)?;
        }

        let votes: Vec<u16> = manifest.pages.iter().filter_map(|p| state.pages.get(&p.id)?.rotation).collect();
        let Some(turn) = document_rotation(&votes) else {
            break;
        };
        redo = manifest
            .pages
            .iter()
            .filter(|p| state.pages.get(&p.id).is_some_and(|s| s.rotation.is_none() && s.applied_rotation != Some(turn)))
            .map(|p| p.id)
            .collect();
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
) -> Result<OutputPage, String> {
    let id = part.output_id(source);
    let image_rel = format!("pages/{id}.jpg");
    write_jpeg(&ctx.dir.join(&image_rel), image, JPEG_QUALITY)?;
    let thumb_height = (image.height() as f64 * THUMB_WIDTH as f64 / image.width().max(1) as f64).round().max(1.0) as u32;
    let thumb = image::imageops::thumbnail(image, THUMB_WIDTH, thumb_height);
    write_jpeg(&thumb_path(ctx.cache, document_id, &id), &thumb, 80)?;
    on_ocr();
    // 글자 인식이 실패해도 페이지 이미지는 남긴다. ocr이 없으면 화면이 "글자 없음"으로 보여 준다
    let ocr_rel = format!("ocr/{id}.json");
    let ocr = match crate::ocr::recognize(image) {
        Ok(page) => {
            write_json(&ctx.dir.join(&ocr_rel), &page)?;
            Some(ocr_rel)
        }
        Err(error) => {
            eprintln!("OCR 실패 ({id}): {error}");
            None
        }
    };
    Ok(OutputPage {
        id,
        source_page: source,
        part,
        image: image_rel,
        ocr,
        options: *options,
        pipeline_version: PIPELINE_VERSION,
        processed_at: Utc::now(),
    })
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
    use super::choose_quad;
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
}
