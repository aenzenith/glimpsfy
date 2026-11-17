// Canvas 2D drawing of the objects that are drawn rather than computed: arrows, lines, shapes,
// paths, text, step markers and the spotlight's dim. The canvas is in image pixels (the caller
// only translates it to the exported area), so a drawing lands on the same pixels in the preview
// and in the export. Blur and pixelate are not drawn here; they are pixels (redact.ts).

import type { AnnotationObject, Point, Rect } from "../lib/annotate";
import {
  DIM_ALPHA,
  FONT_FAMILY,
  DIM_RGB,
  HIGHLIGHTER_ALPHA,
  LINE_HEIGHT,
  RECT_RADIUS,
  SHADOW,
  SHADOW_STRONG,
  SPOTLIGHT_RADIUS,
  STEP_RING,
  TEXT_PAD,
  TEXT_RADIUS,
  arrowMetrics,
  contrastInk,
  cssOf,
  inkHex,
  textFont,
} from "./style";

type Ctx = CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D;
type Text = Extract<AnnotationObject, { kind: "text" }>;
type Spotlight = Extract<AnnotationObject, { kind: "spotlight" }>;

/** A 2D context for measuring text, made once. */
let measurer: Ctx | null = null;
function measuring(): Ctx | null {
  if (measurer) return measurer;
  if (typeof OffscreenCanvas !== "undefined") measurer = new OffscreenCanvas(1, 1).getContext("2d");
  else if (typeof document !== "undefined") measurer = document.createElement("canvas").getContext("2d");
  return measurer;
}

export type TextLayout = { lines: string[]; width: number; height: number; lineHeight: number; padX: number; padTop: number };

/** Breaks a text into lines: at its own line breaks, and at word boundaries before `maxWidth`. */
export function layoutText(object: Text, ctx: Ctx | null = measuring()): TextLayout {
  const size = object.fontSize;
  const padX = TEXT_PAD.x * size;
  const padTop = TEXT_PAD.top * size;
  const lineHeight = LINE_HEIGHT * size;
  const measure = (s: string) => (ctx ? ctx.measureText(s).width : s.length * size * 0.55);
  if (ctx) ctx.font = textFont(size);
  const lines: string[] = [];
  const limit = object.maxWidth === null ? Infinity : Math.max(size, object.maxWidth - 2 * padX);
  for (const paragraph of object.text.split("\n")) {
    if (limit === Infinity || measure(paragraph) <= limit) {
      lines.push(paragraph);
      continue;
    }
    let line = "";
    for (const word of paragraph.split(/(?<=\s)/)) {
      if (line && measure(line + word) > limit) {
        lines.push(line.trimEnd());
        line = "";
      }
      // A word longer than the line breaks between its letters.
      let rest = word;
      while (measure(rest) > limit && rest.length > 1) {
        let cut = rest.length - 1;
        while (cut > 1 && measure(rest.slice(0, cut)) > limit) cut--;
        lines.push(rest.slice(0, cut));
        rest = rest.slice(cut);
      }
      line += rest;
    }
    lines.push(line.trimEnd());
  }
  const width = Math.max(size * 0.5, ...lines.map(measure)) + 2 * padX;
  const height = lines.length * lineHeight + padTop + TEXT_PAD.bottom * size;
  return { lines, width, height, lineHeight, padX, padTop };
}

export const textSize = (object: Text) => {
  const layout = layoutText(object);
  return { width: layout.width, height: layout.height };
};

function shadow(ctx: Ctx, kind: typeof SHADOW | typeof SHADOW_STRONG | null, scale: number): void {
  if (!kind) {
    ctx.shadowColor = "transparent";
    ctx.shadowBlur = 0;
    ctx.shadowOffsetY = 0;
    return;
  }
  ctx.shadowColor = kind.color;
  ctx.shadowBlur = kind.blur * scale;
  ctx.shadowOffsetX = 0;
  ctx.shadowOffsetY = kind.y * scale;
}

function roundRect(ctx: Ctx, r: Rect, radius: number): void {
  const k = Math.max(0, Math.min(radius, r.width / 2, r.height / 2));
  ctx.beginPath();
  ctx.roundRect(r.x, r.y, r.width, r.height, k);
}

/** A smooth path through the points: quadratic curves between the midpoints of the segments. */
function smoothPath(ctx: Ctx, points: readonly Point[]): void {
  ctx.beginPath();
  ctx.moveTo(points[0].x, points[0].y);
  if (points.length === 1) {
    ctx.lineTo(points[0].x + 0.01, points[0].y);
    return;
  }
  if (points.length === 2) {
    ctx.lineTo(points[1].x, points[1].y);
    return;
  }
  for (let i = 1; i < points.length - 1; i++) {
    const mid = { x: (points[i].x + points[i + 1].x) / 2, y: (points[i].y + points[i + 1].y) / 2 };
    ctx.quadraticCurveTo(points[i].x, points[i].y, mid.x, mid.y);
  }
  const last = points[points.length - 1];
  ctx.lineTo(last.x, last.y);
}

/** The arrow as one filled outline: a body that widens from a fine tail to the head. */
export function arrowOutline(from: Point, to: Point, width: number): Point[] {
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  const length = Math.hypot(dx, dy) || 1;
  const ux = dx / length;
  const uy = dy / length;
  const px = -uy;
  const py = ux;
  const { body, headLength, headHalf } = arrowMetrics(width, length);
  const tail = body * 0.25;
  const bx = to.x - ux * headLength;
  const by = to.y - uy * headLength;
  return [
    { x: from.x + px * tail, y: from.y + py * tail },
    { x: bx + (px * body) / 2, y: by + (py * body) / 2 },
    { x: bx + px * headHalf, y: by + py * headHalf },
    { x: to.x, y: to.y },
    { x: bx - px * headHalf, y: by - py * headHalf },
    { x: bx - (px * body) / 2, y: by - (py * body) / 2 },
    { x: from.x - px * tail, y: from.y - py * tail },
  ];
}

function drawText(ctx: Ctx, object: Text, scale: number): void {
  const layout = layoutText(object, ctx);
  const box = { x: object.origin.x, y: object.origin.y, width: layout.width, height: layout.height };
  if (object.background) {
    shadow(ctx, SHADOW_STRONG, scale);
    ctx.fillStyle = cssOf(object.background);
    roundRect(ctx, box, TEXT_RADIUS * object.fontSize);
    ctx.fill();
    shadow(ctx, null, scale);
  } else {
    shadow(ctx, SHADOW, scale);
  }
  ctx.font = textFont(object.fontSize);
  ctx.fillStyle = cssOf(object.color);
  ctx.textBaseline = "middle";
  ctx.textAlign = "left";
  layout.lines.forEach((line, i) => {
    ctx.fillText(line, box.x + layout.padX, box.y + layout.padTop + layout.lineHeight * (i + 0.5));
  });
}

/** Draws one object that is not a redaction, a spotlight or a crop. */
export function drawObject(ctx: Ctx, object: AnnotationObject, scale: number): void {
  ctx.save();
  ctx.lineCap = "round";
  ctx.lineJoin = "round";
  switch (object.kind) {
    case "arrow": {
      const outline = arrowOutline(object.from, object.to, object.stroke.width);
      shadow(ctx, SHADOW, scale);
      ctx.beginPath();
      outline.forEach((p, i) => (i === 0 ? ctx.moveTo(p.x, p.y) : ctx.lineTo(p.x, p.y)));
      ctx.closePath();
      ctx.fillStyle = ctx.strokeStyle = cssOf(object.stroke.color);
      ctx.lineWidth = 1.6 * scale;
      ctx.fill();
      ctx.stroke();
      break;
    }
    case "line":
      shadow(ctx, SHADOW, scale);
      ctx.beginPath();
      ctx.moveTo(object.from.x, object.from.y);
      ctx.lineTo(object.to.x, object.to.y);
      ctx.strokeStyle = cssOf(object.stroke.color);
      ctx.lineWidth = object.stroke.width;
      ctx.stroke();
      break;
    case "rectangle":
    case "ellipse": {
      const r = object.rect;
      const path = () => {
        if (object.kind === "rectangle") roundRect(ctx, r, RECT_RADIUS * scale);
        else {
          ctx.beginPath();
          ctx.ellipse(r.x + r.width / 2, r.y + r.height / 2, r.width / 2, r.height / 2, 0, 0, Math.PI * 2);
        }
      };
      shadow(ctx, SHADOW, scale);
      if (object.fill) {
        path();
        ctx.fillStyle = cssOf(object.fill);
        ctx.fill();
      }
      path();
      ctx.strokeStyle = cssOf(object.stroke.color);
      ctx.lineWidth = object.stroke.width;
      ctx.stroke();
      break;
    }
    case "pen":
      shadow(ctx, SHADOW, scale);
      smoothPath(ctx, object.points);
      ctx.strokeStyle = cssOf(object.stroke.color);
      ctx.lineWidth = object.stroke.width;
      ctx.stroke();
      break;
    case "highlighter":
      // Multiply darkens the text under the ink and never lightens it, so the text stays
      // readable; flat ends make the stroke look like a marker's.
      ctx.globalCompositeOperation = "multiply";
      ctx.globalAlpha = HIGHLIGHTER_ALPHA;
      ctx.lineCap = "butt";
      smoothPath(ctx, object.points);
      ctx.strokeStyle = cssOf(object.color);
      ctx.lineWidth = object.width;
      ctx.stroke();
      break;
    case "text":
      drawText(ctx, object, scale);
      break;
    case "step": {
      const { center, diameter } = object;
      shadow(ctx, SHADOW_STRONG, scale);
      ctx.beginPath();
      ctx.arc(center.x, center.y, diameter / 2 + STEP_RING * scale, 0, Math.PI * 2);
      ctx.fillStyle = inkHex("white");
      ctx.fill();
      shadow(ctx, null, scale);
      ctx.beginPath();
      ctx.arc(center.x, center.y, diameter / 2, 0, Math.PI * 2);
      ctx.fillStyle = cssOf(object.color);
      ctx.fill();
      const size = diameter * (14 / 26);
      ctx.font = `700 ${size}px ${FONT_FAMILY}`;
      ctx.fillStyle = inkHex(contrastInk(object.color));
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      ctx.fillText(String(object.number), center.x, center.y + size * 0.04);
      break;
    }
    default:
      break;
  }
  ctx.restore();
}

/**
 * Draws the spotlights' shared dim over `area` (in image pixels, as the context is): everything
 * outside the union of the regions turns dark, with the darkest dim any of them asks for. The dim
 * is drawn on its own layer first, so overlapping regions cut one hole instead of cancelling
 * each other.
 */
export function drawSpotlights(ctx: Ctx, spots: readonly Spotlight[], area: Rect, scale: number, layer: OffscreenCanvas | HTMLCanvasElement): void {
  if (spots.length === 0) return;
  const w = Math.max(1, Math.ceil(area.width));
  const h = Math.max(1, Math.ceil(area.height));
  if (layer.width !== w) layer.width = w;
  if (layer.height !== h) layer.height = h;
  const g = layer.getContext("2d") as Ctx | null;
  if (!g) return;
  const alpha = Math.max(...spots.map((s) => DIM_ALPHA[s.dim]));
  g.save();
  g.setTransform(1, 0, 0, 1, 0, 0);
  g.clearRect(0, 0, w, h);
  g.fillStyle = `rgba(${DIM_RGB.join(", ")}, ${alpha})`;
  g.fillRect(0, 0, w, h);
  g.globalCompositeOperation = "destination-out";
  g.fillStyle = "#000";
  g.translate(-area.x, -area.y);
  for (const spot of spots) {
    roundRect(g, spot.rect, SPOTLIGHT_RADIUS * scale);
    g.fill();
  }
  g.restore();
  ctx.save();
  ctx.drawImage(layer, area.x, area.y);
  ctx.restore();
}
