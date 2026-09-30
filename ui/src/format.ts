// Formatting helpers (sizes, dates, counts, kind labels).

const nf = new Intl.NumberFormat();

export function num(n: number): string {
  return nf.format(n);
}

export function bytes(b: number): string {
  if (b < 1024) return `${b} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = b / 1024;
  let u = 0;
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024;
    u++;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[u]}`;
}

const df = new Intl.DateTimeFormat(undefined, { year: "numeric", month: "short", day: "numeric" });
const dtf = new Intl.DateTimeFormat(undefined, { year: "numeric", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });

export function date(unixSecs: number): string {
  return unixSecs > 0 ? df.format(new Date(unixSecs * 1000)) : "—";
}

export function dateTime(unixSecs: number): string {
  return unixSecs > 0 ? dtf.format(new Date(unixSecs * 1000)) : "—";
}

export function ago(unixSecs: number | null, now = Date.now() / 1000): string {
  if (!unixSecs) return "never";
  const d = Math.max(0, now - unixSecs);
  if (d < 45) return "just now";
  if (d < 3600) return `${Math.round(d / 60)} min ago`;
  if (d < 86400) return `${Math.round(d / 3600)} h ago`;
  return `${Math.round(d / 86400)} d ago`;
}

export function duration(ms: number): string {
  return ms < 1000 ? `${ms < 10 ? ms.toFixed(1) : Math.round(ms)} ms` : `${(ms / 1000).toFixed(2)} s`;
}

/** Short badge label and colour class for a result. */
export function kindBadge(kind: string, ext: string): { label: string; cls: string } {
  const e = ext.toLowerCase();
  const map: Record<string, [string, string]> = {
    pdf: ["PDF", "k-pdf"],
    word: [e === "rtf" ? "RTF" : e.startsWith("od") ? "ODT" : "DOC", "k-word"],
    spreadsheet: [e === "csv" ? "CSV" : e.startsWith("od") ? "ODS" : "XLS", "k-sheet"],
    presentation: [e.startsWith("od") ? "ODP" : "PPT", "k-slides"],
    web: ["HTML", "k-web"],
    data: [(e || "DATA").slice(0, 4).toUpperCase(), "k-data"],
    code: [(e || "CODE").slice(0, 4).toUpperCase(), "k-code"],
    text: [(e || "TXT").slice(0, 4).toUpperCase(), "k-text"],
    ebook: ["EPUB", "k-ebook"],
    image: ["IMG", "k-image"],
  };
  const [label, cls] = map[kind] ?? [(e || "FILE").slice(0, 4).toUpperCase(), "k-other"];
  return { label, cls };
}

export const KIND_OPTIONS: [string, string][] = [
  ["pdf", "PDF"],
  ["word", "Documents"],
  ["spreadsheet", "Spreadsheets"],
  ["presentation", "Presentations"],
  ["text", "Text & notes"],
  ["code", "Source code"],
  ["data", "Data & config"],
  ["web", "Web pages"],
  ["ebook", "E-books"],
  ["image", "Images"],
  ["other", "Other"],
];
