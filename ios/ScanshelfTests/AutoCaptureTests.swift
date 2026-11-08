import Foundation
import Testing
@testable import Scanshelf

struct AutoCaptureTests {
    private let page = Quad(
        topLeft: .init(x: 0.1, y: 0.1),
        topRight: .init(x: 0.9, y: 0.1),
        bottomRight: .init(x: 0.9, y: 0.9),
        bottomLeft: .init(x: 0.1, y: 0.9)
    )

    private func frame(
        _ time: TimeInterval,
        quad: Quad?? = .none,
        motion: Double = 0,
        hand: Bool = false,
        distance: Float? = nil,
        pageDistance: Float? = nil
    ) -> FrameMeasurement {
        FrameMeasurement(
            time: time,
            quad: quad ?? page,
            motion: motion,
            handVisible: hand,
            distanceFromLastCapture: distance,
            pageDistanceFromLastCapture: pageDistance
        )
    }

    /// 0.1초 간격(분석 10fps)으로 프레임을 넣고 결정을 모은다
    private func run(
        _ engine: inout AutoCaptureEngine,
        from start: TimeInterval,
        to end: TimeInterval,
        make: (TimeInterval) -> FrameMeasurement
    ) -> [AutoCaptureDecision] {
        var decisions: [AutoCaptureDecision] = []
        var time = start
        while time <= end + 0.0001 {
            decisions.append(engine.process(make(time)))
            time += 0.1
        }
        return decisions
    }

    @Test func 문서가_안정되면_정해진_시간_뒤에_찍는다() {
        var engine = AutoCaptureEngine(tuning: .normal)

        let early = run(&engine, from: 0, to: 0.5) { frame($0) }
        let atThreshold = engine.process(frame(0.6))

        #expect(!early.contains(.capture))
        #expect(atThreshold == .capture)
        #expect(engine.phase == .capturing)
    }

    @Test func 문서가_없으면_찍지_않는다() {
        var engine = AutoCaptureEngine()

        let decisions = run(&engine, from: 0, to: 2) { frame($0, quad: .some(nil)) }

        #expect(!decisions.contains(.capture))
        #expect(engine.status == .noDocument)
    }

    @Test func 손이_보이면_안정돼도_찍지_않는다() {
        var engine = AutoCaptureEngine()

        let decisions = run(&engine, from: 0, to: 2) { frame($0, hand: true) }

        #expect(!decisions.contains(.capture))
        #expect(engine.status == .handVisible)
    }

    @Test func 움직이면_안정_시간을_처음부터_다시_잰다() {
        var engine = AutoCaptureEngine()
        _ = run(&engine, from: 0, to: 0.5) { frame($0) }

        _ = engine.process(frame(0.6, motion: 0.2))
        let afterMove = run(&engine, from: 0.7, to: 1.2) { frame($0) }
        let later = engine.process(frame(1.3))

        #expect(!afterMove.contains(.capture))
        #expect(later == .capture)
    }

    @Test func 너무_작은_테두리는_문서로_보지_않는다() {
        var engine = AutoCaptureEngine()
        let tiny = Quad(
            topLeft: .init(x: 0.45, y: 0.45),
            topRight: .init(x: 0.55, y: 0.45),
            bottomRight: .init(x: 0.55, y: 0.55),
            bottomLeft: .init(x: 0.45, y: 0.55)
        )

        let decisions = run(&engine, from: 0, to: 2) { frame($0, quad: .some(tiny)) }

        #expect(!decisions.contains(.capture))
    }

    @Test func 찍은_뒤에는_종이가_바뀔_때까지_다시_찍지_않는다() {
        var engine = AutoCaptureEngine()
        _ = run(&engine, from: 0, to: 0.6) { frame($0) }
        engine.captureFinished(succeeded: true)

        let decisions = run(&engine, from: 0.7, to: 3) { frame($0, distance: 0.1) }

        #expect(!decisions.contains(.capture))
        #expect(engine.phase == .waitingForChange)
        #expect(engine.wantsDistance)
    }

    @Test func 손으로_종이를_바꾸면_다음_장을_찍는다() {
        var engine = AutoCaptureEngine()
        _ = run(&engine, from: 0, to: 0.6) { frame($0) }
        engine.captureFinished(succeeded: true)

        _ = run(&engine, from: 0.7, to: 1.0) { frame($0, hand: true, distance: 0.3) }
        let next = run(&engine, from: 1.1, to: 1.8) { frame($0, distance: 0.8) }

        #expect(next.contains(.capture))
    }

    @Test func 종이를_바꿨다가_같은_장을_다시_놓으면_건너뛴다() {
        var engine = AutoCaptureEngine()
        _ = run(&engine, from: 0, to: 0.6) { frame($0) }
        engine.captureFinished(succeeded: true)

        _ = engine.process(frame(0.7, hand: true))
        let decisions = run(&engine, from: 0.8, to: 1.5) { frame($0, distance: 0.1) }

        #expect(decisions.contains(.skippedDuplicate))
        #expect(!decisions.contains(.capture))
        #expect(engine.phase == .waitingForChange)
    }

    @Test func 거리를_아직_못_쟀으면_기다린다() {
        var engine = AutoCaptureEngine()
        _ = run(&engine, from: 0, to: 0.6) { frame($0) }
        engine.captureFinished(succeeded: true)
        _ = engine.process(frame(0.7, quad: .some(nil)))

        let withoutDistance = run(&engine, from: 0.8, to: 2) { frame($0) }
        let withDistance = engine.process(frame(2.1, distance: 0.9))

        #expect(!withoutDistance.contains(.capture))
        #expect(withDistance == .capture)
    }

    @Test func 촬영이_실패하면_다시_기다린다() {
        var engine = AutoCaptureEngine()
        _ = run(&engine, from: 0, to: 0.6) { frame($0) }

        engine.captureFinished(succeeded: false)
        let retry = run(&engine, from: 0.7, to: 1.3) { frame($0) }

        #expect(retry.contains(.capture))
        #expect(!engine.hasLastCapture)
    }

    @Test func 마지막_장을_지우면_같은_장을_다시_찍을_수_있다() {
        var engine = AutoCaptureEngine()
        _ = run(&engine, from: 0, to: 0.6) { frame($0) }
        engine.captureFinished(succeeded: true)

        engine.forgetLastCapture()
        let decisions = run(&engine, from: 0.7, to: 1.3) { frame($0, distance: 0.05) }

        #expect(decisions.contains(.capture))
    }

    @Test func 수동_셔터_중에는_자동으로_찍지_않는다() {
        var engine = AutoCaptureEngine()

        engine.manualCaptureStarted()
        let decisions = run(&engine, from: 0, to: 2) { frame($0) }

        #expect(!decisions.contains(.capture))
    }

    @Test func 민감하게는_보통보다_빨리_찍는다() {
        var sensitive = AutoCaptureEngine(tuning: AutoCaptureSensitivity.high.tuning)
        var normal = AutoCaptureEngine(tuning: AutoCaptureSensitivity.normal.tuning)

        let fast = run(&sensitive, from: 0, to: 0.4) { frame($0) }
        let slow = run(&normal, from: 0, to: 0.4) { frame($0) }

        #expect(fast.contains(.capture))
        #expect(!slow.contains(.capture))
    }

    /// 펼친 3단 접지(A4 가로, 1.414:1)가 세로 4:3 화면에서 차지하는 테두리. 가로 폭 비율만 주면 세로는 비율로 정해진다
    private func unfoldedSheet(widthFraction width: Double, ratio: Double = 297.0 / 210.0) -> Quad {
        // 화면은 가로 3, 세로 4. 정규화 세로 = 폭 * 3 / 비율 / 4
        let height = width * 3 / ratio / 4
        let left = (1 - width) / 2
        let top = (1 - height) / 2
        return Quad(
            topLeft: .init(x: left, y: top),
            topRight: .init(x: left + width, y: top),
            bottomRight: .init(x: left + width, y: top + height),
            bottomLeft: .init(x: left, y: top + height)
        )
    }

    @Test func 펼친_3단_접지는_어느_민감도에서도_문서로_보고_찍는다() {
        // 화면 폭의 80%를 채운 가로 종이. 넓이는 약 0.34로 가장 높은 하한(둔하게 0.18)보다 크다
        let sheet = unfoldedSheet(widthFraction: 0.8)
        #expect(sheet.area > AutoCaptureTuning.low.minimumDocumentArea)

        for sensitivity in AutoCaptureSensitivity.allCases {
            var engine = AutoCaptureEngine(tuning: sensitivity.tuning)
            let decisions = run(&engine, from: 0, to: 1.2) { frame($0, quad: .some(sheet)) }
            #expect(decisions.contains(.capture), "민감도 \(sensitivity.rawValue)")
        }
    }

    @Test func 거치대를_낮춰_폭의_60퍼센트만_채워도_찍는다() {
        // 세로 화면에 가로 종이를 놓으면 위아래가 비어 넓이가 줄어든다. 폭 60%면 넓이 약 0.19
        let sheet = unfoldedSheet(widthFraction: 0.6)
        var engine = AutoCaptureEngine(tuning: .low)

        let decisions = run(&engine, from: 0, to: 1.2) { frame($0, quad: .some(sheet)) }

        #expect(decisions.contains(.capture))
    }
    // MARK: 테두리를 믿을 수 없거나 화면 밖으로 나간 종이 (2026-11-08 실제 촬영에서 나온 경우)

    /// 로보월드_3 8번: 흰 면 쪽 꼭짓점을 접힌 선에 붙인 삼각형
    private let triangle = Quad(
        topLeft: .init(x: 0.444, y: 0.164),
        topRight: .init(x: 0.486, y: 0.164),
        bottomRight: .init(x: 0.924, y: 0.629),
        bottomLeft: .init(x: 0.028, y: 0.633)
    )

    /// 로보월드_2 1번: 양옆이 화면 밖으로 나간 3단 접지
    private let cutOffSheet = Quad(
        topLeft: .init(x: 0.007, y: 0.172),
        topRight: .init(x: 1.0, y: 0.164),
        bottomRight: .init(x: 1.0, y: 0.578),
        bottomLeft: .init(x: 0.007, y: 0.590)
    )

    @Test func 꼭짓점을_잘못_잡은_테두리로는_찍지_않는다() {
        var engine = AutoCaptureEngine()

        let decisions = run(&engine, from: 0, to: 2) { frame($0, quad: .some(triangle)) }

        #expect(!decisions.contains(.capture))
        #expect(engine.status == .unclearEdges)
    }

    @Test func 종이가_화면_밖으로_나가면_찍지_않는다() {
        for sensitivity in AutoCaptureSensitivity.allCases {
            var engine = AutoCaptureEngine(tuning: sensitivity.tuning)

            let decisions = run(&engine, from: 0, to: 2) { frame($0, quad: .some(cutOffSheet)) }

            #expect(!decisions.contains(.capture), "민감도 \(sensitivity.rawValue)")
            #expect(engine.status == .cutOff)
        }
    }

    @Test func 손이_보이면_테두리보다_손_안내가_먼저다() {
        var engine = AutoCaptureEngine()

        _ = engine.process(frame(0, quad: .some(triangle), hand: true))

        #expect(engine.status == .handVisible)
    }

    // MARK: 같은 장을 옮기거나 돌려 다시 놓기

    private func shifted(_ quad: Quad, by dx: Double) -> Quad {
        Quad(
            topLeft: .init(x: quad.topLeft.x + dx, y: quad.topLeft.y),
            topRight: .init(x: quad.topRight.x + dx, y: quad.topRight.y),
            bottomRight: .init(x: quad.bottomRight.x + dx, y: quad.bottomRight.y),
            bottomLeft: .init(x: quad.bottomLeft.x + dx, y: quad.bottomLeft.y)
        )
    }

    /// 한 장을 찍고, 손으로 종이를 만진 뒤 다시 안정될 때까지의 결정
    private func replaceScenario(
        quad: Quad,
        distance: Float,
        pageDistance: Float
    ) -> (decisions: [AutoCaptureDecision], engine: AutoCaptureEngine) {
        var engine = AutoCaptureEngine()
        let first = run(&engine, from: 0, to: 0.6) { frame($0) }
        #expect(first.last == .capture)
        engine.captureFinished(succeeded: true)
        _ = run(&engine, from: 0.7, to: 1.0) { frame($0, hand: true, distance: 0.3) }
        let decisions = run(&engine, from: 1.1, to: 2.0) {
            frame($0, quad: .some(quad), distance: distance, pageDistance: pageDistance)
        }
        return (decisions, engine)
    }

    @Test func 옮겨_놓은_같은_장은_찍고_앞_장을_지울지_묻는다() {
        let moved = shifted(page, by: -0.08)

        let result = replaceScenario(quad: moved, distance: 0.5, pageDistance: 0.25)

        #expect(result.decisions.contains(.captureRepeat))
        #expect(!result.decisions.contains(.capture))
    }

    @Test func 제자리에_놓인_다음_장은_묻지_않고_찍는다() {
        // 사양표가 이어지는 카탈로그는 보정 영역 거리가 가까울 수 있다(2026-11-08 로보월드_1 7번과 8번 0.27)
        let result = replaceScenario(quad: page, distance: 0.5, pageDistance: 0.27)

        #expect(result.decisions.contains(.capture))
        #expect(!result.decisions.contains(.captureRepeat))
    }

    @Test func 옮겨_놓았어도_다른_장이면_그냥_찍는다() {
        let moved = shifted(page, by: -0.08)

        let result = replaceScenario(quad: moved, distance: 0.6, pageDistance: 0.6)

        #expect(result.decisions.contains(.capture))
    }

    @Test func 보정_영역_거리는_찍기_전_안정을_기다릴_때만_잰다() {
        var engine = AutoCaptureEngine()
        #expect(!engine.wantsPageDistance)
        _ = run(&engine, from: 0, to: 0.6) { frame($0) }
        engine.captureFinished(succeeded: true)
        #expect(!engine.wantsPageDistance)

        _ = engine.process(frame(0.7, hand: true))
        _ = engine.process(frame(0.8, distance: 0.8))

        #expect(engine.wantsPageDistance)
    }

    @Test func 수동_셔터는_테두리를_기억해_다음_판단에_쓴다() {
        var engine = AutoCaptureEngine()

        engine.manualCaptureStarted(quad: page)
        engine.captureFinished(succeeded: true)

        #expect(engine.lastCaptureQuad == page)
    }
}
