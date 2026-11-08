import SwiftUI

// 첫 화면: 문서 목록과 보낸 상태, 새 문서 시작.
// @see docs/ios.md

struct DocumentsView: View {
    @Environment(AppModel.self) private var app
    @State private var showingNew = false
    @State private var capturing: CaptureRequest?
    @State private var showingSettings = false
    @State private var showingStorage = false
    @Environment(\.openURL) private var openURL

    var body: some View {
        List {
            if let update = app.updateBanner {
                UpdateBanner(update: update) {
                    // AltStore가 없거나 주소를 받지 않으면 Release 페이지를 연다
                    openURL(UpdateCheck.altStoreURL) { accepted in
                        if !accepted { openURL(update.releaseURL) }
                    }
                } onDismiss: {
                    app.dismissUpdate()
                }
            }
            if let notice = app.notice {
                Text(notice).foregroundStyle(.secondary)
            }
            ForEach(sessions, id: \.self) { session in
                Section(session.isEmpty ? "묶지 않은 문서" : session) {
                    ForEach(app.documents.filter { ($0.session ?? "") == session }) { manifest in
                        NavigationLink(value: manifest.id) {
                            DocumentRow(manifest: manifest, status: app.status(of: manifest), cloud: app.cloudStatus(of: manifest))
                        }
                    }
                    .onDelete { offsets in
                        let inSession = app.documents.filter { ($0.session ?? "") == session }
                        for index in offsets {
                            try? app.deleteDocument(inSession[index].id)
                        }
                    }
                }
            }
        }
        .overlay {
            if app.documents.isEmpty {
                ContentUnavailableView(
                    "아직 문서가 없습니다",
                    systemImage: "scanshelf",
                    description: Text("거치대에 iPhone을 올리고 새 문서를 시작하세요. 종이만 바꿔 놓으면 한 장씩 찍힙니다.")
                )
            }
        }
        .navigationTitle("scanshelf")
        .navigationDestination(for: UUID.self) { id in
            DocumentDetailView(documentID: id) { manifest in capturing = CaptureRequest(manifest: manifest) }
        }
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                Button("설정", systemImage: "gearshape") { showingSettings = true }
                    .accessibilityIdentifier("settings")
            }
            ToolbarItem(placement: .topBarTrailing) {
                // 보내기는 앱을 열 때와 촬영을 마칠 때 저절로 한다. 버튼은 어디에 무엇이 있는지 보여 주는 화면을 연다
                Button("보관 현황", systemImage: "externaldrive.badge.icloud") { showingStorage = true }
                    .accessibilityIdentifier("storage")
            }
        }
        // 목록을 내려도 가려지지 않게 목록 밖에 둔다
        .safeAreaInset(edge: .top) {
            if let notice = app.backupNotice {
                BackupNoticeView(notice: notice)
            }
        }
        .safeAreaInset(edge: .bottom) {
            Button {
                showingNew = true
            } label: {
                Label("새 문서", systemImage: "plus")
                    .font(.headline)
                    .frame(maxWidth: .infinity, minHeight: 50)
            }
            .buttonStyle(.borderedProminent)
            .tint(.green)
            .padding()
            .accessibilityIdentifier("newDocument")
        }
        .sheet(isPresented: $showingNew) {
            NewDocumentView { manifest, layout in
                showingNew = false
                capturing = CaptureRequest(manifest: manifest, layout: layout)
            }
        }
        .sheet(isPresented: $showingSettings) {
            NavigationStack { SettingsView() }
        }
        .sheet(isPresented: $showingStorage) {
            NavigationStack { StorageOverviewView() }
        }
        .fullScreenCover(item: $capturing) { request in
            CaptureView(document: request.manifest, layout: request.layout, app: app)
                .onDisappear {
                    // 촬영을 마치면 바로 보내 본다. Mac이 없으면 기록만 남고 다음에 다시 시도한다
                    Task { await app.sendPending() }
                }
        }
        // 앱이 꺼져 있다 "스캔 시작"으로 켜지면 요청이 화면보다 먼저 온다. 나타날 때 한 번, 그 뒤로는 올 때마다 가져간다
        .task {
            #if DEBUG
            openDebugScreen()
            #endif
            startRequestedScan()
        }
        .onChange(of: ScanLauncher.shared.pending) { startRequestedScan() }
    }

    /// "스캔 시작" 동작. 마지막 종류, 묶음, 배치로 새 문서를 만들고 촬영 화면을 연다
    private func startRequestedScan() {
        guard let request = ScanLauncher.shared.take() else { return }
        // 이미 찍는 중이면 그 화면을 그대로 둔다
        guard capturing == nil else { return }
        let plan = ScanStartPlan.make(requested: request.kind, settings: app.settings)
        let sheetOpen = showingNew || showingSettings
        showingNew = false
        showingSettings = false
        do {
            let manifest = try app.createDocument(name: "", kind: plan.kind, session: plan.session)
            let open = CaptureRequest(manifest: manifest, layout: plan.layout)
            if sheetOpen {
                // 시트가 내려간 뒤에 띄워야 전체 화면이 뜬다
                Task {
                    try? await Task.sleep(for: .milliseconds(600))
                    capturing = open
                }
            } else {
                capturing = open
            }
        } catch {
            app.notice = "새 문서를 만들지 못했습니다: \(error.localizedDescription)"
        }
    }

    #if DEBUG
    /// 시뮬레이터 화면 확인용. `-scanshelfScreen capture|new|settings|scan|update` 로 켜면 그 화면을 바로 연다(docs/ios.md)
    private func openDebugScreen() {
        guard let screen = UserDefaults.standard.string(forKey: "scanshelfScreen") else { return }
        switch screen {
        case "capture":
            let manifest = app.documents.first { $0.pages.isEmpty }
                ?? (try? app.createDocument(name: "", kind: .pamphlet, session: "2026 예시박람회"))
            capturing = manifest.map { CaptureRequest(manifest: $0) }
        case "new": showingNew = true
        case "settings": showingSettings = true
        case "storage": showingStorage = true
        case "scan":
            // "스캔 시작" 동작과 같은 길. `-scanshelfScanKind book` 으로 종류를 줄 수 있다
            let kind = UserDefaults.standard.string(forKey: "scanshelfScanKind").flatMap(DocumentKind.init(rawValue:))
            ScanLauncher.shared.request(kind: kind)
        case "update":
            app.settings.availableUpdate = AvailableUpdate(
                version: "9.9.9",
                releaseURL: URL(string: "https://github.com/nobel6018/scanshelf/releases/latest")!
            )
            app.settings.dismissedUpdateVersion = nil
            // 실제 확인이 가짜 알림을 덮지 않게 오늘은 확인한 것으로 둔다
            app.settings.lastUpdateCheckAt = .now
        default: break
        }
    }
    #endif

    /// 최근 문서의 묶음이 위로 온다
    private var sessions: [String] {
        var seen: [String] = []
        for manifest in app.documents where !seen.contains(manifest.session ?? "") {
            seen.append(manifest.session ?? "")
        }
        return seen
    }
}

struct DocumentRow: View {
    let manifest: Manifest
    let status: SendStatus
    let cloud: CloudStatus

    var body: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text(manifest.name).font(.headline).lineLimit(1)
                Text("\(manifest.kind.label), \(manifest.pages.count)장, \(manifest.createdAt.formatted(date: .abbreviated, time: .shortened))")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            StorageBadges(mac: status, cloud: cloud)
        }
        .padding(.vertical, 2)
    }
}

/// 사본이 있는 곳을 장소마다 하나씩 보여 준다: Mac, iCloud. 둘 다 없으면 "iPhone에만".
/// 예전에는 한 칸에 "iCloud에 있음"과 "Mac에 있음" 중 하나만 보여서, Mac에 간 문서가 iCloud에도 있는지 알 수 없었다
struct StorageBadges: View {
    let mac: SendStatus
    let cloud: CloudStatus

    var body: some View {
        let macChip = macChip
        let cloudChip = cloudChip
        HStack(spacing: 10) {
            if macChip == nil, cloudChip == nil {
                Label("iPhone에만", systemImage: "iphone").labelStyle(.badge).foregroundStyle(.orange)
            }
            if let macChip { macChip }
            if let cloudChip { cloudChip }
        }
    }

    private var macChip: AnyView? {
        switch mac {
        case .sent:
            AnyView(Label("Mac", systemImage: "checkmark.circle.fill").labelStyle(.badge).foregroundStyle(.green))
        case let .sending(progress):
            AnyView(HStack(spacing: 4) {
                ProgressView(value: progress).frame(width: 36)
                Text("Mac").font(.caption.weight(.medium))
            }.foregroundStyle(.green))
        case .changedSinceSent:
            AnyView(Label("Mac", systemImage: "arrow.triangle.2.circlepath").labelStyle(.badge).foregroundStyle(.orange))
        case .failed:
            AnyView(Label("Mac", systemImage: "exclamationmark.triangle.fill").labelStyle(.badge).foregroundStyle(.red))
        case .notSent, .backupPending, .awaitingMac:
            nil
        }
    }

    private var cloudChip: AnyView? {
        switch cloud {
        case .archived, .inInbox:
            AnyView(Label("iCloud", systemImage: "checkmark.icloud").labelStyle(.badge).foregroundStyle(.blue))
        case .uploading:
            AnyView(Label("iCloud", systemImage: "icloud.and.arrow.up").labelStyle(.badge).foregroundStyle(.orange))
        case .off:
            nil
        }
    }
}

struct BadgeLabelStyle: LabelStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 4) {
            configuration.icon
            configuration.title
        }
        .font(.caption.weight(.medium))
    }
}

extension LabelStyle where Self == BadgeLabelStyle {
    static var badge: BadgeLabelStyle { BadgeLabelStyle() }
}

/// 새 버전 알림. AltStore에서 업데이트하고, 닫으면 그 버전은 다시 알리지 않는다
private struct UpdateBanner: View {
    let update: AvailableUpdate
    let onUpdate: () -> Void
    let onDismiss: () -> Void

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "arrow.down.circle.fill")
                .font(.title2)
                .foregroundStyle(.green)
            VStack(alignment: .leading, spacing: 2) {
                Text("새 버전 \(update.version)").font(.headline)
                Text("AltStore에서 업데이트합니다").font(.caption).foregroundStyle(.secondary)
            }
            Spacer()
            Button("업데이트", action: onUpdate)
                .buttonStyle(.borderedProminent)
                .tint(.green)
                .frame(minHeight: 44)
                .accessibilityIdentifier("updateButton")
            Button("닫기", systemImage: "xmark", action: onDismiss)
                .labelStyle(.iconOnly)
                .buttonStyle(.borderless)
                .foregroundStyle(.secondary)
                .frame(minWidth: 44, minHeight: 44)
                .accessibilityIdentifier("dismissUpdate")
        }
        .accessibilityIdentifier("updateBanner")
    }
}

/// 촬영 화면을 여는 요청. 새 문서는 고른 배치로, 이어 찍기는 마지막 장의 배치로 시작한다
struct CaptureRequest: Identifiable {
    var manifest: Manifest
    var layout: PageLayout?

    var id: UUID { manifest.id }
}
