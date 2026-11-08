// 쓰기 통로 클라이언트를 진짜 HTTP로 확인한다. 앱 대신 node:http 서버를 띄운다
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, describe, test } from "node:test";
import { AppNotRunning, ControlClient, ControlError, controlFilePath } from "../dist/control.js";
import { PIPELINE_VERSION } from "../dist/library.js";

const TOKEN = "ab".repeat(32);
let dir;
let server;
let port;
const seen = [];

before(async () => {
  dir = mkdtempSync(join(tmpdir(), "scanshelf-control-test-"));
  server = createServer((req, res) => {
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      seen.push({ method: req.method, url: req.url, auth: req.headers.authorization, origin: req.headers.origin, body });
      res.setHeader("content-type", "application/json");
      if (req.headers.authorization !== `Bearer ${TOKEN}`) {
        res.statusCode = 401;
        return res.end(JSON.stringify({ error: "토큰이 맞지 않는다" }));
      }
      if (req.url.endsWith("/rotate")) return res.end(JSON.stringify({ rotation: 90 }));
      if (req.url.endsWith("/tags")) {
        res.statusCode = 404;
        return res.end(JSON.stringify({ error: "문서를 찾지 못했다: x" }));
      }
      if (req.url.endsWith("/flat")) {
        res.setHeader("content-type", "image/jpeg");
        res.setHeader("x-scanshelf-view", JSON.stringify({ width: 3, folds: [0.5] }));
        return res.end("jpeg-bytes");
      }
      if (req.url === "/v1/review") return res.end(JSON.stringify({ documents: [{ documentId: "d", pages: [] }] }));
      res.end("{}");
    });
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  port = server.address().port;
});

after(() => {
  server.close();
  rmSync(dir, { recursive: true, force: true });
});

const writeInfo = (token) => {
  const file = join(dir, "control.json");
  writeFileSync(file, JSON.stringify({ port, token, pid: 1 }));
  return file;
};

describe("ControlClient", () => {
  test("토큰을 실어 정해진 경로로 보내고 결과를 돌려준다", async () => {
    const client = new ControlClient(writeInfo(TOKEN));
    seen.length = 0;
    assert.deepEqual(await client.review(), [{ documentId: "d", pages: [] }]);
    assert.equal(await client.rotate("doc", "page", 90), 90);
    await client.setReviewed("doc", "page", true);
    assert.deepEqual(
      seen.map((s) => [s.method, s.url, s.auth, s.origin, s.body]),
      [
        ["GET", "/v1/review", `Bearer ${TOKEN}`, undefined, ""],
        ["POST", "/v1/documents/doc/pages/page/rotate", `Bearer ${TOKEN}`, undefined, '{"degrees":90}'],
        ["PUT", "/v1/documents/doc/pages/page/reviewed", `Bearer ${TOKEN}`, undefined, '{"reviewed":true}'],
      ],
    );
  });

  test("중간 그림은 바이트와 좌표 정보 머리를 받는다", async () => {
    const client = new ControlClient(writeInfo(TOKEN));
    const view = await client.view("doc", "page", "flat");
    assert.equal(view.data.toString(), "jpeg-bytes");
    assert.deepEqual(view.info, { width: 3, folds: [0.5] });
    assert.equal(seen.at(-1).url, "/v1/documents/doc/pages/page/flat");
  });

  test("앱이 거절한 이유를 그대로 전한다", async () => {
    const client = new ControlClient(writeInfo(TOKEN));
    await assert.rejects(client.updateTags("doc", ["a"], []), (e) => e instanceof ControlError && /문서를 찾지 못했다/.test(e.message));
  });

  test("토큰이 바뀌었거나, 파일이 없거나, 포트가 닫혔으면 앱이 꺼진 것으로 본다", async () => {
    await assert.rejects(new ControlClient(writeInfo("cd".repeat(32))).review(), (e) => e instanceof AppNotRunning && /다시 켜져/.test(e.message));
    await assert.rejects(new ControlClient(join(dir, "없음.json")).review(), AppNotRunning);
    const closed = join(dir, "closed.json");
    writeFileSync(closed, JSON.stringify({ port: 1, token: TOKEN, pid: 1 }));
    await assert.rejects(new ControlClient(closed).review(), AppNotRunning);
  });

  test("접속 정보는 보관함 폴더의 부모에 있다", () => {
    assert.equal(controlFilePath("/a/b/library"), "/a/b/control.json");
  });
});

describe("Rust와 같은 값", () => {
  test("PIPELINE_VERSION이 scanshelf-core와 같다", () => {
    const rust = readFileSync(new URL("../../crates/scanshelf-core/src/processing.rs", import.meta.url), "utf8");
    assert.equal(Number(rust.match(/pub const PIPELINE_VERSION: u32 = (\d+);/)[1]), PIPELINE_VERSION);
  });
});
