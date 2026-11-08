//! 조명 보정과 필터(원본, 보정, 흑백 회색조, 흑백 이진).
//!
//! 보정(Enhanced)은 종이 배경을 추정해 나눈다. 배경은 블록마다 밝은 화소의 색이고, 블록이 사진이나
//! 굵은 제목처럼 종이가 아닌 곳이면 주변 종이 블록에서 메워 넣는다. 채널마다 나누므로 그림자 제거와
//! 흰색 맞춤이 한 번에 된다. 사진 영역은 종이처럼 흰색까지 끌어올리면 밝은 부분이 하얗게 날아가므로
//! 배경의 일부만 보정하고 수준 맞춤도 하지 않는다(실제 팜플렛의 흰 로봇 사진에서 확인).
//!
//! 이진(BlackWhite)은 보정한 휘도에 Sauvola 적응 문턱을 쓴다. 지역 평균과 표준편차는 축소 영상에서
//! 구해 늘린다(문턱은 천천히 변하는 값이라 차이가 거의 없고 4800만 화소에서도 빠르다).
//!
//! @see docs/vision.md

use image::RgbImage;
use scanshelf_core::processing::Filter;
use rayon::prelude::*;

use crate::util::{Plane, box_blur, downscale_area, fit_long_side, luma, luma_of};

/// 긴 변 기준 배경 블록 수
const BACKGROUND_BLOCKS: u32 = 64;
/// 보정 뒤 수준 맞춤. 종이 잡티는 흰색으로, 잉크는 더 진하게
const BLACK_POINT: f32 = 22.0;
const WHITE_POINT: f32 = 246.0;
/// Sauvola 계수. 보정 뒤라 배경이 평평해 일반 문서보다 조금 낮게 둔다
/// 사진 영역에 쓰는 밝기 보정 비율. 0이면 그대로, 1이면 종이와 같이 끌어올린다
const PHOTO_GAIN: f32 = 0.35;
/// 블록에서 이 비율 넘게 중간 톤이면 사진으로 본다
const PHOTO_MIN_MIDTONES: f32 = 0.45;
const PHOTO_CLOSE_BLOCKS: i64 = 2;
/// 긴 변 64블록 기준 이보다 작으면 사진으로 보지 않는다(글자 덩어리)
const PHOTO_MIN_BLOCKS: usize = 24;
const SAUVOLA_K: f32 = 0.18;
const SAUVOLA_R: f32 = 128.0;

/// 종이 바탕 블록이 이 비율보다 적으면 사진 페이지로 본다. 2026-11-08 실제 팜플렛 60여 쪽에서
/// 사진이 꽉 찬 표지는 0.00~0.19, 글과 표가 있는 쪽은 0.38 이상이었다
pub const PHOTO_PAGE_MAX_PAPER: f32 = 0.3;
/// 사진 페이지의 수준 맞춤 한도. 어두운 표지를 억지로 밝히지 않는다
const PHOTO_PAGE_MAX_GAIN: f32 = 1.25;

pub fn apply_filter(img: &RgbImage, filter: Filter) -> RgbImage {
    match filter {
        Filter::Original => img.clone(),
        Filter::Enhanced => enhance(img),
        Filter::Grayscale => to_gray_rgb(&enhance(img)),
        Filter::BlackWhite => binarize(&enhance(img)),
    }
}

/// 종이 바탕이 거의 없는 페이지(사진이 꽉 찬 표지)인지. 축소본으로 빠르게 본다
pub fn is_photo_page(img: &RgbImage) -> bool {
    let (w, h) = fit_long_side(img.width(), img.height(), 1600);
    Background::estimate(&downscale_area(img, w, h)).paper_fraction < PHOTO_PAGE_MAX_PAPER
}

/// 사진 페이지 보정. 블록마다 나누면 사진 속 밝은 곳을 종이로 알고 색이 깨진다(2026-11-08 데이터센터 박람회_9 표지).
/// 페이지 전체에 한 번만 흰색과 검은색 점을 맞추고, 끌어올리는 폭도 제한한다
fn enhance_photo(img: &RgbImage) -> RgbImage {
    let mut hist = [[0u32; 256]; 3];
    for p in img.pixels() {
        for c in 0..3 {
            hist[c][p[c] as usize] += 1;
        }
    }
    let n = img.width() * img.height();
    let levels: [(f32, f32); 3] = std::array::from_fn(|c| {
        let low = hist_percentile(&hist[c], n, 0.005) as f32;
        let high = (hist_percentile(&hist[c], n, 0.995) as f32).max(low + 1.0);
        let gain = (255.0 / (high - low)).min(PHOTO_PAGE_MAX_GAIN);
        (low.min(24.0), gain)
    });
    let mut out = img.clone();
    for p in out.pixels_mut() {
        for c in 0..3 {
            let (low, gain) = levels[c];
            p[c] = ((p[c] as f32 - low) * gain).clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// 종이 배경 추정. 블록 중심 격자에 RGB를 담고 양선형으로 읽는다
pub struct Background {
    cols: usize,
    rows: usize,
    block: f32,
    /// 채널별 격자
    grid: [Plane; 3],
    /// 사진 같은 영역(0~1). 종이로 인정되지 않은 블록을 넓혀 부드럽게 한 값
    photo: Plane,
    /// 종이 바탕으로 인정된 블록 비율(채도까지 보는 엄격한 기준). 사진이 꽉 찬 표지는 낮다
    pub paper_fraction: f32,
}

impl Background {
    pub fn estimate(img: &RgbImage) -> Background {
        let (w, h) = (img.width() as usize, img.height() as usize);
        let block = (w.max(h) as f32 / BACKGROUND_BLOCKS as f32).max(8.0);
        let cols = (w as f32 / block).ceil() as usize;
        let rows = (h as f32 / block).ceil() as usize;
        let raw = img.as_raw();

        // 블록마다 (밝은 화소 평균색, 중앙값/상위값 비)
        let stats: Vec<([f32; 3], f32, f32, f32)> = (0..rows * cols)
            .into_par_iter()
            .map(|i| {
                let (bx, by) = (i % cols, i / cols);
                let x0 = (bx as f32 * block) as usize;
                let x1 = (((bx + 1) as f32 * block) as usize).min(w);
                let y0 = (by as f32 * block) as usize;
                let y1 = (((by + 1) as f32 * block) as usize).min(h);
                let mut hist = [0u32; 256];
                for y in y0..y1 {
                    for p in raw[(y * w + x0) * 3..(y * w + x1) * 3].as_chunks::<3>().0.iter() {
                        hist[luma_of([p[0], p[1], p[2]]) as usize] += 1;
                    }
                }
                let n: u32 = hist.iter().sum();
                let p50 = hist_percentile(&hist, n, 0.5);
                let p92 = hist_percentile(&hist, n, 0.92);
                let mut acc = [0f64; 3];
                let mut cnt = 0u32;
                for y in y0..y1 {
                    for p in raw[(y * w + x0) * 3..(y * w + x1) * 3].as_chunks::<3>().0.iter() {
                        if luma_of([p[0], p[1], p[2]]) as usize >= p92 {
                            acc[0] += p[0] as f64;
                            acc[1] += p[1] as f64;
                            acc[2] += p[2] as f64;
                            cnt += 1;
                        }
                    }
                }
                let c = cnt.max(1) as f64;
                let color = [(acc[0] / c) as f32, (acc[1] / c) as f32, (acc[2] / c) as f32];
                // 중간 톤 비율. 글자와 큰 제목은 종이와 잉크 두 덩어리라 낮고, 사진은 높다
                let (low, high) = ((p92 as f32 * 0.3) as usize, (p92 as f32 * 0.88) as usize);
                let mid: u32 = hist[low..=high.max(low)].iter().sum();
                (color, p50 as f32, p92 as f32, mid as f32 / n.max(1) as f32)
            })
            .collect();

        let paper = {
            let mut l: Vec<f32> = stats.iter().map(|s| s.2).collect();
            let k = (l.len() as f32 * 0.9) as usize;
            let k = k.min(l.len() - 1);
            l.select_nth_unstable_by(k, f32::total_cmp);
            l[k].max(1.0)
        };
        let is_paper = |s: &([f32; 3], f32, f32, f32), strict: bool| {
            let (c, p50, p92, mid) = *s;
            let chroma = c.iter().copied().fold(f32::MIN, f32::max) - c.iter().copied().fold(f32::MAX, f32::min);
            let uniform = p50 >= 0.72 * p92;
            let bright = p92 >= 0.25 * paper;
            uniform && bright && mid < PHOTO_MIN_MIDTONES && (!strict || chroma <= 45.0)
        };
        let mask = photo_mask(&stats.iter().map(|s| s.3 >= PHOTO_MIN_MIDTONES).collect::<Vec<_>>(), cols, rows);
        let mut valid: Vec<bool> = stats.iter().zip(&mask.data).map(|(s, m)| *m < 0.5 && is_paper(s, true)).collect();
        let paper_fraction = valid.iter().filter(|v| **v).count() as f32 / valid.len().max(1) as f32;
        // 색지 전체가 종이면 채도 조건으로는 하나도 안 남는다. 그때는 채도를 보지 않는다
        if valid.iter().filter(|v| **v).count() < valid.len() / 7 {
            valid = stats.iter().zip(&mask.data).map(|(s, m)| *m < 0.5 && is_paper(s, false)).collect();
        }
        if valid.iter().filter(|v| **v).count() == 0 {
            valid.iter_mut().for_each(|v| *v = true);
        }

        let grid = [0, 1, 2].map(|c| {
            let mut p = Plane::new(cols, rows, 0.0);
            for (i, s) in stats.iter().enumerate() {
                p.data[i] = s.0[c];
            }
            let filled = pull_push(&p, &valid);
            box_blur(&box_blur(&filled, 1), 1)
        });
        let photo = box_blur(&mask, 1);
        Background { cols, rows, block, grid, photo, paper_fraction }
    }

    /// 화소 (x, y)가 사진 영역인 정도(0~1)
    #[inline]
    pub fn photo_at(&self, x: f32, y: f32) -> f32 {
        let gx = ((x + 0.5) / self.block - 0.5).clamp(0.0, (self.cols - 1) as f32);
        let gy = ((y + 0.5) / self.block - 0.5).clamp(0.0, (self.rows - 1) as f32);
        self.photo.sample(gx, gy)
    }

    /// 화소 (x, y)의 배경색
    #[inline]
    pub fn at(&self, x: f32, y: f32) -> [f32; 3] {
        let gx = ((x + 0.5) / self.block - 0.5).clamp(0.0, (self.cols - 1) as f32);
        let gy = ((y + 0.5) / self.block - 0.5).clamp(0.0, (self.rows - 1) as f32);
        [self.grid[0].sample(gx, gy), self.grid[1].sample(gx, gy), self.grid[2].sample(gx, gy)]
    }
}

fn hist_percentile(hist: &[u32; 256], n: u32, q: f32) -> usize {
    let target = (n as f32 * q) as u32;
    let mut acc = 0;
    for (v, &c) in hist.iter().enumerate() {
        acc += c;
        if acc > target {
            return v;
        }
    }
    255
}

/// 비어 있는 칸을 둘레 값으로 메운다. 절반 크기로 줄여 가며 평균을 내고(pull), 다시 늘리며 빈 칸에 채운다(push)
pub(crate) fn pull_push(p: &Plane, valid: &[bool]) -> Plane {
    if valid.iter().all(|v| *v) || p.width * p.height <= 1 {
        return p.clone();
    }
    let (w, h) = (p.width, p.height);
    let (hw, hh) = (w.div_ceil(2), h.div_ceil(2));
    let mut coarse = Plane::new(hw, hh, 0.0);
    let mut coarse_valid = vec![false; hw * hh];
    for cy in 0..hh {
        for cx in 0..hw {
            let mut acc = 0.0;
            let mut n = 0;
            for y in (cy * 2)..(cy * 2 + 2).min(h) {
                for x in (cx * 2)..(cx * 2 + 2).min(w) {
                    if valid[y * w + x] {
                        acc += p.get(x, y);
                        n += 1;
                    }
                }
            }
            if n > 0 {
                coarse.set(cx, cy, acc / n as f32);
                coarse_valid[cy * hw + cx] = true;
            }
        }
    }
    if !coarse_valid.iter().any(|v| *v) {
        return p.clone();
    }
    let coarse = pull_push(&coarse, &coarse_valid);
    let mut out = p.clone();
    for y in 0..h {
        for x in 0..w {
            if !valid[y * w + x] {
                let v = coarse.sample((x as f32 + 0.5) / 2.0 - 0.5, (y as f32 + 0.5) / 2.0 - 0.5);
                out.set(x, y, v);
            }
        }
    }
    out
}

/// 사진 영역. 중간 톤 블록은 사진의 가장자리와 질감에 몰리고 매끈한 안쪽(흰 물체의 몸체)은 빠진다.
/// 안쪽을 종이로 보면 그 색을 흰색으로 끌어올려 사진이 날아가므로, 닫힘 연산으로 잇고 둘러싸인 곳을 채운다.
/// 글자 몇 블록이 사진으로 잡히지 않도록 작은 덩어리는 버린다
fn photo_mask(mid: &[bool], cols: usize, rows: usize) -> Plane {
    let at = |m: &[bool], x: i64, y: i64| x >= 0 && y >= 0 && (x as usize) < cols && (y as usize) < rows && m[y as usize * cols + x as usize];
    let morph = |m: &[bool], grow: bool| -> Vec<bool> {
        let r = PHOTO_CLOSE_BLOCKS;
        (0..rows as i64)
            .flat_map(|y| (0..cols as i64).map(move |x| (x, y)))
            .map(|(x, y)| {
                let mut window = (-r..=r).flat_map(|dy| (-r..=r).map(move |dx| (dx, dy)));
                if grow {
                    window.any(|(dx, dy)| at(m, x + dx, y + dy))
                } else {
                    // 이미지 밖은 채운 것으로 쳐서 가장자리에 닿은 사진이 깎이지 않게 한다
                    window.all(|(dx, dy)| {
                        let (nx, ny) = (x + dx, y + dy);
                        nx < 0 || ny < 0 || nx >= cols as i64 || ny >= rows as i64 || at(m, nx, ny)
                    })
                }
            })
            .collect()
    };
    let closed = morph(&morph(mid, true), false);
    // 바깥에서 닿지 못하는 빈 곳은 둘러싸인 안쪽이다
    let mut outside = vec![false; cols * rows];
    let mut stack: Vec<usize> = (0..cols * rows)
        .filter(|&i| (i % cols == 0 || i % cols == cols - 1 || i / cols == 0 || i / cols == rows - 1) && !closed[i])
        .collect();
    while let Some(i) = stack.pop() {
        if outside[i] || closed[i] {
            continue;
        }
        outside[i] = true;
        let (x, y) = (i % cols, i / cols);
        if x > 0 { stack.push(i - 1); }
        if x + 1 < cols { stack.push(i + 1); }
        if y > 0 { stack.push(i - cols); }
        if y + 1 < rows { stack.push(i + cols); }
    }
    let filled: Vec<bool> = (0..cols * rows).map(|i| closed[i] || !outside[i]).collect();
    // 작은 덩어리는 사진이 아니다
    let mut seen = vec![false; cols * rows];
    let mut plane = Plane::new(cols, rows, 0.0);
    for start in 0..cols * rows {
        if !filled[start] || seen[start] {
            continue;
        }
        let mut blob = vec![start];
        seen[start] = true;
        let mut k = 0;
        while k < blob.len() {
            let i = blob[k];
            k += 1;
            let (x, y) = (i % cols, i / cols);
            let mut visit = |n: usize| {
                if filled[n] && !seen[n] {
                    seen[n] = true;
                    blob.push(n);
                }
            };
            if x > 0 { visit(i - 1); }
            if x + 1 < cols { visit(i + 1); }
            if y > 0 { visit(i - cols); }
            if y + 1 < rows { visit(i + cols); }
        }
        if blob.len() >= PHOTO_MIN_BLOCKS {
            for i in blob {
                plane.data[i] = 1.0;
            }
        }
    }
    plane
}

fn luma_of_f32(c: [f32; 3]) -> f32 {
    (0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]).max(8.0)
}

/// 배경으로 나누고 수준을 맞춘다. 종이 바탕이 거의 없는 사진 페이지는 전체 수준만 맞춘다
pub fn enhance(img: &RgbImage) -> RgbImage {
    let bg = Background::estimate(img);
    if bg.paper_fraction < PHOTO_PAGE_MAX_PAPER {
        return enhance_photo(img);
    }
    let w = img.width() as usize;
    let mut out = img.clone();
    let scale = 255.0 / (WHITE_POINT - BLACK_POINT);
    out.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        for (x, p) in row.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let b = bg.at(x as f32, y as f32);
            let area = bg.photo_at(x as f32, y as f32);
            // 사진 블록 안이어도 종이처럼 밝은 화소는 종이로 다룬다. 사진 가장자리에 회색 띠가 남지 않는다
            let paperness = ((luma_of([p[0], p[1], p[2]]) / luma_of_f32(b) - 0.88) / 0.09).clamp(0.0, 1.0);
            let photo = area * (1.0 - paperness);
            for c in 0..3 {
                let value = p[c] as f32;
                let gain = 255.0 / b[c].max(8.0);
                let paper = (value * gain - BLACK_POINT) * scale;
                let picture = value * (1.0 + (gain - 1.0) * PHOTO_GAIN);
                p[c] = (paper * (1.0 - photo) + picture * photo).clamp(0.0, 255.0) as u8;
            }
        }
    });
    out
}

fn to_gray_rgb(img: &RgbImage) -> RgbImage {
    let mut out = img.clone();
    out.par_chunks_mut(3).for_each(|p| {
        let v = luma_of([p[0], p[1], p[2]]).round().min(255.0) as u8;
        p.copy_from_slice(&[v, v, v]);
    });
    out
}

/// Sauvola: T = m * (1 + k * (s / R - 1))
fn binarize(img: &RgbImage) -> RgbImage {
    let (w, h) = (img.width(), img.height());
    let (sw, sh) = fit_long_side(w, h, 1200);
    let small = luma(&downscale_area(img, sw, sh));
    let scale = w as f32 / sw as f32;
    // 창 크기는 원본 긴 변의 약 4%(글자 몇 줄)
    let r = ((sw.max(sh) as f32 * 0.02) as usize).max(4);
    let mean = box_blur(&small, r);
    let sq = Plane { width: small.width, height: small.height, data: small.data.iter().map(|v| v * v).collect() };
    let mean_sq = box_blur(&sq, r);
    let mut threshold = Plane::new(small.width, small.height, 0.0);
    for i in 0..threshold.data.len() {
        let m = mean.data[i];
        let s = (mean_sq.data[i] - m * m).max(0.0).sqrt();
        threshold.data[i] = m * (1.0 + SAUVOLA_K * (s / SAUVOLA_R - 1.0));
    }
    let raw = img.as_raw();
    let mut out = RgbImage::new(w, h);
    let wu = w as usize;
    out.par_chunks_mut(wu * 3).enumerate().for_each(|(y, row)| {
        let ty = (y as f32 + 0.5) / scale - 0.5;
        for (x, p) in row.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let i = (y * wu + x) * 3;
            let v = luma_of([raw[i], raw[i + 1], raw[i + 2]]);
            let t = threshold.sample((x as f32 + 0.5) / scale - 0.5, ty);
            let o = if v > t { 255 } else { 0 };
            p.copy_from_slice(&[o, o, o]);
        }
    });
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn 사진의_밝은_부분이_하얗게_날아가지_않는다() {
        // 회색빛 종이 위 큰 사진
        let img = RgbImage::from_fn(1200, 1600, |x, y| {
            if (200..1000).contains(&x) && (300..1300).contains(&y) {
                // 물체 안쪽은 매끈하고 종이보다 살짝 어둡다. 둘레는 질감이 있어 중간 톤이 많다
                let inner = (400..800).contains(&x) && (500..1100).contains(&y);
                let v = if inner { 168 } else { 70 + ((x * 37 + y * 53) % 90) as u8 };
                Rgb([v, v, v])
            } else {
                Rgb([185, 185, 180])
            }
        });

        let out = enhance(&img);

        let (mut clipped, mut total) = (0, 0);
        for y in 500..1100 {
            for x in 400..800 {
                total += 1;
                if out.get_pixel(x, y).0[1] >= 225 {
                    clipped += 1;
                }
            }
        }
        assert!(clipped * 100 / total < 5, "사진 안 날아간 화소 {clipped}/{total}");
        // 종이는 여전히 희게 올라간다
        assert!(out.get_pixel(50, 50).0[1] >= 240, "{:?}", out.get_pixel(50, 50));
    }

    use super::*;
    use image::Rgb;

    /// 왼쪽이 어두운 그림자와 노란 조명 아래의 줄글 페이지
    fn shaded_page(w: u32, h: u32) -> RgbImage {
        RgbImage::from_fn(w, h, |x, y| {
            let shade = 0.45 + 0.55 * (x as f32 / w as f32).sqrt();
            let line = (y / 10) % 3 == 0 && (x / 7) % 5 != 0 && x > 30 && x < w - 30;
            let base = if line { [30.0, 30.0, 30.0] } else { [235.0, 225.0, 190.0] };
            Rgb(base.map(|v: f32| (v * shade) as u8))
        })
    }

    fn paper_stats(img: &RgbImage) -> (f32, f32, f32) {
        // 글자 없는 줄의 밝기 평균과 표준편차, 파랑-빨강 차이
        let mut v = Vec::new();
        let mut cast = 0.0;
        for (_, y, p) in img.enumerate_pixels() {
            if (y / 10) % 3 == 1 {
                v.push(luma_of(p.0));
                cast += p.0[2] as f32 - p.0[0] as f32;
            }
        }
        let m = v.iter().sum::<f32>() / v.len() as f32;
        let s = (v.iter().map(|a| (a - m).powi(2)).sum::<f32>() / v.len() as f32).sqrt();
        (m, s, cast / v.len() as f32)
    }

    #[test]
    fn 보정은_그림자와_색을_걷어낸다() {
        let img = shaded_page(600, 800);
        let (m0, s0, c0) = paper_stats(&img);
        let out = apply_filter(&img, Filter::Enhanced);
        let (m1, s1, c1) = paper_stats(&out);
        assert!(m1 > 245.0 && m1 > m0, "종이 밝기 {m0} -> {m1}");
        assert!(s1 < s0 / 4.0, "종이 고르기 {s0} -> {s1}");
        assert!(c1.abs() < 6.0 && c0.abs() > 20.0, "색 치우침 {c0} -> {c1}");
    }

    #[test]
    fn 이진은_글자와_종이만_남긴다() {
        let img = shaded_page(600, 800);
        let out = apply_filter(&img, Filter::BlackWhite);
        let mut wrong = 0;
        let mut total = 0;
        for (x, y, p) in out.enumerate_pixels() {
            let line = (y / 10) % 3 == 0 && (x / 7) % 5 != 0 && x > 30 && x < 570;
            let gap = (y / 10) % 3 == 1;
            if line || gap {
                total += 1;
                let black = p.0[0] == 0;
                if black != line {
                    wrong += 1;
                }
            }
            assert!(p.0[0] == 0 || p.0[0] == 255);
        }
        assert!((wrong as f32) < total as f32 * 0.03, "틀린 화소 {wrong}/{total}");
    }

    #[test]
    fn 종이_바탕이_없는_사진_페이지는_전체_밝기만_맞춘다() {
        // 큰 색 덩어리가 이어진 사진. 블록마다 나누면 밝은 덩어리가 종이로 잡혀 하얗게 날아가고 색이 깨진다
        let img = RgbImage::from_fn(400, 560, |x, y| {
            let v = ((x as f32 * 0.05).sin() * 60.0 + (y as f32 * 0.03).cos() * 50.0 + 120.0) as u8;
            image::Rgb([v / 3, v / 2, v])
        });
        assert!(is_photo_page(&img));
        let out = apply_filter(&img, Filter::Enhanced);
        // 전체 수준 맞춤은 순서를 바꾸지 않는다: 원본에서 더 밝던 화소는 결과에서도 더 밝거나 같다
        let (a, b) = (img.get_pixel(10, 10)[2], img.get_pixel(200, 300)[2]);
        let (oa, ob) = (out.get_pixel(10, 10)[2], out.get_pixel(200, 300)[2]);
        assert_eq!(a.cmp(&b), oa.cmp(&ob));
    }

    #[test]
    fn 글이_있는_종이_페이지는_사진_페이지가_아니다() {
        assert!(!is_photo_page(&shaded_page(400, 560)));
    }

    #[test]
    fn 원본은_그대로다() {
        let img = shaded_page(64, 64);
        assert_eq!(apply_filter(&img, Filter::Original), img);
    }

    #[test]
    fn 빈_칸을_메운다() {
        let mut p = Plane::new(4, 4, 100.0);
        let mut valid = vec![true; 16];
        p.set(1, 1, 0.0);
        valid[5] = false;
        let f = pull_push(&p, &valid);
        assert!((f.get(1, 1) - 100.0).abs() < 1e-3);
    }
}
