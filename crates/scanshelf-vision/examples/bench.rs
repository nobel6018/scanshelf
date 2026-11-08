//! 1200만 화소 페이지 처리 시간을 단계별로 잰다.
//! `SCANSHELF_MODELS_DIR=~/leedo/scanshelf-models cargo run -p scanshelf-vision --release --example bench`
//!
//! @see docs/vision.md

use std::path::PathBuf;
use std::time::Instant;

use image::{Rgb, RgbImage};
use scanshelf_core::processing::Filter;
use scanshelf_vision::{Models, apply_filter, dewarp, inpaint};

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// 글줄 무늬와 그림자가 있는 4000x3000 세로 페이지(실제 사진 대신)
fn page() -> RgbImage {
    let (w, h) = (3000, 4000);
    RgbImage::from_fn(w, h, |x, y| {
        let shade = 0.6 + 0.4 * (x as f32 / w as f32);
        let line = (y / 40) % 3 == 0 && ((x + y / 120 * 37) / 30) % 6 != 0 && x > 300 && x < 2700;
        let v = if line { 30.0 } else { 240.0 } * shade;
        Rgb([v as u8, v as u8, (v * 0.95) as u8])
    })
}

fn main() {
    let dir = PathBuf::from(std::env::var_os("SCANSHELF_MODELS_DIR").expect("SCANSHELF_MODELS_DIR가 필요합니다"));
    let img = page();
    println!("입력 {}x{}, 코어 {}", img.width(), img.height(), rayon::current_num_threads());
    for (label, models) in [("CoreML 우선", Models::new(&dir)), ("CPU", Models::new(&dir).cpu_only())] {
        let t = Instant::now();
        let _ = dewarp::predict(&img, &models).unwrap();
        let first = ms(t);
        let mut predict = Vec::new();
        let mut remap = Vec::new();
        let mut filter = Vec::new();
        let mut total = Vec::new();
        for _ in 0..5 {
            let t0 = Instant::now();
            let grid = dewarp::predict(&img, &models).unwrap();
            predict.push(ms(t0));
            let t = Instant::now();
            let (w, h) = dewarp::output_size(&grid, img.width(), img.height());
            let flat = dewarp::remap(&img, &grid, w, h);
            remap.push(ms(t));
            if predict.len() == 1 {
                println!("  출력 {w}x{h}");
            }
            let t = Instant::now();
            let out = apply_filter(&flat, Filter::Enhanced);
            filter.push(ms(t));
            total.push(ms(t0));
            std::hint::black_box(out);
        }
        let med = |v: &mut Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        println!(
            "{label} ({:?}): 첫 호출 {first:.0} ms | 예측 {:.0} ms, 재표본 {:.0} ms, 보정 필터 {:.0} ms, 합계 {:.0} ms (중앙값 5회)",
            models.backend(scanshelf_vision::ModelId::UvDoc),
            med(&mut predict),
            med(&mut remap),
            med(&mut filter),
            med(&mut total)
        );
        let t = Instant::now();
        let _ = dewarp(&img, scanshelf_core::processing::Dewarp::Strong, scanshelf_vision::Framing::Page, &models).unwrap();
        println!("  더 펴기 전체 {:.0} ms", ms(t));
    }
    for f in [Filter::Grayscale, Filter::BlackWhite] {
        let t = Instant::now();
        std::hint::black_box(apply_filter(&img, f));
        println!("{f:?} {:.0} ms", ms(t));
    }
    // 손가락 크기(약 450x900) 구멍 하나 메우기
    let mut mask = image::GrayImage::new(img.width(), img.height());
    for y in 1800..2700 {
        for x in 0..450 {
            mask.put_pixel(x, y, image::Luma([255]));
        }
    }
    for (label, models) in [("LaMa", Models::new(&dir)), ("고전", Models::none())] {
        // 첫 호출은 세션 생성이 들어간다
        for run in ["첫 호출", "두 번째"] {
            let t = Instant::now();
            let (_, e) = inpaint(&img, &mask, &models).unwrap();
            println!("메우기 {label} {e:?} {run} {:.0} ms", ms(t));
        }
    }
}
