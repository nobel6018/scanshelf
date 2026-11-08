//! 펼친 3단 접지 팜플렛을 세 면으로 나눈다.
//!
//! 두 페이지 분리(`split.rs`)의 접힌 선 찾기를 넓힌 것이다. 다른 점은 셋이다.
//! 1. 선이 두 개다. 1/3, 2/3 근처 두 구간에서 따로 찾는다. 안쪽으로 접혀 들어가는 면은 2~3mm 좁아 면 폭이 정확히 같지 않다
//! 2. 단서가 약하다. 책 가운데 같은 넓은 그림자 골짜기가 없고, 접은 자국(가는 어두운 선이나 밝은 선)과
//!    면마다 빛을 받는 각도가 달라 생기는 밝기 계단이 전부다. 글자 화소는 빼고 종이 밝기만 단면으로 낸다
//! 3. 원근이 남아 두 선의 기울기가 다를 수 있다(사다리꼴). 선마다 기울기를 따로 찾는다
//!
//! 사진(팜플렛 그림)의 가장자리도 밝기 계단을 만든다. 접은 자국은 위에서 아래까지 이어지므로,
//! 높이를 네 띠로 나눠 띠마다 점수를 내고 네 띠 중 세 띠 이상에서 보이는 선만 믿는다.
//! 그림자도 자국도 없는 사진은 두 면 여백이 만나는 글자 없는 세로 띠로 찾는다(`split.rs`와 같은 방법).
//!
//! 두 선을 다 확신하지 못하면 나누지 않고 `Trifold::Whole`을 돌려준다.
//!
//! 자동으로 틀린 장은 사람이나 AI가 대략의 위치를 준다(`snap_folds`). 준 위치 근처 좁은 범위에서만 자국을 찾아 붙이고,
//! 자국이 약하면 준 위치를 세로로 그대로 쓴다. 눈으로 고른 위치는 2~3% 어긋나기 쉬워 자국에 붙이는 편이 깔끔하다.
//!
//! @see docs/vision.md

use image::RgbImage;
use rayon::prelude::*;

use crate::perspective::{Corners, Interpolation, PixelPoint, warp_to_size};
use crate::split::percentile;
use crate::util::{Plane, downscale_area, fit_long_side, luma};

/// 이보다 낮으면 나누지 않는다
pub const MIN_FOLD_CONFIDENCE: f32 = 0.35;
/// 원근 보정 뒤 남는 기울기 범위(도)
const MAX_TILT_DEG: f32 = 6.0;
/// 접은 자국은 2~3화소 폭이라 기울기를 촘촘히 본다(높이 700화소에서 0.25도 = 끝에서 1.5화소)
const TILT_STEP_DEG: f32 = 0.25;
const ANALYSIS_LONG_SIDE: u32 = 1000;
/// 높이를 나누는 띠 수. 접은 자국은 이 중 `BANDS - 1`개 이상에서 보여야 한다
const BANDS: usize = 4;
/// 접힌 선을 찾는 구간(폭 대비). 첫 선은 1/3, 둘째 선은 2/3 근처
const LEFT_RANGE: (f32, f32) = (0.2, 0.47);
const RIGHT_RANGE: (f32, f32) = (0.53, 0.8);
/// 3단 접지 면은 거의 같은 폭이다(안으로 접히는 면만 2~3mm 좁다). 면 폭이 모두 이 안이면 고르다고 본다
const REGULAR_WIDTH: (f32, f32) = (0.28, 0.38);
/// 넓게 찾은 선으로 면 폭이 고르지 않으면 1/3, 2/3 근처만 다시 찾는다.
/// 2026-11-08 로보월드_6 RBY 바깥 면: 오른쪽 선을 엉뚱한 곳에서 찾아 표지 면이 21%로 좁아졌다
const NARROW_LEFT: (f32, f32) = (0.28, 0.39);
const NARROW_RIGHT: (f32, f32) = (0.61, 0.72);
/// 한 선이 이만큼 확실하고 면 폭이 고르면, 다른 선은 이 정도만 돼도 받는다.
/// 2026-11-08 로보월드_6 RBM 안쪽: 빛 반사로 오른쪽 선 신뢰도가 0.28이었지만 제자리였다
const STRONG_FOLD: f32 = 0.9;
const WEAK_FOLD_WITH_REGULAR: f32 = 0.2;
/// 펼친 책의 접힌 선을 찾는 구간
pub(crate) const CENTER_RANGE: (f32, f32) = (0.35, 0.65);
/// 이보다 약하면 접은 자국으로 보지 않는다(종이 밝기 대비)
const MIN_CENTER_CREASE: f32 = 0.03;
/// 이보다 얕으면 그림자 골짜기로 보지 않는다(종이 밝기 대비)
const MIN_CENTER_VALLEY: f32 = 0.06;
/// 이보다 깊으면 책이 크게 휜 것으로 보고 골짜기를 바로 믿는다
const DEEP_CENTER_VALLEY: f32 = 0.2;

/// 원본 해상도 기준 접힌 선 하나. 위쪽 끝 x와 아래쪽 끝 x로 기울기를 표현한다
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fold {
    pub x_top: f32,
    pub x_bottom: f32,
    /// 0~1
    pub confidence: f32,
    /// 접은 자국 세기(종이 밝기 대비). 골짜기, 밝은 선, 밝기 계단 중 가장 큰 것
    pub crease: f32,
    /// 글자 없는 띠 폭(이미지 폭 대비 0~1)
    pub blank_width: f32,
}

impl Fold {
    pub fn center_x(&self) -> f32 {
        (self.x_top + self.x_bottom) / 2.0
    }

    pub fn angle_deg(&self, height: u32) -> f32 {
        ((self.x_bottom - self.x_top) / height as f32).atan().to_degrees()
    }
}

/// 두 접힌 선. `confidence`는 두 선 중 약한 쪽에 가로세로 비와 면 폭 조건을 곱한 값
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Folds {
    pub left: Fold,
    pub right: Fold,
    pub confidence: f32,
}

impl Folds {
    /// 왼쪽부터 세 면의 폭(가운데 높이 기준, 이미지 폭 대비)
    pub fn panel_widths(&self, width: u32) -> [f32; 3] {
        let w = width as f32;
        let (a, b) = (self.left.center_x(), self.right.center_x());
        [a / w, (b - a) / w, (w - b) / w]
    }
}

#[derive(Debug, Clone)]
pub enum Trifold {
    /// 왼쪽부터 세 면
    Panels { panels: [RgbImage; 3], folds: Folds },
    /// 접힌 선을 확신하지 못했다. 원본을 통째 한 장으로 쓴다. folds는 가장 그럴듯했던 후보(진단용)
    Whole { folds: Folds },
}

/// 접힌 선 두 개를 찾는다. 신뢰도와 상관없이 가장 그럴듯한 후보를 돌려준다
pub fn find_folds(img: &RgbImage) -> Folds {
    let (aw, ah) = fit_long_side(img.width(), img.height(), ANALYSIS_LONG_SIDE);
    let small = downscale_area(img, aw, ah);
    let g = luma(&small);
    let (w, h) = (g.width, g.height);
    let paper = percentile(&g.data, 0.9).max(1.0);

    let tilts: Vec<f32> = {
        let n = (MAX_TILT_DEG / TILT_STEP_DEG).round() as i32;
        (-n..=n).map(|i| i as f32 * TILT_STEP_DEG).collect()
    };
    let profiles: Vec<TiltProfile> = tilts.par_iter().map(|&deg| tilt_profile(&g, deg, paper)).collect();

    let scale_x = img.width() as f32 / w as f32;
    let to_fold = |c: FoldCandidate| {
        let tan = c.deg.to_radians().tan();
        let half = img.height() as f32 / 2.0;
        let cx = (c.x + 0.5) * scale_x - 0.5;
        Fold {
            x_top: cx - half * tan,
            x_bottom: cx + half * tan,
            confidence: c.confidence,
            crease: c.crease,
            blank_width: c.blank_width,
        }
    };
    let search = |l: (f32, f32), r: (f32, f32)| {
        let left = to_fold(best_fold(&profiles, w, l, 1.0 / 3.0));
        let right = to_fold(best_fold(&profiles, w, r, 2.0 / 3.0));
        let widths = Folds { left, right, confidence: 0.0 }.panel_widths(img.width());
        (left, right, widths)
    };
    let regular = |widths: &[f32; 3]| widths.iter().all(|p| (REGULAR_WIDTH.0..=REGULAR_WIDTH.1).contains(p));
    let (mut left, mut right, mut widths) = search(LEFT_RANGE, RIGHT_RANGE);
    if !regular(&widths) {
        let narrow = search(NARROW_LEFT, NARROW_RIGHT);
        if regular(&narrow.2) {
            (left, right, widths) = narrow;
        }
    }

    // 펼친 3단 접지는 가로가 길다(A4 접지 1.41). 세로로 긴 사진은 접지가 아닐 가능성이 크다
    let aspect = w as f32 / h as f32;
    let aspect_factor = ((aspect - 0.85) / 0.35).clamp(0.2, 1.0);
    let widths_ok = widths.iter().all(|&p| (0.2..=0.46).contains(&p));
    let width_factor = if widths_ok { 1.0 } else { 0.3 };
    let (strong, weak) = (left.confidence.max(right.confidence), left.confidence.min(right.confidence));
    let mut confidence = weak * aspect_factor * width_factor;
    if regular(&widths) && strong >= STRONG_FOLD && weak >= WEAK_FOLD_WITH_REGULAR {
        confidence = confidence.max(MIN_FOLD_CONFIDENCE);
    }
    Folds { left, right, confidence }
}

/// 접힌 선 두 개를 찾아 세 면으로 나눈다. 신뢰도가 낮으면 나누지 않는다
pub fn split_trifold(img: &RgbImage) -> Trifold {
    let folds = find_folds(img);
    if folds.confidence < MIN_FOLD_CONFIDENCE {
        return Trifold::Whole { folds };
    }
    Trifold::Panels { panels: cut_panels(img, &folds), folds }
}

/// 주어진 두 선으로 자른다. 선이 기울었으면 면마다 네 꼭짓점(위아래 선 끝)을 직사각형으로 편다
pub fn cut_panels(img: &RgbImage, folds: &Folds) -> [RgbImage; 3] {
    let [a, b, c]: [RgbImage; 3] = cut_at(img, &[folds.left, folds.right]).try_into().expect("선 두 개면 세 면이다");
    [a, b, c]
}

/// 준 위치(이미지 폭 대비 0~1) 근처 ±이만큼에서만 자국을 찾는다
pub const SNAP_RANGE: f32 = 0.03;

/// 사람이나 AI가 고른 접힌 선 위치(0~1, 왼쪽부터)를 근처의 접은 자국에 붙인다. 자국이 약하면 준 위치를 세로선으로 쓴다
pub fn snap_folds(img: &RgbImage, hints: &[f32]) -> Vec<Fold> {
    let (aw, ah) = fit_long_side(img.width(), img.height(), ANALYSIS_LONG_SIDE);
    let small = downscale_area(img, aw, ah);
    let g = luma(&small);
    let paper = percentile(&g.data, 0.9).max(1.0);
    let tilts: Vec<f32> = {
        let n = (MAX_TILT_DEG / TILT_STEP_DEG).round() as i32;
        (-n..=n).map(|i| i as f32 * TILT_STEP_DEG).collect()
    };
    let profiles: Vec<TiltProfile> = tilts.par_iter().map(|&deg| tilt_profile(&g, deg, paper)).collect();
    let scale_x = img.width() as f32 / g.width as f32;
    let half = img.height() as f32 / 2.0;
    hints
        .iter()
        .map(|&hint| {
            let c = best_fold(&profiles, g.width, (hint - SNAP_RANGE, hint + SNAP_RANGE), hint);
            if c.confidence < MIN_FOLD_CONFIDENCE {
                let x = hint * img.width() as f32;
                return Fold { x_top: x, x_bottom: x, confidence: 0.0, crease: c.crease, blank_width: c.blank_width };
            }
            let tan = c.deg.to_radians().tan();
            let cx = (c.x + 0.5) * scale_x - 0.5;
            Fold { x_top: cx - half * tan, x_bottom: cx + half * tan, confidence: c.confidence, crease: c.crease, blank_width: c.blank_width }
        })
        .collect()
}

/// 선 여러 개(왼쪽부터)로 자른다. 펼친 두 쪽은 선 하나, 3단 접지는 둘이다
pub fn cut_at(img: &RgbImage, folds: &[Fold]) -> Vec<RgbImage> {
    let (w, h) = (img.width(), img.height());
    let wf = w as f32;
    let clamp = |x: f32| x.clamp(1.0, wf - 1.0);
    // 위, 아래 끝 x. 앞 선보다 적어도 1픽셀 오른쪽
    let mut edges: Vec<(f32, f32)> = vec![(0.0, 0.0)];
    for f in folds {
        let (pt, pb) = *edges.last().expect("처음 값이 있다");
        edges.push((clamp(f.x_top).max(pt + 1.0), clamp(f.x_bottom).max(pb + 1.0)));
    }
    edges.push((wf, wf));
    let straight = folds.iter().all(|f| (f.x_top - f.x_bottom).abs() < 0.5);
    if straight {
        let xs: Vec<u32> = edges.iter().map(|(t, _)| (t.round() as u32).min(w)).collect();
        return xs
            .windows(2)
            .map(|p| {
                let (x0, x1) = (p[0], p[1].max(p[0] + 1).min(w));
                image::imageops::crop_imm(img, x0.min(w - 1), 0, (x1 - x0).max(1), h).to_image()
            })
            .collect();
    }
    let hf = h as f32;
    let panel = |x0t: f32, x1t: f32, x1b: f32, x0b: f32| -> RgbImage {
        let p = |x: f32, y: f32| PixelPoint::new(x as f64, y as f64);
        let corners = Corners {
            top_left: p(x0t, 0.0),
            top_right: p(x1t, 0.0),
            bottom_right: p(x1b, hf),
            bottom_left: p(x0b, hf),
        };
        let width = ((x1t - x0t + x1b - x0b) / 2.0).round().max(1.0) as u32;
        // 네 꼭짓점이 사다리꼴이라 퇴화하지 않는다. 혹시 실패하면 바깥 상자를 자른다
        warp_to_size(img, &corners, width, h, Interpolation::Bicubic).unwrap_or_else(|_| {
            let x0 = x0t.min(x0b).max(0.0) as u32;
            let x1 = (x1t.max(x1b) as u32).clamp(x0 + 1, w);
            image::imageops::crop_imm(img, x0, 0, x1 - x0, h).to_image()
        })
    };
    edges.windows(2).map(|p| panel(p[0].0, p[1].0, p[1].1, p[0].1)).collect()
}

/// 펼친 책 가운데의 접은 자국을 찾는다. 빈 띠는 보지 않는다. 표의 칸 사이도 글자 없는 띠라 그걸 믿으면 표가 잘린다.
/// 자국은 위에서 아래까지 이어지고 표의 빈 칸에는 없으므로 둘을 가른다. 뚜렷하지 않으면 None.
/// 돌려주는 값: (기울기 도, 분석 해상도 기준 가운데 높이의 x)
pub(crate) fn find_center_crease(g: &Plane, paper: f32) -> Option<(f32, f32)> {
    let n = (MAX_TILT_DEG / TILT_STEP_DEG).round() as i32;
    let tilts: Vec<f32> = (-n..=n).map(|i| i as f32 * TILT_STEP_DEG).collect();
    let profiles: Vec<TiltProfile> = tilts.par_iter().map(|&deg| tilt_profile(g, deg, paper)).collect();
    let w = g.width;
    let lo = (CENTER_RANGE.0 * w as f32) as usize;
    let hi = (CENTER_RANGE.1 * w as f32) as usize;
    // 책은 거의 가운데에서 접힌다. 같은 세기면 가운데에 가까운 선을 고른다
    let prior = |x: usize| 1.0 - 0.5 * ((x as f32 / w as f32 - 0.5).abs() / 0.12).min(1.0);
    // 넓은 그림자 골짜기: 글자를 뺀 종이 밝기라 표의 칸 사이에는 생기지 않는다. 보이면 가장 어두운 곳이 접힌 곳이다
    let reach = (w as f32 * 0.12) as usize;
    let valley = profiles
        .iter()
        .flat_map(|t| {
            let paper_line = &t.paper;
            (lo..hi).map(move |x| {
                let v = paper_line[x];
                let side = |range: &[f32]| fmax(range).unwrap_or(f32::NAN);
                let left = side(&paper_line[x.saturating_sub(reach)..x]);
                let right = side(&paper_line[x + 1..(x + reach + 1).min(paper_line.len())]);
                (t.deg, x, (left.min(right) - v) / paper)
            })
        })
        .filter(|c| c.2.is_finite())
        .map(|(deg, x, depth)| (deg, x, depth * prior(x)))
        .max_by(|a, b| a.2.total_cmp(&b.2).then((-a.0.abs()).total_cmp(&-b.0.abs())));
    // 깊은 그림자는 책이 크게 휜 사진이다. 가장 어두운 곳이 접힌 곳이고 자국 단서는 그림자 비탈에서 잘못 잡힌다
    if let Some((deg, x, depth)) = valley
        && depth >= DEEP_CENTER_VALLEY
    {
        return Some((deg, x as f32));
    }
    let crease_at = |range: std::ops::Range<usize>| {
        profiles
            .iter()
            .flat_map(|t| range.clone().map(move |x| (t.deg, x, t.crease[x])))
            .map(|(deg, x, crease)| (deg, x, crease * prior(x)))
            .max_by(|a, b| a.2.total_cmp(&b.2).then((-a.0.abs()).total_cmp(&-b.0.abs())))
    };
    let (deg, x, score) = crease_at(lo..hi)?;
    if score >= MIN_CENTER_CREASE {
        return Some((deg, x as f32));
    }
    // 자국이 약하면 얕은 골짜기로 자리만 좁히고, 그 근처에서 가장 뚜렷한 자국을 고른다. 골짜기는 넓어서 위치가 부정확하다
    let (_, valley_x, depth) = valley?;
    if depth < MIN_CENTER_VALLEY {
        return None;
    }
    let near = (w as f32 * 0.06) as usize;
    crease_at(valley_x.saturating_sub(near).max(lo)..(valley_x + near).min(hi)).map(|(deg, x, _)| (deg, x as f32))
}

/// 기울기 하나에서 낸 단면들
struct TiltProfile {
    deg: f32,
    /// 띠마다 접은 자국 점수를 낸 뒤 네 띠 중 둘째로 약한 값(종이 밝기 대비)
    crease: Vec<f32>,
    /// 글자 화소 비율(높이 전체)
    ink: Vec<f32>,
    /// 글자를 뺀 종이 밝기(띠 평균). 그림자 골짜기를 본다
    paper: Vec<f32>,
}

#[derive(Debug, Clone, Copy)]
struct FoldCandidate {
    deg: f32,
    /// 분석 해상도 기준, 가운데 높이에서의 x
    x: f32,
    confidence: f32,
    crease: f32,
    blank_width: f32,
}

fn tilt_profile(g: &Plane, deg: f32, paper: f32) -> TiltProfile {
    let (w, h) = (g.width, g.height);
    let tan = deg.to_radians().tan();
    let ink_level = paper * 0.6;
    let y0 = h / 20;
    let y1 = h - h / 20;
    let band_h = ((y1 - y0) / BANDS).max(1);
    let cy = h as f32 / 2.0;
    // 열마다 띠별 종이 밝기 평균(글자 화소는 뺀다)과 전체 글자 비율
    let columns: Vec<([f32; BANDS], f32)> = (0..w)
        .into_par_iter()
        .map(|x| {
            let mut sum = [0.0f32; BANDS];
            let mut count = [0u32; BANDS];
            let mut total = [0u32; BANDS];
            let mut ink = 0u32;
            let mut n = 0u32;
            for y in y0..y1 {
                let sx = (x as f32 + (y as f32 - cy) * tan).round();
                if sx < 0.0 || sx >= w as f32 {
                    continue;
                }
                let b = ((y - y0) / band_h).min(BANDS - 1);
                let v = g.get(sx as usize, y);
                total[b] += 1;
                n += 1;
                if v < ink_level {
                    ink += 1;
                } else {
                    sum[b] += v;
                    count[b] += 1;
                }
            }
            let mut means = [f32::NAN; BANDS];
            for b in 0..BANDS {
                // 글자로 거의 덮인 띠는 종이 밝기를 믿을 수 없다
                if total[b] > 0 && count[b] * 3 >= total[b] {
                    means[b] = sum[b] / count[b] as f32;
                }
            }
            let ink = if n == 0 { f32::NAN } else { ink as f32 / n as f32 };
            (means, ink)
        })
        .collect();
    let r = (w / 120).max(2);
    let mut band_scores = vec![[0.0f32; BANDS]; w];
    for b in 0..BANDS {
        let p: Vec<f32> = columns.iter().map(|c| c.0[b]).collect();
        let p = smooth(&p, 1);
        for (x, score) in band_scores.iter_mut().enumerate() {
            score[b] = crease_score(&p, x, r) / paper;
        }
    }
    let crease = band_scores
        .into_iter()
        .map(|mut s| {
            s.sort_by(f32::total_cmp);
            // 한 띠는 그림이나 굵은 제목에 가려도 된다
            s[1]
        })
        .collect();
    let paper_line: Vec<f32> = columns
        .iter()
        .map(|c| {
            let vals: Vec<f32> = c.0.iter().copied().filter(|v| v.is_finite()).collect();
            if vals.is_empty() { f32::NAN } else { vals.iter().sum::<f32>() / vals.len() as f32 }
        })
        .collect();
    TiltProfile { deg, crease, ink: columns.iter().map(|c| c.1).collect(), paper: smooth(&paper_line, (w / 200).max(1)) }
}

/// 접은 자국 점수: 가는 골짜기(어두운 자국), 가는 봉우리(빛 받은 자국), 밝기 계단(면마다 다른 기울기) 중 큰 값
fn crease_score(p: &[f32], x: usize, r: usize) -> f32 {
    let w = p.len();
    let v = p[x];
    if !v.is_finite() || x < r || x + r >= w {
        return 0.0;
    }
    let left = &p[x - r..x];
    let right = &p[x + 1..=x + r];
    let (Some(lmax), Some(rmax), Some(lmean), Some(rmean)) = (fmax(left), fmax(right), fmean(left), fmean(right)) else {
        return 0.0;
    };
    let valley = (lmax.min(rmax) - v).max(0.0);
    let ridge = (v - lmean.max(rmean)).max(0.0);
    let step = (lmean - rmean).abs() * 0.6;
    valley.max(ridge).max(step)
}

fn fmax(s: &[f32]) -> Option<f32> {
    s.iter().copied().filter(|v| v.is_finite()).reduce(f32::max)
}

fn fmean(s: &[f32]) -> Option<f32> {
    let vals: Vec<f32> = s.iter().copied().filter(|v| v.is_finite()).collect();
    (vals.len() * 2 >= s.len()).then(|| vals.iter().sum::<f32>() / vals.len() as f32)
}

/// 한 구간에서 가장 그럴듯한 접힌 선. expected는 면 폭이 같을 때의 위치(폭 대비)
fn best_fold(profiles: &[TiltProfile], w: usize, range: (f32, f32), expected: f32) -> FoldCandidate {
    let lo = (range.0 * w as f32) as usize;
    let hi = ((range.1 * w as f32) as usize).min(w);
    let prior = |x: usize| 1.0 - 0.5 * ((x as f32 / w as f32 - expected).abs() / 0.15).min(1.0);
    let candidates: Vec<(FoldCandidate, f32)> = profiles
        .iter()
        .map(|t| {
            let (cx, cs) = (lo..hi)
                .map(|x| (x, t.crease[x] * prior(x)))
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap_or((w / 2, 0.0));
            let crease = t.crease[cx];
            let (gs, ge) = widest_blank(&t.ink, lo, hi);
            let blank_width = (ge - gs) as f32 / w as f32;
            let crease_conf = ((crease - 0.012) / 0.04).clamp(0.0, 1.0);
            let blank_conf = ((blank_width - 0.012) / 0.03).clamp(0.0, 1.0) * 0.8;
            let in_band = cx >= gs && cx < ge;
            // 자국이 뚜렷하면 자국을 믿는다(면 여백은 양쪽이 다를 수 있다). 약하면 빈 띠 가운데
            let x = if crease_conf >= 0.5 || blank_conf < 0.3 || (in_band && crease_conf >= 0.25) {
                cx as f32
            } else {
                (gs + ge) as f32 / 2.0 - 0.5
            };
            let (strong, weak) = (crease_conf.max(blank_conf), crease_conf.min(blank_conf));
            let confidence = (strong + 0.25 * weak).min(1.0);
            (FoldCandidate { deg: t.deg, x, confidence, crease, blank_width }, cs)
        })
        .collect();
    // 신뢰도가 같으면(둘 다 꽉 찼으면) 자국 점수가 큰 기울기, 그것도 같으면 덜 기운 쪽
    candidates
        .into_iter()
        .max_by(|(a, sa), (b, sb)| {
            a.confidence
                .total_cmp(&b.confidence)
                .then(sa.total_cmp(sb))
                .then((-a.deg.abs()).total_cmp(&-b.deg.abs()))
        })
        .map(|(c, _)| c)
        .expect("기울기 후보가 있다")
}

/// [lo, hi) 안에 가운데가 있는 잉크 없는 가장 넓은 구간. 이미지 끝에 닿은 구간(바깥 여백)은 뺀다
fn widest_blank(ink: &[f32], lo: usize, hi: usize) -> (usize, usize) {
    let w = ink.len();
    let blank = |x: usize| ink[x].is_finite() && ink[x] < 0.003;
    let mut best = (lo, lo);
    let mut x = 0;
    while x < w {
        if !blank(x) {
            x += 1;
            continue;
        }
        let start = x;
        while x < w && blank(x) {
            x += 1;
        }
        let mid = (start + x) / 2;
        if start > 0 && x < w && (lo..hi).contains(&mid) && x - start > best.1 - best.0 {
            best = (start, x);
        }
    }
    best
}

fn smooth(p: &[f32], r: usize) -> Vec<f32> {
    (0..p.len())
        .map(|i| {
            let a = i.saturating_sub(r);
            let b = (i + r + 1).min(p.len());
            fmean(&p[a..b]).unwrap_or(f32::NAN)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    /// 글자 대신 막대를 늘어놓은 3단 접지. folds는 가운데 높이 기준 x, 기울기는 두 선이 같다.
    /// crease는 접은 자국 어둡기(0~1), tone은 가운데 면 밝기 배율(빛 받는 각도)
    fn trifold(w: u32, h: u32, folds: [f32; 2], tilt_deg: f32, crease: f32, tone: f32) -> RgbImage {
        let tan = tilt_deg.to_radians().tan();
        RgbImage::from_fn(w, h, |x, y| {
            let dy = y as f32 - h as f32 / 2.0;
            let f = folds.map(|f| f + dy * tan);
            let xf = x as f32;
            let (start, end, panel) = if xf < f[0] {
                (0.0, f[0], 0)
            } else if xf < f[1] {
                (f[0], f[1], 1)
            } else {
                (f[1], w as f32, 2)
            };
            let pw = end - start;
            let local = xf - start;
            let in_text = local > pw * 0.1 && local < pw * 0.9 && y > h / 12 && y < h * 11 / 12;
            let line = (y / 12) % 2 == 0 && ((x + (y / 24) * 23) / 9) % 6 != 0;
            let mut v = if in_text && line { 40.0 } else { 235.0 };
            if panel == 1 {
                v *= tone;
            }
            let d = f.iter().map(|fx| (xf - fx).abs()).fold(f32::MAX, f32::min);
            v *= 1.0 - crease * (-d * d / 2.0).exp();
            let v = v.clamp(0.0, 255.0) as u8;
            Rgb([v, v, (v as f32 * 0.97) as u8])
        })
    }

    #[test]
    fn 접은_자국이_있는_3단_접지를_나눈다() {
        let img = trifold(1500, 1060, [505.0, 1010.0], 0.0, 0.25, 1.0);
        let Trifold::Panels { panels, folds } = split_trifold(&img) else { panic!("나눠야 한다") };
        assert!((folds.left.center_x() - 505.0).abs() < 5.0, "{folds:?}");
        assert!((folds.right.center_x() - 1010.0).abs() < 5.0, "{folds:?}");
        assert_eq!(panels.iter().map(|p| p.width()).sum::<u32>(), 1500);
    }

    #[test]
    fn 면_밝기_계단만_있어도_찾는다() {
        // 자국 없이 가운데 면만 7% 어둡다(빛 받는 각도 차이)
        let img = trifold(1500, 1060, [490.0, 1000.0], 0.0, 0.0, 0.93);
        let folds = find_folds(&img);
        assert!(folds.confidence >= MIN_FOLD_CONFIDENCE, "{folds:?}");
        assert!((folds.left.center_x() - 490.0).abs() < 8.0, "{folds:?}");
        assert!((folds.right.center_x() - 1000.0).abs() < 8.0, "{folds:?}");
    }

    #[test]
    fn 기울어진_접힌_선을_찾는다() {
        let img = trifold(1500, 1060, [500.0, 1000.0], 2.5, 0.2, 1.0);
        let Trifold::Panels { panels, folds } = split_trifold(&img) else { panic!("나눠야 한다") };
        assert!((folds.left.angle_deg(1060) - 2.5).abs() < 0.5, "{folds:?}");
        assert!((folds.right.center_x() - 1000.0).abs() < 6.0, "{folds:?}");
        assert!(panels.iter().all(|p| p.height() == 1060));
    }

    #[test]
    fn 자국이_없으면_빈_띠로_찾는다() {
        let img = trifold(1500, 1060, [500.0, 1000.0], 0.0, 0.0, 1.0);
        let folds = find_folds(&img);
        assert!(folds.confidence >= MIN_FOLD_CONFIDENCE, "{folds:?}");
        assert!((folds.left.center_x() - 500.0).abs() < 20.0, "{folds:?}");
    }

    #[test]
    fn 글자가_이어지는_한_장은_나누지_않는다() {
        let img = RgbImage::from_fn(1500, 1060, |x, y| {
            let in_text = x > 100 && x < 1400 && y > 90 && y < 970;
            let line = (y / 12) % 2 == 0 && ((x + (y / 24) * 23) / 9) % 6 != 0;
            if in_text && line { Rgb([40, 40, 40]) } else { Rgb([235, 235, 230]) }
        });
        assert!(matches!(split_trifold(&img), Trifold::Whole { .. }));
    }

    #[test]
    fn 준_위치_근처의_약한_자국에_붙인다() {
        // 오른쪽 자국이 아주 약해 자동으로는 나누지 않는 장. 대략 준 위치(2% 어긋남)를 실제 자국에 붙인다
        let img = trifold(1500, 1060, [500.0, 1000.0], 0.0, 0.05, 1.0);
        let folds = snap_folds(&img, &[0.32, 0.68]);
        assert!((folds[0].center_x() - 500.0).abs() < 6.0, "{folds:?}");
        assert!((folds[1].center_x() - 1000.0).abs() < 6.0, "{folds:?}");
    }

    #[test]
    fn 자국이_없으면_준_위치를_그대로_쓴다() {
        let img = RgbImage::from_pixel(1000, 600, Rgb([235; 3]));
        let folds = snap_folds(&img, &[0.4]);
        assert_eq!(folds[0].x_top, 400.0);
        assert_eq!(folds[0].x_bottom, 400.0);
        let parts = cut_at(&img, &folds);
        assert_eq!(parts.iter().map(|p| p.width()).collect::<Vec<_>>(), vec![400, 600]);
    }

    #[test]
    fn 기운_선_여러_개로_자르면_높이가_같다() {
        let img = trifold(1500, 1060, [500.0, 1000.0], 2.5, 0.2, 1.0);
        let folds = snap_folds(&img, &[0.33, 0.67]);
        let parts = cut_at(&img, &folds);
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|p| p.height() == 1060));
    }

    #[test]
    fn 한_선이_확실하고_면이_고르면_약한_선도_받는다() {
        // 오른쪽 선은 빛 반사로 아주 약하다(밝기 차 2%)
        let base = trifold(1500, 1060, [500.0, 1000.0], 0.0, 0.3, 1.0);
        let img = RgbImage::from_fn(1500, 1060, |x, y| {
            let mut p = *base.get_pixel(x, y);
            if (995..1005).contains(&x) {
                p = *base.get_pixel(x - 20, y);
            }
            if x >= 1000 {
                p.0.iter_mut().for_each(|c| *c = (*c as f32 * 0.98) as u8);
            }
            p
        });
        let folds = find_folds(&img);
        assert!(folds.left.confidence >= STRONG_FOLD, "{folds:?}");
        assert!(folds.confidence >= MIN_FOLD_CONFIDENCE, "{folds:?}");
        assert!((folds.right.center_x() - 1000.0).abs() < 20.0, "{folds:?}");
    }

    #[test]
    fn 면_폭이_고르지_않으면_삼분점_근처를_다시_찾는다() {
        // 진짜 접힌 선은 500, 1000. 1180에 위아래로 이어진 더 센 세로 선(그림 가장자리 같은 것)이 있다
        let base = trifold(1500, 1060, [500.0, 1000.0], 0.0, 0.25, 1.0);
        let img = RgbImage::from_fn(1500, 1060, |x, y| if (1178..1183).contains(&x) { Rgb([60, 60, 60]) } else { *base.get_pixel(x, y) });
        let folds = find_folds(&img);
        assert!((folds.right.center_x() - 1000.0).abs() < 8.0, "{folds:?}");
        assert!(folds.panel_widths(1500).iter().all(|p| (REGULAR_WIDTH.0..=REGULAR_WIDTH.1).contains(p)));
    }

    #[test]
    fn 빈_종이는_나누지_않는다() {
        assert!(matches!(split_trifold(&RgbImage::from_pixel(900, 640, Rgb([240; 3]))), Trifold::Whole { .. }));
    }
}
