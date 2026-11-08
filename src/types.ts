// Rust 쪽 직렬화 모양과 같다. 원본: crates/scanshelf-core, src-tauri/src/{library,search,export}.rs, transfer, inbox
export type DocumentKind = "pamphlet" | "book" | "document";
export type PageLayout = "single" | "spread" | "trifold";
export type PagePart = "whole" | "left" | "right" | { panel: number };
export type Dewarp = "off" | "normal" | "strong";
export type Filter = "original" | "enhanced" | "grayscale" | "blackWhite";

export interface Point {
  x: number;
  y: number;
}

export interface Quad {
  topLeft: Point;
  topRight: Point;
  bottomRight: Point;
  bottomLeft: Point;
}

export type DocumentState =
  | { status: "unprocessed" }
  | { status: "stale"; pages: number }
  | { status: "ready" }
  | { status: "queued" }
  | { status: "processing"; done: number; total: number }
  | { status: "failed"; message: string };

export interface DocumentSummary {
  id: string;
  name: string;
  kind: DocumentKind;
  session: string | null;
  pageCount: number;
  createdAt: string;
  state: DocumentState;
  cover: string | null;
  hasSummary: boolean;
}

export interface ProcessOptions {
  dewarp: Dewarp;
  filter: Filter;
  removeFingers: boolean;
  removeHandwriting: boolean;
}

export interface Edits {
  name?: string;
  kind?: DocumentKind;
  session?: string;
  quads?: Record<string, Quad>;
  layouts?: Record<string, PageLayout>;
  /** 자동 회전 뒤에 더 돌린 각도(시계 방향) */
  rotations?: Record<string, number>;
  deletedPages?: string[];
  pageOrder?: string[];
  /** `/` 계층 태그. 고칠 때는 api.setDocumentTags를 쓴다(tagsByUser를 함께 켠다) */
  tags?: string[];
  tagsByUser?: boolean;
}

export interface OutputView {
  id: string;
  part: PagePart;
  image: string;
  thumb: string | null;
  hasOcr: boolean;
  processedAt: string;
}

export type QuadSource = "manual" | "phone" | "detected" | "none";

export interface PageView {
  id: string;
  raw: string;
  capturedAt: string;
  layout: PageLayout;
  quad: Quad | null;
  quadSource: QuadSource;
  outputs: OutputView[];
}

export interface DocumentDetail {
  summary: DocumentSummary;
  dir: string;
  pages: PageView[];
  deletedPages: string[];
  options: ProcessOptions;
  edits: Edits;
}

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface OcrLine {
  text: string;
  bbox: Rect;
  confidence: number;
}

export interface OcrBlock {
  kind: "title" | "paragraph" | "list" | "table";
  text: string;
  bbox: Rect;
  lines: OcrLine[];
}

export interface OcrPage {
  engine: string;
  imageWidth: number;
  imageHeight: number;
  text: string;
  blocks: OcrBlock[];
}

export interface Segment {
  text: string;
  hit: boolean;
}

export interface PageHit {
  outputId: string;
  pageNumber: number;
  snippet: Segment[];
  matches: number;
}

export interface DocumentHits {
  documentId: string;
  name: string;
  session: string | null;
  kind: DocumentKind;
  nameMatch: boolean;
  hits: PageHit[];
  totalPages: number;
}

export type ExportFormat = "pdf" | "jpg" | "txt";

export interface ExportFile {
  path: string;
  name: string;
  size: number;
  modifiedAt: string;
}

export type Relevance = "competitor" | "partnerCandidate" | "supplier" | "unrelated";

export interface PamphletSummary {
  company: string;
  country: string;
  event: string;
  date: string;
  products: string;
  keySpecs: string;
  targetMarket: string;
  references: string;
  relevance: Relevance[];
  relevanceNote: string;
  contact: string;
  sourceFile: string;
  generatedBy: string;
  generatedAt: string;
  editedFields: string[];
}

export interface Progress {
  documentId: string;
  done: number;
  total: number;
  stage: "decode" | "correct" | "ocr" | "done";
}

export interface PairedDevice {
  deviceId: string;
  deviceName: string;
  pairedAt: string;
}

export interface TransferStatus {
  listening: boolean;
  port: number;
  macName: string;
  pairedDevices: PairedDevice[];
}

export interface PairingCode {
  code: string;
  expiresAt: string;
}

export interface PendingInboxDocument {
  documentId: string;
  name: string | null;
  reason: string;
}

export interface InboxStatus {
  path: string;
  available: boolean;
  pending: number;
  waiting: PendingInboxDocument[];
  archivePath: string;
  archived: number;
  archivePending: number;
}

// 받은 경로와 iCloud 보관(src-tauri/src/archive.rs)
export interface Arrival {
  route: "wifi" | "icloud";
  at: string;
  added: number;
}

export interface DocumentOrigin {
  arrivals: Arrival[];
  archive: "done" | "partial" | "notApplicable";
}

// 단축키, scanshelf:// 링크, 가져오기 (src-tauri/src/actions.rs, import.rs)
export type AppAction =
  | { type: "showWindow" }
  | { type: "open"; id: string; page: number | null }
  | { type: "search"; query: string }
  | { type: "import"; paths: string[] }
  | { type: "pickImport" }
  | { type: "showShortcuts" };

export interface QuickOpenEntry {
  id: string;
  name: string;
  kind: DocumentKind;
  session: string | null;
  tags: string[];
  pageCount: number;
  createdAt: string;
}

export interface ImportedDocument {
  documentId: string;
  name: string;
  pages: number;
}

export interface ImportResult {
  documents: ImportedDocument[];
  skipped: { path: string; reason: string }[];
}

// 질문, 태그, 캘린더 묶음 추천 (src-tauri/src/{ask,tagger,calendar}.rs, docs/ask.md, docs/organize.md)
export interface AskSource {
  /** 답의 [D3 p.12]에서 D3 */
  key: string;
  documentId: string;
  name: string;
  /** 페이지 번호 순서의 출력 페이지 id. pages[11]이 p.12 */
  pages: string[];
}

export interface AskAnswer {
  markdown: string;
  sources: AskSource[];
}

export interface DocumentTags {
  tags: string[];
  tagsByUser: boolean;
}

export interface TagSuggestion {
  tags: string[];
  /** edits.json에 썼는지. 사람이 고친 태그가 있으면 제안만 */
  applied: boolean;
}

export type CalendarAccess = "notDetermined" | "granted" | "denied" | "unsupported";

export interface CalendarEvent {
  title: string;
  start: string;
  end: string;
  allDay: boolean;
  location: string | null;
  calendar: string | null;
}

export interface SessionSuggestion {
  access: CalendarAccess;
  candidates: CalendarEvent[];
  chosen: number | null;
  session: string | null;
  reason: string;
}
