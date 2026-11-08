import { useEffect, useState } from "react";
import { api } from "../api";
import type { DocumentOrigin } from "../types";

// 문서가 어떻게 들어왔는지(Wi-Fi, iCloud)와 iCloud 보관 상태. @see docs/transfer.md
const ROUTE_LABEL = { wifi: "Wi-Fi로 받음", icloud: "iCloud로 받음" } as const;

function formatTime(iso: string): string {
  return new Date(iso).toLocaleString("ko-KR", { month: "numeric", day: "numeric", hour: "numeric", minute: "2-digit" });
}

export function OriginPanel({ documentId, revision }: { documentId: string; revision: number }) {
  const [origin, setOrigin] = useState<DocumentOrigin | null>(null);

  useEffect(() => {
    api
      .documentOrigin(documentId)
      .then((o) => setOrigin(o ?? null))
      .catch(() => setOrigin(null));
  }, [documentId, revision]);

  if (!origin || (origin.arrivals.length === 0 && origin.archive === "notApplicable")) return null;
  const last = origin.arrivals[origin.arrivals.length - 1];
  const routes = new Set(origin.arrivals.map((a) => a.route));
  return (
    <section className="panel">
      <h3>받은 경로</h3>
      {last ? (
        <p>
          {ROUTE_LABEL[last.route]}, {formatTime(last.at)}
          {routes.size > 1 && <span className="hint"> (Wi-Fi와 iCloud로 나눠 받음)</span>}
        </p>
      ) : (
        <p className="hint">받은 기록이 없습니다(0.3.2 전에 받은 문서)</p>
      )}
      {origin.archive === "done" && <p>iCloud에 원본 보관됨</p>}
      {origin.archive === "partial" && <p className="hint">iCloud에 원본을 올리는 중</p>}
    </section>
  );
}
