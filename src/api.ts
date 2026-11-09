import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import type {
  AskAnswer,
  CalendarAccess,
  DocumentTags,
  SessionSuggestion,
  TagSuggestion,
  DocumentDetail,
  DocumentHits,
  DocumentSummary,
  Edits,
  ExportFile,
  ExportFormat,
  AppAction,
  ImportResult,
  QuickOpenEntry,
  InboxStatus,
  InboxRefresh,
  DocumentOrigin,
  OcrPage,
  PairingCode,
  PamphletSummary,
  ProcessOptions,
  TransferStatus,
} from "./types";
import type { BookReport, SortResult } from "./views/BookPanel";

/** 다른 팀원 모듈(전송, AI)의 명령은 아직 등록되지 않았을 수 있다. 그때는 null */
export async function invokeOptional<T>(command: string, args?: Record<string, unknown>): Promise<T | null> {
  try {
    return await invoke<T>(command, args);
  } catch (e) {
    const message = String(e);
    if (message.includes("not found") || message.includes("unknown command")) return null;
    throw e;
  }
}

/** 파일 경로를 웹뷰 주소로. 같은 경로에 다시 쓴 그림이 캐시에 남지 않게 버전을 붙인다 */
export function fileUrl(path: string, version?: string): string {
  const url = convertFileSrc(path);
  return version ? `${url}?v=${encodeURIComponent(version)}` : url;
}

export const api = {
  listDocuments: () => invoke<DocumentSummary[]>("list_documents"),
  getDocument: (documentId: string) => invoke<DocumentDetail>("get_document", { documentId }),
  saveEdits: (documentId: string, edits: Edits) => invoke<void>("save_edits", { documentId, edits }),
  setOptions: (documentId: string, options: ProcessOptions) =>
    invoke<void>("set_processing_options", { documentId, options }),
  reprocess: (documentId: string, force: boolean) => invoke<void>("reprocess_document", { documentId, force }),
  rawPreview: (documentId: string, pageId: string) => invoke<string>("raw_preview", { documentId, pageId }),
  getOcr: (documentId: string, outputId: string) => invoke<OcrPage | null>("get_ocr", { documentId, outputId }),
  search: (query: string) => invoke<DocumentHits[]>("search_library", { query }),
  exportDocument: (documentId: string, format: ExportFormat) =>
    invoke<ExportFile>("export_document", { documentId, format }),
  listExports: (documentId: string) => invoke<ExportFile[]>("list_exports", { documentId }),
  revealExport: (path: string) => invoke<void>("reveal_export", { path }),
  shareExport: (path: string, x: number, y: number) => invoke<void>("share_export", { path, x, y }),
  saveExportAs: (path: string) => invoke<string | null>("save_export_as", { path }),
  getSummary: (documentId: string) => invoke<PamphletSummary | null>("get_summary", { documentId }),
  saveSummaryField: (documentId: string, field: string, value: unknown) =>
    invoke<PamphletSummary>("save_summary_field", { documentId, edit: { field, value } }),

  // 책 쪽 번호 확인(src-tauri/src/book.rs, docs/book.md). 정렬은 사용자가 누를 때만
  bookPageReport: (documentId: string) => invoke<BookReport>("book_page_report", { documentId }),
  sortByPageNumbers: (documentId: string) => invoke<SortResult>("sort_by_page_numbers", { documentId }),

  // 전송(mac-transfer 담당)
  transferStatus: () => invokeOptional<TransferStatus>("transfer_status"),
  startPairing: () => invokeOptional<PairingCode>("start_pairing"),
  cancelPairing: () => invokeOptional<void>("cancel_pairing"),
  removePairedDevice: (deviceId: string) => invokeOptional<void>("remove_paired_device", { deviceId }),
  icloudInboxStatus: () => invokeOptional<InboxStatus>("icloud_inbox_status"),
  /** 보관함 ⌘R. iCloud inbox를 지금 한 번 훑고 결과를 돌려준다(src-tauri/src/inbox.rs refresh_inbox) */
  refreshInbox: () => invokeOptional<InboxRefresh>("refresh_inbox"),
  documentOrigin: (documentId: string) => invokeOptional<DocumentOrigin>("document_origin", { documentId }),

  // AI 정리(ai 담당, 리드가 연결)
  summarizePamphlet: (documentId: string) => invokeOptional<PamphletSummary>("summarize_pamphlet", { documentId }),
  suggestDocumentName: (documentId: string) =>
    invokeOptional<{ name: string } | string | null>("suggest_document_name", { documentId }),
  exportNotionCsv: (session: string) =>
    invokeOptional<{ folder: string; csv: string; rows: number; pdfs: number; skipped: string[] }>("export_notion_csv", { session }),
  exportDocx: (documentId: string) => invokeOptional<unknown>("export_docx", { documentId }),
  exportAiBundle: (documentId: string) => invokeOptional<unknown>("export_ai_bundle", { documentId }),

  // 단축키, 링크, 우클릭 메뉴, 가져오기 (docs/shortcuts-and-deep-links.md, docs/context-menu.md, docs/import.md)
  frontendReady: () => invoke<{ pending: AppAction[]; shortcutError: string | null }>("frontend_ready"),
  revealDocument: (documentId: string) => invoke<void>("reveal_document", { documentId }),
  quickOpenEntries: () => invoke<QuickOpenEntry[]>("quick_open_entries"),
  documentMarkdown: (documentId: string) => invoke<string>("document_markdown", { documentId }),
  deleteDocument: (documentId: string) => invoke<void>("delete_document", { documentId }),
  importFiles: (paths: string[]) => invoke<ImportResult>("import_files", { paths }),
  pickImportFiles: () => invoke<string[]>("pick_import_files"),
  // 질문, 태그, 캘린더 묶음 추천(docs/ask.md, docs/organize.md)
  askDocument: (documentId: string, question: string) => invoke<AskAnswer>("ask_document", { documentId, question }),
  askLibrary: (question: string, tag: string | null, session: string | null) =>
    invoke<AskAnswer>("ask_library", { question, tag, session }),
  documentTags: () => invoke<Record<string, DocumentTags>>("document_tags"),
  setDocumentTags: (documentId: string, tags: string[]) => invoke<string[]>("set_document_tags", { documentId, tags }),
  suggestTags: (documentId: string) => invoke<TagSuggestion>("suggest_tags", { documentId }),
  calendarAccess: () => invoke<CalendarAccess>("calendar_access"),
  suggestSession: (documentId: string) => invoke<SessionSuggestion>("suggest_session", { documentId }),
};

// 선택 목록 순서. 스캔이 중심이라 일반 문서가 먼저, 팜플렛(AI 정리 대상)은 마지막
export const KIND_LABEL: Record<string, string> = {
  document: "문서",
  book: "책",
  pamphlet: "팜플렛",
};

/** 화면 강조와 검색이 같은 규칙: 공백 빼고 소문자 (src-tauri/src/search.rs compact) */
export function compact(s: string): string {
  return s.replace(/\s+/g, "").toLowerCase();
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n}B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)}KB`;
  return `${(n / 1024 / 1024).toFixed(1)}MB`;
}
