//! 페이지 보정 단계. 단계마다 함수 하나이고 실제 처리는 scanshelf-vision이 한다.
//!
//! 순서: 원근 보정 → 회전 → 손가락 지우기 → 두 페이지 분리 → 곡면 보정 → 필기 지우기 → 조명과 필터
//! 손가락은 곡면 보정 전에 지운다. 손가락은 펼친 책의 바깥 가장자리에서 들어오므로 원근만 편 사진에서 찾기 쉽고,
//! 곡면 보정이 손 모양을 늘리기 전이라 지울 면적도 작다(scanshelf-vision process_source와 같은 순서, docs/vision.md).
//! 모델이 없으면 곡면 보정은 건너뛰고 인페인팅은 종이 색 채우기로 바뀐다(docs/models.md)
//!
//! @see docs/pipeline.md

use image::RgbImage;
use scanshelf_core::processing::{Dewarp, Filter, PagePart, ProcessOptions};
use scanshelf_core::{PageLayout, Quad};
use scanshelf_vision::Models;
use scanshelf_vision::perspective::{self, Corners, Interpolation};

/// 출력 페이지 긴 변 상한. A4를 약 340dpi로 담는 크기다. 4800만 화소 원본을 그대로 두면 PDF가 너무 커지고 OCR도 느려진다
pub const MAX_OUTPUT_SIDE: u32 = 4000;

/// 꼭짓점이 사진 모서리에 거의 붙어 있으면 원근 보정을 건너뛴다(감지 실패 대신 사진 전체가 잡힌 경우)
const FULL_FRAME_TOLERANCE: f64 = 0.004;
const AUTO_SPREAD_MIN_ASPECT: f32 = 1.15;
const AUTO_SPREAD_MIN_CONFIDENCE: f32 = 0.8;
const AUTO_SPREAD_MAX_OFF_CENTER: f32 = 0.08;

/// 처리하며 확인한 것. 자동으로 고친 것과 사람이 볼 것을 페이지마다 남겨 화면에 "확인 필요"로 띄운다.
/// @see docs/pipeline.md "페이지 확인"
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PageCheck {
    /// iPhone 테두리가 사각형이 아니어서 Mac이 다시 찾았다
    QuadRedetected,
    /// 믿을 만한 테두리가 없어 사진 전체를 썼다. 배경이 남는다
    QuadMissing,
    /// 펼침인데 편 결과가 세로로 길다. 한 쪽만 테두리로 잡혔을 수 있다
    QuadOnePage,
    /// 펼침인데 접힌 선을 찾지 못해 한 장으로 남겼다
    NotSplit,
    /// 그림자 없는 펼침을 가운데 이음매에서 나눴다
    SplitAtSeam,
    /// 글자 방향이 돌려야 한다고 했지만 돌려 보니 덜 읽혀 그대로 뒀다
    RotationDoubtful,
    /// 종이 바탕이 거의 없는 사진 페이지라 선명하게 대신 전체 밝기만 맞췄다
    PhotoPage,
}

impl PageCheck {
    /// 사람이 봐야 하는지. 아니면 자동으로 처리한 기록이다
    pub fn needs_review(self) -> bool {
        matches!(self, Self::QuadMissing | Self::QuadOnePage | Self::NotSplit | Self::SplitAtSeam | Self::RotationDoubtful)
    }
}

pub struct Corrected {
    pub parts: Vec<(PagePart, RgbImage)>,
    pub checks: Vec<PageCheck>,
}

/// 펼침을 편 결과가 이보다 세로로 길면(폭/높이) 한 쪽만 잡혔다고 본다. A4 펼침은 1.41, 한 쪽은 0.71
const SPREAD_MIN_ASPECT: f32 = 1.0;

pub struct CorrectInput<'a> {
    pub image: RgbImage,
    pub quad: Option<Quad>,
    pub layout: PageLayout,
    pub options: &'a ProcessOptions,
    pub models: &'a Models,
    /// 바로 세우려면 시계 방향으로 몇 도 돌려야 하는지. 모르면 None(macOS는 OCR로 판단, ocr::detect_rotation)
    pub detect_rotation: &'a dyn Fn(&RgbImage) -> Option<u16>,
    /// 자동 회전 뒤에 사람이 더 돌린 각도
    pub extra_rotation: u16,
    /// 문서 대부분이 펼친 사진이라 "한 쪽"으로 찍힌 장도 접은 자국이 뚜렷하면 나눈다
    pub auto_spread: bool,
}

/// 글자로 방향을 정하지 못한 펼침, 접지는 가로로 긴 쪽이 맞으므로 세로로 서 있으면 눕힌다
pub fn rotate_step(
    image: RgbImage,
    layout: PageLayout,
    detect: &dyn Fn(&RgbImage) -> Option<u16>,
    extra: u16,
) -> RgbImage {
    let wide_layout = layout != PageLayout::Single;
    let auto = detect(&image).unwrap_or(if wide_layout && image.height() > image.width() { 90 } else { 0 });
    let total = (auto + extra) % 360;
    if total == 0 {
        image
    } else {
        crate::ocr::rotate(&image, total)
    }
}

/// 사람이 고르지 않은 한 쪽 사진이 실제로는 펼침인지. 잘못 나누면 내용이 잘리므로 조건을 좁게 둔다
fn effective_layout(image: &RgbImage, layout: PageLayout, auto_spread: bool) -> PageLayout {
    if layout != PageLayout::Single || !auto_spread || (image.width() as f32) < image.height() as f32 * AUTO_SPREAD_MIN_ASPECT {
        return layout;
    }
    let gutter = scanshelf_vision::find_gutter(image);
    let at = gutter.center_x() / image.width() as f32;
    if gutter.confidence >= AUTO_SPREAD_MIN_CONFIDENCE && (at - 0.5).abs() <= AUTO_SPREAD_MAX_OFF_CENTER {
        PageLayout::Spread
    } else {
        layout
    }
}

/// 원본 한 장을 출력 페이지 한 장, 두 장(펼친 책), 세 장(3단 접지)으로 만든다
pub fn correct_page(input: CorrectInput<'_>) -> Result<Corrected, String> {
    let models = input.models;
    let mut checks = Vec::new();
    let flat = perspective_step(input.image, input.quad)?;
    // 접힌 선은 세로로 찾으므로 나누기 전에 세워야 한다. 누운 채로는 펼친 두 쪽을 나누지 못한다
    let flat = rotate_step(flat, input.layout, input.detect_rotation, input.extra_rotation);
    let flat = if input.options.remove_fingers { remove_fingers_step(flat, models)? } else { flat };
    let mut outputs = Vec::new();
    let layout = effective_layout(&flat, input.layout, input.auto_spread);
    if layout == PageLayout::Spread && input.quad.is_some() && (flat.width() as f32) < flat.height() as f32 * SPREAD_MIN_ASPECT {
        checks.push(PageCheck::QuadOnePage);
    }
    let (parts, split_check) = split_step_checked(flat, layout);
    checks.extend(split_check);
    for (part, image) in parts {
        let image = dewarp_step(image, input.options.dewarp, models)?;
        let image = if input.options.remove_handwriting {
            remove_handwriting_step(image, models)?
        } else {
            image
        };
        if input.options.filter != Filter::Original
            && !checks.contains(&PageCheck::PhotoPage)
            && scanshelf_vision::filter::is_photo_page(&image)
        {
            checks.push(PageCheck::PhotoPage);
        }
        let image = filter_step(image, input.options.filter);
        outputs.push((part, image));
    }
    Ok(Corrected { parts: outputs, checks })
}

/// 나누기와 그 확인. 사람이 한 쪽으로 둔 장(QuadOnePage가 아닌 한 쪽 표지 등)은 나누지 못해도 확인 대상이 아니다
fn split_step_checked(image: RgbImage, layout: PageLayout) -> (Vec<(PagePart, RgbImage)>, Option<PageCheck>) {
    if layout != PageLayout::Spread {
        return (split_step(image, layout), None);
    }
    let wide = image.width() as f32 >= image.height() as f32 * SPREAD_MIN_ASPECT;
    match scanshelf_vision::split_spread(&image) {
        scanshelf_vision::Split::Spread { left, right, gutter } => {
            let check = (gutter.confidence < scanshelf_vision::split::MIN_CONFIDENCE + 0.1).then_some(PageCheck::SplitAtSeam);
            (vec![(PagePart::Left, left), (PagePart::Right, right)], check)
        }
        scanshelf_vision::Split::Single { .. } => (vec![(PagePart::Whole, image)], wide.then_some(PageCheck::NotSplit)),
    }
}

/// 1단계: 원근 보정(scanshelf-vision perspective). 꼭짓점이 없으면 사진 전체를 크기만 맞춰 쓴다
pub fn perspective_step(image: RgbImage, quad: Option<Quad>) -> Result<RgbImage, String> {
    let Some(quad) = quad.filter(|q| !is_full_frame(q)) else {
        return Ok(limit_size(image));
    };
    let points = quad.points().map(|p| (p.x, p.y));
    let corners = Corners::from_normalized(points, image.width(), image.height());
    let (w, h) = perspective::output_size(&corners);
    let (w, h) = fit_within(w, h, MAX_OUTPUT_SIDE);
    perspective::warp_to_size(&image, &corners, w, h, Interpolation::Bicubic).map_err(|e| e.to_string())
}

/// 3단계: 펼친 책을 접힌 선에서 왼쪽, 오른쪽으로, 3단 접지는 접힌 선 두 개에서 세 면으로 나눈다.
/// 접힌 선을 확신하지 못하면(한 쪽만 찍힌 경우) 나누지 않는다
pub fn split_step(image: RgbImage, layout: PageLayout) -> Vec<(PagePart, RgbImage)> {
    match layout {
        PageLayout::Single => vec![(PagePart::Whole, image)],
        // 왼쪽부터 Panel(0), Panel(1), Panel(2). 출력 id는 -P1~-P3(docs/document-format.md)
        PageLayout::Trifold => match scanshelf_vision::split_trifold(&image) {
            scanshelf_vision::Trifold::Panels { panels, .. } => {
                (0u8..).zip(panels).map(|(index, panel)| (PagePart::Panel(index), panel)).collect()
            }
            scanshelf_vision::Trifold::Whole { .. } => vec![(PagePart::Whole, image)],
        },
        PageLayout::Spread => match scanshelf_vision::split_spread(&image) {
            scanshelf_vision::Split::Spread { left, right, .. } => vec![(PagePart::Left, left), (PagePart::Right, right)],
            scanshelf_vision::Split::Single { .. } => vec![(PagePart::Whole, image)],
        },
    }
}

/// 4단계: 곡면 보정. 모델이 없으면 그대로
pub fn dewarp_step(image: RgbImage, mode: Dewarp, models: &Models) -> Result<RgbImage, String> {
    if mode == Dewarp::Off {
        return Ok(image);
    }
    scanshelf_vision::dewarp(&image, mode, models).map(|r| r.image).map_err(|e| e.to_string())
}

/// 2단계: 손가락 지우기
pub fn remove_fingers_step(image: RgbImage, models: &Models) -> Result<RgbImage, String> {
    scanshelf_vision::remove_fingers(&image, None, models).map(|(out, _)| out).map_err(|e| e.to_string())
}

/// 5단계: 색 펜 필기 지우기(파랑, 빨강, 초록). 연필과 검은 펜은 남는다(docs/vision.md)
pub fn remove_handwriting_step(image: RgbImage, models: &Models) -> Result<RgbImage, String> {
    scanshelf_vision::remove_handwriting(&image, models).map(|(out, _)| out).map_err(|e| e.to_string())
}

/// 6단계: 조명 보정과 필터. 흑백 필터가 인페인팅보다 먼저 오면 지운 자국이 도드라져 맨 끝에 둔다
pub fn filter_step(image: RgbImage, filter: Filter) -> RgbImage {
    scanshelf_vision::apply_filter(&image, filter)
}

fn is_full_frame(quad: &Quad) -> bool {
    let near = |a: f64, b: f64| (a - b).abs() < FULL_FRAME_TOLERANCE;
    near(quad.top_left.x, 0.0)
        && near(quad.top_left.y, 0.0)
        && near(quad.top_right.x, 1.0)
        && near(quad.top_right.y, 0.0)
        && near(quad.bottom_right.x, 1.0)
        && near(quad.bottom_right.y, 1.0)
        && near(quad.bottom_left.x, 0.0)
        && near(quad.bottom_left.y, 1.0)
}

fn fit_within(w: u32, h: u32, max_side: u32) -> (u32, u32) {
    let long = w.max(h);
    if long <= max_side {
        return (w, h);
    }
    let scale = max_side as f64 / long as f64;
    (((w as f64 * scale).round() as u32).max(1), ((h as f64 * scale).round() as u32).max(1))
}

fn limit_size(image: RgbImage) -> RgbImage {
    let (w, h) = fit_within(image.width(), image.height(), MAX_OUTPUT_SIDE);
    if (w, h) == image.dimensions() {
        image
    } else {
        image::imageops::resize(&image, w, h, image::imageops::FilterType::CatmullRom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scanshelf_core::Point;

    fn quad(points: [(f64, f64); 4]) -> Quad {
        let p = |(x, y): (f64, f64)| Point { x, y };
        Quad {
            top_left: p(points[0]),
            top_right: p(points[1]),
            bottom_right: p(points[2]),
            bottom_left: p(points[3]),
        }
    }

    fn spread_input(image: RgbImage) -> Vec<PagePart> {
        layout_input(image, PageLayout::Spread)
    }

    fn layout_input(image: RgbImage, layout: PageLayout) -> Vec<PagePart> {
        let options = ProcessOptions { filter: Filter::Original, remove_fingers: false, ..ProcessOptions::default() };
        correct_page(CorrectInput {
            image,
            quad: None,
            layout,
            options: &options,
            models: &Models::none(),
            detect_rotation: &|_| None,
            extra_rotation: 0,
            auto_spread: false,
        })
        .unwrap()
        .parts
        .into_iter()
        .map(|(part, _)| part)
        .collect()
    }

    #[test]
    fn 접힌_선_그림자가_있으면_왼쪽과_오른쪽_두_장이_된다() {
        // 가운데로 갈수록 어두워지는 책 접힌 선 그림자와, 양쪽 페이지의 글줄 무늬
        let image = RgbImage::from_fn(800, 600, |x, y| {
            let shadow = (1.0 - ((x as f32 - 400.0).abs() / 40.0)).max(0.0);
            let line = y % 24 < 3 && (60..360).contains(&(x % 400)) && x % 400 > 40;
            let v = if line { 60.0 } else { 245.0 - 170.0 * shadow };
            image::Rgb([v as u8; 3])
        });
        assert_eq!(spread_input(image), vec![PagePart::Left, PagePart::Right]);
    }

    #[test]
    fn 자동_회전에_사람이_돌린_각도를_더한다() {
        let image = || RgbImage::new(40, 20);
        let single = PageLayout::Single;
        assert_eq!(rotate_step(image(), single, &|_| Some(90), 0).dimensions(), (20, 40));
        assert_eq!(rotate_step(image(), single, &|_| Some(90), 270).dimensions(), (40, 20));
        assert_eq!(rotate_step(image(), single, &|_| None, 0).dimensions(), (40, 20));
    }

    #[test]
    fn 글자로_못_정한_펼침이_세로로_서_있으면_눕힌다() {
        let tall = RgbImage::new(20, 40);
        assert_eq!(rotate_step(tall.clone(), PageLayout::Spread, &|_| None, 0).dimensions(), (40, 20));
        assert_eq!(rotate_step(tall, PageLayout::Single, &|_| None, 0).dimensions(), (20, 40));
    }

    #[test]
    fn 접힌_선을_못_찾으면_나누지_않는다() {
        assert_eq!(spread_input(RgbImage::from_pixel(400, 300, image::Rgb([240; 3]))), vec![PagePart::Whole]);
    }

    #[test]
    fn 접은_자국이_있는_3단_접지는_세_면이_된다() {
        // 1/3, 2/3에 가는 접은 자국, 면마다 글줄 무늬
        let image = RgbImage::from_fn(900, 640, |x, y| {
            let local = x % 300;
            let crease = local.min(300 - local) < 2 && x > 10 && x < 890;
            let line = y % 20 < 3 && (30..270).contains(&local) && (60..580).contains(&y);
            let v = if crease { 170 } else if line { 50 } else { 240 };
            image::Rgb([v; 3])
        });
        assert_eq!(
            layout_input(image, PageLayout::Trifold),
            vec![PagePart::Panel(0), PagePart::Panel(1), PagePart::Panel(2)]
        );
    }

    #[test]
    fn 접힌_선을_못_찾은_3단_접지는_통째로_둔다() {
        assert_eq!(layout_input(RgbImage::from_pixel(900, 640, image::Rgb([240; 3])), PageLayout::Trifold), vec![PagePart::Whole]);
    }

    #[test]
    fn 꼭짓점_안쪽만_잘라_편다() {
        let image = RgbImage::from_fn(200, 100, |x, _| if (50..150).contains(&x) { image::Rgb([255, 255, 255]) } else { image::Rgb([0, 0, 0]) });
        let out = perspective_step(image, Some(quad([(0.25, 0.0), (0.75, 0.0), (0.75, 1.0), (0.25, 1.0)]))).unwrap();
        assert_eq!(out.dimensions(), (100, 100));
        assert!(out.pixels().all(|p| p.0[0] > 200), "흰 영역만 남아야 한다");
    }

    #[test]
    fn 긴_변은_상한에_맞춰_줄인다() {
        assert_eq!(fit_within(8000, 6000, 4000), (4000, 3000));
        assert_eq!(fit_within(300, 200, 4000), (300, 200));
    }
}
