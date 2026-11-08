import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { registerCommand, sendCommand } from "../shortcuts";
import { OriginPanel } from "./OriginPanel";
import { api, compact, fileUrl, KIND_LABEL } from "../api";
import type {
  PagePart,
  DocumentDetail,
  DocumentKind,
  Edits,
  OcrPage,
  Filter,
  OutputView,
  PageCheck,
  PageLayout,
  PageView,
  ProcessOptions,
  Progress,
  Quad,
} from "../types";
import { VirtualList } from "../components/VirtualList";
import { stateLabel } from "./LibraryView";
import { QuadEditor } from "./QuadEditor";
import { ExportPanel } from "./ExportPanel";
import { AiPanel } from "./AiPanel";
import { DocumentOrganize } from "./TagEditor";
import { AskPanel } from "./AskPanel";
import { BookPanel } from "./BookPanel";

interface Props {
  documentId: string;
  initialOutputId?: string;
  initialQuery?: string;
  revision: number;
  progress?: Progress;
  onBack: () => void;
  onChanged: () => void;
  onError: (e: string) => void;
}

const FILTER_NAME: Record<Filter, string> = {
  original: "원본",
  enhanced: "선명하게",
  grayscale: "회색",
  blackWhite: "흑백",
};

/** 처리하며 확인한 것. review면 사람이 볼 것이라 썸네일에 ! 표시 */
const CHECK_TEXT: Record<PageCheck, { text: string; review: boolean }> = {
  quadRedetected: { text: "iPhone 꼭짓점이 틀려 Mac이 다시 찾음", review: false },
  quadMissing: { text: "종이 테두리를 못 찾아 사진 전체를 씀. 책상이 보이면 꼭짓점 고치기", review: true },
  quadOnePage: { text: "펼침인데 한 쪽만 잡힌 듯함. 옆 쪽이 잘렸으면 꼭짓점 고치기", review: true },
  notSplit: { text: "접힌 선을 못 찾아 한 장으로 둠. 두 쪽이 붙어 있으면 '펼친 두 쪽' 확인", review: true },
  splitAtSeam: { text: "그림자가 없어 가운데 이음매에서 나눔", review: false },
  rotationDoubtful: { text: "글자 방향이 애매해 돌리지 않음. 거꾸로면 돌리기", review: true },
  photoPage: { text: "사진 페이지라 선명하게 대신 밝기만 맞춤", review: false },
  textAtCut: { text: "나눈 경계에 글자가 걸침. 잘렸으면 배치를 바꾸거나 AI(MCP)로 접힌 선 고치기", review: true },
};

/** 썸네일 줄 하나. 출력 페이지가 없으면(처리 전) 원본 페이지 자리를 둔다 */
interface Item {
  page: PageView;
  output: OutputView | null;
  pageIndex: number;
}

const THUMB_ROW = 196;

const PART_LABEL = { whole: "", left: "왼쪽", right: "오른쪽" } as const;

/** 출력 페이지가 원본의 어느 부분인지. 3단 접지는 왼쪽부터 1~3면 */
function partLabel(part: PagePart): string {
  return typeof part === "object" ? `${part.panel + 1}면` : PART_LABEL[part];
}

export function DocumentView({ documentId, initialOutputId, initialQuery, revision, progress, onBack, onChanged, onError }: Props) {
  const [detail, setDetail] = useState<DocumentDetail | null>(null);
  const [selected, setSelected] = useState(0);
  const [editingQuad, setEditingQuad] = useState<PageView | null>(null);
  const [ocr, setOcr] = useState<OcrPage | null>(null);
  const [showText, setShowText] = useState(false);
  const [highlight, setHighlight] = useState(initialQuery ?? "");
  const [nameDraft, setNameDraft] = useState<string | null>(null);
  const [sessionDraft, setSessionDraft] = useState<string | null>(null);
  // 오른쪽 칸: 정보(처리, 내보내기, 정리) 또는 질문(docs/ask.md)
  const [sideTab, setSideTab] = useState<"info" | "ask">("info");

  const load = useCallback(() => {
    api
      .getDocument(documentId)
      .then(setDetail)
      .catch((e) => onError(String(e)));
  }, [documentId, onError]);

  useEffect(load, [load, revision]);

  const items: Item[] = useMemo(() => {
    if (!detail) return [];
    return detail.pages.flatMap((page, pageIndex): Item[] =>
      page.outputs.length > 0
        ? page.outputs.map((output) => ({ page, output, pageIndex }))
        : [{ page, output: null, pageIndex }],
    );
  }, [detail]);

  // 검색에서 들어오면 그 페이지를 먼저 보여 준다
  useEffect(() => {
    if (!initialOutputId) return;
    const index = items.findIndex((i) => i.output?.id === initialOutputId);
    if (index >= 0) setSelected(index);
  }, [items, initialOutputId]);

  const current = items[Math.min(selected, Math.max(0, items.length - 1))];

  const reviewed = detail?.edits.reviewed;
  const isReviewed = useCallback((page: PageView) => reviewed?.includes(page.id) ?? false, [reviewed]);
  const needsReview = useCallback(
    (page: PageView) => !isReviewed(page) && page.checks.some((c) => CHECK_TEXT[c].review),
    [isReviewed],
  );

  // 사람이 볼 쪽(원본 사진 단위로 첫 출력). 확인 필요 문서를 열면 그 쪽부터 차례로 본다
  const reviewItems = useMemo(
    () =>
      items
        .map((item, index) => ({ item, index }))
        .filter(({ item }, i, all) => needsReview(item.page) && all.findIndex((x) => x.item.page.id === item.page.id) === i)
        .map(({ index }) => index),
    [items, needsReview],
  );
  const goNextReview = () => {
    if (reviewItems.length === 0) return;
    setSelected(reviewItems.find((i) => i > selected) ?? reviewItems[0]);
  };

  useEffect(() => {
    setOcr(null);
    if (!current?.output?.hasOcr) return;
    api
      .getOcr(documentId, current.output.id)
      .then(setOcr)
      .catch((e) => onError(String(e)));
  }, [documentId, current?.output?.id, current?.output?.hasOcr, current?.output?.processedAt, onError]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement || editingQuad) return;
      // ⌘↑, ⌘↓는 쪽 순서 바꾸기다(src/shortcuts.ts). 고른 쪽만 옮기는 일과 겹치지 않게 수식키가 있으면 건너뛴다
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (e.key === "ArrowDown" || e.key === "ArrowRight") setSelected((s) => Math.min(items.length - 1, s + 1));
      if (e.key === "ArrowUp" || e.key === "ArrowLeft") setSelected((s) => Math.max(0, s - 1));
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [items.length, editingQuad]);

  // 도구 막대 단축키(src/shortcuts.ts). 처리 함수는 아래에서 그릴 때마다 최신으로 바꿔 넣는다
  const findRef = useRef<HTMLInputElement>(null);
  // 페이지는 화면에 맞춰 통째로 보여 준다. 알림 줄 개수에 따라 무대 높이가 바뀌므로 고정 값 대신 잰다
  const [stage, setStage] = useState<HTMLDivElement | null>(null);
  const [stageHeight, setStageHeight] = useState(0);
  const [zoomed, setZoomed] = useState(false);
  useEffect(() => {
    if (!stage) return;
    const observer = new ResizeObserver(() => setStageHeight(stage.clientHeight));
    observer.observe(stage);
    return () => observer.disconnect();
  }, [stage]);
  useEffect(() => setZoomed(false), [selected]);
  const toolbar = useRef<Partial<Record<string, () => void>>>({});
  useEffect(() => {
    const names = ["editQuad", "rotatePage", "movePageUp", "movePageDown", "removePage", "toggleText", "findInPage"];
    const offs = names.map((name) => registerCommand(name, () => toolbar.current[name]?.()));
    return () => offs.forEach((off) => off());
  }, []);

  const saveEdits = useCallback(
    async (change: (edits: Edits) => Edits) => {
      if (!detail) return;
      try {
        await api.saveEdits(documentId, change(structuredClone(detail.edits)));
        load();
        onChanged();
      } catch (e) {
        onError(String(e));
      }
    },
    [detail, documentId, load, onChanged, onError],
  );

  if (!detail) {
    return (
      <section className="page">
        <button className="ghost back" data-shortcut="library" onClick={onBack}>
          보관함으로
        </button>
        <p className="empty">여는 중</p>
      </section>
    );
  }

  const summary = detail.summary;
  const label = stateLabel(summary.state, progress);
  const ready = detail.pages.some((p) => p.outputs.length > 0);
  const pageIds = detail.pages.map((p) => p.id);

  const movePage = (pageId: string, delta: number) =>
    saveEdits((edits) => {
      const order = [...pageIds];
      const from = order.indexOf(pageId);
      const to = from + delta;
      if (from < 0 || to < 0 || to >= order.length) return edits;
      [order[from], order[to]] = [order[to], order[from]];
      setSelected((s) => Math.max(0, s + delta * (current?.page.outputs.length || 1)));
      return { ...edits, pageOrder: order };
    });

  const deletePage = (pageId: string) => {
    if (!window.confirm("이 페이지를 목록에서 뺄까요? 원본 사진은 남아 있어 아래 '지운 페이지'에서 되살릴 수 있습니다.")) return;
    saveEdits((edits) => ({ ...edits, deletedPages: [...(edits.deletedPages ?? []), pageId] }));
  };

  const restorePage = (pageId: string) =>
    saveEdits((edits) => ({ ...edits, deletedPages: (edits.deletedPages ?? []).filter((id) => id !== pageId) }));

  // 사람이 보고 고칠 게 없다고 한 페이지. 확인 표시와 "확인 필요" 목록에서 뺀다
  const toggleReviewed = (page: PageView) =>
    saveEdits((edits) => {
      const reviewed = edits.reviewed ?? [];
      return {
        ...edits,
        reviewed: reviewed.includes(page.id) ? reviewed.filter((id) => id !== page.id) : [...reviewed, page.id],
      };
    });

  // 이 쪽만 다른 필터. "default"면 문서 처리 옵션을 따른다
  const setPageFilter = (page: PageView, filter: Filter | "default") =>
    saveEdits((edits) => {
      const filters = { ...(edits.filters ?? {}) };
      if (filter === "default") delete filters[page.id];
      else filters[page.id] = filter;
      return { ...edits, filters };
    });

  const setLayout = (page: PageView, layout: PageLayout) =>
    saveEdits((edits) => ({
      ...edits,
      layouts: { ...(edits.layouts ?? {}), [page.id]: layout },
    }));

  // 자동 회전이 틀렸을 때. 누를 때마다 시계 방향으로 90도
  const rotatePage = (page: PageView) =>
    saveEdits((edits) => {
      const rotations = edits.rotations ?? {};
      return { ...edits, rotations: { ...rotations, [page.id]: ((rotations[page.id] ?? 0) + 90) % 360 } };
    });

  // 꼭짓점 창이 열려 있으면 도구 막대 단축키는 쉰다. 창 뒤의 쪽을 돌리거나 빼지 않게
  toolbar.current = current && !editingQuad
    ? {
        editQuad: () => setEditingQuad(current.page),
        rotatePage: () => rotatePage(current.page),
        movePageUp: () => current.pageIndex > 0 && movePage(current.page.id, -1),
        movePageDown: () => current.pageIndex < detail.pages.length - 1 && movePage(current.page.id, 1),
        removePage: () => deletePage(current.page.id),
        toggleText: () => ocr && setShowText((v) => !v),
        findInPage: () => {
          findRef.current?.focus();
          findRef.current?.select();
        },
      }
    : {};

  const saveQuad = async (pageId: string, quad: Quad | null) => {
    await saveEdits((edits) => {
      const quads = { ...(edits.quads ?? {}) };
      if (quad) quads[pageId] = quad;
      else delete quads[pageId];
      return { ...edits, quads };
    });
  };

  const commitName = () => {
    const name = nameDraft?.trim();
    setNameDraft(null);
    if (name && name !== summary.name) saveEdits((edits) => ({ ...edits, name }));
  };

  const commitSession = () => {
    const session = sessionDraft?.trim();
    setSessionDraft(null);
    if (session !== undefined && session !== (summary.session ?? "")) saveEdits((edits) => ({ ...edits, session }));
  };

  const setOptions = async (options: ProcessOptions) => {
    try {
      await api.setOptions(documentId, options);
      load();
      onChanged();
    } catch (e) {
      onError(String(e));
    }
  };

  const needle = compact(highlight);
  const hitLines = ocr && needle ? ocr.blocks.flatMap((b) => b.lines).filter((l) => compact(l.text).includes(needle)) : [];

  return (
    <section className="document">
      <header className="doc-header">
        <button className="ghost back" data-shortcut="library" onClick={onBack}>
          보관함으로
        </button>
        <input
          className="title-input"
          aria-label="문서 이름"
          value={nameDraft ?? summary.name}
          onChange={(e) => setNameDraft(e.target.value)}
          onBlur={commitName}
          onKeyDown={(e) => e.key === "Enter" && e.currentTarget.blur()}
        />
        <select
          aria-label="종류"
          value={summary.kind}
          onChange={(e) => saveEdits((edits) => ({ ...edits, kind: e.target.value as DocumentKind }))}
        >
          {Object.entries(KIND_LABEL).map(([value, text]) => (
            <option key={value} value={value}>
              {text}
            </option>
          ))}
        </select>
        <input
          className="session-input"
          aria-label="묶음"
          placeholder="묶음 (예: 박람회명, 책 시리즈)"
          value={sessionDraft ?? summary.session ?? ""}
          onChange={(e) => setSessionDraft(e.target.value)}
          onBlur={commitSession}
          onKeyDown={(e) => e.key === "Enter" && e.currentTarget.blur()}
        />
        <span className={`badge state-${label.tone}`} title={summary.state.status === "failed" ? summary.state.message : undefined}>
          {label.text}
        </span>
      </header>

      <div className="doc-body">
        <VirtualList
          className="thumbs"
          items={items}
          itemHeight={THUMB_ROW}
          selectedIndex={selected}
          render={(item, index) => (
            <button className={index === selected ? "thumb selected" : "thumb"} onClick={() => setSelected(index)}>
              <div className="thumb-image">
                {item.output?.thumb ? (
                  <img src={fileUrl(item.output.thumb, item.output.processedAt)} alt="" loading="lazy" />
                ) : (
                  <span className="cover-empty">처리 전</span>
                )}
              </div>
              <span className="thumb-label">
                {needsReview(item.page) && (
                  <span className="check-dot" title={item.page.checks.map((c) => CHECK_TEXT[c].text).join("\n")}>
                    !
                  </span>
                )}
                {item.pageIndex + 1}
                {item.output && partLabel(item.output.part) && ` ${partLabel(item.output.part)}`}
              </span>
            </button>
          )}
        />

        <div className="viewer">
          {current && (
            <div className="viewer-toolbar">
              <button className="secondary" data-shortcut="editQuad" onClick={() => setEditingQuad(current.page)}>
                꼭짓점 고치기
              </button>
              <select aria-label="배치" value={current.page.layout} onChange={(e) => setLayout(current.page, e.target.value as PageLayout)}>
                <option value="single">한 쪽</option>
                <option value="spread">펼친 두 쪽</option>
                <option value="trifold">3단 접지</option>
              </select>
              <button className="secondary" data-shortcut="rotatePage" onClick={() => rotatePage(current.page)}>
                돌리기
              </button>
              <select
                aria-label="이 쪽 필터"
                title={`이 쪽만 다른 필터로 처리합니다. 기본은 처리 옵션의 ${FILTER_NAME[detail.options.filter]}`}
                value={detail.edits.filters?.[current.page.id] ?? "default"}
                onChange={(e) => setPageFilter(current.page, e.target.value as Filter | "default")}
              >
                <option value="default">필터: 기본</option>
                {(Object.keys(FILTER_NAME) as Filter[]).map((f) => (
                  <option key={f} value={f}>
                    필터: {FILTER_NAME[f]}
                  </option>
                ))}
              </select>
              <button
                className="secondary"
                data-shortcut="movePageUp"
                disabled={current.pageIndex === 0}
                onClick={() => movePage(current.page.id, -1)}
              >
                앞으로
              </button>
              <button
                className="secondary"
                data-shortcut="movePageDown"
                disabled={current.pageIndex === detail.pages.length - 1}
                onClick={() => movePage(current.page.id, 1)}
              >
                뒤로
              </button>
              <button className="danger" data-shortcut="removePage" onClick={() => deletePage(current.page.id)}>
                페이지 빼기
              </button>
              <button
                className="secondary"
                data-shortcut="reprocess"
                title="이 문서의 모든 페이지를 지금 설정으로 다시 처리합니다"
                onClick={() => sendCommand("reprocess")}
              >
                다시 처리
              </button>
              <span className="spacer" />
              <input
                ref={findRef}
                data-shortcut="findInPage"
                className="find-input"
                placeholder="페이지에서 찾기"
                value={highlight}
                onChange={(e) => setHighlight(e.target.value)}
              />
              <button
                className={showText ? "secondary on" : "secondary"}
                data-shortcut="toggleText"
                disabled={!ocr}
                onClick={() => setShowText((v) => !v)}
              >
                글자
              </button>
            </div>
          )}
          <div className="viewer-stage" ref={setStage}>
            {current?.output ? (
              <div className={zoomed ? "page-frame zoomed" : "page-frame"}>
                <img
                  src={fileUrl(current.output.image, current.output.processedAt)}
                  alt={`${current.pageIndex + 1}쪽`}
                  title={zoomed ? "눌러서 화면에 맞추기" : "눌러서 크게 보기"}
                  style={zoomed || stageHeight === 0 ? undefined : { maxHeight: stageHeight - 32 }}
                  onClick={() => setZoomed((z) => !z)}
                />
                {hitLines.map((line, i) => (
                  <div
                    key={i}
                    className="hit-box"
                    style={{
                      left: `${line.bbox.x * 100}%`,
                      top: `${line.bbox.y * 100}%`,
                      width: `${line.bbox.width * 100}%`,
                      height: `${line.bbox.height * 100}%`,
                    }}
                  />
                ))}
              </div>
            ) : (
              <p className="empty">{items.length === 0 ? "페이지가 없습니다" : "아직 처리하지 않은 페이지입니다"}</p>
            )}
            {showText && ocr && (
              <div className="ocr-text">
                {ocr.blocks.map((block, i) => (
                  <p key={i} className={`ocr-${block.kind}`}>
                    {block.text}
                  </p>
                ))}
                {ocr.blocks.length === 0 && <p className="hint">읽은 글자가 없습니다</p>}
              </div>
            )}
          </div>
          {current && (
            <div className="viewer-foot hint">
              {current.pageIndex + 1}/{detail.pages.length}쪽 | 꼭짓점: {QUAD_SOURCE[current.page.quadSource]}
              {needle && ocr && ` | 찾은 줄 ${hitLines.length}개`}
              {reviewItems.length > 0 && (
                <button className="link" onClick={goNextReview} title="꼭짓점, 나누기, 방향을 볼 다음 쪽">
                  다음 확인 쪽 (전체 {reviewItems.length}쪽)
                </button>
              )}
              {current.page.checks.map((check) => (
                <span key={check} className={CHECK_TEXT[check].review && !isReviewed(current.page) ? "check review" : "check"}>
                  {CHECK_TEXT[check].text}
                </span>
              ))}
              {current.page.checks.some((c) => CHECK_TEXT[c].review) && (
                <button
                  className="link"
                  onClick={() => toggleReviewed(current.page)}
                  title="보고 나서 고칠 게 없으면 누릅니다. 확인 필요 목록에서 빠집니다"
                >
                  {isReviewed(current.page) ? "확인 취소" : "확인함"}
                </button>
              )}
            </div>
          )}
        </div>

        <aside className="side">
          <div className="segmented side-tabs" role="tablist">
            <button role="tab" aria-selected={sideTab === "info"} className={sideTab === "info" ? "on" : ""} onClick={() => setSideTab("info")}>
              정보
            </button>
            <button role="tab" aria-selected={sideTab === "ask"} className={sideTab === "ask" ? "on" : ""} onClick={() => setSideTab("ask")}>
              질문
            </button>
          </div>
          {/* 탭을 바꿔도 질문과 기다리던 답이 남도록 숨기기만 한다 */}
          <div className="side-pane" hidden={sideTab !== "ask"}>
            <AskPanel
              documentId={documentId}
              onCite={(_, outputId) => {
                const index = outputId ? items.findIndex((i) => i.output?.id === outputId) : -1;
                if (index >= 0) setSelected(index);
              }}
            />
          </div>
          <div className="side-pane" hidden={sideTab !== "info"}>
            <DocumentOrganize
              documentId={documentId}
              kind={summary.kind}
              session={summary.session}
              tags={detail.edits.tags ?? []}
              tagsByUser={detail.edits.tagsByUser ?? false}
              ready={ready}
              onAcceptSession={(session) => saveEdits((edits) => ({ ...edits, session }))}
              onChanged={() => {
                load();
                onChanged();
              }}
              onError={onError}
            />
          <OriginPanel documentId={documentId} revision={revision} />
          <OptionsPanel options={detail.options} onChange={setOptions} onReprocess={() => api.reprocess(documentId, true).then(onChanged).catch((e) => onError(String(e)))} />
          <ExportPanel documentId={documentId} isPamphlet={summary.kind === "pamphlet"} ready={ready} onError={onError} />
          {summary.kind === "pamphlet" && (
            <AiPanel
              documentId={documentId}
              ready={ready}
              onRename={(name) => saveEdits((edits) => ({ ...edits, name }))}
              onError={onError}
            />
          )}
          {summary.kind === "book" && (
            <BookPanel
              documentId={documentId}
              revision={`${revision}:${detail.pages.map((p) => p.outputs.map((o) => o.processedAt).join()).join()}`}
              ready={ready}
              onSelect={(outputId) => {
                const index = items.findIndex((i) => i.output?.id === outputId);
                if (index >= 0) setSelected(index);
              }}
              onSorted={() => {
                load();
                onChanged();
              }}
              onError={onError}
            />
          )}
          {detail.deletedPages.length > 0 && (
            <section className="panel">
              <h3>지운 페이지</h3>
              <ul className="plain-list">
                {detail.deletedPages.map((id, i) => (
                  <li key={id}>
                    <span>지운 페이지 {i + 1}</span>
                    <button className="ghost" onClick={() => restorePage(id)}>
                      되살리기
                    </button>
                  </li>
                ))}
              </ul>
            </section>
          )}
          </div>
        </aside>
      </div>

      {editingQuad && (
        <QuadEditor
          documentId={documentId}
          page={editingQuad}
          hasManual={editingQuad.quadSource === "manual"}
          onSave={(quad) => saveQuad(editingQuad.id, quad)}
          onClose={() => setEditingQuad(null)}
          onError={onError}
        />
      )}
    </section>
  );
}

const QUAD_SOURCE = {
  manual: "손으로 고침",
  phone: "iPhone 감지",
  detected: "Mac 감지",
  none: "사진 전체",
} as const;

function OptionsPanel({
  options,
  onChange,
  onReprocess,
}: {
  options: ProcessOptions;
  onChange: (o: ProcessOptions) => void;
  onReprocess: () => void;
}) {
  const segment = <K extends "dewarp" | "filter">(key: K, choices: [ProcessOptions[K], string][]) => (
    <div className="segmented" role="radiogroup">
      {choices.map(([value, text]) => (
        <button
          key={String(value)}
          role="radio"
          aria-checked={options[key] === value}
          className={options[key] === value ? "on" : ""}
          onClick={() => onChange({ ...options, [key]: value })}
        >
          {text}
        </button>
      ))}
    </div>
  );
  return (
    <section className="panel">
      <h3>처리 옵션</h3>
      <div className="field">
        <span>곡면 보정</span>
        {segment("dewarp", [
          ["off", "끔"],
          ["normal", "보통"],
          ["strong", "더 펴기"],
        ])}
      </div>
      <div className="field">
        <span>필터</span>
        {segment("filter", [
          ["original", "원본"],
          ["enhanced", "선명하게"],
          ["grayscale", "회색"],
          ["blackWhite", "흑백"],
        ])}
      </div>
      <label className="toggle">
        <input type="checkbox" checked={options.removeFingers} onChange={(e) => onChange({ ...options, removeFingers: e.target.checked })} />
        손가락 지우기
      </label>
      <label className="toggle">
        <input
          type="checkbox"
          checked={options.removeHandwriting}
          onChange={(e) => onChange({ ...options, removeHandwriting: e.target.checked })}
        />
        필기 지우기
      </label>
      <p className="hint">옵션을 바꾸면 모든 페이지를 다시 처리합니다.</p>
      <button className="secondary" data-shortcut="reprocess" onClick={onReprocess}>
        전체 다시 처리
      </button>
    </section>
  );
}
