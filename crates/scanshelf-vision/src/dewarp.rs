//! 곡면 보정. UVDoc 모델이 낮은 해상도(488x712)에서 역방향 격자(출력 각 점이 원본 어디에서 왔는지, 45x31)를
//! 예측하면, 격자를 출력 해상도로 늘려 원본 고해상도 사진에서 직접 표본을 뜬다. 모델 해상도로 펴고 늘리는 방식보다
//! 글자가 훨씬 선명하다.
//!
//! - `Dewarp::Normal`: 한 번
//! - `Dewarp::Strong`(vFlat "더 펴기"): 한 번 편 결과를 다시 모델에 넣어 남은 굽음을 잡고, 두 격자를 합성해 원본에서 한 번만 표본을 뜬다
//! - `Dewarp::Off` 또는 모델 없음: 그대로
//!
//! 모델은 바탕 위에 놓인 페이지 사진을 보고 페이지 경계를 찾도록 학습됐다. 꼭짓점으로 이미 잘라 낸 페이지를
//! 그대로 넣으면 글자 덩어리 가장자리를 경계로 보고 둘레를 4~25% 잘라 낸다(실측, docs/vision.md).
//! 그래서 `Framing::Page`는 둘레에 어두운 바탕을 붙여 예측한다. `Framing::Photo`(꼭짓점 없이 사진 전체)는
//! 그대로 넣어 모델이 페이지 경계도 찾게 둔다.
//!
//! 출력 크기는 격자 가로줄과 세로줄의 원본 위 길이로 정한다. 굽은 페이지를 펴면 원래 가로세로 비가 돌아온다.
//!
//! @see docs/vision.md

use image::RgbImage;
use scanshelf_core::processing::Dewarp;
use ort::value::TensorRef;
use rayon::prelude::*;

use crate::VisionError;
use crate::models::{ModelId, Models};
use crate::util::{downscale_area, sample_rgb};

pub const MODEL_W: usize = 488;
pub const MODEL_H: usize = 712;
const GRID_W: usize = 31;
const GRID_H: usize = 45;
/// 더 펴기 2차 예측 때 둘레에 붙이는 바탕(이미지 크기 대비)과 색. 책상 같은 어두운 바탕을 흉내 낸다
const PAD_FRACTION: f32 = 0.06;
const PAD_COLOR: [u8; 3] = [64, 60, 56];

/// 곡면 보정에 넣는 이미지가 어떤 모양인지
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    /// 꼭짓점으로 잘라 낸 페이지. 화면을 페이지가 꽉 채운다
    Page,
    /// 바탕 위에 페이지가 놓인 사진(꼭짓점 없음)
    Photo,
}

#[derive(Debug, Clone)]
pub struct DewarpResult {
    pub image: RgbImage,
    /// 모델로 폈는지. 꺼져 있거나 모델이 없으면 false이고 image는 입력 그대로
    pub applied: bool,
    /// 모델을 돌린 횟수(0, 1, 2)
    pub passes: u8,
}

/// 역방향 격자. 각 점은 원본의 정규화 좌표(-1~1, 양 끝 화소 중심이 -1과 1)
#[derive(Debug, Clone)]
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    /// (x, y) 쌍, 행 우선
    pub points: Vec<[f32; 2]>,
}

impl Grid {
    /// 격자 좌표(0..cols-1, 0..rows-1)에서 양선형 보간
    #[inline]
    pub fn sample(&self, gx: f32, gy: f32) -> [f32; 2] {
        let gx = gx.clamp(0.0, (self.cols - 1) as f32);
        let gy = gy.clamp(0.0, (self.rows - 1) as f32);
        let x0 = (gx as usize).min(self.cols - 2);
        let y0 = (gy as usize).min(self.rows - 2);
        let fx = gx - x0 as f32;
        let fy = gy - y0 as f32;
        let p = |x: usize, y: usize| self.points[y * self.cols + x];
        let (a, b, c, d) = (p(x0, y0), p(x0 + 1, y0), p(x0, y0 + 1), p(x0 + 1, y0 + 1));
        let mut out = [0.0; 2];
        for k in 0..2 {
            let top = a[k] * (1.0 - fx) + b[k] * fx;
            let bottom = c[k] * (1.0 - fx) + d[k] * fx;
            out[k] = top * (1.0 - fy) + bottom * fy;
        }
        out
    }

    /// 정규화 출력 좌표(0~1)에서 원본 정규화 좌표
    #[inline]
    fn at_unit(&self, u: f32, v: f32) -> [f32; 2] {
        self.sample(u * (self.cols - 1) as f32, v * (self.rows - 1) as f32)
    }

    /// 격자를 원본 화소 단위로 늘였을 때 가로줄과 세로줄 평균 길이
    fn arc_lengths(&self, width: u32, height: u32) -> (f32, f32) {
        let sx = (width - 1) as f32 / 2.0;
        let sy = (height - 1) as f32 / 2.0;
        let dist = |a: [f32; 2], b: [f32; 2]| ((a[0] - b[0]) * sx).hypot((a[1] - b[1]) * sy);
        let mut row_len = 0.0;
        for y in 0..self.rows {
            for x in 1..self.cols {
                row_len += dist(self.points[y * self.cols + x - 1], self.points[y * self.cols + x]);
            }
        }
        let mut col_len = 0.0;
        for x in 0..self.cols {
            for y in 1..self.rows {
                col_len += dist(self.points[(y - 1) * self.cols + x], self.points[y * self.cols + x]);
            }
        }
        (row_len / self.rows as f32, col_len / self.cols as f32)
    }
}

pub fn dewarp(img: &RgbImage, mode: Dewarp, framing: Framing, models: &Models) -> Result<DewarpResult, VisionError> {
    let passthrough = || DewarpResult { image: img.clone(), applied: false, passes: 0 };
    if mode == Dewarp::Off || img.width() < 32 || img.height() < 32 {
        return Ok(passthrough());
    }
    if !models.available(ModelId::UvDoc) {
        return Ok(passthrough());
    }
    let mut grid = match framing {
        Framing::Page => predict_padded(img, models)?,
        Framing::Photo => predict(img, models)?,
    };
    let mut passes = 1;
    if mode == Dewarp::Strong {
        // 중간 결과는 모델 입력의 두 배 해상도면 충분하다. 최종 표본은 원본에서 뜬다
        let mid = remap(img, &grid, (MODEL_W * 2) as u32, (MODEL_H * 2) as u32);
        let second = predict_padded(&mid, models)?;
        grid = compose(&grid, &second, 2);
        passes = 2;
    }
    let (w, h) = output_size(&grid, img.width(), img.height());
    Ok(DewarpResult { image: remap(img, &grid, w, h), applied: true, passes })
}

/// 모델로 역방향 격자를 예측한다
pub fn predict(img: &RgbImage, models: &Models) -> Result<Grid, VisionError> {
    let loaded = models
        .get(ModelId::UvDoc)
        .ok_or_else(|| VisionError::Model("UVDoc 모델을 불러오지 못했습니다".into()))?;
    let small = downscale_area(img, MODEL_W as u32, MODEL_H as u32);
    let plane = MODEL_W * MODEL_H;
    let mut input = vec![0f32; 3 * plane];
    for (i, p) in small.as_raw().as_chunks::<3>().0.iter().enumerate() {
        for c in 0..3 {
            input[c * plane + i] = p[c] as f32 / 255.0;
        }
    }
    let err = |e: ort::Error| VisionError::Model(format!("UVDoc 실행 실패: {e}"));
    let tensor = TensorRef::from_array_view(([1usize, 3, MODEL_H, MODEL_W], &input[..])).map_err(err)?;
    let mut session = loaded.session.lock().map_err(|_| VisionError::Model("세션 잠금 실패".into()))?;
    let outputs = session.run(ort::inputs!["image" => tensor]).map_err(err)?;
    let (shape, data) = outputs["grid"].try_extract_tensor::<f32>().map_err(err)?;
    if shape.iter().product::<i64>() as usize != 2 * GRID_W * GRID_H {
        return Err(VisionError::Model(format!("UVDoc 출력 모양이 다릅니다: {shape:?}")));
    }
    let n = GRID_W * GRID_H;
    let points = (0..n).map(|i| [data[i], data[n + i]]).collect();
    Ok(Grid { cols: GRID_W, rows: GRID_H, points })
}

/// 둘레에 어두운 바탕을 붙여 예측하고 격자를 원래 이미지 좌표로 되돌린다.
/// 한 번 편 결과는 페이지가 화면을 꽉 채워서, 그대로 넣으면 모델이 글자 덩어리 가장자리를 페이지 경계로 보고
/// 여백을 잘라 낸다. 촬영 사진처럼 바탕을 둘러 주면 경계를 제대로 잡는다
pub fn predict_padded(img: &RgbImage, models: &Models) -> Result<Grid, VisionError> {
    let (w, h) = img.dimensions();
    let px = ((w as f32 * PAD_FRACTION).round() as u32).max(1);
    let py = ((h as f32 * PAD_FRACTION).round() as u32).max(1);
    let mut padded = RgbImage::from_pixel(w + 2 * px, h + 2 * py, image::Rgb(PAD_COLOR));
    image::imageops::replace(&mut padded, img, px as i64, py as i64);
    let grid = predict(&padded, models)?;
    let (pw, ph) = ((padded.width() - 1) as f32, (padded.height() - 1) as f32);
    let (iw, ih) = ((w - 1) as f32, (h - 1) as f32);
    let points = grid
        .points
        .iter()
        .map(|&[nx, ny]| {
            let x = (nx + 1.0) / 2.0 * pw - px as f32;
            let y = (ny + 1.0) / 2.0 * ph - py as f32;
            [x / iw * 2.0 - 1.0, y / ih * 2.0 - 1.0]
        })
        .collect();
    Ok(Grid { points, ..grid })
}

/// 두 번째 격자(중간 결과 기준)를 첫 격자에 합성한다. density배로 촘촘히 표본을 떠 합성 오차를 줄인다
pub fn compose(first: &Grid, second: &Grid, density: usize) -> Grid {
    let cols = (second.cols - 1) * density + 1;
    let rows = (second.rows - 1) * density + 1;
    let mut points = Vec::with_capacity(cols * rows);
    for y in 0..rows {
        for x in 0..cols {
            let [nx, ny] = second.at_unit(x as f32 / (cols - 1) as f32, y as f32 / (rows - 1) as f32);
            points.push(first.at_unit((nx + 1.0) / 2.0, (ny + 1.0) / 2.0));
        }
    }
    Grid { cols, rows, points }
}

/// 펴진 페이지 크기. 원본보다 화소 수가 늘지 않게 맞춘다
pub fn output_size(grid: &Grid, width: u32, height: u32) -> (u32, u32) {
    let (gw, gh) = grid.arc_lengths(width, height);
    // 길이는 양 끝 화소 중심 사이 거리라 한 화소를 더한다
    let (mut ow, mut oh) = ((gw + 1.0).max(16.0), (gh + 1.0).max(16.0));
    let limit = width as f32 * height as f32;
    if ow * oh > limit {
        let s = (limit / (ow * oh)).sqrt();
        ow *= s;
        oh *= s;
    }
    (ow.round() as u32, oh.round() as u32)
}

/// 격자를 따라 원본에서 표본을 떠 w x h 출력을 만든다. 행마다 격자 행을 먼저 보간해 화소당 계산을 줄인다
pub fn remap(img: &RgbImage, grid: &Grid, w: u32, h: u32) -> RgbImage {
    let (sw, sh) = (img.width() as usize, img.height() as usize);
    let raw = img.as_raw();
    let sx = (sw - 1) as f32 / 2.0;
    let sy = (sh - 1) as f32 / 2.0;
    let (w, h) = (w as usize, h as usize);
    let mut out = vec![0u8; w * h * 3];
    out.par_chunks_mut(w * 3).enumerate().for_each(|(v, row)| {
        let gy = v as f32 / (h.max(2) - 1) as f32 * (grid.rows - 1) as f32;
        let line: Vec<[f32; 2]> = (0..grid.cols).map(|gx| grid.sample(gx as f32, gy)).collect();
        for (u, px) in row.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let gx = u as f32 / (w.max(2) - 1) as f32 * (grid.cols - 1) as f32;
            let x0 = (gx as usize).min(grid.cols - 2);
            let f = gx - x0 as f32;
            let nx = line[x0][0] * (1.0 - f) + line[x0 + 1][0] * f;
            let ny = line[x0][1] * (1.0 - f) + line[x0 + 1][1] * f;
            let c = sample_rgb(raw, sw, sh, (nx + 1.0) * sx, (ny + 1.0) * sy);
            for k in 0..3 {
                px[k] = (c[k] + 0.5) as u8;
            }
        }
    });
    RgbImage::from_raw(w as u32, h as u32, out).expect("크기 계산이 맞다")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(cols: usize, rows: usize) -> Grid {
        let mut points = Vec::new();
        for y in 0..rows {
            for x in 0..cols {
                points.push([x as f32 / (cols - 1) as f32 * 2.0 - 1.0, y as f32 / (rows - 1) as f32 * 2.0 - 1.0]);
            }
        }
        Grid { cols, rows, points }
    }

    #[test]
    fn 항등_격자는_원본을_그대로_낸다() {
        let img = RgbImage::from_fn(120, 80, |x, y| image::Rgb([(x * 2) as u8, (y * 3) as u8, 7]));
        let g = identity(GRID_W, GRID_H);
        assert_eq!(output_size(&g, 120, 80), (120, 80));
        let out = remap(&img, &g, 120, 80);
        assert_eq!(out, img);
    }

    #[test]
    fn 합성은_항등을_보존한다() {
        let a = identity(GRID_W, GRID_H);
        let c = compose(&a, &a, 2);
        for (y, row) in c.points.chunks(c.cols).enumerate() {
            for (x, p) in row.iter().enumerate() {
                let ex = x as f32 / (c.cols - 1) as f32 * 2.0 - 1.0;
                let ey = y as f32 / (c.rows - 1) as f32 * 2.0 - 1.0;
                assert!((p[0] - ex).abs() < 1e-4 && (p[1] - ey).abs() < 1e-4);
            }
        }
    }

    #[test]
    fn 모델이_없으면_그대로_둔다() {
        let img = RgbImage::new(100, 100);
        let r = dewarp(&img, Dewarp::Strong, Framing::Page, &Models::none()).unwrap();
        assert!(!r.applied);
        assert_eq!(r.image, img);
    }
}
