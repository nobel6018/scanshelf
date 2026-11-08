//! 합성 페이지로 각 단계가 실제로 나아지게 하는지 잰다.
//! 모델이 필요한 시험은 SCANSHELF_MODELS_DIR이 있을 때만 돈다. 전후 이미지를 남기는 시험은 `--ignored`로 부른다.
//!
//! @see docs/vision.md

mod common;

use std::path::PathBuf;
use std::time::Instant;

use common::*;
use image::{Rgb, RgbImage};
use scanshelf_core::PageLayout;
use scanshelf_core::processing::{Dewarp, Filter, ProcessOptions};
use scanshelf_vision::split::MIN_CONFIDENCE;
use scanshelf_vision::util::luma_of;
use scanshelf_vision::*;

fn models() -> Option<Models> {
    let dir = std::env::var_os("SCANSHELF_MODELS_DIR")?;
    Some(Models::new(PathBuf::from(dir)))
}

/// 두 페이지를 붙이고 가운데 그림자를 넣은 뒤 살짝 돌린다
fn spread(seed: u64, tilt_deg: f32) -> (RgbImage, f32) {
    let (pw, ph) = (700, 1000);
    let left = render_page(pw, ph, seed);
    let right = render_page(pw, ph, seed + 1);
    let w = pw * 2;
    let theta = tilt_deg.to_radians();
    let (cx, cy) = (w as f32 / 2.0, ph as f32 / 2.0);
    let img = RgbImage::from_fn(w, ph, |x, y| {
        let (dx, dy) = (x as f32 - cx, y as f32 - cy);
        let sx = cx + dx * theta.cos() - dy * theta.sin();
        let sy = cy + dx * theta.sin() + dy * theta.cos();
        if sx < 0.0 || sy < 0.0 || sx >= (w - 1) as f32 || sy >= (ph - 1) as f32 {
            return Rgb(PAPER);
        }
        let (sxu, syu) = (sx as u32, sy as u32);
        let p = if sxu < pw { left.get_pixel(sxu, syu).0 } else { right.get_pixel(sxu - pw, syu).0 };
        let d = (sx - cx).abs();
        let s = 1.0 - 0.4 * (-d * d / (2.0 * 25.0f32.powi(2))).exp();
        Rgb(p.map(|v| (v as f32 * s) as u8))
    });
    // 회전 뒤 접힌 선은 가운데 높이에서 cx를 지난다
    (img, cx)
}

#[test]
fn 펼친_책을_두_페이지로_나눈다() {
    for (seed, tilt) in [(1, 0.0), (3, 2.5), (5, -3.0)] {
        let (img, cx) = spread(seed, tilt);
        let g = find_gutter(&img);
        assert!(g.confidence >= MIN_CONFIDENCE, "{seed}: {g:?}");
        assert!((g.center_x() - cx).abs() < 10.0, "{seed}: 위치 {g:?}");
        // 역방향 사상이 x = cx + (y - cy) tan(theta)인 선을 원본 접힌 선으로 보낸다
        assert!((g.angle_deg(img.height()) - tilt).abs() <= 1.0, "{seed}: 기울기 {}", g.angle_deg(img.height()));
    }
}

/// 표가 있는 펼친 면: 왼쪽 페이지 표에 칸 사이 빈 띠가 있고, 접은 자국은 가운데 가는 선과 밝기 차이뿐이다
fn spread_with_table(fold_x: u32) -> RgbImage {
    let (w, h) = (2000u32, 1400u32);
    RgbImage::from_fn(w, h, |x, y| {
        let paper = if x < fold_x { 205u8 } else { 235u8 };
        let in_table = (150..1250).contains(&y);
        // 칸 사이(760~860)만 비운 표. 칸 안은 글줄 무늬
        let cell = (100..760).contains(&x) || (860..fold_x.saturating_sub(60)).contains(&x) || (fold_x + 60..w - 100).contains(&x);
        if in_table && cell && y % 30 < 6 {
            return Rgb([40, 40, 40]);
        }
        if x.abs_diff(fold_x) <= 2 {
            return Rgb([150, 150, 150]);
        }
        Rgb([paper, paper, paper])
    })
}

#[test]
fn 표의_칸_사이가_아니라_접은_자국에서_나눈다() {
    let img = spread_with_table(1000);

    let g = find_gutter(&img);

    assert!(g.confidence >= MIN_CONFIDENCE, "{g:?}");
    assert!((g.center_x() - 1000.0).abs() < 12.0, "표 빈 칸(810)이 아니라 접은 자국(1000)이어야 한다: {g:?}");
}

#[test]
fn 자국이_없고_가운데에서_먼_빈_띠로는_나누지_않는다() {
    // 접은 자국 없이 왼쪽으로 치우친 넓은 빈 칸만 있는 사진
    let img = RgbImage::from_fn(2000, 1400, |x, y| {
        let filled = ((100..650).contains(&x) || (800..1900).contains(&x)) && (150..1250).contains(&y);
        if filled && y % 30 < 6 { Rgb([40, 40, 40]) } else { Rgb([235, 235, 235]) }
    });

    assert!(matches!(split_spread(&img), Split::Single { .. }));
}

#[test]
fn 한_페이지는_그대로_둔다() {
    let page = render_page(700, 1000, 9);
    let split = split_spread(&page);
    assert!(matches!(split, Split::Single { .. }), "{split:?}");
}

#[test]
fn 조명_보정은_그림자를_걷는다() {
    let page = render_page(800, 1100, 11);
    let shaded = curl(&page, 0.0, 0.45);
    let enhanced = apply_filter(&shaded, Filter::Enhanced);
    // 그림자가 진한 왼쪽 여백 띠와 밝은 오른쪽 여백 띠의 밝기 차
    let band = |img: &RgbImage, x0: u32, x1: u32| {
        let mut acc = 0.0;
        let mut n = 0;
        for y in img.height() / 4..img.height() * 3 / 4 {
            for x in x0..x1 {
                acc += luma_of(img.get_pixel(x, y).0);
                n += 1;
            }
        }
        acc / n as f32
    };
    let (w, m) = (shaded.width(), (shaded.width() as f32 * 0.05) as u32);
    let gap_before = band(&shaded, w - m - 60, w - m - 20) - band(&shaded, m + 5, m + 30);
    let gap_after = band(&enhanced, w - m - 60, w - m - 20) - band(&enhanced, m + 5, m + 30);
    assert!(gap_before > 60.0, "{gap_before}");
    assert!(gap_after.abs() < 15.0, "그림자 차 {gap_before} -> {gap_after}");
}

#[test]
fn 손가락을_찾아_지운다() {
    let mut page = render_page(800, 1100, 13);
    let t1 = add_finger(&mut page, true, 700.0, 110.0, 45.0);
    let t2 = add_finger(&mut page, false, 300.0, 95.0, 40.0);
    let mut truth = t1;
    for (a, b) in truth.iter_mut().zip(t2.iter()) {
        *a = (*a).max(*b);
    }
    let (mask, n) = detect_fingers(&page);
    assert_eq!(n, 2);
    let (hit, extra) = coverage(&mask, &truth);
    assert!(hit > 0.97 && extra < 0.02, "덮음 {hit}, 넘침 {extra}");
    let (out, _) = remove_fingers(&page, None, &Models::none()).unwrap();
    // 글자 화소가 섞여 회색 얼룩이 되지 않고 종이색으로 메워진다
    for (x, y) in [(20, 700), (60, 690), (780, 300)] {
        let l = luma_of(out.get_pixel(x, y).0);
        assert!(l > 225.0, "({x}, {y}) 밝기 {l}");
    }
    let (mask_after, n_after) = detect_fingers(&out);
    assert_eq!(n_after, 0, "남은 손 {:?}", coverage(&mask_after, &truth));
}

#[test]
fn 색_펜_필기를_지우고_글자는_남긴다() {
    let clean = render_page(800, 1100, 17);
    let mut page = clean.clone();
    add_scribble(&mut page, BLUE_PEN, 400.0, 25.0, 40.0, 2.0);
    add_scribble(&mut page, RED_PEN, 750.0, 15.0, 22.0, 1.5);
    let (out, report) = remove_handwriting(&page, &Models::none()).unwrap();
    assert!(report.pixels[0] > 1000 && report.pixels[1] > 500, "{report:?}");
    let colored = |img: &RgbImage| img.pixels().filter(|p| handwriting::classify(p.0.map(|v| v as f32)).is_some()).count();
    let before = colored(&page);
    let after = colored(&out);
    assert!((after as f32) < before as f32 * 0.02, "색 화소 {before} -> {after}");
    // 펜이 지나지 않은 곳의 글자는 그대로
    let mut ink = 0;
    let mut kept = 0;
    for (x, y, p) in clean.enumerate_pixels() {
        let far = !(360..440).contains(&y) && !(725..775).contains(&y);
        if far && luma_of(p.0) < 100.0 {
            ink += 1;
            if luma_of(out.get_pixel(x, y).0) < 100.0 {
                kept += 1;
            }
        }
    }
    assert!(kept as f32 > ink as f32 * 0.995, "글자 {kept}/{ink}");
}

#[test]
fn 모델로_곡면을_편다() {
    let Some(models) = models() else {
        eprintln!("SCANSHELF_MODELS_DIR 없음: 건너뜀");
        return;
    };
    let page = render_page(900, 1250, 21);
    let warped = curl(&page, 1.0, 0.3);
    let normal = dewarp(&warped, Dewarp::Normal, Framing::Photo, &models).unwrap();
    assert!(normal.applied);
    let strong = dewarp(&warped, Dewarp::Strong, Framing::Photo, &models).unwrap();
    let (s0, s1, s2) = (ssim(&warped, &page), ssim(&normal.image, &page), ssim(&strong.image, &page));
    let (l0, l1, l2) = (line_sharpness(&warped), line_sharpness(&normal.image), line_sharpness(&strong.image));
    eprintln!("SSIM 휜 {s0:.3} 보정 {s1:.3} 더펴기 {s2:.3} / 글줄 {l0:.3} {l1:.3} {l2:.3}");
    assert!(s1 > s0, "SSIM {s0} -> {s1}");
    assert!(l1 > l0, "글줄 {l0} -> {l1}");
}

/// 꼭짓점으로 잘라 낸 평평한 페이지는 둘레를 잃지 않아야 한다. 바탕 없이 넣으면 모델이 글자 덩어리를
/// 페이지로 보고 여백과 첫 줄을 잘라 냈다(실제 팜플렛에서 가장자리 4~25%)
#[test]
fn 잘라_낸_페이지는_곡면_보정이_둘레를_자르지_않는다() {
    let Some(models) = models() else {
        eprintln!("SCANSHELF_MODELS_DIR 없음: 건너뜀");
        return;
    };
    let page = render_page(900, 1250, 7);
    let padded = dewarp(&page, Dewarp::Normal, Framing::Page, &models).unwrap();
    let plain = dewarp(&page, Dewarp::Normal, Framing::Photo, &models).unwrap();
    let resized = |img: &RgbImage| image::imageops::resize(img, page.width(), page.height(), image::imageops::FilterType::Triangle);
    let (s_page, s_photo) = (ssim(&resized(&padded.image), &page), ssim(&resized(&plain.image), &page));
    eprintln!("SSIM 바탕 두름 {s_page:.3} / 그대로 {s_photo:.3}");
    // 글자는 몇 화소만 밀려도 SSIM이 크게 떨어진다. 실측 바탕 두름 0.68, 그대로 0.39
    assert!(s_page > 0.6, "잘라 낸 페이지가 많이 바뀜: {s_page}");
    assert!(s_page > s_photo + 0.15, "바탕을 둘러도 나아지지 않음: {s_page} vs {s_photo}");
}

#[test]
fn 모델로_손가락을_메운다() {
    let Some(models) = models() else {
        eprintln!("SCANSHELF_MODELS_DIR 없음: 건너뜀");
        return;
    };
    let mut page = render_page(800, 1100, 23);
    add_finger(&mut page, true, 600.0, 110.0, 45.0);
    let (out, report) = remove_fingers(&page, None, &models).unwrap();
    assert_eq!(report.engine, InpaintEngine::Lama);
    let p = out.get_pixel(30, 600).0;
    assert!(luma_of(p) > 200.0, "{p:?}");
    assert_eq!(detect_fingers(&out).1, 0);
}

fn out_dir() -> PathBuf {
    let dir = std::env::var_os("SCANSHELF_EVAL_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("scanshelf-vision-eval"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn save(img: &RgbImage, name: &str) {
    let path = out_dir().join(name);
    img.save(&path).unwrap();
}

/// 실제 모델로 전후 이미지를 남기고 숫자를 출력한다.
/// `SCANSHELF_MODELS_DIR=~/leedo/scanshelf-models cargo test -p scanshelf-vision --release --test synthetic -- --ignored --nocapture`
#[test]
#[ignore]
fn 모델_전후_이미지와_속도() {
    let dir = std::env::var_os("SCANSHELF_MODELS_DIR").expect("SCANSHELF_MODELS_DIR가 필요합니다");
    let models = Models::new(PathBuf::from(&dir));
    for id in [ModelId::UvDoc, ModelId::Lama] {
        let spec = models::spec(id);
        models::verify(&models.path(id).unwrap(), spec).expect("모델 해시");
        eprintln!("{:?}: {:?}", id, models.backend(id));
    }
    eprintln!("출력 폴더 {}", out_dir().display());

    // 곡면 보정
    let page = render_page(1200, 1650, 31);
    let warped = curl(&page, 1.0, 0.35);
    save(&page, "curl_0_flat.png");
    save(&warped, "curl_1_warped.png");
    for (mode, name) in [(Dewarp::Normal, "normal"), (Dewarp::Strong, "strong")] {
        let r = dewarp(&warped, mode, Framing::Photo, &models).unwrap();
        save(&r.image, &format!("curl_2_{name}.png"));
        let filtered = apply_filter(&r.image, Filter::Enhanced);
        save(&filtered, &format!("curl_3_{name}_enhanced.png"));
        eprintln!(
            "{name}: SSIM {:.3} -> {:.3}, 글줄 {:.3} -> {:.3} (평평한 원본 {:.3}), 크기 {:?}",
            ssim(&warped, &page),
            ssim(&r.image, &page),
            line_sharpness(&warped),
            line_sharpness(&r.image),
            line_sharpness(&page),
            r.image.dimensions()
        );
    }
    save(&apply_filter(&warped, Filter::BlackWhite), "curl_4_bw.png");
    save(&apply_filter(&warped, Filter::Grayscale), "curl_4_gray.png");

    // 손가락
    let mut fingers = render_page(1200, 1650, 33);
    add_finger(&mut fingers, true, 1000.0, 170.0, 65.0);
    add_finger(&mut fingers, false, 500.0, 150.0, 60.0);
    save(&fingers, "finger_0_before.png");
    let (lama, report) = remove_fingers(&fingers, None, &models).unwrap();
    save(&lama, "finger_1_lama.png");
    let (classic, _) = remove_fingers(&fingers, None, &Models::none()).unwrap();
    save(&classic, "finger_2_classical.png");
    eprintln!("손가락 {report:?}");

    // 필기
    let mut pen = render_page(1200, 1650, 35);
    add_scribble(&mut pen, BLUE_PEN, 600.0, 30.0, 50.0, 2.5);
    add_scribble(&mut pen, RED_PEN, 1100.0, 20.0, 25.0, 2.0);
    save(&pen, "pen_0_before.png");
    let (clean, report) = remove_handwriting(&pen, &models).unwrap();
    save(&clean, "pen_1_after.png");
    eprintln!("필기 {report:?}");

    // 펼친 책 전체 흐름
    let (sp, _) = spread(41, 2.0);
    let sp = curl(&sp, 0.6, 0.2);
    save(&sp, "spread_0_source.png");
    let (pages, report) = process_source(&sp, PageLayout::Spread, &ProcessOptions::default(), None, &models).unwrap();
    for p in &pages {
        save(&p.image, &format!("spread_1_{:?}.png", p.part));
    }
    eprintln!("펼친 책 {:?}, 쪽 {}", report.gutter, pages.len());

    // 속도: 1200만 화소(4000x3000 세로 페이지) 곡면 보정 + 보정 필터
    let big_page = render_page(3000, 4000, 51);
    let big = curl(&big_page, 1.0, 0.3);
    let big = image::imageops::resize(&big, 3000, 4000, image::imageops::FilterType::Triangle);
    let options = ProcessOptions { remove_fingers: false, ..ProcessOptions::default() };
    for (label, m) in [("CoreML 우선", Models::new(PathBuf::from(&dir))), ("CPU", Models::new(PathBuf::from(&dir)).cpu_only())] {
        // 첫 호출은 세션 생성(CoreML 컴파일 포함)이라 따로 잰다
        let start = Instant::now();
        let _ = dewarp(&RgbImage::new(64, 64), Dewarp::Normal, Framing::Page, &m).unwrap();
        let warm = start.elapsed();
        let mut runs = Vec::new();
        for _ in 0..3 {
            let start = Instant::now();
            let (_, r) = process_page(&big, &options, &m).unwrap();
            runs.push((start.elapsed().as_millis(), r.timings));
        }
        let strong = ProcessOptions { dewarp: Dewarp::Strong, ..options };
        let start = Instant::now();
        let _ = process_page(&big, &strong, &m).unwrap();
        let strong_ms = start.elapsed().as_millis();
        eprintln!(
            "{label} ({:?}): 세션 준비 {} ms, 12MP 보정+필터 {:?}, 더 펴기 {} ms",
            m.backend(ModelId::UvDoc),
            warm.as_millis(),
            runs,
            strong_ms
        );
    }
    let start = Instant::now();
    let (_, r) = remove_fingers(&{
        let mut b = big.clone();
        add_finger(&mut b, true, 2500.0, 450.0, 160.0);
        b
    }, None, &models)
    .unwrap();
    eprintln!("12MP 손가락 지우기 {} ms ({:?})", start.elapsed().as_millis(), r);
}
