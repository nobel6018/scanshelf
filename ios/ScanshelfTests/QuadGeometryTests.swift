import Foundation
import CoreGraphics
import ImageIO
import Testing
@testable import Scanshelf

struct QuadGeometryTests {
    private func close(_ a: NormalizedPoint, _ b: NormalizedPoint) -> Bool {
        abs(a.x - b.x) < 1e-9 && abs(a.y - b.y) < 1e-9
    }

    @Test func Vision_좌표는_y를_뒤집어_왼쪽_위_원점으로_바꾼다() {
        // Vision은 원점이 왼쪽 아래라 위쪽 꼭짓점의 y가 크다
        let quad = QuadGeometry.fromVision(
            topLeft: CGPoint(x: 0.1, y: 0.9),
            topRight: CGPoint(x: 0.8, y: 0.95),
            bottomRight: CGPoint(x: 0.9, y: 0.2),
            bottomLeft: CGPoint(x: 0.05, y: 0.1)
        )

        #expect(close(quad.topLeft, .init(x: 0.1, y: 0.1)))
        #expect(close(quad.topRight, .init(x: 0.8, y: 0.05)))
        #expect(close(quad.bottomRight, .init(x: 0.9, y: 0.8)))
        #expect(close(quad.bottomLeft, .init(x: 0.05, y: 0.9)))
    }

    @Test func 범위를_조금_벗어난_Vision_값은_0과_1로_자른다() {
        let point = QuadGeometry.fromVision(CGPoint(x: -0.01, y: 1.02))

        #expect(point == NormalizedPoint(x: 0, y: 0))
    }

    @Test func 세로_촬영_버퍼_방향을_적용하면_왼쪽_위가_오른쪽_위로_간다() {
        // 후면 카메라를 세로로 들면 버퍼는 가로로 누워 있고 EXIF는 .right(시계 방향 90도 돌려 표시)
        let sensorTopLeft = NormalizedPoint(x: 0, y: 0)
        let sensorTopRight = NormalizedPoint(x: 1, y: 0)
        let sensorPoint = NormalizedPoint(x: 0.25, y: 0.1)

        #expect(close(QuadGeometry.applying(.right, to: sensorTopLeft), .init(x: 1, y: 0)))
        #expect(close(QuadGeometry.applying(.right, to: sensorTopRight), .init(x: 1, y: 1)))
        #expect(close(QuadGeometry.applying(.right, to: sensorPoint), .init(x: 0.9, y: 0.25)))
    }

    @Test func 방향을_적용한_뒤_꼭짓점_이름을_위치로_다시_붙인다() {
        // 센서 기준 문서. 세로 화면으로 돌리면 센서의 왼쪽 아래가 화면의 왼쪽 위가 된다
        let sensor = Quad(
            topLeft: .init(x: 0.1, y: 0.2),
            topRight: .init(x: 0.9, y: 0.2),
            bottomRight: .init(x: 0.9, y: 0.8),
            bottomLeft: .init(x: 0.1, y: 0.8)
        )

        let display = QuadGeometry.applying(.right, to: sensor)

        #expect(close(display.topLeft, .init(x: 0.2, y: 0.1)))
        #expect(close(display.topRight, .init(x: 0.8, y: 0.1)))
        #expect(close(display.bottomRight, .init(x: 0.8, y: 0.9)))
        #expect(close(display.bottomLeft, .init(x: 0.2, y: 0.9)))
    }

    @Test func 네_방향을_차례로_돌리면_제자리로_온다() {
        let point = NormalizedPoint(x: 0.3, y: 0.7)

        let quarter = QuadGeometry.applying(.right, to: point)
        let half = QuadGeometry.applying(.right, to: quarter)
        let threeQuarters = QuadGeometry.applying(.right, to: half)
        let full = QuadGeometry.applying(.right, to: threeQuarters)

        #expect(close(half, QuadGeometry.applying(.down, to: point)))
        #expect(close(threeQuarters, QuadGeometry.applying(.left, to: point)))
        #expect(close(full, point))
    }

    @Test func 넓이는_정규화_좌표로_잰다() {
        let quad = Quad(
            topLeft: .init(x: 0, y: 0),
            topRight: .init(x: 0.5, y: 0),
            bottomRight: .init(x: 0.5, y: 0.5),
            bottomLeft: .init(x: 0, y: 0.5)
        )

        #expect(abs(quad.area - 0.25) < 1e-9)
    }
    // MARK: 테두리 타당성. fixtures/quad-plausibility.json은 2026-11-08 실제 촬영값이고 Rust(crates/scanshelf-core/tests/quad.rs)도 읽는다

    private struct PlausibilityFixture: Decodable {
        struct Case: Decodable {
            var name: String
            var quad: Quad
            var plausible: Bool
            var cutOff: Bool
        }

        var aspect: Double
        var cases: [Case]
    }

    private func plausibilityFixture() throws -> PlausibilityFixture {
        let url = URL(filePath: #filePath)
            .deletingLastPathComponent()
            .appending(path: "../../fixtures/quad-plausibility.json")
            .standardized
        return try JSONDecoder().decode(PlausibilityFixture.self, from: Data(contentsOf: url))
    }

    @Test func 실제_촬영의_테두리를_정상과_잘못_잡은_것으로_가른다() throws {
        let fixture = try plausibilityFixture()
        #expect(fixture.aspect == QuadGeometry.captureAspect)
        for item in fixture.cases {
            #expect(QuadGeometry.isPlausible(item.quad, aspect: fixture.aspect) == item.plausible, "\(item.name)")
        }
    }

    @Test func 화면_밖으로_나간_종이를_가려낸다() throws {
        for item in try plausibilityFixture().cases {
            #expect(QuadGeometry.touchesFrameEdge(item.quad) == item.cutOff, "\(item.name)")
        }
    }

    @Test func 꼬이거나_겹친_꼭짓점은_믿지_않는다() {
        let twisted = Quad(
            topLeft: .init(x: 0.2, y: 0.2),
            topRight: .init(x: 0.8, y: 0.6),
            bottomRight: .init(x: 0.8, y: 0.2),
            bottomLeft: .init(x: 0.2, y: 0.6)
        )
        let collapsed = Quad(
            topLeft: .init(x: 0.3, y: 0.2),
            topRight: .init(x: 0.3, y: 0.2),
            bottomRight: .init(x: 0.8, y: 0.6),
            bottomLeft: .init(x: 0.2, y: 0.6)
        )

        #expect(!QuadGeometry.isPlausible(twisted))
        #expect(!QuadGeometry.isPlausible(collapsed))
    }

    @Test func 꼭짓점_이동은_세로_비율을_맞춰_잰다() {
        let a = Quad(topLeft: .init(x: 0.1, y: 0.1), topRight: .init(x: 0.9, y: 0.1), bottomRight: .init(x: 0.9, y: 0.7), bottomLeft: .init(x: 0.1, y: 0.7))
        var b = a
        b.bottomLeft.y += 0.03

        #expect(abs(QuadGeometry.largestCornerShift(a, b) - 0.04) < 1e-9)
    }
}
