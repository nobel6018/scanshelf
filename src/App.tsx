import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { api } from "./api";
import { confirmAndDelete, isBusy, showDocumentMenu } from "./documentMenu";
import {
  availableOn,
  isModifierOnly,
  labelFor,
  matchShortcut,
  onCommand,
  runRegistered,
  SCREEN_LABEL,
  shortcutLabel,
  type CommandName,
} from "./shortcuts";
import { ShortcutOverlay } from "./views/ShortcutOverlay";
import { ShortcutBadges } from "./views/ShortcutBadges";
import type { AppAction, DocumentSummary, Progress } from "./types";
import { UpdateBanner } from "./update/UpdateBanner";
import { useAppUpdate } from "./update/useAppUpdate";
import { LibraryView } from "./views/LibraryView";
import { DocumentView } from "./views/DocumentView";
import { QuickOpen } from "./views/QuickOpen";
import { SearchView } from "./views/SearchView";
import { SettingsView } from "./views/SettingsView";
import { AskWindow } from "./views/AskWindow";

export type Route =
  | { view: "library" }
  | { view: "document"; id: string; outputId?: string; query?: string }
  | { view: "search" }
  | { view: "settings" };

interface Notice {
  text: string;
  /** 있으면 Finder에서 보기 단추를 같이 띄운다 */
  path?: string;
}

export default function App() {
  const [route, setRoute] = useState<Route>({ view: "library" });
  /** 단축키 보기. hold는 ⌘를 누르는 동안 단추 위 배지, sticky는 ? 키나 메뉴로 연 전체 목록 */
  const [overlay, setOverlay] = useState<"hold" | "sticky" | null>(null);
  const [documents, setDocuments] = useState<DocumentSummary[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<Notice | null>(null);
  // 빠른 찾기 창. null이면 닫힘, 문자열이면 그 검색어로 열림
  const [quickOpen, setQuickOpen] = useState<string | null>(null);
  const [dragging, setDragging] = useState(false);
  // 처리 진행은 이벤트로 오고, 문서 화면도 같은 값을 본다
  const [progress, setProgress] = useState<Record<string, Progress>>({});
  // 문서가 바뀌면 열려 있는 문서 화면이 다시 읽도록 올린다
  const [revision, setRevision] = useState(0);
  const update = useAppUpdate();
  const refreshTimer = useRef<number | null>(null);

  const refresh = useCallback(() => {
    api
      .listDocuments()
      .then((docs) => {
        setDocuments(docs);
        setError(null);
      })
      .catch((e) => setError(String(e)));
  }, []);

  // 여러 이벤트가 몰려도 한 번만 다시 읽는다
  const scheduleRefresh = useCallback(() => {
    if (refreshTimer.current !== null) return;
    refreshTimer.current = window.setTimeout(() => {
      refreshTimer.current = null;
      refresh();
      setRevision((r) => r + 1);
    }, 250);
  }, [refresh]);

  const onDeleted = useCallback(
    (id: string) => {
      setRoute((r) => (r.view === "document" && r.id === id ? { view: "library" } : r));
      setNotice({ text: "휴지통으로 옮겼습니다" });
      scheduleRefresh();
    },
    [scheduleRefresh],
  );

  useEffect(() => {
    refresh();
    const unlisten = [
      listen<Progress>("processing-progress", (e) => {
        setProgress((p) => ({ ...p, [e.payload.documentId]: e.payload }));
        if (e.payload.stage === "decode" || e.payload.stage === "done") scheduleRefresh();
      }),
      listen<{ documentId: string; error: string | null }>("document-processed", (e) => {
        setProgress((p) => {
          const next = { ...p };
          delete next[e.payload.documentId];
          return next;
        });
        if (e.payload.error) setError(`처리 실패: ${e.payload.error}`);
        scheduleRefresh();
      }),
      listen("library-changed", scheduleRefresh),
      // MCP나 다른 창이 지웠을 수도 있다. 보고 있던 문서면 보관함으로 돌아간다
      listen<{ documentId: string }>("document-deleted", (e) => {
        setRoute((r) => (r.view === "document" && r.id === e.payload.documentId ? { view: "library" } : r));
        scheduleRefresh();
      }),
    ];
    return () => {
      unlisten.forEach((p) => p.then((f) => f()));
    };
  }, [refresh, scheduleRefresh]);

  /** 화면 순서(원본 쪽 순서, 펼친 면 순서)로 n번째 쪽. 링크의 page=N이 이 순서를 따른다 */
  const openDocument = useCallback(async (id: string, page: number | null) => {
    setQuickOpen(null);
    let outputId: string | undefined;
    if (page !== null) {
      try {
        const detail = await api.getDocument(id);
        outputId = detail.pages.flatMap((p) => p.outputs)[page - 1]?.id;
      } catch (e) {
        return setError(String(e));
      }
    }
    setRoute({ view: "document", id, outputId });
  }, []);

  const importPaths = useCallback(
    async (paths: string[]) => {
      if (paths.length === 0) return;
      setNotice({ text: `${paths.length}개 파일을 가져오는 중` });
      try {
        const result = await api.importFiles(paths);
        const pages = result.documents.reduce((n, d) => n + d.pages, 0);
        setNotice(
          result.documents.length > 0
            ? { text: `${result.documents.length}개 문서(${pages}쪽)를 가져왔습니다. 보정과 글자 읽기를 시작합니다` }
            : null,
        );
        if (result.skipped.length > 0) {
          const names = result.skipped.map((s) => `${s.path.split("/").pop()} (${s.reason})`);
          setError(`가져오지 못한 파일: ${names.join(" / ")}`);
        }
        if (result.documents.length === 1) setRoute({ view: "document", id: result.documents[0].documentId });
        scheduleRefresh();
      } catch (e) {
        setNotice(null);
        setError(String(e));
      }
    },
    [scheduleRefresh],
  );

  const pickAndImport = useCallback(async () => {
    try {
      await importPaths(await api.pickImportFiles());
    } catch (e) {
      setError(String(e));
    }
  }, [importPaths]);

  const openId = route.view === "document" ? route.id : null;

  /** 단축키, 메뉴, 다른 화면이 보낸 명령. 열린 문서가 필요한 명령은 문서 화면에서만 동작한다 */
  const runCommand = useCallback(
    async (command: CommandName) => {
      const current = openId ? documents.find((d) => d.id === openId) : undefined;
      switch (command) {
        case "quickOpen":
          return setQuickOpen("");
        case "search":
          return setRoute({ view: "search" });
        case "library":
          return setRoute({ view: "library" });
        case "settings":
          return setRoute({ view: "settings" });
        case "import":
          return pickAndImport();
        case "exportPdf":
          if (!openId) return;
          try {
            setNotice({ text: "PDF를 만드는 중" });
            const file = await api.exportDocument(openId, "pdf");
            setNotice({ text: `PDF로 내보냈습니다: ${file.name}`, path: file.path });
          } catch (e) {
            setNotice(null);
            setError(String(e));
          }
          return;
        case "reprocess":
          if (!openId) return;
          if (current && isBusy(current)) return setNotice({ text: "이미 처리 중입니다" });
          return api
            .reprocess(openId, true)
            .then(() => {
              setNotice({ text: "다시 처리합니다" });
              scheduleRefresh();
            })
            .catch((e) => setError(String(e)));
        case "delete":
          if (!openId) return;
          return confirmAndDelete({ id: openId, name: current?.name ?? "이 문서" }, { onDeleted, onError: setError });
        case "showShortcuts":
          return setOverlay("sticky");
        case "openAsk":
          // 질문 창(mac-organize)이 registerCommand("openAsk")로 붙인다
          if (!runRegistered(command)) setNotice({ text: "보관함에 물어보기는 아직 준비되지 않았습니다" });
          return;
        default:
          runRegistered(command);
      }
    },
    [openId, documents, pickAndImport, scheduleRefresh, onDeleted],
  );

  const handleAction = useCallback(
    async (a: AppAction) => {
      switch (a.type) {
        case "showWindow":
          return;
        case "open":
          return openDocument(a.id, a.page);
        case "search":
          return setQuickOpen(a.query);
        case "import":
          return importPaths(a.paths);
        case "pickImport":
          return pickAndImport();
        case "showShortcuts":
          return setOverlay("sticky");
      }
    },
    [openDocument, importPaths, pickAndImport],
  );

  // scanshelf:// 링크, Finder 열기, 메뉴. 구독을 마친 뒤 그 전에 들어온 요청(링크로 앱을 띄운 경우)을 받는다
  const actionRef = useRef(handleAction);
  actionRef.current = handleAction;
  useEffect(() => {
    let cancelled = false;
    const unlisten = listen<AppAction>("app-action", (e) => actionRef.current(e.payload));
    unlisten.then(async () => {
      if (cancelled) return;
      try {
        const ready = await api.frontendReady();
        if (ready.shortcutError) setError(ready.shortcutError);
        for (const a of ready.pending) await actionRef.current(a);
      } catch (e) {
        setError(String(e));
      }
    });
    return () => {
      cancelled = true;
      unlisten.then((f) => f());
    };
  }, []);

  const commandRef = useRef(runCommand);
  commandRef.current = runCommand;
  const modalOpen = quickOpen !== null;
  const screen = route.view;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // 빠른 찾기 같은 창이 이미 처리한 키
      if (e.defaultPrevented || modalOpen) return;
      const shortcut = matchShortcut(e);
      if (!shortcut) return;
      // 다른 화면 전용 키도 웹뷰 기본 동작(⌘R 새로고침 등)은 막는다
      e.preventDefault();
      if (!availableOn(shortcut, screen)) {
        const where = (shortcut.screens ?? []).map((s) => SCREEN_LABEL[s]).join(", ");
        setNotice({ text: `${shortcutLabel(shortcut)}은 ${where} 화면에서 씁니다: ${shortcut.label}` });
        return;
      }
      commandRef.current(shortcut.run);
    };
    window.addEventListener("keydown", onKey);
    const off = onCommand((c) => commandRef.current(c));
    return () => {
      window.removeEventListener("keydown", onKey);
      off();
    };
  }, [modalOpen, screen]);

  // ⌘를 혼자 0.8초 누르고 있으면 단추마다 단축키 배지를 띄우고, 떼면 닫는다. 다른 키와 같이 누르면(⌘K 등) 띄우지 않는다
  useEffect(() => {
    let timer: number | undefined;
    const cancel = () => {
      window.clearTimeout(timer);
      timer = undefined;
    };
    const onDown = (e: KeyboardEvent) => {
      if (isModifierOnly(e)) {
        if (!e.repeat && timer === undefined && !modalOpen) timer = window.setTimeout(() => setOverlay((o) => o ?? "hold"), 800);
        return;
      }
      cancel();
      setOverlay((o) => {
        if (o === "hold") return null;
        if (o === "sticky" && e.key === "Escape") {
          e.preventDefault();
          return null;
        }
        return o;
      });
    };
    const onUp = (e: KeyboardEvent) => {
      if (!isModifierOnly(e)) return;
      cancel();
      setOverlay((o) => (o === "hold" ? null : o));
    };
    const onBlur = () => {
      cancel();
      setOverlay(null);
    };
    window.addEventListener("keydown", onDown, true);
    window.addEventListener("keyup", onUp, true);
    window.addEventListener("blur", onBlur);
    return () => {
      cancel();
      window.removeEventListener("keydown", onDown, true);
      window.removeEventListener("keyup", onUp, true);
      window.removeEventListener("blur", onBlur);
    };
  }, [modalOpen]);

  // 창에 파일을 끌어다 놓으면 가져온다
  useEffect(() => {
    const unlisten = getCurrentWebview().onDragDropEvent((e) => {
      if (e.payload.type === "enter" || e.payload.type === "over") setDragging(true);
      else setDragging(false);
      if (e.payload.type === "drop") importPaths(e.payload.paths);
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, [importPaths]);

  // 웹뷰 기본 우클릭 메뉴는 "다시 불러오기"뿐이고 누르면 화면 상태가 날아간다. 입력칸과 선택한 글에서는 둔다.
  // 개발 빌드는 요소 검사를 쓰려고 남긴다 (docs/context-menu.md)
  useEffect(() => {
    if (!import.meta.env.PROD) return;
    const onMenu = (e: MouseEvent) => {
      const target = e.target as HTMLElement | null;
      const editable = target?.closest("input, textarea, [contenteditable='true']");
      if (!editable && !window.getSelection()?.toString()) e.preventDefault();
    };
    window.addEventListener("contextmenu", onMenu);
    return () => window.removeEventListener("contextmenu", onMenu);
  }, []);

  const NAV_COMMAND: Record<Route["view"], CommandName> = { library: "library", search: "search", settings: "settings", document: "library" };
  const nav = (target: Route["view"], label: string) => (
    <button
      data-shortcut={NAV_COMMAND[target]}
      className={route.view === target || (target === "library" && route.view === "document") ? "nav active" : "nav"}
      onClick={() => setRoute({ view: target } as Route)}
    >
      {label}
    </button>
  );

  return (
    <div className="shell">
      <aside className="sidebar">
        <div className="brand">scanshelf</div>
        {nav("library", "보관함")}
        {nav("search", "검색")}
        {nav("settings", "설정")}
        <div className="spacer" />
        <button className="nav nav-action" data-shortcut="quickOpen" onClick={() => setQuickOpen("")}>
          빠른 찾기 <span className="nav-key">{labelFor("quickOpen")}</span>
        </button>
        <button className="nav nav-action" data-shortcut="import" onClick={pickAndImport}>
          가져오기 <span className="nav-key">{labelFor("import")}</span>
        </button>
      </aside>
      <main className="content">
        {update.state.kind !== "none" && <UpdateBanner update={update} onError={setError} />}
        {error && (
          <div className="error-bar" role="alert">
            <span>{error}</span>
            <button className="ghost" onClick={() => setError(null)}>
              닫기
            </button>
          </div>
        )}
        {notice && (
          <div className="notice-bar" role="status">
            <span>{notice.text}</span>
            <span className="spacer" />
            {notice.path && (
              <button className="ghost" onClick={() => api.revealExport(notice.path!).catch((e) => setError(String(e)))}>
                Finder에서 보기
              </button>
            )}
            <button className="ghost" onClick={() => setNotice(null)}>
              닫기
            </button>
          </div>
        )}
        {route.view === "library" && (
          <LibraryView
            documents={documents}
            progress={progress}
            onOpen={(id) => setRoute({ view: "document", id })}
            onError={setError}
            onContextMenu={(doc) =>
              showDocumentMenu(doc, {
                open: (d) => setRoute({ view: "document", id: d.id }),
                refresh: scheduleRefresh,
                onDeleted,
                onNotice: (text, path) => setNotice({ text, path }),
                onError: setError,
              }).catch((e) => setError(String(e)))
            }
          />
        )}
        {route.view === "document" && (
          <DocumentView
            key={`${route.id}:${route.outputId ?? ""}`}
            documentId={route.id}
            initialOutputId={route.outputId}
            initialQuery={route.query}
            revision={revision}
            progress={progress[route.id]}
            onBack={() => setRoute({ view: "library" })}
            onChanged={scheduleRefresh}
            onError={setError}
          />
        )}
        {route.view === "search" && (
          <SearchView
            onOpen={(id, outputId, query) => setRoute({ view: "document", id, outputId, query })}
            onError={setError}
          />
        )}
        {route.view === "settings" && <SettingsView update={update} onError={setError} />}
      </main>
      {quickOpen !== null && (
        <QuickOpen
          initialQuery={quickOpen}
          onClose={() => setQuickOpen(null)}
          onOpen={(id, outputId, query) => {
            setQuickOpen(null);
            setRoute({ view: "document", id, outputId, query });
          }}
        />
      )}
      {dragging && (
        <div className="drop-overlay" aria-hidden>
          <div className="drop-card">놓으면 보관함으로 가져옵니다 (jpg, png, heic, PDF)</div>
        </div>
      )}
      {overlay === "hold" && <ShortcutBadges screen={screen} />}
      {overlay === "sticky" && <ShortcutOverlay screen={screen} sticky onClose={() => setOverlay(null)} />}
      {/* ⌘J 모든 문서에 질문(docs/ask.md). 다른 곳에서는 openAsk()로 연다 */}
      <AskWindow documents={documents} onOpen={(id, outputId) => setRoute({ view: "document", id, outputId: outputId ?? undefined })} />
    </div>
  );
}
