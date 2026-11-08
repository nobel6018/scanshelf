//! 테두리 타당성 검사. fixtures/quad-plausibility.json은 2026-11-08 실제 촬영값이고
//! iPhone 테스트(QuadGeometryTests)도 같은 파일을 읽는다.

use scanshelf_core::{Point, Quad};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    aspect: f64,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    name: String,
    quad: Quad,
    plausible: bool,
}

fn fixture() -> Fixture {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/quad-plausibility.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn 실제_촬영의_테두리를_정상과_잘못_잡은_것으로_가른다() {
    let fixture = fixture();
    for case in &fixture.cases {
        assert_eq!(case.quad.is_plausible(fixture.aspect), case.plausible, "{}", case.name);
    }
}

fn quad(points: [(f64, f64); 4]) -> Quad {
    let p = points.map(|(x, y)| Point { x, y });
    Quad { top_left: p[0], top_right: p[1], bottom_right: p[2], bottom_left: p[3] }
}

#[test]
fn 꼭짓점이_겹치거나_꼬이면_믿지_않는다() {
    assert!(!quad([(0.2, 0.2), (0.2, 0.2), (0.8, 0.8), (0.2, 0.8)]).is_plausible(1.0));
    // 오른쪽 위와 오른쪽 아래가 뒤바뀐 꼬인 사각형
    assert!(!quad([(0.2, 0.2), (0.8, 0.8), (0.8, 0.2), (0.2, 0.8)]).is_plausible(1.0));
}

#[test]
fn 가로세로_비율을_맞춰_잰다() {
    // 정규화 좌표로는 정사각형이 아니어도 4:3 사진에서는 반듯한 직사각형이다
    let q = quad([(0.1, 0.1), (0.9, 0.1), (0.9, 0.4), (0.1, 0.4)]);
    assert!(q.is_plausible(4.0 / 3.0));
    assert!(quad([(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]).is_plausible(4.0 / 3.0));
}
