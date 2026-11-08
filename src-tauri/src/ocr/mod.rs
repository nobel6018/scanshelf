//! 페이지 글자 인식. 엔진은 플랫폼마다 다르고(`macos.rs`, 나중에 `windows.rs`), 블록 묶기는 공통이다.
//!
//! @see docs/ocr.md

pub mod blocks;
#[cfg(target_os = "macos")]
mod macos;

use image::RgbImage;
use scanshelf_core::ocr::OcrPage;

#[derive(Debug, thiserror::Error)]
pub enum OcrError {
    #[error("글자 인식 실패: {0}")]
    Engine(String),
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    #[error("이 플랫폼은 글자 인식을 아직 지원하지 않는다")]
    Unsupported,
}

/// 방향 판단에 쓰는 크기. 작은 글자까지 읽을 필요 없이 글줄 방향만 보면 된다
const ROTATION_PROBE_SIDE: u32 = 1400;
/// 이보다 적게 읽히면 글자가 거의 없는 페이지로 보고 방향을 정하지 않는다
const ROTATION_MIN_SCORE: f64 = 15.0;
/// 1등 방향이 2등보다 이만큼 앞서야 믿는다. 도면 옆에 세로로 적힌 글자처럼 다른 방향 글줄이 섞인다
const ROTATION_MIN_LEAD: f64 = 1.5;
/// 돌린 뒤 읽힌 글자량이 돌리기 전의 이 비율보다 적으면 돌리지 않는다. 바로 선 글은 보통 Vision이 더 많이 읽는다.
/// 2026-11-08 실측: 잘못 뒤집은(180도) 실제 두 장은 0.66, 0.76, 제대로 돌린 책 14장은 0.98 이상.
/// 합성 영문 시험 페이지는 거꾸로 된 글을 더 읽어 0.86이 나온다. 그 사이에 둔다(여유가 크지 않다)
const ROTATION_MIN_KEEP: f64 = 0.8;

/// 방향 판단 결과
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RotationReading {
    pub rotation: Option<u16>,
    /// 글줄 방향은 돌리라고 했는데 돌려 보니 덜 읽혀 그대로 뒀다(사람이 볼 것)
    pub doubtful: bool,
}

/// 페이지를 바로 세우려면 시계 방향으로 몇 도 돌려야 하는지. 글자가 적거나 애매하면 None
#[allow(dead_code)]
pub fn detect_rotation(image: &RgbImage) -> Option<u16> {
    detect_rotation_checked(image).rotation
}

/// 글줄의 읽는 방향을 글자 수로 투표하고, 돌린 뒤 다시 읽어 확인한다.
/// 글자가 적거나 애매하면 None. 그림만 있는 페이지를 잘못 돌리느니 그대로 둔다
pub fn detect_rotation_checked(image: &RgbImage) -> RotationReading {
    #[cfg(target_os = "macos")]
    {
        let (w, h) = image.dimensions();
        let scale = (ROTATION_PROBE_SIDE as f64 / w.max(h) as f64).min(1.0);
        let probe = image::imageops::thumbnail(
            image,
            ((w as f64 * scale) as u32).max(1),
            ((h as f64 * scale) as u32).max(1),
        );
        let lines = match macos::text_directions(&probe) {
            Ok(lines) => lines,
            Err(error) => {
                eprintln!("방향 판단 중 글자 인식 실패: {error}");
                return RotationReading::default();
            }
        };
        let Some(picked) = pick_rotation(direction_votes(&lines)) else {
            return RotationReading::default();
        };
        if picked == 0 {
            return RotationReading { rotation: Some(0), doubtful: false };
        }
        // 돌린 뒤 다시 읽어 확인한다. Vision이 바로 선 페이지의 글줄 방향을 거꾸로 알려 주는 일이 있다
        // (2026-11-08 데이터센터 박람회_9 3쪽, 로보월드_6 3단 접지: 180도로 뒤집혔다). 그때는 돌린 쪽이 덜 읽힌다
        match macos::text_directions(&rotate(&probe, picked)) {
            Ok(after) if rotation_confirmed(&lines, &after) => RotationReading { rotation: Some(picked), doubtful: false },
            Ok(_) => RotationReading { rotation: None, doubtful: true },
            Err(_) => RotationReading::default(),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = image;
        RotationReading::default()
    }
}

/// 시계 방향으로 돌린다. 90도 단위가 아니면 그대로 둔다
pub fn rotate(image: &RgbImage, degrees: u16) -> RgbImage {
    match degrees % 360 {
        90 => image::imageops::rotate90(image),
        180 => image::imageops::rotate180(image),
        270 => image::imageops::rotate270(image),
        _ => image.clone(),
    }
}

/// 읽는 방향(dx, dy, y는 위로)을 필요한 시계 방향 회전 칸(0, 90, 180, 270)에 모은다.
/// 글이 위로 읽히면(반시계 90도로 누운 글) 시계 방향 90도를 돌려야 바로 선다
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn direction_votes(lines: &[(f64, f64, f64)]) -> [f64; 4] {
    let mut votes = [0.0; 4];
    for &(dx, dy, weight) in lines {
        let quarter = (dy.atan2(dx).to_degrees().rem_euclid(360.0) / 90.0).round() as usize % 4;
        votes[quarter] += weight;
    }
    votes
}

/// 돌린 뒤에 바로 선 글줄(0도 칸)이 1등이고, 읽힌 글자량이 돌리기 전보다 크게 줄지 않았는지
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn rotation_confirmed(before: &[(f64, f64, f64)], after: &[(f64, f64, f64)]) -> bool {
    let total = |lines: &[(f64, f64, f64)]| lines.iter().map(|l| l.2).sum::<f64>();
    pick_rotation(direction_votes(after)) == Some(0) && total(after) >= total(before) * ROTATION_MIN_KEEP
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn pick_rotation(votes: [f64; 4]) -> Option<u16> {
    let best = (0..4).max_by(|&a, &b| votes[a].total_cmp(&votes[b]))?;
    let second = (0..4).filter(|&i| i != best).map(|i| votes[i]).fold(0.0, f64::max);
    (votes[best] >= ROTATION_MIN_SCORE && votes[best] >= second * ROTATION_MIN_LEAD).then_some(best as u16 * 90)
}

/// 처리된 페이지 이미지에서 글자를 읽어 블록으로 묶는다
pub fn recognize(image: &RgbImage) -> Result<OcrPage, OcrError> {
    #[cfg(target_os = "macos")]
    {
        let lines = macos::recognize_lines(image)?;
        Ok(blocks::build_page(macos::ENGINE, image.width(), image.height(), lines))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = image;
        Err(OcrError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 읽는_방향을_필요한_회전으로_모은다() {
        let votes = direction_votes(&[(100.0, 2.0, 10.0), (-1.0, 100.0, 20.0), (-100.0, 1.0, 30.0), (1.0, -100.0, 40.0)]);
        assert_eq!(votes, [10.0, 20.0, 30.0, 40.0]);
    }

    #[test]
    fn 가장_표가_많은_방향을_고른다() {
        assert_eq!(pick_rotation([3.0, 120.0, 10.0, 8.0]), Some(90));
        assert_eq!(pick_rotation([200.0, 5.0, 40.0, 2.0]), Some(0));
    }

    #[test]
    fn 돌린_뒤_덜_읽히면_돌리지_않는다() {
        // 2026-11-08 데이터센터 박람회_9 3쪽: 180도라고 했지만 돌리니 201 → 132로 덜 읽혔다
        let before = [(-100.0, 0.0, 201.0)];
        assert!(!rotation_confirmed(&before, &[(100.0, 0.0, 132.0)]));
        // 실제로 누운 책 페이지: 돌리니 461 → 695로 더 읽혔다
        assert!(rotation_confirmed(&[(0.0, -100.0, 461.0)], &[(100.0, 0.0, 695.0)]));
        // 누운 글은 Vision이 누운 채로도 잘 읽는다(합성 페이지 47 → 40.5)
        assert!(rotation_confirmed(&[(0.0, -100.0, 47.0)], &[(100.0, 0.0, 40.5)]));
        // 돌린 뒤에도 바로 서지 않으면 믿지 않는다
        assert!(!rotation_confirmed(&before, &[(0.0, 100.0, 300.0)]));
    }

    #[test]
    fn 글자가_적거나_애매하면_방향을_정하지_않는다() {
        assert_eq!(pick_rotation([4.0, 1.0, 0.0, 2.0]), None);
        assert_eq!(pick_rotation([100.0, 80.0, 10.0, 5.0]), None);
    }
}

