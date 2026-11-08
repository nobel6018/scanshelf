import { readFile, readdir, stat } from "node:fs/promises";
import { homedir } from "node:os";
import { isAbsolute, join, normalize as normalizePath, sep } from "node:path";

// scanshelf 보관함 폴더를 직접 읽는다. 쓰지는 않는다(쓰기는 Mac 앱에 부탁한다, control.ts).
// 형식은 crates/scanshelf-core/src/ 와 같아야 한다(lib.rs, overrides.rs, processing.rs, ocr.rs, summary.rs, tags.rs)
// @see ../../docs/mcp.md
// @see ../../docs/document-format.md

/** scanshelf-core SCHEMA_VERSION. 이보다 큰 manifest는 읽지 않는다(앱과 같은 규칙) */
export const SCHEMA_VERSION = 1;
/** scanshelf-core processing.rs PIPELINE_VERSION. 이보다 낮은 출력은 다시 처리할 대상이다. 테스트가 Rust 값과 맞는지 본다 */
export const PIPELINE_VERSION = 7;

/** Mac 앱 데이터 폴더(Tauri app_data_dir) 아래 library/. SCANSHELF_LIBRARY_DIR로 바꿀 수 있다 */
export function defaultLibraryDir(): string {
  if (process.env.SCANSHELF_LIBRARY_DIR) return process.env.SCANSHELF_LIBRARY_DIR;
  return join(homedir(), "Library/Application Support/com.nobel6018.scanshelf/library");
}

export type DocumentKind = "pamphlet" | "book" | "document";
export type PageLayout = "single" | "spread" | "trifold";

export interface Page {
  id: string;
  raw: string;
  capturedAt: string;
  layout: PageLayout;
  quad?: unknown;
}

export interface Manifest {
  schemaVersion: number;
  id: string;
  name: string;
  kind: DocumentKind;
  session?: string | null;
  createdAt: string;
  pages: Page[];
}

/** overrides.rs Edits */
export interface Edits {
  name?: string | null;
  kind?: DocumentKind | null;
  session?: string | null;
  quads?: Record<string, unknown>;
  layouts?: Record<string, PageLayout>;
  /** 사람이 더 돌린 각도(시계 방향) */
  rotations?: Record<string, number>;
  deletedPages?: string[];
  /** "확인함"을 누른 원본 페이지 */
  reviewed?: string[];
  tags?: string[];
  tagsByUser?: boolean;
  pageOrder?: string[] | null;
}

/** processing.rs PagePart. JSON은 "whole", "left", "right", {"panel": 0} */
export type PagePart = "whole" | "left" | "right" | { panel: number };

export interface OutputPage {
  id: string;
  sourcePage: string;
  part: PagePart;
  image: string;
  ocr?: string | null;
  pipelineVersion: number;
  processedAt: string;
}

export interface Processing {
  outputs: OutputPage[];
}

export type Relevance = "competitor" | "partnerCandidate" | "supplier" | "unrelated";

/** summary.rs PamphletSummary */
export interface Summary {
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
  editedFields?: string[];
}

/** 처리 상태. 앱 화면과 달리 캐시(꼭짓점 지문)는 보지 않아 꼭짓점을 고친 뒤의 "다시 처리"는 알아채지 못한다 */
export type ProcessedState = "unprocessed" | "stale" | "processed";

/** 문서 순서대로 놓인 출력 페이지. number는 1부터 */
export interface PageRef {
  number: number;
  output: OutputPage;
}

export interface Doc {
  dir: string;
  /** edits를 적용한 manifest */
  manifest: Manifest;
  /** Mac에서 뺀 원본 페이지(촬영 순서). 되살릴 수 있다 */
  removed: Page[];
  tags: string[];
  pages: PageRef[];
  state: ProcessedState;
  /** OCR이 있는 출력 페이지 수 */
  ocrPages: number;
  hasSummary: boolean;
}

async function readJson<T>(path: string): Promise<T | null> {
  let raw: string;
  try {
    raw = await readFile(path, "utf8");
  } catch {
    return null;
  }
  try {
    return JSON.parse(raw) as T;
  } catch {
    return null;
  }
}

const lower = (id: string) => id.toLowerCase();

/** scanshelf-core tags.rs normalize_tag */
export function normalizeTag(raw: string): string | null {
  const parts = raw
    .trim()
    .replace(/^#+/, "")
    .split("/")
    .map((p) => p.split(/\s+/).filter(Boolean).join(" "))
    .filter((p) => p.length > 0);
  if (!parts.length) return null;
  const tag = [...parts.join("/")].slice(0, 60).join("");
  return tag.replace(/\/+$/, "");
}

/** scanshelf-core tags.rs normalize_tags. 순서는 지키고 대소문자만 다른 태그는 처음 것만 남긴다 */
export function normalizeTags(raw: string[]): string[] {
  const out: string[] = [];
  for (const t of raw) {
    const tag = normalizeTag(t);
    if (tag && !out.some((x) => x.toLowerCase() === tag.toLowerCase())) out.push(tag);
    if (out.length === 10) break;
  }
  return out;
}

/**
 * scanshelf-core tags.rs matches. 하위 태그(분야/냉각/액침)가 붙은 문서도 상위(분야/냉각)에 든다.
 * 필터는 normalizeTag로 정리하고, 대소문자는 무시한다(normalize_tags가 대소문자만 다른 태그를 하나로 보기 때문)
 */
export function tagMatches(tags: string[], filter: string): boolean {
  const f = normalizeTag(filter)?.toLowerCase();
  if (!f) return false;
  return tags.some((t) => {
    const x = t.toLowerCase();
    return x === f || x.startsWith(`${f}/`);
  });
}

/** overrides.rs Edits::apply. 이름, 종류, 묶음을 덮고, 지운 페이지를 빼고, 순서를 바꾼다 */
export function applyEdits(manifest: Manifest, edits: Edits): Manifest {
  const merged: Manifest = { ...manifest, pages: manifest.pages.map((p) => ({ ...p })) };
  if (edits.name != null) merged.name = edits.name;
  if (edits.kind != null) merged.kind = edits.kind;
  if (edits.session != null) merged.session = edits.session;
  const deleted = new Set((edits.deletedPages ?? []).map(lower));
  merged.pages = merged.pages.filter((p) => !deleted.has(lower(p.id)));
  const quads = new Map(Object.entries(edits.quads ?? {}).map(([k, v]) => [lower(k), v]));
  const layouts = new Map(Object.entries(edits.layouts ?? {}).map(([k, v]) => [lower(k), v]));
  for (const page of merged.pages) {
    const q = quads.get(lower(page.id));
    if (q !== undefined) page.quad = q;
    const l = layouts.get(lower(page.id));
    if (l !== undefined) page.layout = l;
  }
  if (edits.pageOrder) {
    const order = edits.pageOrder.map(lower);
    const position = (id: string) => {
      const i = order.indexOf(lower(id));
      return i < 0 ? Number.MAX_SAFE_INTEGER : i;
    };
    // Array.prototype.sort는 안정 정렬이라 순서에 없는 새 페이지는 촬영 순서를 지킨 채 뒤로 간다(Rust와 같다)
    merged.pages.sort((a, b) => position(a.id) - position(b.id));
  }
  return merged;
}

function partRank(part: PagePart): number {
  if (part === "whole") return 0;
  if (part === "left") return 1;
  if (part === "right") return 2;
  return 3 + part.panel;
}

/**
 * 출력 페이지를 문서 순서로 놓는다. 원본 페이지 순서(edits 적용 뒤)를 따르고, 한 원본 안에서는 왼쪽부터.
 * 지운 원본의 출력(다시 처리 전이라 남은 것)은 뺀다. 처리 직후에는 processing.json 순서와 같다
 */
export function orderOutputs(manifest: Manifest, processing: Processing | null): PageRef[] {
  const outputs = processing?.outputs ?? [];
  const refs: PageRef[] = [];
  for (const page of manifest.pages) {
    const mine = outputs.filter((o) => lower(o.sourcePage) === lower(page.id));
    mine.sort((a, b) => partRank(a.part) - partRank(b.part));
    for (const output of mine) refs.push({ number: refs.length + 1, output });
  }
  return refs;
}

function stateOf(manifest: Manifest, processing: Processing | null, pages: PageRef[]): ProcessedState {
  const outputs = processing?.outputs ?? [];
  if (!outputs.length) return manifest.pages.length ? "unprocessed" : "processed";
  const ids = new Set(manifest.pages.map((p) => lower(p.id)));
  const covered = new Set(pages.map((r) => lower(r.output.sourcePage)));
  const orphan = outputs.some((o) => !ids.has(lower(o.sourcePage)));
  const missing = manifest.pages.some((p) => !covered.has(lower(p.id)));
  const old = pages.some((r) => (r.output.pipelineVersion ?? 0) < PIPELINE_VERSION);
  return orphan || missing || old ? "stale" : "processed";
}

/** 문서 폴더 안의 상대 경로만 받는다. 다른 기기가 보낸 파일이라 경로를 믿지 않는다(lib.rs validate_page와 같은 뜻) */
export function insideDir(dir: string, relative: string): string | null {
  if (!relative || isAbsolute(relative)) return null;
  const parts = relative.split(/[\\/]/);
  if (parts.some((p) => p === "" || p === "." || p === "..")) return null;
  const full = normalizePath(join(dir, relative));
  return full.startsWith(dir.endsWith(sep) ? dir : dir + sep) ? full : null;
}

/** 문서 폴더 하나를 읽는다. manifest가 없거나 깨졌거나 모르는 버전이면 null(앱도 건너뛴다) */
export async function loadDocument(dir: string): Promise<Doc | null> {
  const raw = await readJson<Manifest>(join(dir, "manifest.json"));
  if (!raw || typeof raw.id !== "string" || !Array.isArray(raw.pages)) return null;
  if (typeof raw.schemaVersion !== "number" || raw.schemaVersion > SCHEMA_VERSION) return null;
  // edits.json, processing.json이 깨졌으면 앱처럼 없는 것으로 본다(src-tauri/src/library.rs load_edits)
  const edits = (await readJson<Edits>(join(dir, "edits.json"))) ?? {};
  const processing = await readJson<Processing>(join(dir, "processing.json"));
  const manifest = applyEdits(raw, edits);
  const pages = orderOutputs(manifest, processing && Array.isArray(processing.outputs) ? processing : null);
  const hasSummary = await stat(join(dir, "summary.json")).then(
    (s) => s.isFile(),
    () => false,
  );
  const deleted = new Set((edits.deletedPages ?? []).map(lower));
  return {
    dir,
    manifest,
    removed: raw.pages.filter((p) => deleted.has(lower(p.id))),
    tags: normalizeTags(Array.isArray(edits.tags) ? edits.tags : []),
    pages,
    state: stateOf(manifest, processing, pages),
    ocrPages: pages.filter((r) => r.output.ocr).length,
    hasSummary,
  };
}

/** 보관함 전체. 최신순. 읽지 못한 문서는 건너뛴다(전송 중인 문서 하나 때문에 전체가 안 보이면 안 된다) */
export async function listDocuments(libraryDir: string): Promise<Doc[]> {
  let names: string[];
  try {
    names = await readdir(libraryDir);
  } catch {
    return [];
  }
  const docs = await Promise.all(names.filter((n) => !n.startsWith(".")).map((n) => loadDocument(join(libraryDir, n))));
  return docs
    .filter((d): d is Doc => d !== null)
    .sort((a, b) => Date.parse(b.manifest.createdAt) - Date.parse(a.manifest.createdAt));
}

/** id 전체 또는 앞부분(8글자 이상, 하나로 정해질 때)으로 찾는다. 사용자 입력을 경로에 바로 붙이지 않는다 */
export function findDocument(docs: Doc[], key: string): Doc | { ambiguous: Doc[] } | null {
  const k = key.trim().toLowerCase();
  const exact = docs.find((d) => d.manifest.id.toLowerCase() === k);
  if (exact) return exact;
  if (k.length < 8) return null;
  const hits = docs.filter((d) => d.manifest.id.toLowerCase().startsWith(k));
  if (hits.length === 1) return hits[0];
  return hits.length ? { ambiguous: hits } : null;
}

const ocrCache = new Map<string, { mtime: number; text: string }>();

/** 출력 페이지의 OCR 글자. 없거나 못 읽으면 null. 검색이 매번 읽지 않게 수정 시각으로 캐시한다 */
export async function getOcrText(doc: Doc, ref: PageRef): Promise<string | null> {
  if (!ref.output.ocr) return null;
  const path = insideDir(doc.dir, ref.output.ocr);
  if (!path) return null;
  let mtime: number;
  try {
    mtime = (await stat(path)).mtimeMs;
  } catch {
    return null;
  }
  const hit = ocrCache.get(path);
  if (hit && hit.mtime === mtime) return hit.text;
  const page = await readJson<{ text?: unknown }>(path);
  if (!page || typeof page.text !== "string") return null;
  ocrCache.set(path, { mtime, text: page.text });
  return page.text;
}

/** ocr.rs Rect. 그림 기준 0~1, 원점 왼쪽 위 */
export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface OcrBlock {
  kind: "title" | "paragraph" | "list" | "table";
  text: string;
  bbox: Rect;
  lines: Array<{ text: string; bbox: Rect; confidence: number }>;
}

/** 출력 페이지의 OCR 블록(줄과 위치). 검색과 달리 자주 읽지 않아 캐시하지 않는다 */
export async function getOcrBlocks(doc: Doc, ref: PageRef): Promise<OcrBlock[] | null> {
  if (!ref.output.ocr) return null;
  const path = insideDir(doc.dir, ref.output.ocr);
  if (!path) return null;
  const page = await readJson<{ blocks?: unknown }>(path);
  return page && Array.isArray(page.blocks) ? (page.blocks as OcrBlock[]) : null;
}

export async function getSummary(doc: Doc): Promise<Summary | null> {
  if (!doc.hasSummary) return null;
  return readJson<Summary>(join(doc.dir, "summary.json"));
}

export function imagePath(doc: Doc, ref: PageRef): string | null {
  return insideDir(doc.dir, ref.output.image);
}
