import SwiftUI

// 촬영 화면. 미리보기(4:3)와 테두리, 자동 촬영 안내, 페이지 띠, 셔터.
// 거치대에 올려 두고 멀리서 보므로 안내 글자와 셔터를 크게 둔다.
// @see docs/capture.md

struct CaptureView: View {
    @State private var model: CaptureModel
    @State private var flashOpacity = 0.0
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    private let app: AppModel

    init(document: Manifest, layout: PageLayout?, app: AppModel) {
        self.app = app
        _model = State(initialValue: CaptureModel(document: document, layout: layout, app: app))
    }

    var body: some View {
        VStack(spacing: 0) {
            topBar
            if let notice = app.backupNotice {
                BackupNoticeView(notice: notice, compact: true)
            }
            preview
                .padding(.top, 8)
            Spacer(minLength: 8)
            PageStrip(model: model)
            controls
                .padding(.vertical, 10)
        }
        .background(Color.black.ignoresSafeArea())
        .foregroundStyle(.white)
        .task { await model.start() }
        .onChange(of: scenePhase) { _, phase in
            switch phase {
            case .active: Task { await model.start() }
            case .background: model.stop()
            default: break
            }
        }
        .onChange(of: model.flashCount) {
            flashOpacity = 0.85
            withAnimation(.easeOut(duration: CaptureModel.flashDuration * 2)) { flashOpacity = 0 }
        }
        .statusBarHidden()
        // 거치대에 올려 두고 찍는 동안 화면이 꺼지지 않게 한다
        .onAppear { UIApplication.shared.isIdleTimerDisabled = true }
        .onDisappear {
            model.stop()
            UIApplication.shared.isIdleTimerDisabled = false
        }
    }

    private var topBar: some View {
        HStack(spacing: 12) {
            Button("완료") { dismiss() }
                .font(.headline)
                .frame(minWidth: 44, minHeight: 44)
            VStack(spacing: 2) {
                Text(model.document.name)
                    .font(.headline)
                    .lineLimit(1)
                Text("\(model.document.pages.count)장")
                    .font(.caption)
                    .foregroundStyle(.white.opacity(0.7))
            }
            .frame(maxWidth: .infinity)
            Toggle(isOn: $model.autoEnabled) {
                Text("자동")
                    .font(.subheadline.weight(.semibold))
            }
            .toggleStyle(.button)
            .tint(.green)
            .frame(minHeight: 44)
            .accessibilityIdentifier("autoToggle")
        }
        .padding(.horizontal)
    }

    private var preview: some View {
        ZStack {
            if let camera = model.cameraSession {
                CameraPreview(session: camera.session) { model.shutter() }
            } else if let synthetic = model.syntheticSource {
                SyntheticPreview(source: synthetic)
            }
            GeometryReader { proxy in
                if let quad = model.quad {
                    QuadShape(quad: quad)
                        .fill(Color.green.opacity(0.18))
                        .overlay(QuadShape(quad: quad).stroke(statusColor, lineWidth: 3))
                        .frame(width: proxy.size.width, height: proxy.size.height)
                        .animation(.linear(duration: 0.1), value: quad)
                }
            }
            Color.white.opacity(flashOpacity)
                .allowsHitTesting(false)
            VStack {
                if model.usingHint {
                    Text("시뮬레이터 합성 화면")
                        .font(.caption2)
                        .padding(4)
                        .background(.black.opacity(0.5), in: .capsule)
                }
                Spacer()
                if model.repeatOffer != nil {
                    repeatOfferBar
                }
                statusLabel
            }
            .padding(10)
            if let error = model.cameraError {
                Text(error)
                    .multilineTextAlignment(.center)
                    .padding()
                    .background(.black.opacity(0.7), in: .rect(cornerRadius: 12))
                    .padding()
            }
        }
        .aspectRatio(3.0 / 4.0, contentMode: .fit)
        .clipped()
    }

    private var statusLabel: some View {
        let text = model.message ?? statusText
        return Text(text)
            .font(.title3.weight(.semibold))
            .padding(.horizontal, 14)
            .padding(.vertical, 8)
            .background(.black.opacity(0.6), in: .capsule)
            .accessibilityIdentifier("captureStatus")
    }

    /// 방금 찍은 장이 앞 장을 옮기거나 돌려 다시 놓은 것으로 보일 때. 비슷한 다음 장일 수도 있어 사람이 고른다
    private var repeatOfferBar: some View {
        HStack(spacing: 8) {
            Text("앞 장을 다시 놓았나요?")
                .font(.callout.weight(.medium))
            Button("앞 장 지우기") { model.deleteRepeatedPage() }
                .buttonStyle(.borderedProminent)
                .tint(.green)
                .frame(minHeight: 44)
                .accessibilityIdentifier("deleteRepeatedPage")
            Button("그대로 두기", systemImage: "xmark") { model.dismissRepeatOffer() }
                .labelStyle(.iconOnly)
                .frame(minWidth: 44, minHeight: 44)
        }
        .padding(.horizontal, 12)
        .background(.black.opacity(0.7), in: .capsule)
    }

    private var statusText: String {
        if model.retakeTarget != nil, model.status != .capturing {
            return "다시 찍을 장을 놓아 주세요"
        }
        if model.layout == .trifold, model.status == .noDocument || model.status == .waitingForChange {
            return model.status == .noDocument ? "3단 접지를 펼쳐 놓아 주세요" : "다음 장을 펼쳐 놓아 주세요"
        }
        guard model.autoEnabled else { return "수동 촬영: 셔터나 볼륨 버튼" }
        return switch model.status {
        case .noDocument: "종이를 놓아 주세요"
        case .handVisible: "손을 치워 주세요"
        case .unclearEdges: "모서리를 찾는 중입니다"
        case .cutOff: "종이 전체가 화면에 들어오게 놓아 주세요"
        case .moving: "움직임이 멈추기를 기다립니다"
        case let .steadying(progress): progress < 0.05 ? "멈추면 찍습니다" : "곧 찍습니다"
        case .capturing: "찍는 중"
        case .waitingForChange: "다음 장을 놓아 주세요"
        }
    }

    private var statusColor: Color {
        switch model.status {
        case .steadying, .capturing: .green
        case .handVisible, .moving, .unclearEdges, .cutOff: .yellow
        default: .white
        }
    }

    /// 배치 선택은 셔터 위 한 줄에 둔다. 팜플렛은 선택지가 셋이라 셔터 옆에 두면 좁은 화면 폭을 넘는다
    private var controls: some View {
        VStack(spacing: 12) {
            Picker("배치", selection: $model.layout) {
                ForEach(model.layoutChoices, id: \.self) { layout in
                    Text(layout.shortLabel).tag(layout)
                }
            }
            .pickerStyle(.segmented)
            .frame(width: CGFloat(model.layoutChoices.count) * 72)
            .colorScheme(.dark)
            .accessibilityIdentifier("layoutPicker")

            Button {
                model.shutter()
            } label: {
                ZStack {
                    Circle().stroke(.white, lineWidth: 4).frame(width: 76, height: 76)
                    Circle().fill(model.isSaving ? Color.gray : Color.white).frame(width: 62, height: 62)
                }
            }
            .disabled(model.isSaving)
            .accessibilityLabel("셔터")
            .accessibilityIdentifier("shutter")
        }
        .frame(maxWidth: .infinity)
        .padding(.horizontal)
    }
}

/// 찍은 장 띠. 누르면 지우기, 다시 찍기
private struct PageStrip: View {
    let model: CaptureModel

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    ForEach(Array(model.document.pages.enumerated()), id: \.element.id) { index, page in
                        Menu {
                            Button("다시 찍기", systemImage: "arrow.counterclockwise") { model.retake(page.id) }
                            Button("지우기", systemImage: "trash", role: .destructive) { model.delete(page.id) }
                        } label: {
                            PageThumbnail(
                                url: model.rawURL(page),
                                quad: page.quad,
                                maxPixel: 200
                            )
                            .frame(width: 54, height: 72)
                            .overlay(alignment: .bottomTrailing) {
                                Text("\(index + 1)")
                                    .font(.caption2.bold())
                                    .padding(3)
                                    .background(.black.opacity(0.6), in: .rect(cornerRadius: 4))
                            }
                            .overlay {
                                if model.retakeTarget == page.id {
                                    RoundedRectangle(cornerRadius: 4).stroke(.orange, lineWidth: 3)
                                }
                            }
                        }
                        .id(page.id)
                    }
                }
                .padding(.horizontal)
            }
            .frame(height: 76)
            .onChange(of: model.document.pages.last?.id) { _, id in
                guard let id else { return }
                withAnimation { proxy.scrollTo(id, anchor: .trailing) }
            }
        }
    }
}
