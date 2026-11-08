import { readFile } from "node:fs/promises";
import { dirname, join } from "node:path";

// Mac 앱의 쓰기 통로(src-tauri/src/control.rs) 클라이언트. MCP는 edits.json에 직접 쓰지 않고 앱에 부탁한다.
// 화면이 edits 전체를 들고 있다가 통째로 저장하므로, 바깥에서 쓴 칸은 앱을 거쳐야 사라지지 않는다
// @see ../../docs/mcp.md "쓰기 통로"

export const CONTROL_FILE = "control.json";

/** 앱 데이터 폴더(보관함 library/의 부모)의 control.json. SCANSHELF_CONTROL_FILE로 바꿀 수 있다 */
export function controlFilePath(libraryDir: string): string {
  return process.env.SCANSHELF_CONTROL_FILE ?? join(dirname(libraryDir), CONTROL_FILE);
}

/** pipeline/steps.rs PageCheck 중 사람이 볼 것 */
export type ReviewCheck = "quadMissing" | "quadOnePage" | "notSplit" | "rotationDoubtful" | "textAtCut";

export interface ReviewDocument {
  documentId: string;
  pages: Array<{ pageId: string; checks: ReviewCheck[] }>;
}

export type PageLayout = "single" | "spread" | "trifold";

export interface Quad {
  topLeft: { x: number; y: number };
  topRight: { x: number; y: number };
  bottomRight: { x: number; y: number };
  bottomLeft: { x: number; y: number };
}

/** control.rs Job. 처리 줄에 없으면 null */
export type Job = { state: "queued" } | { state: "running"; done: number; total: number } | { state: "failed"; message: string } | null;

export interface DocumentStatus {
  job: Job;
  /** 다시 처리할 원본 페이지 수 */
  stalePages: number;
}

/** 앱이 꺼져 있거나 통로가 없을 때. 메시지를 그대로 사용자에게 보여 준다 */
export class AppNotRunning extends Error {}

export class ControlError extends Error {}

const APP_OFF =
  "Mac scanshelf 앱이 켜져 있지 않아 고칠 수 없습니다. 앱을 켠 뒤 다시 시도해 주세요(읽기 도구는 앱 없이도 됩니다).";

export class ControlClient {
  constructor(
    private readonly file: string,
    private readonly fetchImpl: typeof fetch = fetch,
  ) {}

  private async info(): Promise<{ port: number; token: string }> {
    let raw: string;
    try {
      raw = await readFile(this.file, "utf8");
    } catch {
      throw new AppNotRunning(APP_OFF);
    }
    const info = JSON.parse(raw) as { port?: unknown; token?: unknown };
    if (typeof info.port !== "number" || typeof info.token !== "string") throw new AppNotRunning(APP_OFF);
    return { port: info.port, token: info.token };
  }

  private async call<T>(method: string, path: string, body?: unknown): Promise<T> {
    const { port, token } = await this.info();
    let response: Response;
    try {
      response = await this.fetchImpl(`http://127.0.0.1:${port}${path}`, {
        method,
        headers: { authorization: `Bearer ${token}`, ...(body === undefined ? {} : { "content-type": "application/json" }) },
        body: body === undefined ? undefined : JSON.stringify(body),
        signal: AbortSignal.timeout(10_000),
      });
    } catch {
      // 앱이 끝나며 파일을 못 지웠으면(강제 종료) 포트가 닫혀 있다
      throw new AppNotRunning(APP_OFF);
    }
    const payload = (await response.json().catch(() => ({}))) as { error?: string };
    if (response.status === 401) {
      throw new AppNotRunning("Mac scanshelf 앱이 다시 켜져 접속 정보가 바뀌었습니다. 잠시 뒤 다시 시도해 주세요.");
    }
    if (!response.ok) throw new ControlError(payload.error ?? `앱이 요청을 거절했습니다(${response.status})`);
    return payload as T;
  }

  async review(): Promise<ReviewDocument[]> {
    return (await this.call<{ documents: ReviewDocument[] }>("GET", "/v1/review")).documents;
  }

  async rotate(documentId: string, pageId: string, degrees: 90 | 180 | 270): Promise<number> {
    return (await this.call<{ rotation: number }>("POST", `/v1/documents/${documentId}/pages/${pageId}/rotate`, { degrees })).rotation;
  }

  async setReviewed(documentId: string, pageId: string, reviewed: boolean): Promise<void> {
    await this.call("PUT", `/v1/documents/${documentId}/pages/${pageId}/reviewed`, { reviewed });
  }

  private async image(path: string): Promise<{ data: Buffer; info: Record<string, unknown> }> {
    const { port, token } = await this.info();
    let response: Response;
    try {
      response = await this.fetchImpl(`http://127.0.0.1:${port}${path}`, {
        headers: { authorization: `Bearer ${token}` },
        signal: AbortSignal.timeout(60_000),
      });
    } catch {
      throw new AppNotRunning(APP_OFF);
    }
    if (response.status === 401) {
      throw new AppNotRunning("Mac scanshelf 앱이 다시 켜져 접속 정보가 바뀌었습니다. 잠시 뒤 다시 시도해 주세요.");
    }
    if (!response.ok) {
      const payload = (await response.json().catch(() => ({}))) as { error?: string };
      throw new ControlError(payload.error ?? `앱이 요청을 거절했습니다(${response.status})`);
    }
    const info = JSON.parse(response.headers.get("x-scanshelf-view") ?? "{}") as Record<string, unknown>;
    return { data: Buffer.from(await response.arrayBuffer()), info };
  }

  /** raw: 원본 사진과 꼭짓점, flat: 원근 보정과 회전 뒤 나누기 직전 그림과 고른 접힌 선 */
  view(documentId: string, pageId: string, kind: "raw" | "flat") {
    return this.image(`/v1/documents/${documentId}/pages/${pageId}/${kind}`);
  }

  async status(documentId: string): Promise<DocumentStatus> {
    return this.call<DocumentStatus>("GET", `/v1/documents/${documentId}/status`);
  }

  private page(documentId: string, pageId: string, action: string) {
    return `/v1/documents/${documentId}/pages/${pageId}/${action}`;
  }

  async setLayout(documentId: string, pageId: string, layout: PageLayout): Promise<void> {
    await this.call("PUT", this.page(documentId, pageId, "layout"), { layout });
  }

  async setFolds(documentId: string, pageId: string, folds: number[], exact = false): Promise<void> {
    await this.call("PUT", this.page(documentId, pageId, "folds"), { folds, exact });
  }

  async setPanelOrder(documentId: string, pageId: string, order: number[]): Promise<void> {
    await this.call("PUT", this.page(documentId, pageId, "panel-order"), { order });
  }

  async setQuad(documentId: string, pageId: string, quad: Quad | null): Promise<void> {
    await this.call("PUT", this.page(documentId, pageId, "quad"), { quad });
  }

  async move(documentId: string, pageId: string, before: string | null): Promise<string[]> {
    return (await this.call<{ order: string[] }>("POST", this.page(documentId, pageId, "move"), { before })).order;
  }

  async remove(documentId: string, pageId: string): Promise<void> {
    await this.call("POST", this.page(documentId, pageId, "remove"), {});
  }

  async restore(documentId: string, pageId: string): Promise<void> {
    await this.call("POST", this.page(documentId, pageId, "restore"), {});
  }

  async reset(documentId: string, pageId: string): Promise<void> {
    await this.call("POST", this.page(documentId, pageId, "reset"), {});
  }

  async updateTags(documentId: string, add: string[], remove: string[]): Promise<string[]> {
    return (await this.call<{ tags: string[] }>("PATCH", `/v1/documents/${documentId}/tags`, { add, remove })).tags;
  }
}
