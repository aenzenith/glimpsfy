// Where the bars, the strip and the size label go around a selection (the overlay prototype's
// `layout` and `placeTips`, DESIGN.md "Annotation bars"). Tools run along the bottom edge and
// actions stand on the right edge, so the hand travels the shortest way from where the drag
// ended. A bar never jumps to the far side of the selection: it moves inside first.

export type Rect = { x: number; y: number; w: number; h: number };
export type Size = { w: number; h: number };

/** 8 px selection to bar and bar to strip (`--gap-selection`), 40 px bars, a 36 px strip. */
const G = 8;
const B = 40;
const S = 36;
/** Distance kept from the screen edge. */
const E = 8;
/** A tooltip's height. */
const TIP = 26;

export type Placement = {
  tSide: "below" | "inside" | "above";
  aSide: "right" | "inside" | "left";
  tools: { x: number; y: number };
  actions: { x: number; y: number };
  strip: { x: number; y: number } | null;
  size: { x: number; y: number };
  toolTips: "above" | "below";
  actionTips: "left" | "right";
};

const clamp = (v: number, a: number, b: number) => Math.max(a, Math.min(b, v));

/**
 * @param sel the selection in the overlay's points
 * @param view the overlay's size
 * @param tools the tool bar's size; `actions` the action bar's
 * @param strip the strip's size and the centre of the active tool inside the tool bar, if shown
 * @param widestActionTip the widest tooltip of the action bar
 */
export function place(
  sel: Rect,
  view: Size,
  tools: Size,
  actions: Size,
  strip: (Size & { center: number }) | null,
  widestActionTip: number,
): Placement {
  const { w: W, h: H } = view;
  const tw = tools.w;
  const { w: aw, h: ah } = actions;
  const need = G + B + G + S;
  const below = sel.y + sel.h + need + E <= H;
  const right = sel.x + sel.w + G + aw + E <= W;
  const tSide = below ? "below" : sel.h >= need + G ? "inside" : sel.y - need - E >= 0 ? "above" : "inside";
  const aSide = right ? "right" : sel.w >= aw + 2 * G ? "inside" : sel.x - G - aw - E >= 0 ? "left" : "inside";
  const ax = aSide === "right" ? sel.x + sel.w + G : aSide === "left" ? sel.x - G - aw : sel.x + sel.w - G - aw;
  let ay = tSide === "above" ? sel.y : aSide === "inside" ? sel.y + sel.h - G - ah : sel.y + sel.h - ah;
  ay = clamp(ay, E, H - E - ah);
  const tRight = tSide === "inside" ? (aSide === "inside" ? ax - G : sel.x + sel.w - G) : sel.x + sel.w;
  const tx = clamp(tRight - tw, E, W - E - tw);
  const ty = tSide === "below" ? sel.y + sel.h + G : tSide === "above" ? sel.y - G - B : sel.y + sel.h - G - B;

  let stripAt: Placement["strip"] = null;
  if (strip) {
    const cx = tx + strip.center;
    const sx = clamp(clamp(cx - strip.w / 2, tx, tx + tw - strip.w), E, W - E - strip.w);
    const sy = tSide === "below" ? ty + B + G : ty - G - S;
    stripAt = { x: sx, y: sy };
  }

  const top = sel.y - 6 - 24 >= E;
  const size = top ? { x: sel.x, y: sel.y - 6 - 24 } : { x: sel.x + 6, y: sel.y + 6 };

  // Tool tips open on the side away from the strip, and on the strip's side only at the screen
  // edge. Action tips open outward, over the dim, and inward only at the screen edge.
  const room = TIP + 8 + E;
  const pref = tSide === "below" ? "above" : "below";
  const fits = { above: ty - room >= 0, below: ty + B + room <= H };
  const toolTips = fits[pref] ? pref : pref === "above" ? "below" : "above";
  const fitsRight = ax + aw + 8 + widestActionTip + E <= W;
  const fitsLeft = ax - 8 - widestActionTip - E >= 0;
  const actionTips = aSide === "left" ? (fitsLeft ? "left" : "right") : fitsRight ? "right" : "left";

  return {
    tSide,
    aSide,
    tools: { x: Math.round(tx), y: Math.round(ty) },
    actions: { x: Math.round(ax), y: Math.round(ay) },
    strip: stripAt && { x: Math.round(stripAt.x), y: Math.round(stripAt.y) },
    size: { x: Math.round(size.x), y: Math.round(size.y) },
    toolTips,
    actionTips,
  };
}

/** The loupe sits 20 px off the pointer and flips to the other side near an edge. */
export function placeLoupe(x: number, y: number, view: Size): { x: number; y: number } {
  const w = 148;
  const h = 176;
  let lx = x + 20;
  let ly = y + 20;
  if (lx + w > view.w - 8) lx = x - 20 - w;
  if (ly + h > view.h - 8) ly = y - 20 - h;
  return { x: Math.round(lx), y: Math.round(ly) };
}
