import { useCallback, useEffect, useState } from "react";
import { api, formatBytes } from "../api";
import type { ExportFile, ExportFormat } from "../types";

interface Props {
  documentId: string;
  isPamphlet: boolean;
  ready: boolean;
  onError: (e: string) => void;
}

const FORMATS: { format: ExportFormat; label: string }[] = [
  { format: "pdf", label: "PDF" },
  { format: "jpg", label: "JPG 묶음" },
  { format: "txt", label: "텍스트" },
];

/** 내보내기는 문서 폴더 exports/에 쌓이고, 파일마다 Finder, 공유, 다른 곳에 저장을 고른다 */
export function ExportPanel({ documentId, isPamphlet, ready, onError }: Props) {
  const [files, setFiles] = useState<ExportFile[]>([]);
  const [busy, setBusy] = useState<string | null>(null);

  const reload = useCallback(() => {
    api.listExports(documentId).then(setFiles).catch((e) => onError(String(e)));
  }, [documentId, onError]);

  useEffect(reload, [reload]);

  const run = async (key: string, task: () => Promise<unknown>) => {
    setBusy(key);
    try {
      const result = await task();
      if (result === null) onError("이 내보내기는 아직 연결되지 않았습니다");
      reload();
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <section className="panel">
      <h3>내보내기</h3>
      {!ready && <p className="hint">처리가 끝나면 내보낼 수 있습니다</p>}
      <div className="button-row">
        {FORMATS.map(({ format, label }) => (
          <button
            key={format}
            className="secondary"
            data-shortcut={format === "pdf" ? "exportPdf" : undefined}
            disabled={!ready || busy !== null}
            onClick={() => run(format, () => api.exportDocument(documentId, format))}
          >
            {busy === format ? "만드는 중" : label}
          </button>
        ))}
        <button className="secondary" disabled={!ready || busy !== null} onClick={() => run("docx", () => api.exportDocx(documentId))}>
          {busy === "docx" ? "만드는 중" : "Word"}
        </button>
        {isPamphlet && (
          <button
            className="secondary"
            disabled={!ready || busy !== null}
            onClick={() => run("bundle", () => api.exportAiBundle(documentId))}
          >
            {busy === "bundle" ? "만드는 중" : "AI 묶음"}
          </button>
        )}
      </div>
      {files.length > 0 && (
        <ul className="export-list">
          {files.map((file) => (
            <li key={file.path}>
              <div className="export-name">
                {file.name}
                <span className="hint"> {formatBytes(file.size)}</span>
              </div>
              <div className="button-row compact">
                <button className="ghost" onClick={() => api.revealExport(file.path).catch((e) => onError(String(e)))}>
                  Finder
                </button>
                <button
                  className="ghost"
                  onClick={(e) => {
                    const r = e.currentTarget.getBoundingClientRect();
                    api.shareExport(file.path, r.left + r.width / 2, r.bottom).catch((err) => onError(String(err)));
                  }}
                >
                  공유
                </button>
                <button className="ghost" onClick={() => api.saveExportAs(file.path).catch((e) => onError(String(e)))}>
                  저장
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
