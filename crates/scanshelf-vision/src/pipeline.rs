//! 원근 보정이 끝난 사진 한 장을 출력 페이지(한 장, 두 장, 3단 접지는 세 장)로 만든다.
//!
//! 순서와 이유
//! 1. 손가락 지우기: 손은 사진 테두리에서 들어오므로 나누기 전 원본에서 찾는다. 플랫폼 손 마스크도 원본 좌표다
//! 2. 두 페이지 분리(`PageLayout::Spread`), 세 면 분리(`PageLayout::Trifold`). 신뢰도가 낮으면 통째 한 장으로 둔다
//! 3. 곡면 보정: 페이지마다. 접힌 선 그림자가 한쪽 끝에 있어야 모델이 페이지 경계를 잘 잡는다
//! 4. 필기 지우기: 펴진 뒤라 획이 덜 휘어 있고, 조명 보정 전이라 원래 색이 남아 있다
//! 5. 조명 보정과 필터: 마지막
//!
//! @see docs/vision.md

use std::time::Instant;

use image::{GrayImage, RgbImage};
use scanshelf_core::PageLayout;
use scanshelf_core::processing::{PagePart, ProcessOptions};

use crate::VisionError;
use crate::dewarp::dewarp;
use crate::filter::apply_filter;
use crate::fingers::{FingerReport, remove_fingers};
use crate::handwriting::{HandwritingReport, remove_handwriting};
use crate::models::Models;
use crate::fold::{Folds, Trifold, split_trifold};
use crate::split::{Gutter, Split, split_spread};

#[derive(Debug, Clone, Default)]
pub struct PageReport {
    pub dewarp_applied: bool,
    pub dewarp_passes: u8,
    pub handwriting: Option<HandwritingReport>,
    /// 단계별 걸린 시간(밀리초)
    pub timings: Vec<(&'static str, u128)>,
}

#[derive(Debug, Clone)]
pub struct ProcessedPage {
    pub part: PagePart,
    pub image: RgbImage,
    pub report: PageReport,
}

#[derive(Debug, Clone)]
pub struct SourceReport {
    pub fingers: Option<FingerReport>,
    /// Spread로 들어온 사진의 접힌 선. 한 페이지로 떨어졌으면 confidence가 기준 미만
    pub gutter: Option<Gutter>,
    /// Trifold로 들어온 사진의 접힌 선 두 개. 통째로 떨어졌으면 confidence가 기준 미만
    pub folds: Option<Folds>,
    pub timings: Vec<(&'static str, u128)>,
}

fn timed<T>(timings: &mut Vec<(&'static str, u128)>, name: &'static str, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let out = f();
    timings.push((name, start.elapsed().as_millis()));
    out
}

/// 원본 사진 하나(원근 보정 뒤)를 출력 페이지로. finger_mask는 플랫폼 쪽 손 마스크(원본 좌표)
pub fn process_source(
    img: &RgbImage,
    layout: PageLayout,
    options: &ProcessOptions,
    finger_mask: Option<&GrayImage>,
    models: &Models,
) -> Result<(Vec<ProcessedPage>, SourceReport), VisionError> {
    let mut timings = Vec::new();
    let mut fingers = None;
    let cleaned;
    let source = if options.remove_fingers {
        let (out, report) = timed(&mut timings, "fingers", || remove_fingers(img, finger_mask, models))?;
        fingers = Some(report);
        cleaned = out;
        &cleaned
    } else {
        img
    };

    let mut gutter = None;
    let mut folds = None;
    let parts: Vec<(PagePart, RgbImage)> = match layout {
        PageLayout::Single => vec![(PagePart::Whole, source.clone())],
        PageLayout::Trifold => match timed(&mut timings, "split", || split_trifold(source)) {
            Trifold::Panels { panels, folds: f } => {
                folds = Some(f);
                (0u8..).zip(panels).map(|(index, panel)| (PagePart::Panel(index), panel)).collect()
            }
            Trifold::Whole { folds: f } => {
                folds = Some(f);
                vec![(PagePart::Whole, source.clone())]
            }
        },
        PageLayout::Spread => match timed(&mut timings, "split", || split_spread(source)) {
            Split::Spread { left, right, gutter: g } => {
                gutter = Some(g);
                vec![(PagePart::Left, left), (PagePart::Right, right)]
            }
            Split::Single { gutter: g } => {
                gutter = Some(g);
                vec![(PagePart::Whole, source.clone())]
            }
        },
    };

    let mut pages = Vec::with_capacity(parts.len());
    for (part, page) in parts {
        let (image, report) = process_page(&page, options, models)?;
        pages.push(ProcessedPage { part, image, report });
    }
    Ok((pages, SourceReport { fingers, gutter, folds, timings }))
}

/// 나눠진 페이지 하나에 곡면 보정, 필기 지우기, 필터를 건다(손가락 지우기는 `process_source`에서)
pub fn process_page(
    img: &RgbImage,
    options: &ProcessOptions,
    models: &Models,
) -> Result<(RgbImage, PageReport), VisionError> {
    let mut report = PageReport::default();
    let flat = timed(&mut report.timings, "dewarp", || dewarp(img, options.dewarp, crate::Framing::Page, models))?;
    report.dewarp_applied = flat.applied;
    report.dewarp_passes = flat.passes;
    let mut page = flat.image;
    if options.remove_handwriting {
        let (out, hw) = timed(&mut report.timings, "handwriting", || remove_handwriting(&page, models))?;
        report.handwriting = Some(hw);
        page = out;
    }
    let image = timed(&mut report.timings, "filter", || apply_filter(&page, options.filter));
    Ok((image, report))
}
