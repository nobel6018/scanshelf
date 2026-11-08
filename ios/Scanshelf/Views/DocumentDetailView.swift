import SwiftUI

// 문서 하나: 이름, 종류, 묶음 고치기, 찍은 장 보기와 지우기, 이어서 찍기, 보내기.
// @see docs/ios.md

struct DocumentDetailView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss

    let documentID: UUID
    let onContinue: (Manifest) -> Void

    @State private var name = ""
    @State private var session = ""
    @State private var kind: DocumentKind = .document
    @State private var confirmDelete = false
    @State private var failure: String?

    var body: some View {
        if let manifest = app.document(documentID) {
            content(manifest)
        } else {
            ContentUnavailableView("문서가 없습니다", systemImage: "doc")
        }
    }

    private func content(_ manifest: Manifest) -> some View {
        Form {
            Section("정보") {
                TextField("이름", text: $name)
                    .onSubmit { save(manifest) }
                TextField("묶음", text: $session)
                    .onSubmit { save(manifest) }
                Picker("종류", selection: $kind) {
                    ForEach(DocumentKind.displayOrder, id: \.self) { value in
                        Text(value.label).tag(value)
                    }
                }
                .onChange(of: kind) { save(manifest) }
            }

            Section {
                StorageRow(place: "iPhone", systemImage: "iphone", text: "있음", tone: .secondary)
                StorageRow(place: "Mac", systemImage: "desktopcomputer", text: macText(manifest), tone: macTone(manifest))
                StorageRow(place: "iCloud", systemImage: "icloud", text: cloudText(manifest), tone: cloudTone(manifest))
                if let reason = app.failures[manifest.id] {
                    Text("Wi-Fi로 보내지 못했습니다: \(reason)").font(.footnote).foregroundStyle(.red)
                }
                if let reason = app.backup.state.entries[manifest.id]?.lastError {
                    Text("iCloud 복사를 다시 시도합니다: \(reason)").font(.footnote).foregroundStyle(.secondary)
                }
                Button("지금 Mac으로 보내기 (Wi-Fi)", systemImage: "wifi") {
                    Task { await app.send(manifest.id) }
                }
                .disabled(app.settings.pairedMac == nil || manifest.pages.isEmpty)
                Button("iCloud 복사 지금 다시 시도", systemImage: "icloud") {
                    app.retryBackup(manifest.id)
                }
                .disabled(!app.iCloud.isChosen || manifest.pages.isEmpty)
            } header: {
                Text("어디에 있나")
            } footer: {
                Text("같은 Wi-Fi에 Mac이 있으면 Wi-Fi로, 떨어져 있으면 iCloud로 Mac에 갑니다. Mac은 받은 원본을 iCloud scanshelf/archive에 계속 보관합니다.")
            }

            Section("\(manifest.pages.count)장") {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 90), spacing: 10)], spacing: 10) {
                    ForEach(Array(manifest.pages.enumerated()), id: \.element.id) { index, page in
                        PageThumbnail(url: app.store.fileURL(of: manifest.id, relativePath: page.raw), quad: page.quad)
                            .frame(height: 120)
                            .overlay(alignment: .bottomLeading) {
                                Text(page.layout.badge.map { "\(index + 1) \($0)" } ?? "\(index + 1)")
                                    .font(.caption2.bold())
                                    .padding(3)
                                    .background(.thinMaterial, in: .rect(cornerRadius: 4))
                            }
                            .contextMenu {
                                Button("지우기", systemImage: "trash", role: .destructive) {
                                    do { _ = try app.deletePage(page.id, from: manifest) } catch { failure = error.localizedDescription }
                                }
                            }
                    }
                }
                .padding(.vertical, 4)
            }

            if let failure {
                Text(failure).foregroundStyle(.red)
            }

            Section {
                Button("문서 지우기", role: .destructive) { confirmDelete = true }
            }
        }
        .navigationTitle(manifest.name)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button("이어서 찍기", systemImage: "camera") { onContinue(manifest) }
            }
        }
        .onAppear {
            name = manifest.name
            session = manifest.session ?? ""
            kind = manifest.kind
        }
        .onDisappear { save(manifest) }
        .confirmationDialog("이 iPhone에서 문서와 원본을 지웁니다", isPresented: $confirmDelete, titleVisibility: .visible) {
            Button("지우기", role: .destructive) {
                try? app.deleteDocument(manifest.id)
                dismiss()
            }
        } message: {
            Text("Mac과 iCloud 보관 폴더에 있는 사본은 남습니다.")
        }
    }

    private func macText(_ manifest: Manifest) -> String {
        switch app.status(of: manifest) {
        case let .sent(method, date):
            "\(method == .wifi ? "Wi-Fi" : "iCloud")로 받음, \(date.formatted(date: .abbreviated, time: .shortened))"
        case let .sending(progress): "Wi-Fi로 보내는 중 \(Int(progress * 100))%"
        case .changedSinceSent: "예전 판만 있음. 더 찍은 장을 보내는 중"
        case .failed: "보내지 못함"
        case .awaitingMac: "iCloud에서 가져가기를 기다림"
        case .notSent, .backupPending: "아직 없음"
        }
    }

    private func macTone(_ manifest: Manifest) -> Color {
        if case .sent = app.status(of: manifest) { return .green }
        return .secondary
    }

    private func cloudText(_ manifest: Manifest) -> String {
        switch app.cloudStatus(of: manifest) {
        case .archived: "보관됨"
        case .inInbox: "올라감. Mac이 가져가면 보관 폴더로 옮김"
        case .uploading:
            if case .sent = app.status(of: manifest) { "Mac이 보관 폴더에 올리는 중" } else { "올리는 중" }
        case .off: "iCloud 폴더를 고르지 않음"
        }
    }

    private func cloudTone(_ manifest: Manifest) -> Color {
        app.cloudStatus(of: manifest).isSafe ? .blue : .secondary
    }

    private func save(_ manifest: Manifest) {
        guard let current = app.document(manifest.id) else { return }
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        guard trimmed != current.name || session != (current.session ?? "") || kind != current.kind else { return }
        do {
            try app.updateMetadata(current, name: trimmed, kind: kind, session: session)
        } catch {
            failure = error.localizedDescription
        }
    }
}

/// 한 장소의 사본 상태 한 줄
private struct StorageRow: View {
    let place: String
    let systemImage: String
    let text: String
    let tone: Color

    var body: some View {
        HStack {
            Label(place, systemImage: systemImage)
            Spacer()
            Text(text).font(.callout).foregroundStyle(tone).multilineTextAlignment(.trailing)
        }
    }
}
