// 앱 안 단축키 목록과 명령 통로. 단축키, 메뉴, 링크가 모두 같은 명령 이름으로 일을 시킨다.
// App.tsx가 기본 명령을 처리하고, 다른 화면(예: 질문 창)은 registerCommand로 자기 명령을 붙인다.
// @see docs/shortcuts-and-deep-links.md

export const IS_MAC = navigator.userAgent.includes("Mac");

/** App.tsx가 직접 처리하는 명령 */
export type AppCommand =
  | "quickOpen"
  | "search"
  | "settings"
  | "library"
  | "import"
  | "exportPdf"
  | "reprocess"
  | "delete"
  | "openAsk"
  | "showShortcuts";

/** 단축키가 동작하는 화면. App.tsx의 Route.view와 같다 */
export type Screen = "library" | "document" | "search" | "settings";

export const SCREEN_LABEL: Record<Screen, string> = {
  library: "보관함",
  document: "문서",
  search: "검색",
  settings: "설정",
};

/** 다른 팀원 화면이 붙이는 명령도 받도록 문자열을 열어 둔다 */
export type CommandName = AppCommand | (string & {});

export interface Shortcut {
  /** KeyboardEvent.key 기준. 글자는 소문자 */
  key: string;
  /** ⌘(Windows는 Ctrl) */
  mod?: boolean;
  shift?: boolean;
  run: CommandName;
  label: string;
  group: "이동" | "파일" | "문서" | "도움말";
  /** 이 화면에서만 동작하고 목록에 보인다. 없으면 모든 화면 */
  screens?: Screen[];
  /** 키 표기를 직접 정할 때(예: "⌘ 길게") */
  display?: string;
  /** 글자 입력칸에 커서가 있어도 동작하는지. 입력칸 기본 동작(⌘⌫, 화살표)과 겹치면 false */
  inInput?: boolean;
  /** 목록에 보여 주기만 한다. 화면(DocumentView)이 키를 직접 받는다 */
  passive?: boolean;
}

export const SHORTCUTS: Shortcut[] = [
  { key: "k", mod: true, run: "quickOpen", label: "빠른 찾기 (이름, 묶음, 태그, 본문)", group: "이동", inInput: true },
  { key: "f", mod: true, run: "search", label: "검색 화면", group: "이동", inInput: true },
  { key: "1", mod: true, run: "library", label: "보관함", group: "이동", inInput: true },
  { key: ",", mod: true, run: "settings", label: "설정", group: "이동", inInput: true },
  { key: "j", mod: true, run: "openAsk", label: "보관함에 물어보기", group: "이동", inInput: true },
  { key: "o", mod: true, run: "import", label: "그림, PDF 가져오기", group: "파일", inInput: true },
  // 문서 단위 명령은 문서 화면에서만 동작한다. 보관함 화면에서는 무엇을 다시 처리하는지 알 수 없어서다.
  // 다시 처리는 ⌘R(새로고침으로 손에 익은 키)에서 ⇧⌘P(process)로 옮겼다. 실수로 누르기 쉬웠다(2026-11-08 사용자 지적)
  { key: "e", mod: true, run: "exportPdf", label: "이 문서를 PDF로 내보내기", group: "문서", screens: ["document"] },
  { key: "p", mod: true, shift: true, run: "reprocess", label: "이 문서만 다시 처리", group: "문서", screens: ["document"] },
  { key: "Backspace", mod: true, run: "delete", label: "이 문서 휴지통으로 (확인 후)", group: "문서", screens: ["document"] },
  // 문서 화면 도구 막대. 처리는 DocumentView가 registerCommand로 붙인다. 고른 키의 근거는 docs/shortcuts-and-deep-links.md
  { key: "g", mod: true, run: "editQuad", label: "꼭짓점 고치기", group: "문서", screens: ["document"] },
  { key: "t", mod: true, run: "rotatePage", label: "이 쪽 돌리기", group: "문서", screens: ["document"] },
  { key: "ArrowUp", mod: true, run: "movePageUp", label: "이 쪽을 앞으로", group: "문서", screens: ["document"] },
  { key: "ArrowDown", mod: true, run: "movePageDown", label: "이 쪽을 뒤로", group: "문서", screens: ["document"] },
  { key: "Backspace", mod: true, shift: true, run: "removePage", label: "이 쪽 빼기", group: "문서", screens: ["document"] },
  { key: "i", mod: true, run: "toggleText", label: "읽은 글자 보기", group: "문서", screens: ["document"] },
  { key: "f", mod: true, shift: true, run: "findInPage", label: "페이지에서 찾기", group: "문서", screens: ["document"], inInput: true },
  { key: "ArrowLeft", run: "prevPage", label: "이전 쪽", group: "문서", passive: true, screens: ["document"], display: "← ↑" },
  { key: "ArrowRight", run: "nextPage", label: "다음 쪽", group: "문서", passive: true, screens: ["document"], display: "→ ↓" },
  // ?(⇧/)에서 ⌘/로 옮겼다. 다른 Mac 앱의 도움말 키와 같고 ⇧을 누르지 않아도 된다(2026-11-08 사용자 요청)
  { key: "/", mod: true, run: "showShortcuts", label: "단축키 보기", group: "도움말", inInput: true },
  { key: "Meta", run: "showShortcuts", label: "단축키 보기 (누르는 동안)", group: "도움말", passive: true, display: IS_MAC ? "⌘ 길게" : "Ctrl 길게" },
];

const GROUP_ORDER: Shortcut["group"][] = ["이동", "파일", "문서", "도움말"];

/** 화면에서 쓸 수 있는 단축키를 묶음 순서대로. 오버레이가 이 목록을 그대로 보여 준다 */
export function shortcutsFor(screen: Screen): { group: Shortcut["group"]; items: Shortcut[] }[] {
  const visible = SHORTCUTS.filter((s) => !s.screens || s.screens.includes(screen));
  return GROUP_ORDER.map((group) => ({ group, items: visible.filter((s) => s.group === group) })).filter(
    (g) => g.items.length > 0,
  );
}

export function isEditable(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  if (target instanceof HTMLTextAreaElement || target instanceof HTMLSelectElement) return true;
  if (target instanceof HTMLInputElement) return !["button", "checkbox", "radio", "range"].includes(target.type);
  return false;
}

/** 화면을 가리지 않고 키에 맞는 단축키. 다른 화면 전용 키를 눌렀는지 알려 줄 때 쓴다 */
export function matchShortcut(e: KeyboardEvent): Shortcut | undefined {
  // 한글 조합 중 Enter, Space는 글자를 완성하는 키다. WebKit은 isComposing 없이 keyCode 229로 보내기도 한다
  if (e.isComposing || e.keyCode === 229) return undefined;
  const mod = IS_MAC ? e.metaKey : e.ctrlKey;
  // macOS에서 Ctrl, Windows에서 Win 키가 섞이면 다른 조합이다
  const other = IS_MAC ? e.ctrlKey : e.metaKey;
  if (other || e.altKey) return undefined;
  const key = e.key.length === 1 ? e.key.toLowerCase() : e.key;
  const editable = isEditable(e.target);
  return SHORTCUTS.find(
    (s) =>
      !s.passive &&
      s.key === key &&
      !!s.mod === mod &&
      !!s.shift === e.shiftKey &&
      (!editable || s.inInput),
  );
}

export function availableOn(s: Shortcut, screen: Screen): boolean {
  return !s.screens || s.screens.includes(screen);
}

/** ⌘R. 웹뷰 새로고침이라 아무 일도 하지 않게 막는다(화면이 처음부터 다시 그려져 보던 쪽을 잃는다) */
export function isReloadKey(e: KeyboardEvent): boolean {
  const mod = IS_MAC ? e.metaKey : e.ctrlKey;
  return mod && !e.altKey && e.key.toLowerCase() === "r";
}

/** ⌘ 하나만 눌렀는지(오버레이를 띄우는 키) */
export function isModifierOnly(e: KeyboardEvent): boolean {
  return IS_MAC ? e.key === "Meta" : e.key === "Control";
}

const KEY_LABEL: Record<string, string> = {
  Enter: "↩",
  Escape: "Esc",
  Backspace: "⌫",
  ArrowUp: "↑",
  ArrowDown: "↓",
  ArrowLeft: "←",
  ArrowRight: "→",
};

export function shortcutLabel(s: Shortcut): string {
  if (s.display) return s.display;
  const key = KEY_LABEL[s.key] ?? s.key.toUpperCase();
  if (IS_MAC) return `${s.shift ? "⇧" : ""}${s.mod ? "⌘" : ""}${key}`;
  return [s.mod && "Ctrl", s.shift && "Shift", key].filter(Boolean).join("+");
}

/** 명령 이름의 단축키 표기. 없으면 빈 문자열 */
export function labelFor(command: CommandName): string {
  const s = SHORTCUTS.find((x) => x.run === command);
  return s ? shortcutLabel(s) : "";
}

type Handler = () => void;
const handlers = new Map<string, Handler[]>();

/**
 * 명령에 처리 함수를 붙인다. 나중에 붙인 것이 먼저 받는다. 돌려준 함수를 부르면 떨어진다.
 * 예: 질문 창이 `useEffect(() => registerCommand("openAsk", () => setOpen(true)), [])`
 */
export function registerCommand(command: CommandName, handler: Handler): () => void {
  const list = handlers.get(command) ?? [];
  list.push(handler);
  handlers.set(command, list);
  return () => {
    const current = handlers.get(command) ?? [];
    handlers.set(
      command,
      current.filter((h) => h !== handler),
    );
  };
}

/** 붙은 처리 함수가 있으면 가장 최근 것을 부르고 true */
export function runRegistered(command: CommandName): boolean {
  const list = handlers.get(command);
  const handler = list?.[list.length - 1];
  if (!handler) return false;
  handler();
  return true;
}

const EVENT = "scanshelf:command";

/** 단축키 말고 버튼이나 다른 화면에서 명령을 보낼 때. App.tsx가 받아 단축키와 같은 길로 처리한다 */
export function sendCommand(command: CommandName) {
  window.dispatchEvent(new CustomEvent<CommandName>(EVENT, { detail: command }));
}

export function onCommand(listener: (command: CommandName) => void): () => void {
  const wrapped = (e: Event) => listener((e as CustomEvent<CommandName>).detail);
  window.addEventListener(EVENT, wrapped);
  return () => window.removeEventListener(EVENT, wrapped);
}
