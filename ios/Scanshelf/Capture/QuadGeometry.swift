import CoreGraphics
import ImageIO

// manifest의 quad는 "원본 사진에 EXIF 회전을 적용한 뒤" 기준 0~1 좌표이고 원점이 왼쪽 위다.
// Vision 결과는 원점이 왼쪽 아래이고, 카메라 버퍼는 센서 방향(가로)이라 세로 촬영과 축이 다르다.
// 좌표계를 오가는 계산을 여기 모아 테스트로 고정한다.
// @see docs/capture.md, docs/document-format.md

enum QuadGeometry {
    /// Vision 정규화 좌표(원점 왼쪽 아래)를 manifest 좌표(원점 왼쪽 위)로 바꾼다.
    /// Vision에 넘긴 orientation이 표시 방향과 같다면 결과는 이미 표시 방향 기준이라 y만 뒤집으면 된다
    static func fromVision(_ point: CGPoint) -> NormalizedPoint {
        NormalizedPoint(x: clamp(Double(point.x)), y: clamp(1 - Double(point.y)))
    }

    static func fromVision(topLeft: CGPoint, topRight: CGPoint, bottomRight: CGPoint, bottomLeft: CGPoint) -> Quad {
        ordered([fromVision(topLeft), fromVision(topRight), fromVision(bottomRight), fromVision(bottomLeft)])
    }

    /// 버퍼에 저장된 방향(원점 왼쪽 위, 0~1)의 점을 EXIF orientation을 적용한 표시 방향 점으로 바꾼다.
    /// 세로로 든 iPhone 후면 카메라 버퍼는 `.right`다(표시하려면 시계 방향 90도 회전)
    static func applying(_ orientation: CGImagePropertyOrientation, to point: NormalizedPoint) -> NormalizedPoint {
        let x = point.x
        let y = point.y
        switch orientation {
        case .up: return NormalizedPoint(x: x, y: y)
        case .upMirrored: return NormalizedPoint(x: 1 - x, y: y)
        case .down: return NormalizedPoint(x: 1 - x, y: 1 - y)
        case .downMirrored: return NormalizedPoint(x: x, y: 1 - y)
        case .right: return NormalizedPoint(x: 1 - y, y: x)
        case .rightMirrored: return NormalizedPoint(x: 1 - y, y: 1 - x)
        case .left: return NormalizedPoint(x: y, y: 1 - x)
        case .leftMirrored: return NormalizedPoint(x: y, y: x)
        }
    }

    static func applying(_ orientation: CGImagePropertyOrientation, to quad: Quad) -> Quad {
        ordered(quad.points.map { applying(orientation, to: $0) })
    }

    /// 회전하고 나면 "왼쪽 위"였던 점이 다른 자리로 간다. 이름은 위치로 다시 붙인다.
    /// 위쪽 두 점과 아래쪽 두 점을 나눈 뒤 각각 x로 좌우를 정한다(거치대 촬영은 크게 기울지 않는다)
    static func ordered(_ points: [NormalizedPoint]) -> Quad {
        precondition(points.count == 4, "꼭짓점은 4개")
        let byY = points.sorted { $0.y < $1.y }
        let top = byY[0 ..< 2].sorted { $0.x < $1.x }
        let bottom = byY[2 ..< 4].sorted { $0.x < $1.x }
        return Quad(topLeft: top[0], topRight: top[1], bottomRight: bottom[1], bottomLeft: bottom[0])
    }

    private static func clamp(_ value: Double) -> Double {
        min(1, max(0, value))
    }

    // MARK: 테두리를 믿을 수 있는가

    /// 촬영 화면과 사진의 높이/폭. 세로 고정, `.photo` 프리셋이라 4:3이다
    static let captureAspect = 4.0 / 3.0
    /// 마주 보는 두 변 길이의 비(짧은 쪽/긴 쪽) 하한. 2026-11-08 촬영 154장 중 정상 장은 모두 0.89 이상,
    /// 꼭짓점을 잘못 잡은 장(흰 면이나 어두운 표지에서 꼭짓점을 접힌 선에 붙인 삼각형, 사다리꼴)은 0.01~0.52였다
    static let minimumOppositeSideRatio = 0.8
    /// 내각 범위(도). 정상 장은 84~96도, 잘못 잡은 장은 38~167도였다
    static let cornerDegrees = 65.0 ... 115.0
    /// 꼭짓점이 화면 가장자리에서 이보다 가까우면 종이가 화면 밖으로 나간 것으로 본다.
    /// Vision 테두리는 분석 해상도 단위(가로 1/144 ≈ 0.007)로 나오고, 화면 밖으로 나간 종이는 0이나 0.007에 붙는다.
    /// 정상 장 중에는 0.014(두 칸)에 놓인 것도 있어 그보다 안쪽인 0.01로 둔다
    static let edgeMargin = 0.01
    /// 직전 촬영 때와 비교해 꼭짓점이 이보다 많이 움직였으면 종이를 다시 놓은 것으로 본다(화면 폭 대비).
    /// Vision 테두리의 떨림은 한두 칸(0.007~0.014)이다
    static let replacedShift = 0.04

    /// 종이 테두리로 믿을 만한가. 볼록하고, 마주 보는 변 길이가 비슷하고, 내각이 직각에 가까워야 한다.
    /// 잘못 잡은 테두리로 펴면 페이지가 찢긴 것처럼 나온다.
    /// Mac `scanshelf_core::Quad::is_plausible`과 같은 기준이고 fixtures/quad-plausibility.json으로 양쪽을 확인한다
    static func isPlausible(_ quad: Quad, aspect: Double = captureAspect) -> Bool {
        let p = quad.points.map { (x: $0.x, y: $0.y * aspect) }
        func side(_ i: Int) -> Double {
            let a = p[i], b = p[(i + 1) % 4]
            return ((a.x - b.x) * (a.x - b.x) + (a.y - b.y) * (a.y - b.y)).squareRoot()
        }
        let sides = (0 ..< 4).map(side)
        guard sides.allSatisfy({ $0 > 1e-6 }) else { return false }
        func ratio(_ a: Double, _ b: Double) -> Double { min(a, b) / max(a, b) }
        guard ratio(sides[0], sides[2]) >= minimumOppositeSideRatio,
              ratio(sides[1], sides[3]) >= minimumOppositeSideRatio
        else { return false }
        var sign = 0.0
        for i in 0 ..< 4 {
            let prev = p[(i + 3) % 4], at = p[i], next = p[(i + 1) % 4]
            let a = (x: prev.x - at.x, y: prev.y - at.y)
            let b = (x: next.x - at.x, y: next.y - at.y)
            let cross = a.x * b.y - a.y * b.x
            if sign == 0 {
                sign = cross < 0 ? -1 : 1
            } else if (cross < 0 ? -1 : 1) != sign {
                return false
            }
            let cosine = (a.x * b.x + a.y * b.y) / (sides[(i + 3) % 4] * sides[i])
            let degrees = acos(min(1, max(-1, cosine))) * 180 / .pi
            guard cornerDegrees.contains(degrees) else { return false }
        }
        return true
    }

    /// 꼭짓점이 화면 가장자리에 붙었는가. 종이 일부가 화면 밖에 있다는 뜻이다
    static func touchesFrameEdge(_ quad: Quad) -> Bool {
        quad.points.contains { point in
            point.x <= edgeMargin || point.x >= 1 - edgeMargin || point.y <= edgeMargin || point.y >= 1 - edgeMargin
        }
    }

    /// 두 테두리의 같은 꼭짓점끼리 가장 많이 떨어진 거리(화면 폭 단위, 세로는 비율을 맞춘다)
    static func largestCornerShift(_ a: Quad, _ b: Quad, aspect: Double = captureAspect) -> Double {
        zip(a.points, b.points).map { p, q in
            let dx = p.x - q.x, dy = (p.y - q.y) * aspect
            return (dx * dx + dy * dy).squareRoot()
        }.max() ?? 0
    }
}

extension Quad {
    var points: [NormalizedPoint] {
        [topLeft, topRight, bottomRight, bottomLeft]
    }

    /// 화면 그리기용. 정규화 좌표를 주어진 크기의 점으로 바꾼다
    func cgPoints(in size: CGSize) -> [CGPoint] {
        points.map { CGPoint(x: $0.x * size.width, y: $0.y * size.height) }
    }

    /// 신발끈 공식 넓이(0~1). 너무 작은 감지는 문서로 보지 않는다
    var area: Double {
        let p = points
        var sum = 0.0
        for i in 0 ..< 4 {
            let a = p[i]
            let b = p[(i + 1) % 4]
            sum += a.x * b.y - b.x * a.y
        }
        return abs(sum) / 2
    }
}
