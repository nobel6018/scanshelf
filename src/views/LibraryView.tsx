import { useMemo, useState } from "react";
import { api, fileUrl, KIND_LABEL } from "../api";
import type { DocumentState, DocumentSummary, Progress } from "../types";
import { buildTagTree, matchesTag } from "../tags";
import { readTagFilter, saveTagFilter, TagTree, useDocumentTags } from "./TagTree";

const NO_SESSION = "묶지 않은 문서";

export function stateLabel(state: DocumentState, progress?: Progress): { text: string; tone: string } {
  if (progress && progress.stage !== "done") {
    const stage = progress.stage === "ocr" ? "글자 읽는 중" : "보정 중";
    return { text: `${stage} ${progress.done + 1}/${progress.total}`, tone: "busy" };
  }
  switch (state.status) {
    case "unprocessed":
      return { text: "처리 전", tone: "muted" };
    case "queued":
      return { text: "처리 대기", tone: "busy" };
    case "processing":
      return { text: `처리 중 ${state.done}/${state.total}`, tone: "busy" };
    case "stale":
      return { text: `다시 처리할 페이지 ${state.pages}장`, tone: "warn" };
    case "failed":
      return { text: "처리 실패", tone: "error" };
    case "ready":
      return { text: "완료", tone: "ok" };
  }
}

interface Props {
  documents: DocumentSummary[];
  progress: Record<string, Progress>;
  onOpen: (id: string) => void;
  onError: (e: string) => void;
  /** 문서 카드 우클릭. App이 OS 메뉴를 띄운다(src/documentMenu.ts) */
  onContextMenu?: (doc: DocumentSummary) => void;
}

export function LibraryView({ documents, progress, onOpen, onError, onContextMenu }: Props) {
  const [csvBusy, setCsvBusy] = useState<string | null>(null);
  // 태그로 걸러 보기(docs/organize.md). 고른 태그는 다음 실행에도 기억한다
  const tagsById = useDocumentTags(documents);
  const [tagFilter, setTagFilter] = useState<string | null>(readTagFilter);
  const tagged = useMemo(() => documents.map((d) => ({ tags: tagsById[d.id]?.tags ?? [] })), [documents, tagsById]);
  const tagTree = useMemo(() => buildTagTree(tagged), [tagged]);
  const untagged = tagged.filter((d) => d.tags.length === 0).length;
  const selectTag = (path: string | null) => {
    setTagFilter(path);
    saveTagFilter(path);
  };
  // 처리하며 사람이 볼 쪽이 나온 문서만 보기(docs/pipeline.md "페이지 확인")
  const [reviewOnly, setReviewOnly] = useState(false);
  const reviewCount = documents.filter((d) => d.reviewPages > 0).length;
  const visible = useMemo(
    () =>
      documents.filter((d) => matchesTag(tagsById[d.id]?.tags, tagFilter) && (!reviewOnly || d.reviewPages > 0)),
    [documents, tagsById, tagFilter, reviewOnly],
  );

  // 세션은 최근 문서가 있는 순서대로. 문서는 이미 최신순으로 온다
  const groups = useMemo(() => {
    const map = new Map<string, DocumentSummary[]>();
    for (const doc of visible) {
      const key = doc.session?.trim() || NO_SESSION;
      if (!map.has(key)) map.set(key, []);
      map.get(key)!.push(doc);
    }
    return [...map.entries()];
  }, [visible]);

  const exportCsv = async (session: string) => {
    setCsvBusy(session);
    try {
      const result = await api.exportNotionCsv(session);
      if (result === null) onError("노션 CSV 내보내기는 아직 연결되지 않았습니다");
      // CSV와 PDF가 든 폴더는 Finder에 열린다. 빠진 문서만 알린다
      else if (result.skipped.length > 0) onError(`CSV에서 빠진 문서가 있습니다: ${result.skipped.join(" / ")}`);
    } catch (e) {
      onError(String(e));
    } finally {
      setCsvBusy(null);
    }
  };

  if (documents.length === 0) {
    return (
      <section className="page">
        <h1>보관함</h1>
        <p className="empty">iPhone에서 찍은 문서가 여기에 모입니다. 설정에서 iPhone을 연결하세요.</p>
      </section>
    );
  }

  return (
    <section className="page library-layout">
      <TagTree tree={tagTree} total={documents.length} untagged={untagged} selected={tagFilter} onSelect={selectTag} />
      <div className="library-main">
        <div className="library-head">
          <h1>보관함</h1>
          {(reviewCount > 0 || reviewOnly) && (
            <button
              className={reviewOnly ? "chip on" : "chip"}
              aria-pressed={reviewOnly}
              title="꼭짓점, 나누기, 방향을 사람이 볼 쪽이 있는 문서"
              onClick={() => setReviewOnly(!reviewOnly)}
            >
              확인 필요 {reviewCount}개
            </button>
          )}
        </div>
        {groups.length === 0 && (
          <p className="empty">
            {reviewOnly ? "확인할 문서가 없습니다." : "이 태그가 붙은 문서가 없습니다."}{" "}
            <button
              className="ghost"
              onClick={() => {
                selectTag(null);
                setReviewOnly(false);
              }}
            >
              전체 보기
            </button>
          </p>
        )}
        {groups.map(([session, docs]) => {
          const pamphlets = docs.filter((d) => d.kind === "pamphlet").length;
          return (
            <div className="session" key={session}>
              <div className="session-head">
                <h2>
                  {session} <span className="count">{docs.length}개</span>
                </h2>
                {session !== NO_SESSION && pamphlets > 0 && (
                  <button className="secondary" disabled={csvBusy === session} onClick={() => exportCsv(session)}>
                    {csvBusy === session ? "내보내는 중" : "노션 CSV 내보내기"}
                  </button>
                )}
              </div>
              <div className="doc-grid">
                {docs.map((doc) => {
                  const label = stateLabel(doc.state, progress[doc.id]);
                  return (
                    <button
                      className="doc-card"
                      key={doc.id}
                      onClick={() => onOpen(doc.id)}
                      onContextMenu={(e) => {
                        if (!onContextMenu) return;
                        e.preventDefault();
                        onContextMenu(doc);
                      }}
                    >
                      <div className="cover">
                        {doc.cover ? (
                          <img src={fileUrl(doc.cover, doc.state.status)} alt="" loading="lazy" />
                        ) : (
                          <span className="cover-empty">{doc.pageCount}장</span>
                        )}
                      </div>
                      <div className="doc-meta">
                        <span className={`badge kind-${doc.kind}`}>{KIND_LABEL[doc.kind]}</span>
                        {/* 완료는 평소 상태라 띄우지 않는다. 처리 중, 실패, 다시 처리할 때만 보인다 */}
                        {doc.state.status !== "ready" || progress[doc.id] ? (
                          <span className={`badge state-${label.tone}`}>{label.text}</span>
                        ) : null}
                        {doc.reviewPages > 0 && (
                          <span className="badge state-warn" title="꼭짓점, 나누기, 방향을 사람이 볼 쪽이 있습니다">
                            확인 {doc.reviewPages}쪽
                          </span>
                        )}
                      </div>
                      <div className="doc-name">{doc.name}</div>
                      {doc.company && doc.company !== doc.name && <div className="doc-company">{doc.company}</div>}
                      <div className="doc-sub">
                        {doc.pageCount}장 | {new Date(doc.createdAt).toLocaleDateString("ko-KR")}
                      </div>
                    </button>
                  );
                })}
              </div>
            </div>
          );
        })}
      </div>
    </section>
  );
}
