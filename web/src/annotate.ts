// Full-screen screenshot annotator: freehand pen, ovals, boxes and arrows.
// Strokes are kept in image coordinates so a screenshot can be re-edited.

export type Tool = 'pen' | 'ellipse' | 'rect' | 'arrow' | 'text';

export interface Stroke {
  tool: Tool;
  color: string;
  width: number;
  points: [number, number][];
  /** Label for the text tool. */
  text?: string;
}

export interface Shot {
  /** Original screenshot as a data URL. */
  base: string;
  strokes: Stroke[];
  /** Flattened screenshot with annotations, or null when unannotated. */
  output: string | null;
  /** The reporter's caption for this screenshot. */
  note: string;
}

/** Encodes a canvas compactly while keeping text legible. */
export function encodeCanvas(canvas: HTMLCanvasElement): string {
  const png = canvas.toDataURL('image/png');
  if (png.length < 2_500_000) return png;
  return canvas.toDataURL('image/jpeg', 0.88);
}

const COLORS = ['#ef4444', '#facc15', '#3b82f6', '#111827'];
const TOOLS: [Tool, string, string][] = [
  ['pen', '✎', 'Draw'],
  ['ellipse', '◯', 'Circle'],
  ['rect', '▭', 'Box'],
  ['arrow', '➚', 'Arrow'],
  ['text', 'T', 'Text'],
];

export function drawStroke(ctx: CanvasRenderingContext2D, stroke: Stroke) {
  const { points } = stroke;
  if (points.length === 0) return;
  ctx.save();
  ctx.strokeStyle = stroke.color;
  ctx.lineWidth = stroke.width;
  ctx.lineCap = 'round';
  ctx.lineJoin = 'round';
  // A light halo keeps marks visible on both dark and light UIs.
  ctx.shadowColor = stroke.color === '#111827' ? 'rgba(255,255,255,0.8)' : 'rgba(0,0,0,0.45)';
  ctx.shadowBlur = stroke.width;
  const [x0, y0] = points[0];
  const [x1, y1] = points[points.length - 1];
  if (stroke.tool === 'text') {
    drawLabel(ctx, stroke, x0, y0);
    ctx.restore();
    return;
  }
  ctx.beginPath();
  switch (stroke.tool) {
    case 'pen':
      ctx.moveTo(x0, y0);
      for (const [x, y] of points.slice(1)) ctx.lineTo(x, y);
      if (points.length === 1) ctx.lineTo(x0 + 0.1, y0);
      break;
    case 'ellipse':
      ctx.ellipse((x0 + x1) / 2, (y0 + y1) / 2, Math.abs(x1 - x0) / 2, Math.abs(y1 - y0) / 2, 0, 0, Math.PI * 2);
      break;
    case 'rect':
      ctx.rect(Math.min(x0, x1), Math.min(y0, y1), Math.abs(x1 - x0), Math.abs(y1 - y0));
      break;
    case 'arrow': {
      const angle = Math.atan2(y1 - y0, x1 - x0);
      const head = stroke.width * 4;
      ctx.moveTo(x0, y0);
      ctx.lineTo(x1, y1);
      ctx.moveTo(x1, y1);
      ctx.lineTo(x1 - head * Math.cos(angle - Math.PI / 7), y1 - head * Math.sin(angle - Math.PI / 7));
      ctx.moveTo(x1, y1);
      ctx.lineTo(x1 - head * Math.cos(angle + Math.PI / 7), y1 - head * Math.sin(angle + Math.PI / 7));
      break;
    }
  }
  ctx.stroke();
  ctx.restore();
}

/** A filled label with readable text, anchored at its top-left corner. */
function drawLabel(ctx: CanvasRenderingContext2D, stroke: Stroke, x: number, y: number) {
  const size = stroke.width * 5;
  const pad = size * 0.35;
  ctx.shadowBlur = stroke.width;
  ctx.font = `600 ${size}px system-ui, -apple-system, "Segoe UI", Roboto, sans-serif`;
  const lines = (stroke.text ?? '').split('\n');
  const width = Math.max(...lines.map((l) => ctx.measureText(l).width)) + pad * 2;
  const height = lines.length * size * 1.25 + pad * 1.2;
  ctx.fillStyle = stroke.color;
  ctx.beginPath();
  if (ctx.roundRect) ctx.roundRect(x, y, width, height, pad);
  else ctx.rect(x, y, width, height);
  ctx.fill();
  ctx.shadowBlur = 0;
  ctx.fillStyle = stroke.color === '#facc15' ? '#111827' : '#ffffff';
  ctx.textBaseline = 'top';
  lines.forEach((line, i) => ctx.fillText(line, x + pad, y + pad * 0.8 + i * size * 1.25));
}

export class Annotator {
  private overlay = document.createElement('div');
  private canvas = document.createElement('canvas');
  private ctx = this.canvas.getContext('2d')!;
  private image = new Image();
  private strokes: Stroke[];
  private current: Stroke | null = null;
  private tool: Tool = 'pen';
  private color = COLORS[0];

  constructor(
    private root: ShadowRoot,
    private shot: Shot,
    private done: (updated: Shot | null) => void,
  ) {
    this.strokes = shot.strokes.map((s) => ({ ...s, points: [...s.points] }));
  }

  async open() {
    this.image.src = this.shot.base;
    await this.image.decode();
    this.canvas.width = this.image.naturalWidth;
    this.canvas.height = this.image.naturalHeight;
    this.overlay.className = 'annotator';
    this.overlay.innerHTML = `
      <div class="toolbar" role="toolbar" aria-label="Annotation tools">
        <div class="group">
          ${TOOLS.map(([t, icon, label]) => `<button type="button" data-tool="${t}" title="${label}" aria-pressed="${t === this.tool}">${icon}<span>${label}</span></button>`).join('')}
        </div>
        <div class="group">
          ${COLORS.map((c) => `<button type="button" class="swatch" data-color="${c}" style="--c:${c}" aria-label="Color ${c}" aria-pressed="${c === this.color}"></button>`).join('')}
        </div>
        <div class="group">
          <button type="button" data-action="undo" title="Undo">↶<span>Undo</span></button>
          <button type="button" data-action="clear" title="Clear all marks">⌫<span>Clear</span></button>
        </div>
        <div class="group end">
          <button type="button" class="secondary" data-action="cancel">Cancel</button>
          <button type="button" class="primary" data-action="done">Done</button>
        </div>
      </div>
      <div class="stage"></div>`;
    this.overlay.querySelector('.stage')!.append(this.canvas);
    this.root.append(this.overlay);
    this.bind();
    this.render();
  }

  private bind() {
    for (const b of this.overlay.querySelectorAll<HTMLButtonElement>('[data-tool]')) {
      b.addEventListener('click', () => {
        this.tool = b.dataset.tool as Tool;
        this.overlay.querySelectorAll('[data-tool]').forEach((x) => x.setAttribute('aria-pressed', String(x === b)));
      });
    }
    for (const b of this.overlay.querySelectorAll<HTMLButtonElement>('[data-color]')) {
      b.addEventListener('click', () => {
        this.color = b.dataset.color!;
        this.overlay.querySelectorAll('[data-color]').forEach((x) => x.setAttribute('aria-pressed', String(x === b)));
      });
    }
    const action = (name: string, fn: () => void) =>
      this.overlay.querySelector(`[data-action=${name}]`)!.addEventListener('click', fn);
    action('undo', () => {
      this.strokes.pop();
      this.render();
    });
    action('clear', () => {
      this.strokes = [];
      this.render();
    });
    action('cancel', () => this.finish(null));
    action('done', () =>
      this.finish({
        ...this.shot,
        strokes: this.strokes,
        output: this.strokes.length ? encodeCanvas(this.canvas) : null,
      }),
    );

    this.canvas.addEventListener('pointerdown', (e) => {
      e.preventDefault();
      if (this.tool === 'text') {
        this.promptText(e);
        return;
      }
      this.canvas.setPointerCapture(e.pointerId);
      const width = Math.max(3, Math.round(Math.max(this.canvas.width, this.canvas.height) / 300));
      this.current = { tool: this.tool, color: this.color, width, points: [this.point(e)] };
      this.render();
    });
    this.canvas.addEventListener('pointermove', (e) => {
      if (!this.current) return;
      const p = this.point(e);
      if (this.current.tool === 'pen') this.current.points.push(p);
      else this.current.points[1] = p;
      this.render();
    });
    const end = () => {
      if (!this.current) return;
      const [a, b] = [this.current.points[0], this.current.points[this.current.points.length - 1]];
      const moved = Math.hypot(b[0] - a[0], b[1] - a[1]) > 4 || this.current.tool === 'pen';
      if (moved) this.strokes.push(this.current);
      this.current = null;
      this.render();
    };
    this.canvas.addEventListener('pointerup', end);
    this.canvas.addEventListener('pointercancel', end);
    this.overlay.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') this.finish(null);
      if ((e.ctrlKey || e.metaKey) && e.key === 'z') {
        this.strokes.pop();
        this.render();
      }
    });
  }

  /** Types a label at the tapped spot; Enter or leaving the field places it. */
  private promptText(e: PointerEvent) {
    this.overlay.querySelector('.text-entry')?.remove();
    const at = this.point(e);
    const color = this.color;
    const width = Math.max(3, Math.round(Math.max(this.canvas.width, this.canvas.height) / 300));
    const stage = this.overlay.querySelector<HTMLElement>('.stage')!;
    const stageRect = stage.getBoundingClientRect();
    const input = document.createElement('input');
    input.className = 'text-entry';
    input.placeholder = 'Type a label, Enter to place';
    input.maxLength = 200;
    input.style.left = `${Math.min(e.clientX - stageRect.left, stageRect.width - 220)}px`;
    input.style.top = `${e.clientY - stageRect.top}px`;
    input.style.borderColor = color;
    let done = false;
    const commit = () => {
      if (done) return;
      done = true;
      const text = input.value.trim();
      input.remove();
      if (text) {
        this.strokes.push({ tool: 'text', color, width, points: [at], text });
        this.render();
      }
    };
    input.addEventListener('keydown', (k) => {
      k.stopPropagation();
      if (k.key === 'Enter') commit();
      if (k.key === 'Escape') {
        done = true;
        input.remove();
      }
    });
    input.addEventListener('blur', commit);
    stage.append(input);
    setTimeout(() => input.focus(), 0);
  }

  private point(e: PointerEvent): [number, number] {
    const rect = this.canvas.getBoundingClientRect();
    return [
      ((e.clientX - rect.left) / rect.width) * this.canvas.width,
      ((e.clientY - rect.top) / rect.height) * this.canvas.height,
    ];
  }

  private render() {
    this.ctx.drawImage(this.image, 0, 0);
    for (const stroke of this.strokes) drawStroke(this.ctx, stroke);
    if (this.current) drawStroke(this.ctx, this.current);
  }

  private finish(result: Shot | null) {
    this.overlay.remove();
    this.done(result);
  }
}
