//! 펼친 책 사진을 왼쪽, 오른쪽 페이지로 나눈다.
//!
//! 접힌 선(gutter)은 두 가지 단서로 찾는다.
//! 1. 그림자 골짜기: 가운데 근처 세로 밝기 단면에서 양옆보다 어두운 좁은 골짜기. 기울기를 바꿔 가며 가장 깊은 골짜기를 고른다
//! 2. 빈 띠: 그림자가 약한 사진(평평하게 눌러 찍은 책)에서는 두 페이지 여백이 만나는 글자 없는 세로 띠
//!
//! 한 페이지만 찍은 사진이 들어오면 두 단서가 모두 약하고 가로세로 비도 세로로 길어 신뢰도가 낮게 나온다.
//! 그때는 나누지 않고 `Split::Single`을 돌려준다.
//!
//! @see docs/vision.md

use image::RgbImage;
use rayon::prelude::*;

use crate::fold::{CENTER_RANGE, find_center_crease};
use crate::util::{Plane, downscale_area, fit_long_side, luma, sample_rgb};

/// 이보다 낮으면 한 페이지로 본다
pub const MIN_CONFIDENCE: f32 = 0.35;
/// 찾는 기울기 범위(도). 손으로 대충 맞춰 찍은 사진 기준
const MAX_TILT_DEG: f32 = 8.0;
const TILT_STEP_DEG: f32 = 0.5;
/// 분석 해상도. 접힌 선은 굵은 구조라 이 정도면 충분하다
const ANALYSIS_LONG_SIDE: u32 = 1000;
/// 접은 자국을 찾았을 때의 신뢰도(가로세로 비를 곱하기 전)
const CREASE_CONFIDENCE: f32 = 0.9;
/// 자국이 없을 때 믿어 주는 가운데에서의 거리(폭 대비)
const MAX_OFF_CENTER_WITHOUT_CREASE: f32 = 0.05;

/// 원본 해상도 기준 접힌 선. 위쪽 끝 x와 아래쪽 끝 x로 기울기를 표현한다
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gutter {
    pub x_top: f32,
    pub x_bottom: f32,
    /// 0~1. `MIN_CONFIDENCE` 미만이면 한 페이지로 본다
    pub confidence: f32,
    /// 그림자 골짜기 깊이(종이 밝기 대비 0~1)
    pub shadow_depth: f32,
    /// 글자 없는 가운데 띠 폭(이미지 폭 대비 0~1)
    pub blank_width: f32,
}

impl Gutter {
    pub fn angle_deg(&self, height: u32) -> f32 {
        ((self.x_bottom - self.x_top) / height as f32).atan().to_degrees()
    }

    pub fn center_x(&self) -> f32 {
        (self.x_top + self.x_bottom) / 2.0
    }
}

#[derive(Debug, Clone)]
pub enum Split {
    Spread { left: RgbImage, right: RgbImage, gutter: Gutter },
    /// 접힌 선을 못 찾았다. 원본을 한 페이지로 쓴다. gutter는 가장 그럴듯했던 후보(진단용)
    Single { gutter: Gutter },
}

/// 접힌 선 후보를 찾는다. 신뢰도와 상관없이 가장 그럴듯한 후보를 돌려준다
pub fn find_gutter(img: &RgbImage) -> Gutter {
    let (aw, ah) = fit_long_side(img.width(), img.height(), ANALYSIS_LONG_SIDE);
    let small = downscale_area(img, aw, ah);
    let g = luma(&small);
    let (w, h) = (g.width, g.height);
    let paper = percentile(&g.data, 0.9).max(1.0);

    // 기울기마다 기울어진 세로선을 따라 평균 밝기(그림자 골짜기)와 잉크 비율(빈 띠)을 낸다
    let tilts: Vec<f32> = {
        let n = (MAX_TILT_DEG / TILT_STEP_DEG) as i32;
        (-n..=n).map(|i| i as f32 * TILT_STEP_DEG).collect()
    };
    let smooth_r = (w / 150).max(2);
    let ink_level = paper * 0.6;
    let candidates: Vec<Candidate> = tilts
        .par_iter()
        .map(|&deg| {
            let tan = deg.to_radians().tan();
            let profile = smooth(&tilted_profile(&g, tan, |v| v), smooth_r);
            let valley = find_valley(&profile, w);
            let ink = tilted_profile(&g, tan, |v| if v < ink_level { 1.0 } else { 0.0 });
            let gap = widest_blank(&ink, w);
            let valley_conf = ((valley.depth / paper - 0.03) / 0.12).clamp(0.0, 1.0);
            let blank_conf = (((gap.1 - gap.0) as f32 / w as f32 - 0.03) / 0.07).clamp(0.0, 1.0) * 0.8;
            Candidate { deg, valley, gap, valley_conf, blank_conf }
        })
        .collect();
    // 두 단서 중 강한 쪽으로 고르고, 같으면 덜 기운 쪽. 골짜기 점수로 한 번 더 가른다
    let rank = |c: &Candidate| (c.valley_conf.max(c.blank_conf), c.valley.score, -c.deg.abs());
    let best = candidates
        .iter()
        .max_by(|a, b| {
            let (ra, rb) = (rank(a), rank(b));
            ra.0.total_cmp(&rb.0).then(ra.1.total_cmp(&rb.1)).then(ra.2.total_cmp(&rb.2))
        })
        .copied()
        .expect("기울기 후보가 있다");
    let Candidate { deg, valley, gap: (gap_start, gap_end), valley_conf, blank_conf } = best;
    let shadow_depth = valley.depth / paper;
    let blank_width = (gap_end - gap_start) as f32 / w as f32;

    let aspect = w as f32 / h as f32;
    // 펼친 책은 가로가 길다(보통 1.3~1.5). 세로로 긴 사진은 한 페이지일 가능성이 크다
    let aspect_factor = ((aspect - 0.75) / 0.35).clamp(0.2, 1.0);
    let mut confidence = valley_conf.max(blank_conf) * aspect_factor;

    // 접힌 선 그림자는 두 페이지 여백 사이에 있다. 빈 띠가 뚜렷한데 골짜기가 그 밖이면(글자 덩어리 사이 골 등) 띠를 믿는다
    let in_band = valley.x >= gap_start && valley.x < gap_end;
    let center_small = if blank_conf >= 0.3 && !in_band {
        (gap_start + gap_end) as f32 / 2.0
    } else {
        valley.x as f32
    };
    // 접은 자국이 보이면 그 선을 믿는다. 그림자 골짜기와 빈 띠는 표의 칸 사이에도 생겨 실제 문서에서 표를 잘랐다
    let (deg, center_small) = match find_center_crease(&g, paper) {
        Some((crease_deg, crease_x)) => {
            confidence = confidence.max(CREASE_CONFIDENCE * aspect_factor);
            (crease_deg, crease_x)
        }
        None => {
            // 자국 없이 가운데에서 많이 벗어난 후보는 나누지 않는다. 잘못 자르면 내용이 망가지고, 안 나누면 한 장으로 남을 뿐이다
            let at = center_small / w as f32;
            if !(CENTER_RANGE.0..=CENTER_RANGE.1).contains(&at) || (at - 0.5).abs() > MAX_OFF_CENTER_WITHOUT_CREASE {
                confidence = confidence.min(MIN_CONFIDENCE * 0.5);
            }
            (deg, center_small)
        }
    };
    let scale = img.width() as f32 / w as f32;
    let tan = deg.to_radians().tan();
    let half = img.height() as f32 / 2.0;
    let cx = (center_small + 0.5) * scale - 0.5;
    Gutter {
        x_top: cx - half * tan,
        x_bottom: cx + half * tan,
        confidence,
        shadow_depth,
        blank_width,
    }
}

/// 접힌 선을 찾아 두 페이지로 나눈다. 신뢰도가 낮으면 나누지 않는다
pub fn split_spread(img: &RgbImage) -> Split {
    let gutter = find_gutter(img);
    let gutter = if gutter.confidence >= MIN_CONFIDENCE {
        gutter
    } else if let Some(seam) = centered_seam(img) {
        seam
    } else {
        return Split::Single { gutter };
    };
    let (left, right) = cut(img, &gutter);
    Split::Spread { left, right, gutter }
}

/// 이보다 가로로 길어야 그림자도 빈 띠도 없는 펼침을 가운데에서 나눈다. 한 쪽만 찍은 표지(세로로 긴 사진)는 나누지 않는다
const SEAM_MIN_ASPECT: f32 = 1.2;
/// 경계선을 찾는 범위(가운데에서 폭 대비)
const SEAM_REACH: f32 = 0.03;
/// 펼침 판단 신뢰도. MIN_CONFIDENCE 바로 위다(진단용)
const SEAM_CONFIDENCE: f32 = 0.4;

/// 접힌 선 그림자도 빈 띠도 없는 펼침(코팅지 팜플렛을 눌러 찍음)의 이음매. 꼭짓점으로 원근을 편 펼침은 이음매가 가운데 가까이 있다.
/// 가운데 ±3% 안에서 위아래로 길게 이어지는 세로 경계(두 페이지 바탕색이 바뀌는 단, 가는 선)를 고른다. 없으면 나누지 않는다.
/// 2026-11-08 데이터센터 박람회_10 2쪽: 접힌 선은 0.50인데 그림자가 없고, 왼쪽 사진 경계(0.38)가 더 깊은 골이라 나누지 못했다.
/// 가운데 밖으로 찾지 않는 이유는 그 사진 경계 같은 골을 접힌 선으로 잘못 골라 내용을 자르기 때문이다
fn centered_seam(img: &RgbImage) -> Option<Gutter> {
    if (img.width() as f32) < img.height() as f32 * SEAM_MIN_ASPECT {
        return None;
    }
    let (aw, ah) = fit_long_side(img.width(), img.height(), ANALYSIS_LONG_SIDE);
    let g = luma(&downscale_area(img, aw, ah));
    let (w, h) = (g.width, g.height);
    let lo = ((0.5 - SEAM_REACH) * w as f32) as usize;
    let hi = ((0.5 + SEAM_REACH) * w as f32).ceil() as usize;
    // 열마다 "양옆과 다른 행"의 비율. 이음매는 위에서 아래까지 이어지고, 글자나 그림 경계는 일부 행에만 있다
    let score = |x: usize| {
        let mut hits = 0usize;
        for y in 0..h {
            let at = |dx: isize| g.data[y * w + (x as isize + dx) as usize];
            let (l, c, r) = (at(-2), at(0), at(2));
            if (l - r).abs() > 6.0 || (l.min(r) - c) > 4.0 {
                hits += 1;
            }
        }
        hits as f32 / h as f32
    };
    let scores: Vec<(usize, f32)> = (lo.max(2)..hi.min(w - 2)).map(|x| (x, score(x))).collect();
    let mut sorted: Vec<f32> = scores.iter().map(|s| s.1).collect();
    sorted.sort_by(f32::total_cmp);
    let median = sorted.get(sorted.len() / 2).copied().unwrap_or(0.0);
    let (best_x, best) = scores.iter().copied().max_by(|a, b| a.1.total_cmp(&b.1))?;
    // 뚜렷한 세로 경계가 없으면 나누지 않는다. 단서 없이 가운데를 자르면 가로로 긴 한 쪽 그림을 망친다
    if best < 0.5 || best < median * 1.5 {
        return None;
    }
    let x = best_x as f32;
    let cx = (x + 0.5) * (img.width() as f32 / w as f32) - 0.5;
    Some(Gutter {
        x_top: cx,
        x_bottom: cx,
        confidence: SEAM_CONFIDENCE,
        shadow_depth: 0.0,
        blank_width: 0.0,
    })
}

/// 접힌 선이 세로가 되게 돌린 뒤 그 선에서 자른다
pub fn cut(img: &RgbImage, gutter: &Gutter) -> (RgbImage, RgbImage) {
    let (w, h) = (img.width(), img.height());
    let xc = gutter.center_x().round().clamp(1.0, (w - 1) as f32) as u32;
    let theta = ((gutter.x_bottom - gutter.x_top) / h as f32).atan();
    if theta.abs() < 0.1f32.to_radians() {
        let left = image::imageops::crop_imm(img, 0, 0, xc, h).to_image();
        let right = image::imageops::crop_imm(img, xc, 0, w - xc, h).to_image();
        return (left, right);
    }
    let (sin, cos) = theta.sin_cos();
    let cy = h as f32 / 2.0;
    let raw = img.as_raw();
    let render = |u0: u32, width: u32| -> RgbImage {
        let mut out = vec![0u8; (width * h * 3) as usize];
        out.par_chunks_mut((width * 3) as usize).enumerate().for_each(|(v, row)| {
            let dv = v as f32 - cy;
            for (i, px) in row.as_chunks_mut::<3>().0.iter_mut().enumerate() {
                let du = (u0 + i as u32) as f32 - xc as f32;
                let x = xc as f32 + du * cos + dv * sin;
                let y = cy - du * sin + dv * cos;
                let c = sample_rgb(raw, w as usize, h as usize, x, y);
                for k in 0..3 {
                    px[k] = c[k].round() as u8;
                }
            }
        });
        RgbImage::from_raw(width, h, out).expect("크기 계산이 맞다")
    };
    (render(0, xc), render(xc, w - xc))
}

#[derive(Debug, Clone, Copy)]
struct Candidate {
    deg: f32,
    valley: Valley,
    gap: (usize, usize),
    valley_conf: f32,
    blank_conf: f32,
}

#[derive(Debug, Clone, Copy)]
struct Valley {
    x: usize,
    depth: f32,
    score: f32,
}

/// 기울어진 세로선(위아래 10%는 뺀다)을 따라 값을 평균한 단면. 단면 x는 이미지 가운데 높이 기준
fn tilted_profile(g: &Plane, tan: f32, f: impl Fn(f32) -> f32 + Sync) -> Vec<f32> {
    let (w, h) = (g.width, g.height);
    let y0 = h / 10;
    let y1 = h - h / 10;
    let cy = h as f32 / 2.0;
    (0..w)
        .into_par_iter()
        .map(|x| {
            let mut acc = 0.0;
            let mut n = 0u32;
            for y in y0..y1 {
                let sx = (x as f32 + (y as f32 - cy) * tan).round();
                if sx < 0.0 || sx >= w as f32 {
                    continue;
                }
                acc += f(g.get(sx as usize, y));
                n += 1;
            }
            if n == 0 { f32::NAN } else { acc / n as f32 }
        })
        .collect()
}

fn smooth(p: &[f32], r: usize) -> Vec<f32> {
    (0..p.len())
        .map(|i| {
            let a = i.saturating_sub(r);
            let b = (i + r + 1).min(p.len());
            let vals: Vec<f32> = p[a..b].iter().copied().filter(|v| v.is_finite()).collect();
            if vals.is_empty() { f32::NAN } else { vals.iter().sum::<f32>() / vals.len() as f32 }
        })
        .collect()
}

/// 가운데 근처(25~75%)에서 양옆 어깨보다 가장 깊이 파인 곳. 가운데에서 멀수록 점수를 깎는다
fn find_valley(p: &[f32], w: usize) -> Valley {
    let reach = (w as f32 * 0.15) as usize;
    let mut best = Valley { x: w / 2, depth: 0.0, score: f32::MIN };
    for x in (w / 4)..(w * 3 / 4) {
        let v = p[x];
        if !v.is_finite() {
            continue;
        }
        let left = p[x.saturating_sub(reach)..x].iter().copied().filter(|v| v.is_finite()).fold(f32::MIN, f32::max);
        let right = p[x + 1..(x + reach + 1).min(w)].iter().copied().filter(|v| v.is_finite()).fold(f32::MIN, f32::max);
        let depth = (left.min(right) - v).max(0.0);
        let off_center = (x as f32 - w as f32 / 2.0).abs() / w as f32;
        let score = depth * (1.0 - off_center);
        if score > best.score {
            best = Valley { x, depth, score };
        }
    }
    best
}

/// 가운데(30~70%)를 지나는 잉크 없는 가장 넓은 구간 [start, end)
fn widest_blank(ink: &[f32], w: usize) -> (usize, usize) {
    let blank = |x: usize| ink[x].is_finite() && ink[x] < 0.003;
    let (lo, hi) = (w * 3 / 10, w * 7 / 10);
    let mut best = (w / 2, w / 2);
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
        // 가운데 범위와 겹치는 띠만 본다. 바깥 여백은 빈 띠여도 접힌 선이 아니다
        let overlaps = start < hi && x > lo;
        let inner = start > 0 && x < w;
        if overlaps && inner && x - start > best.1 - best.0 {
            best = (start, x);
        }
    }
    best
}

pub(crate) fn percentile(data: &[f32], q: f32) -> f32 {
    let mut v: Vec<f32> = data.iter().step_by(7).copied().collect();
    if v.is_empty() {
        return 0.0;
    }
    let k = ((v.len() - 1) as f32 * q) as usize;
    v.select_nth_unstable_by(k, f32::total_cmp);
    v[k]
}

#[cfg(test)]
mod tests {

    #[test]
    fn 그림자도_빈_띠도_없는_가로_펼침은_가운데_이음매에서_나눈다() {
        // 왼쪽 0~38%는 어두운 사진, 나머지는 밝은 바탕. 두 페이지 바탕이 50%에서 살짝 달라진다
        let img = RgbImage::from_fn(1400, 1000, |x, _| {
            let v = if x < 532 { 40 } else if x < 700 { 235 } else { 225 };
            image::Rgb([v, v, v])
        });
        match split_spread(&img) {
            Split::Spread { gutter, .. } => {
                let at = (gutter.x_top + gutter.x_bottom) / 2.0 / img.width() as f32;
                assert!((at - 0.5).abs() < 0.01, "이음매 {at}");
            }
            Split::Single { gutter } => panic!("나누지 못함: {gutter:?}"),
        }
    }

    #[test]
    fn 세로로_긴_한_쪽_사진은_가운데에서_자르지_않는다() {
        let img = RgbImage::from_fn(700, 1000, |_, _| image::Rgb([230, 230, 230]));
        assert!(matches!(split_spread(&img), Split::Single { .. }));
    }

    use super::*;
    use image::Rgb;

    /// 글자 대신 검은 막대를 늘어놓은 펼친 책. 가운데에 그림자를 넣는다
    fn spread(w: u32, h: u32, gutter: f32, tilt_deg: f32, shadow: f32) -> RgbImage {
        let tan = tilt_deg.to_radians().tan();
        RgbImage::from_fn(w, h, |x, y| {
            let gx = gutter + (y as f32 - h as f32 / 2.0) * tan;
            let d = (x as f32 - gx).abs();
            let shade = 1.0 - shadow * (-d * d / (2.0 * (w as f32 * 0.02).powi(2))).exp();
            let page_w = w as f32 / 2.0;
            let local = if (x as f32) < gx { x as f32 } else { x as f32 - gx };
            let in_text = local > page_w * 0.12 && local < page_w * 0.88 && y > h / 10 && y < h * 9 / 10;
            // 낱말 사이 빈칸은 줄마다 어긋난다
            let line = (y / 12) % 2 == 0 && ((x + (y / 24) * 23) / 9) % 6 != 0;
            let base = if in_text && line { 40.0 } else { 235.0 };
            let v = (base * shade) as u8;
            Rgb([v, v, (v as f32 * 0.97) as u8])
        })
    }

    #[test]
    fn 그림자_있는_펼친_책을_나눈다() {
        let img = spread(1400, 1000, 690.0, 0.0, 0.45);
        let Split::Spread { left, right, gutter } = split_spread(&img) else { panic!("나눠야 한다") };
        assert!((gutter.center_x() - 690.0).abs() < 12.0, "{gutter:?}");
        assert_eq!(left.width() + right.width(), 1400);
    }

    #[test]
    fn 기울어진_접힌_선을_찾는다() {
        let img = spread(1400, 1000, 720.0, 4.0, 0.4);
        let g = find_gutter(&img);
        assert!(g.confidence >= MIN_CONFIDENCE, "{g:?}");
        assert!((g.angle_deg(1000) - 4.0).abs() <= 1.0, "기울기 {}", g.angle_deg(1000));
        assert!((g.center_x() - 720.0).abs() < 15.0, "{g:?}");
    }

    #[test]
    fn 그림자가_없어도_빈_띠로_찾는다() {
        let img = spread(1400, 1000, 700.0, 0.0, 0.0);
        let g = find_gutter(&img);
        assert!(g.confidence >= MIN_CONFIDENCE, "{g:?}");
        assert!((g.center_x() - 700.0).abs() < 40.0, "{g:?}");
    }

    #[test]
    fn 한_페이지는_나누지_않는다() {
        // 세로로 긴 한 페이지, 가운데까지 글자가 이어진다
        let img = RgbImage::from_fn(700, 1000, |x, y| {
            let in_text = x > 80 && x < 620 && y > 100 && y < 900;
            let line = (y / 12) % 2 == 0 && ((x + (y / 24) * 23) / 9) % 6 != 0;
            if in_text && line { Rgb([40, 40, 40]) } else { Rgb([235, 235, 230]) }
        });
        assert!(matches!(split_spread(&img), Split::Single { .. }));
    }
}
