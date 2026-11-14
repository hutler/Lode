// Made-up images for the mock backend's image diff: a harbor map by day (before) and by night
// with the crane moved (after), as SVG data URLs so they render without files.

function svg(body: string, w = 640, h = 400): string {
  const doc = `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}">${body}</svg>`;
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(doc)}`;
}

function harbor(night: boolean): string {
  const sky = night ? "#16213f" : "#9fd3f2";
  const water = night ? "#0d2a4a" : "#3d8fd1";
  const land = night ? "#2f3b2a" : "#7fb06a";
  const pier = night ? "#5a4632" : "#a07a52";
  const crane = night ? "#c9a33a" : "#e0b33c";
  const craneX = night ? 360 : 300;
  const lights = night
    ? [120, 180, 240, 420, 480, 540]
        .map((x) => `<circle cx="${x}" cy="222" r="5" fill="#ffd66b"/><circle cx="${x}" cy="222" r="14" fill="#ffd66b" opacity="0.25"/>`)
        .join("")
    : "";
  const sunOrMoon = night
    ? `<circle cx="540" cy="70" r="26" fill="#f4f1de"/><circle cx="552" cy="62" r="24" fill="${sky}"/>`
    : `<circle cx="540" cy="70" r="30" fill="#ffe27a"/>`;
  return svg(`
    <rect width="640" height="400" fill="${sky}"/>
    ${sunOrMoon}
    <rect y="200" width="640" height="200" fill="${water}"/>
    <path d="M0 200 L0 150 Q80 130 160 150 L200 200 Z" fill="${land}"/>
    <path d="M640 200 L640 140 Q560 120 480 150 L450 200 Z" fill="${land}"/>
    <rect x="100" y="215" width="460" height="16" fill="${pier}"/>
    <rect x="${craneX}" y="90" width="10" height="125" fill="${crane}"/>
    <rect x="${craneX - 60}" y="90" width="130" height="8" fill="${crane}"/>
    <line x1="${craneX + 60}" y1="98" x2="${craneX + 60}" y2="150" stroke="${crane}" stroke-width="2"/>
    <rect x="${craneX + 50}" y="150" width="20" height="14" fill="#8a4b2f"/>
    <path d="M200 280 l50 0 l-10 18 l-30 0 z" fill="${night ? "#c7c7c7" : "#ffffff"}"/>
    <line x1="225" y1="280" x2="225" y2="245" stroke="#6b4b2a" stroke-width="2"/>
    <path d="M227 247 l18 28 l-18 0 z" fill="${night ? "#d8d0b8" : "#f5efe0"}"/>
    ${lights}
  `);
}

/** The demo image of `path` before or after the change. */
export function demoImage(night: boolean): string {
  return harbor(night);
}
