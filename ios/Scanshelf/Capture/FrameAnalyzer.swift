import CoreImage
import CoreVideo
import Foundation
import ImageIO
import Vision

// 분석 프레임 하나에서 자동 촬영 판단에 쓸 값을 잰다. 카메라 분석 큐에서만 부른다.
// 문서 테두리(VNDetectDocumentSegmentationRequest), 손(VNDetectHumanHandPoseRequest),
// 직전 프레임과의 움직임(축소 흑백 평균 차이), 직전 촬영본과의 거리(VNGenerateImageFeaturePrintRequest).
// @see docs/capture.md

/// 분석할 프레임. 카메라는 픽셀 버퍼, 시뮬레이터용 합성 소스는 CGImage를 준다
struct AnalysisFrame: @unchecked Sendable {
    enum Image {
        case pixelBuffer(CVPixelBuffer)
        case cgImage(CGImage)
    }

    var image: Image
    /// 버퍼를 세로 표시 방향으로 돌리는 방향. 세로로 든 후면 카메라는 `.right`, 합성 소스는 `.up`
    var orientation: CGImagePropertyOrientation
    var time: TimeInterval
    /// 합성 소스만 준다. 시뮬레이터 Vision이 결과를 못 낼 때 화면과 판단을 확인하려는 값
    var hint: SyntheticHint?
}

struct SyntheticHint: Sendable {
    var quad: Quad?
    var handVisible: Bool
    /// 지금 놓인 장 번호. 시뮬레이터에서는 feature print 거리를 못 재므로 같은 장인지 이것으로 판단한다
    var pageNumber: Int
}

struct FrameAnalysis: Sendable {
    var measurement: FrameMeasurement
    /// 시뮬레이터처럼 Vision이 돌지 않아 합성 소스의 힌트를 쓴 경우
    var usedHint: Bool
}

/// 분석 큐와 촬영 작업이 같이 부르므로 상태는 lock으로 지킨다
final class FrameAnalyzer: @unchecked Sendable {
    /// 손 관절 신뢰도가 이 값을 넘는 점이 몇 개 있어야 손으로 본다. 종이 무늬를 손으로 오감지하는 일을 줄인다
    static let handJointConfidence: Float = 0.5
    static let handMinimumJoints = 6
    /// 이보다 신뢰도가 낮은 문서 감지는 버린다
    static let documentConfidence: Float = 0.5
    /// 움직임을 잴 때 줄이는 크기. 작을수록 잡음(센서 노이즈, 자동 노출 흔들림)에 둔하다
    static let motionSide = 32

    private let lock = NSLock()
    private let context = CIContext(options: [.useSoftwareRenderer: false, .cacheIntermediates: false])
    private var previousThumbnail: [UInt8]?
    private var lastFrame: AnalysisFrame?
    private var lastCapturePrint: VNFeaturePrintObservation?
    /// 직전 촬영본의 테두리 안쪽을 원근 보정해 만든 프린트. 종이를 옮기거나 돌려 다시 놓아도 같은 장이면 가깝다
    private var lastCapturePagePrint: VNFeaturePrintObservation?
    private var lastQuad: Quad?
    private var lastCaptureHintPage: Int?

    func analyze(_ frame: AnalysisFrame, wantsDistance: Bool, wantsPageDistance: Bool = false) -> FrameAnalysis {
        lock.lock()
        defer { lock.unlock() }
        lastFrame = frame
        let handler = makeHandler(frame)

        let documentRequest = VNDetectDocumentSegmentationRequest()
        let handRequest = VNDetectHumanHandPoseRequest()
        handRequest.maximumHandCount = 1
        try? handler.perform([documentRequest, handRequest])

        var quad = documentRequest.results?
            .first { $0.confidence >= Self.documentConfidence }
            .map { QuadGeometry.fromVision(topLeft: $0.topLeft, topRight: $0.topRight, bottomRight: $0.bottomRight, bottomLeft: $0.bottomLeft) }
        var hand = (handRequest.results ?? []).contains { observation in
            let points = (try? observation.recognizedPoints(.all)) ?? [:]
            return points.values.filter { $0.confidence > Self.handJointConfidence }.count >= Self.handMinimumJoints
        }
        // 합성 소스는 정답을 같이 준다. 시뮬레이터 Vision은 문서 감지가 엉뚱한 띠를 내거나 비어 있어(2026-11-05 확인)
        // 실기기와 다르므로 믿지 않는다. 손은 그림이라 어차피 못 찾는다
        var usedHint = false
        if let hint = frame.hint {
            quad = hint.quad
            hand = hint.handVisible
            usedHint = true
        }

        lastQuad = quad
        let motion = measureMotion(frame)
        var distance: Float?
        if wantsDistance, let hint = frame.hint, let page = lastCaptureHintPage {
            distance = hint.pageNumber == page ? 0 : 1
        } else if wantsDistance, let reference = lastCapturePrint, let print = featurePrint(handler) {
            var value: Float = 0
            if (try? print.computeDistance(&value, to: reference)) != nil {
                distance = value
            }
        }

        var pageDistance: Float?
        if wantsPageDistance, let hint = frame.hint, let page = lastCaptureHintPage {
            pageDistance = hint.pageNumber == page ? 0 : 1
        } else if wantsPageDistance, let reference = lastCapturePagePrint, let quad,
                  let print = pagePrint(frame, quad: quad) {
            var value: Float = 0
            if (try? print.computeDistance(&value, to: reference)) != nil {
                pageDistance = value
            }
        }

        return FrameAnalysis(
            measurement: FrameMeasurement(
                time: frame.time,
                quad: quad,
                motion: motion,
                handVisible: hand,
                distanceFromLastCapture: distance,
                pageDistanceFromLastCapture: pageDistance
            ),
            usedHint: usedHint
        )
    }

    /// 찍은 순간의 분석 프레임을 기준으로 둔다. 같은 해상도의 프레임끼리 비교해야 거리가 고르다
    func rememberCapture() {
        lock.lock()
        defer { lock.unlock() }
        guard let lastFrame else { return }
        if let hint = lastFrame.hint {
            lastCaptureHintPage = hint.pageNumber
            return
        }
        lastCapturePrint = featurePrint(makeHandler(lastFrame))
        lastCapturePagePrint = lastQuad.flatMap { pagePrint(lastFrame, quad: $0) }
    }

    func forgetCapture() {
        lock.lock()
        defer { lock.unlock() }
        lastCapturePrint = nil
        lastCapturePagePrint = nil
        lastCaptureHintPage = nil
    }

    /// 저장한 사진(HEIC)에서 다시 테두리를 찾는다. 원본에 EXIF 회전을 적용한 축소본을 쓰므로 결과가 곧 manifest 좌표다
    static func detectQuad(inPhoto data: Data) -> Quad? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: 1600,
        ]
        guard let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else { return nil }
        let request = VNDetectDocumentSegmentationRequest()
        try? VNImageRequestHandler(cgImage: image, orientation: .up).perform([request])
        return request.results?
            .first { $0.confidence >= documentConfidence }
            .map { QuadGeometry.fromVision(topLeft: $0.topLeft, topRight: $0.topRight, bottomRight: $0.bottomRight, bottomLeft: $0.bottomLeft) }
    }

    private func makeHandler(_ frame: AnalysisFrame) -> VNImageRequestHandler {
        switch frame.image {
        case let .pixelBuffer(buffer): VNImageRequestHandler(cvPixelBuffer: buffer, orientation: frame.orientation)
        case let .cgImage(image): VNImageRequestHandler(cgImage: image, orientation: frame.orientation)
        }
    }

    /// 테두리 안쪽을 원근 보정해 잘라 낸 종이의 프린트. 믿지 못할 테두리면 재지 않는다
    private func pagePrint(_ frame: AnalysisFrame, quad: Quad) -> VNFeaturePrintObservation? {
        guard QuadGeometry.isPlausible(quad) else { return nil }
        let source: CIImage = switch frame.image {
        case let .pixelBuffer(buffer): CIImage(cvPixelBuffer: buffer)
        case let .cgImage(cgImage): CIImage(cgImage: cgImage)
        }
        // quad는 표시 방향 기준이라 버퍼도 표시 방향으로 세운다. Core Image 좌표는 원점이 왼쪽 아래다
        let image = source.oriented(frame.orientation)
        let extent = image.extent
        func point(_ p: NormalizedPoint) -> CIVector {
            CIVector(x: extent.minX + p.x * extent.width, y: extent.minY + (1 - p.y) * extent.height)
        }
        guard let filter = CIFilter(name: "CIPerspectiveCorrection") else { return nil }
        filter.setValue(image, forKey: kCIInputImageKey)
        filter.setValue(point(quad.topLeft), forKey: "inputTopLeft")
        filter.setValue(point(quad.topRight), forKey: "inputTopRight")
        filter.setValue(point(quad.bottomRight), forKey: "inputBottomRight")
        filter.setValue(point(quad.bottomLeft), forKey: "inputBottomLeft")
        guard let page = filter.outputImage, !page.extent.isEmpty,
              let cgImage = context.createCGImage(page, from: page.extent)
        else { return nil }
        return featurePrint(VNImageRequestHandler(cgImage: cgImage, orientation: .up))
    }

    private func featurePrint(_ handler: VNImageRequestHandler) -> VNFeaturePrintObservation? {
        let request = VNGenerateImageFeaturePrintRequest()
        request.imageCropAndScaleOption = .scaleFill
        try? handler.perform([request])
        return request.results?.first
    }

    /// 32x32 흑백으로 줄여 직전 프레임과의 평균 밝기 차이(0~1)를 잰다. 첫 프레임은 움직인 것으로 본다
    private func measureMotion(_ frame: AnalysisFrame) -> Double {
        let image: CIImage = switch frame.image {
        case let .pixelBuffer(buffer): CIImage(cvPixelBuffer: buffer)
        case let .cgImage(cgImage): CIImage(cgImage: cgImage)
        }
        let side = Self.motionSide
        let scaled = image.transformed(by: CGAffineTransform(
            scaleX: CGFloat(side) / image.extent.width,
            y: CGFloat(side) / image.extent.height
        ))
        var pixels = [UInt8](repeating: 0, count: side * side)
        context.render(
            scaled,
            toBitmap: &pixels,
            rowBytes: side,
            bounds: CGRect(x: 0, y: 0, width: side, height: side),
            format: .L8,
            colorSpace: nil
        )
        defer { previousThumbnail = pixels }
        guard let previous = previousThumbnail else { return 1 }
        return Self.meanDifference(previous, pixels)
    }

    static func meanDifference(_ a: [UInt8], _ b: [UInt8]) -> Double {
        guard a.count == b.count, !a.isEmpty else { return 1 }
        var sum = 0
        for i in a.indices {
            sum += abs(Int(a[i]) - Int(b[i]))
        }
        return Double(sum) / Double(a.count) / 255
    }
}
