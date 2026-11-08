import jpeg from "jpeg-js";

// 처리된 페이지(pages/<출력 id>.jpg)를 Claude가 보기 좋은 크기로 줄인다.
// 순수 JS(jpeg-js)라 네이티브 모듈 없이 어디서나 돈다. 긴 변이 이미 작으면 다시 인코딩하지 않는다

/** JPEG 머리(SOF)에서 크기만 읽는다. 못 읽으면 null */
export function jpegSize(bytes: Uint8Array): { width: number; height: number } | null {
  if (bytes[0] !== 0xff || bytes[1] !== 0xd8) return null;
  let i = 2;
  while (i + 9 < bytes.length) {
    if (bytes[i] !== 0xff) {
      i++;
      continue;
    }
    const marker = bytes[i + 1];
    // SOF0~SOF15 중 DHT(C4), JPG(C8), DAC(CC)를 뺀 것
    if (marker >= 0xc0 && marker <= 0xcf && marker !== 0xc4 && marker !== 0xc8 && marker !== 0xcc) {
      return { height: (bytes[i + 5] << 8) | bytes[i + 6], width: (bytes[i + 7] << 8) | bytes[i + 8] };
    }
    if (marker === 0xd8 || marker === 0x01 || (marker >= 0xd0 && marker <= 0xd7)) {
      i += 2;
      continue;
    }
    i += 2 + ((bytes[i + 2] << 8) | bytes[i + 3]);
  }
  return null;
}

/** 상자 평균으로 줄인다. 글자가 많은 스캔이라 최근접보다 덜 깨진다 */
function downscale(src: Uint8Array, w: number, h: number, nw: number, nh: number): Uint8Array {
  const out = new Uint8Array(nw * nh * 4);
  const sx = w / nw;
  const sy = h / nh;
  for (let y = 0; y < nh; y++) {
    const y0 = Math.floor(y * sy);
    const y1 = Math.max(y0 + 1, Math.min(h, Math.floor((y + 1) * sy)));
    for (let x = 0; x < nw; x++) {
      const x0 = Math.floor(x * sx);
      const x1 = Math.max(x0 + 1, Math.min(w, Math.floor((x + 1) * sx)));
      let r = 0;
      let g = 0;
      let b = 0;
      for (let yy = y0; yy < y1; yy++) {
        let p = (yy * w + x0) * 4;
        for (let xx = x0; xx < x1; xx++, p += 4) {
          r += src[p];
          g += src[p + 1];
          b += src[p + 2];
        }
      }
      const n = (y1 - y0) * (x1 - x0);
      const o = (y * nw + x) * 4;
      out[o] = r / n;
      out[o + 1] = g / n;
      out[o + 2] = b / n;
      out[o + 3] = 255;
    }
  }
  return out;
}

export interface Fitted {
  data: Buffer;
  width: number;
  height: number;
  /** 원본 크기 */
  original: { width: number; height: number };
  resized: boolean;
}

/** 긴 변이 maxEdge를 넘으면 줄여서 JPEG로 다시 인코딩한다 */
export function fitJpeg(bytes: Buffer, maxEdge: number, quality = 85): Fitted {
  const size = jpegSize(bytes);
  if (size && Math.max(size.width, size.height) <= maxEdge) {
    return { data: bytes, ...size, original: size, resized: false };
  }
  const img = jpeg.decode(bytes, { useTArray: true, maxResolutionInMP: 200, maxMemoryUsageInMB: 1024 });
  const original = { width: img.width, height: img.height };
  const scale = Math.min(1, maxEdge / Math.max(img.width, img.height));
  if (scale === 1) return { data: bytes, ...original, original, resized: false };
  const width = Math.max(1, Math.round(img.width * scale));
  const height = Math.max(1, Math.round(img.height * scale));
  const data = jpeg.encode({ data: downscale(img.data, img.width, img.height, width, height), width, height }, quality).data;
  return { data, width, height, original, resized: true };
}

/** 그림 기준 정규화 사각형(0~1, 원점 왼쪽 위). OCR 줄 위치(ocr.rs Rect)와 같은 단위 */
export interface Region {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** 정규화 영역을 그림 안 픽셀 사각형으로. 그림 밖으로 나간 부분은 잘라 낸다. 남는 것이 없으면 null */
export function regionPixels(region: Region, width: number, height: number): { x: number; y: number; w: number; h: number } | null {
  const x0 = Math.max(0, Math.floor(region.x * width));
  const y0 = Math.max(0, Math.floor(region.y * height));
  const x1 = Math.min(width, Math.ceil((region.x + region.width) * width));
  const y1 = Math.min(height, Math.ceil((region.y + region.height) * height));
  if (x1 - x0 < 1 || y1 - y0 < 1) return null;
  return { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
}

/** 한 부분만 원본 해상도로 잘라 낸다. 잘라 낸 것도 긴 변이 maxEdge를 넘으면 줄인다. 작은 글자를 읽을 때 쓴다 */
export function cropJpeg(bytes: Buffer, region: Region, maxEdge: number, quality = 90): Fitted & { box: { x: number; y: number; w: number; h: number } } {
  const img = jpeg.decode(bytes, { useTArray: true, maxResolutionInMP: 200, maxMemoryUsageInMB: 1024 });
  const original = { width: img.width, height: img.height };
  const box = regionPixels(region, img.width, img.height);
  if (!box) throw new Error("영역이 그림 밖이거나 너무 작습니다");
  const cut = new Uint8Array(box.w * box.h * 4);
  for (let y = 0; y < box.h; y++) {
    const from = ((box.y + y) * img.width + box.x) * 4;
    cut.set(img.data.subarray(from, from + box.w * 4), y * box.w * 4);
  }
  const scale = Math.min(1, maxEdge / Math.max(box.w, box.h));
  const width = Math.max(1, Math.round(box.w * scale));
  const height = Math.max(1, Math.round(box.h * scale));
  const pixels = scale < 1 ? downscale(cut, box.w, box.h, width, height) : cut;
  const data = jpeg.encode({ data: pixels, width, height }, quality).data;
  return { data, width, height, original, resized: scale < 1, box };
}
