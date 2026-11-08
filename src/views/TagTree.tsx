import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "../api";
import type { DocumentTags } from "../types";
import { TagNode, UNTAGGED } from "../tags";

const COLLAPSED_KEY = "scanshelf.tagTreeCollapsed";
/** 보관함에서 고른 태그. 다음 실행에도 기억하고, ⌘J 질문 창이 처음 범위로 쓴다 */
export const TAG_FILTER_KEY = "scanshelf.tagFilter";

/** 보관함 모든 문서의 태그(문서 id별). 태그가 바뀌면(tags-changed) 다시 읽는다 */
export function useDocumentTags(revision?: unknown): Record<string, DocumentTags> {
  const [tags, setTags] = useState<Record<string, DocumentTags>>({});
  const load = useCallback(() => {
    api
      .documentTags()
      .then(setTags)
      .catch((e) => console.error(e));
  }, []);
  useEffect(load, [load, revision]);
  useEffect(() => {
    const unlisten = listen("tags-changed", load);
    return () => {
      unlisten.then((f) => f());
    };
  }, [load]);
  return tags;
}

export function readTagFilter(): string | null {
  try {
    return localStorage.getItem(TAG_FILTER_KEY);
  } catch {
    return null;
  }
}

export function saveTagFilter(path: string | null) {
  try {
    if (path === null) localStorage.removeItem(TAG_FILTER_KEY);
    else localStorage.setItem(TAG_FILTER_KEY, path);
  } catch {
    // 저장하지 못해도 이번 실행에서는 걸러 본다
  }
}

interface Props {
  tree: TagNode[];
  total: number;
  untagged: number;
  /** 고른 태그 경로. null이면 전체, UNTAGGED면 태그 없는 문서 */
  selected: string | null;
  onSelect: (path: string | null) => void;
}

/** 보관함 옆 태그 트리. `/`로 나눈 태그를 폴더처럼 펼친다. 상위를 고르면 하위 태그 문서도 보인다
 * @see docs/organize.md */
export function TagTree({ tree, total, untagged, selected, onSelect }: Props) {
  // 처음에는 첫 단계만 보이고, 고른 태그의 상위는 펼쳐 둔다
  const [open, setOpen] = useState<Set<string>>(() => new Set(ancestors(selected)));
  const toggle = (path: string) =>
    setOpen((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });

  // 트리 전체를 접어 문서 목록을 넓게 본다. 접어도 고른 태그는 머리줄에 남겨 걸러 보는 중임을 알린다
  const [collapsed, setCollapsed] = useState(() => {
    try {
      return localStorage.getItem(COLLAPSED_KEY) === "1";
    } catch {
      return false;
    }
  });
  const toggleAll = () => {
    const next = !collapsed;
    setCollapsed(next);
    try {
      if (next) localStorage.setItem(COLLAPSED_KEY, "1");
      else localStorage.removeItem(COLLAPSED_KEY);
    } catch {
      // 저장하지 못해도 이번 실행에서는 접힌다
    }
  };
  // 문서 하나에만 붙은 태그는 길게 늘어서 자주 쓰는 태그를 가린다(박람회 22개 문서에 태그 50여 개). 접어 두고 더 보기로 연다.
  // 고른 태그가 그 안에 있으면 펼친다
  const RARE_MIN_TAGS = 12;
  const rareAll = tree.length > RARE_MIN_TAGS ? tree.filter((n) => n.count <= 1) : [];
  const main = tree.filter((n) => !rareAll.includes(n));
  const rare = rareAll;
  const [showRare, setShowRare] = useState(() => rare.some((n) => selected !== null && (n.path === selected || selected.startsWith(`${n.path}/`))));
  const current =
    selected === null
      ? { name: "전체", count: total }
      : selected === UNTAGGED
        ? { name: "태그 없음", count: untagged }
        : { name: selected, count: findNode(tree, selected)?.count ?? 0 };

  const row = (node: TagNode, depth: number) => (
    <li key={node.path}>
      <div className={`tag-row ${selected === node.path ? "selected" : ""}`} style={{ paddingLeft: 4 + depth * 14 }}>
        {node.children.length > 0 ? (
          <button
            className="tag-toggle"
            aria-label={open.has(node.path) ? `${node.name} 접기` : `${node.name} 펼치기`}
            aria-expanded={open.has(node.path)}
            onClick={() => toggle(node.path)}
          >
            {open.has(node.path) ? "▾" : "▸"}
          </button>
        ) : (
          <span className="tag-toggle" />
        )}
        <button
          className="tag-name"
          aria-pressed={selected === node.path}
          onClick={() => onSelect(selected === node.path ? null : node.path)}
          title={node.path}
        >
          {node.name}
        </button>
        <span className="tag-count">{node.count}</span>
      </div>
      {open.has(node.path) && node.children.length > 0 && <ul>{node.children.map((c) => row(c, depth + 1))}</ul>}
    </li>
  );

  return (
    <nav className={`tag-tree ${collapsed ? "collapsed" : ""}`} aria-label="태그">
      <button className="tag-tree-head" aria-expanded={!collapsed} onClick={toggleAll}>
        <span className="tag-toggle">{collapsed ? "▸" : "▾"}</span>
        <span className="tag-tree-title">태그</span>
        {collapsed && (
          <>
            <span className={`tag-tree-current ${selected === null ? "" : "active"}`}>{current.name}</span>
            <span className="tag-count">{current.count}</span>
          </>
        )}
      </button>
      {!collapsed && (
        <ul>
          <li>
            <div className={`tag-row ${selected === null ? "selected" : ""}`} style={{ paddingLeft: 4 }}>
              <span className="tag-toggle" />
              <button className="tag-name" aria-pressed={selected === null} onClick={() => onSelect(null)}>
                전체
              </button>
              <span className="tag-count">{total}</span>
            </div>
          </li>
          {main.map((n) => row(n, 0))}
          {rare.length > 0 && (
            <li>
              <button className="tag-more" aria-expanded={showRare} onClick={() => setShowRare(!showRare)}>
                {showRare ? "문서 1개 태그 접기" : `문서 1개 태그 ${rare.length}개 더 보기`}
              </button>
            </li>
          )}
          {showRare && rare.map((n) => row(n, 0))}
          {untagged > 0 && untagged < total && (
            <li>
              <div className={`tag-row ${selected === UNTAGGED ? "selected" : ""}`} style={{ paddingLeft: 4 }}>
                <span className="tag-toggle" />
                <button
                  className="tag-name muted"
                  aria-pressed={selected === UNTAGGED}
                  onClick={() => onSelect(selected === UNTAGGED ? null : UNTAGGED)}
                >
                  태그 없음
                </button>
                <span className="tag-count">{untagged}</span>
              </div>
            </li>
          )}
          {tree.length === 0 && <li className="hint tag-empty">문서 화면에서 태그를 붙이면 여기에 모입니다</li>}
        </ul>
      )}
    </nav>
  );
}

function findNode(nodes: TagNode[], path: string): TagNode | undefined {
  for (const n of nodes) {
    if (n.path === path) return n;
    if (path.startsWith(`${n.path}/`)) return findNode(n.children, path);
  }
  // 모든 문서에 붙은 상위 태그는 트리에서 건너뛰므로(liftUniversal) 하위에서 더 찾는다
  for (const n of nodes) {
    const found = findNode(n.children, path);
    if (found) return found;
  }
  return undefined;
}

function ancestors(path: string | null): string[] {
  if (!path || path === UNTAGGED) return [];
  const parts = path.split("/");
  return parts.slice(0, -1).map((_, i) => parts.slice(0, i + 1).join("/"));
}
