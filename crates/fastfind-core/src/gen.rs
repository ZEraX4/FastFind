//! Deterministic sample-data generator for tests and benchmarks.
//!
//! Produces a directory tree of text, CSV, JSON, Markdown, HTML, XML, DOCX, XLSX, PPTX and PDF
//! files filled with pseudo-random vocabulary. Known "needle" words are planted at controlled
//! rates so benchmarks can issue queries with predictable selectivity:
//!
//! | word         | appears in                 |
//! |--------------|----------------------------|
//! | `fastfind`   | every file                 |
//! | `invoice`    | ~10 % of files             |
//! | `customer`   | ~5 % of files              |
//! | `quarterly`  | ~1 % of files              |
//! | `zeppelin`   | ~0.1 % of files            |
//! | `doc{N}`     | exactly file N             |

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// Realistic mix of formats and sizes.
    Mixed,
    /// Many small text files (1–8 KB).
    Small,
    /// Few large text files (`large_mb` each).
    LargeText,
    /// Office documents only.
    Office,
    /// PDFs only.
    Pdf,
}

impl Profile {
    pub fn parse(s: &str) -> Option<Profile> {
        Some(match s {
            "mixed" => Profile::Mixed,
            "small" => Profile::Small,
            "large-text" | "large" => Profile::LargeText,
            "office" => Profile::Office,
            "pdf" => Profile::Pdf,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct GenOptions {
    pub files: usize,
    pub profile: Profile,
    pub seed: u64,
    pub large_mb: u64,
}

impl Default for GenOptions {
    fn default() -> Self {
        Self { files: 1000, profile: Profile::Mixed, seed: 42, large_mb: 50 }
    }
}

#[derive(Debug, Default, Clone)]
pub struct GenStats {
    pub files: usize,
    pub bytes: u64,
    /// Files containing each needle.
    pub invoice: usize,
    pub customer: usize,
    pub quarterly: usize,
    pub zeppelin: usize,
}

/// xorshift64* — tiny, fast, deterministic.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }
    pub fn chance(&mut self, per_mille: u64) -> bool {
        self.below(1000) < per_mille
    }
}

const SYLLABLES: &[&str] = &[
    "ka", "lo", "mi", "ne", "ra", "su", "ti", "vo", "ba", "de", "fi", "go", "hu", "ja", "ke", "li",
    "mo", "nu", "pa", "qui", "re", "sa", "to", "ul", "ve", "wa", "xe", "yo", "za", "an", "el", "or",
];

/// A fixed vocabulary of ~4000 pronounceable pseudo-words (plus some English).
pub fn vocabulary() -> Vec<String> {
    let mut v: Vec<String> = [
        "report", "project", "budget", "meeting", "summary", "analysis", "design", "system", "review",
        "contract", "schedule", "delivery", "service", "product", "market", "strategy", "release",
        "network", "security", "database", "performance", "latency", "throughput", "storage",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut r = Rng::new(7);
    while v.len() < 4000 {
        let n = 2 + r.below(3) as usize;
        let w: String = (0..n).map(|_| SYLLABLES[r.below(SYLLABLES.len() as u64) as usize]).collect();
        v.push(w);
    }
    v
}

struct Content {
    paragraphs: Vec<String>,
}

fn sentence(r: &mut Rng, vocab: &[String], words: usize) -> String {
    let mut s = String::new();
    for i in 0..words {
        if i > 0 {
            s.push(' ');
        }
        // Zipf-ish: favour the start of the vocabulary.
        let idx = ((r.below(1000) * r.below(1000)) / 1000) as usize % vocab.len();
        s.push_str(&vocab[idx]);
    }
    s.push('.');
    s
}

fn content(r: &mut Rng, vocab: &[String], n: usize, target_bytes: usize, stats: &mut GenStats) -> Content {
    let mut paragraphs = Vec::new();
    let mut size = 0;
    while size < target_bytes {
        let words = 8 + r.below(24) as usize;
        let p = sentence(r, vocab, words);
        size += p.len() + 1;
        paragraphs.push(p);
    }
    let mut plant = |word: &str, per_mille: u64, count: &mut usize, r: &mut Rng| {
        if r.chance(per_mille) {
            let i = r.below(paragraphs.len() as u64) as usize;
            paragraphs[i].push_str(&format!(" The {word} total was approved."));
            *count += 1;
        }
    };
    plant("invoice", 100, &mut stats.invoice, r);
    plant("customer", 50, &mut stats.customer, r);
    plant("quarterly", 10, &mut stats.quarterly, r);
    plant("zeppelin", 1, &mut stats.zeppelin, r);
    paragraphs.insert(0, format!("fastfind sample document doc{n}"));
    Content { paragraphs }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn zip_file(path: &Path, parts: &[(&str, String)]) -> io::Result<()> {
    let mut z = ZipWriter::new(BufWriter::new(File::create(path)?));
    let opt = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, body) in parts {
        z.start_file(*name, opt).map_err(io::Error::other)?;
        z.write_all(body.as_bytes())?;
    }
    z.finish().map_err(io::Error::other)?;
    Ok(())
}

const REL_DOC: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

pub fn write_docx(path: &Path, title: &str, paragraphs: &[String]) -> io::Result<()> {
    let body: String = paragraphs
        .iter()
        .map(|p| format!("<w:p><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>", xml_escape(p)))
        .collect();
    zip_file(path, &[
        ("[Content_Types].xml", r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/></Types>"#.into()),
        ("_rels/.rels", format!(r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL_DOC}/officeDocument" Target="word/document.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/></Relationships>"#)),
        ("word/document.xml", format!(r#"<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}</w:body></w:document>"#)),
        ("docProps/core.xml", format!(r#"<?xml version="1.0" encoding="UTF-8"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>{}</dc:title><dc:creator>FastFind Generator</dc:creator></cp:coreProperties>"#, xml_escape(title))),
    ])
}

/// `sheets`: (name, rows of cells).
pub fn write_xlsx(path: &Path, sheets: &[(String, Vec<Vec<String>>)]) -> io::Result<()> {
    let mut parts: Vec<(String, String)> = Vec::new();
    let mut wb_sheets = String::new();
    let mut wb_rels = String::new();
    let mut overrides = String::new();
    for (i, (name, rows)) in sheets.iter().enumerate() {
        let n = i + 1;
        wb_sheets.push_str(&format!(r#"<sheet name="{}" sheetId="{n}" r:id="rId{n}"/>"#, xml_escape(name)));
        wb_rels.push_str(&format!(r#"<Relationship Id="rId{n}" Type="{REL_DOC}/worksheet" Target="worksheets/sheet{n}.xml"/>"#));
        overrides.push_str(&format!(r#"<Override PartName="/xl/worksheets/sheet{n}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#));
        let mut data = String::new();
        for (ri, row) in rows.iter().enumerate() {
            data.push_str(&format!(r#"<row r="{}">"#, ri + 1));
            for (ci, cell) in row.iter().enumerate() {
                let r = format!("{}{}", crate::parsers::locmap::column_letters(ci as u32), ri + 1);
                if cell.parse::<f64>().is_ok() {
                    data.push_str(&format!(r#"<c r="{r}"><v>{cell}</v></c>"#));
                } else {
                    data.push_str(&format!(r#"<c r="{r}" t="inlineStr"><is><t>{}</t></is></c>"#, xml_escape(cell)));
                }
            }
            data.push_str("</row>");
        }
        parts.push((format!("xl/worksheets/sheet{n}.xml"), format!(r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>{data}</sheetData></worksheet>"#)));
    }
    let mut all: Vec<(&str, String)> = vec![
        ("[Content_Types].xml", format!(r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>{overrides}</Types>"#)),
        ("_rels/.rels", format!(r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL_DOC}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#)),
        ("xl/workbook.xml", format!(r#"<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="{REL_DOC}"><sheets>{wb_sheets}</sheets></workbook>"#)),
        ("xl/_rels/workbook.xml.rels", format!(r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{wb_rels}</Relationships>"#)),
    ];
    for (n, b) in &parts {
        all.push((n.as_str(), b.clone()));
    }
    zip_file(path, &all)
}

/// `slides`: (title, body lines).
pub fn write_pptx(path: &Path, slides: &[(String, Vec<String>)]) -> io::Result<()> {
    let mut ids = String::new();
    let mut rels = String::new();
    let mut overrides = String::new();
    let mut parts: Vec<(String, String)> = Vec::new();
    let ns = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;
    for (i, (title, body)) in slides.iter().enumerate() {
        let n = i + 1;
        ids.push_str(&format!(r#"<p:sldId id="{}" r:id="rId{n}"/>"#, 255 + n));
        rels.push_str(&format!(r#"<Relationship Id="rId{n}" Type="{REL_DOC}/slide" Target="slides/slide{n}.xml"/>"#));
        overrides.push_str(&format!(r#"<Override PartName="/ppt/slides/slide{n}.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>"#));
        let paras: String = body.iter().map(|l| format!("<a:p><a:r><a:t>{}</a:t></a:r></a:p>", xml_escape(l))).collect();
        parts.push((format!("ppt/slides/slide{n}.xml"), format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><p:sld {ns}><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="Title"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>{}</a:t></a:r></a:p></p:txBody></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="Body"/><p:cNvSpPr/><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr><p:txBody>{paras}</p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
            xml_escape(title)
        )));
    }
    let mut all: Vec<(&str, String)> = vec![
        ("[Content_Types].xml", format!(r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>{overrides}</Types>"#)),
        ("_rels/.rels", format!(r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL_DOC}/officeDocument" Target="ppt/presentation.xml"/></Relationships>"#)),
        ("ppt/presentation.xml", format!(r#"<?xml version="1.0" encoding="UTF-8"?><p:presentation {ns} xmlns:r="{REL_DOC}"><p:sldIdLst>{ids}</p:sldIdLst></p:presentation>"#)),
        ("ppt/_rels/presentation.xml.rels", format!(r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels}</Relationships>"#)),
    ];
    for (n, b) in &parts {
        all.push((n.as_str(), b.clone()));
    }
    zip_file(path, &all)
}

fn pdf_escape(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii() && !c.is_ascii_control())
        .flat_map(|c| match c {
            '(' | ')' | '\\' => vec!['\\', c],
            c => vec![c],
        })
        .collect()
}

/// Minimal valid PDF 1.4: Helvetica text, one content stream per page, correct xref table.
pub fn write_pdf(path: &Path, title: &str, pages: &[Vec<String>]) -> io::Result<()> {
    let mut objs: Vec<String> = Vec::new();
    let n_pages = pages.len().max(1);
    // 1 catalog, 2 pages, 3 font, 4 info, then (page, content) pairs.
    objs.push("<< /Type /Catalog /Pages 2 0 R >>".into());
    let kids: String = (0..n_pages).map(|i| format!("{} 0 R ", 5 + i * 2)).collect();
    objs.push(format!("<< /Type /Pages /Kids [{kids}] /Count {n_pages} >>"));
    objs.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into());
    objs.push(format!("<< /Title ({}) /Producer (FastFind generator) >>", pdf_escape(title)));
    for (i, lines) in pages.iter().enumerate() {
        let mut stream = String::from("BT /F1 11 Tf 14 TL 50 800 Td\n");
        for l in lines.iter().take(55) {
            stream.push_str(&format!("({}) Tj T*\n", pdf_escape(l)));
        }
        stream.push_str("ET");
        objs.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>", 6 + i * 2));
        objs.push(format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()));
    }
    let mut out: Vec<u8> = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R /Info 4 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    fs::write(path, out)
}

/// A "scanned" PDF: every page is a single greyscale image and there is no text layer.
pub fn write_image_pdf(path: &Path, pages: usize) -> io::Result<()> {
    let (w, h) = (64u32, 64u32);
    let pixels: Vec<u8> = (0..(w * h) as usize).map(|i| if (i / w as usize + i % w as usize) % 8 < 4 { 0 } else { 255 }).collect();
    let page = ScanPage { width: w, height: h, gray: &pixels };
    write_scanned_pdf(path, &vec![page; pages])
}

/// One page image for [`write_scanned_pdf`]: 8-bit greyscale, row-major.
#[derive(Clone, Copy)]
pub struct ScanPage<'a> {
    pub width: u32,
    pub height: u32,
    pub gray: &'a [u8],
}

/// A scanned-document PDF: each page is one full-page greyscale image (Flate-compressed) and
/// there is no text layer — exactly what a document scanner produces.
pub fn write_scanned_pdf(path: &Path, pages: &[ScanPage]) -> io::Result<()> {
    let mut objs: Vec<Vec<u8>> = Vec::new();
    objs.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    let n = pages.len();
    // Objects: 1 catalog, 2 pages, then per page: image, page, content.
    let kids: String = (0..n).map(|i| format!("{} 0 R ", 4 + i * 3)).collect();
    objs.push(format!("<< /Type /Pages /Kids [{kids}] /Count {n} >>").into_bytes());
    for (i, p) in pages.iter().enumerate() {
        let mut z = flate2_compress(p.gray)?;
        let mut img = format!(
            "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>\nstream\n",
            p.width, p.height, z.len()
        )
        .into_bytes();
        img.append(&mut z);
        img.extend_from_slice(b"\nendstream");
        objs.push(img);
        let content = "q 595 0 0 842 0 0 cm /Im1 Do Q";
        objs.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /XObject << /Im1 {} 0 R >> >> /Contents {} 0 R >>", 3 + i * 3, 5 + i * 3).into_bytes());
        objs.push(format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()).into_bytes());
    }
    let mut out: Vec<u8> = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    fs::write(path, out)
}

fn flate2_compress(data: &[u8]) -> io::Result<Vec<u8>> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data)?;
    e.finish()
}

fn subdir(root: &Path, i: usize) -> PathBuf {
    root.join(format!("d{:03}", i / 1000)).join(format!("s{:02}", (i / 100) % 10))
}

/// Generate `opts.files` files under `root`.
pub fn generate(root: &Path, opts: &GenOptions) -> io::Result<GenStats> {
    let vocab = vocabulary();
    let mut r = Rng::new(opts.seed);
    let mut stats = GenStats::default();
    for i in 0..opts.files {
        let dir = subdir(root, i);
        fs::create_dir_all(&dir)?;
        let pick = r.below(100);
        let (kind, target) = match opts.profile {
            Profile::Small => ("txt", 1024 + r.below(7 * 1024) as usize),
            Profile::LargeText => ("log", (opts.large_mb as usize) << 20),
            Profile::Office => (["docx", "xlsx", "pptx"][(pick % 3) as usize], 4096 + r.below(60_000) as usize),
            Profile::Pdf => ("pdf", 2048 + r.below(40_000) as usize),
            Profile::Mixed => match pick {
                0..=29 => ("txt", 512 + r.below(16 * 1024) as usize),
                30..=39 => ("md", 1024 + r.below(8 * 1024) as usize),
                40..=46 => ("csv", 2048 + r.below(32 * 1024) as usize),
                47..=53 => ("json", 1024 + r.below(16 * 1024) as usize),
                54..=59 => ("html", 2048 + r.below(16 * 1024) as usize),
                60..=63 => ("xml", 1024 + r.below(8 * 1024) as usize),
                64..=66 => ("log", 64 * 1024 + r.below(1 << 20) as usize),
                67..=77 => ("docx", 4096 + r.below(40_000) as usize),
                78..=84 => ("xlsx", 2048 + r.below(20_000) as usize),
                85..=89 => ("pptx", 2048 + r.below(12_000) as usize),
                _ => ("pdf", 2048 + r.below(30_000) as usize),
            },
        };
        let c = content(&mut r, &vocab, i, target, &mut stats);
        let path = dir.join(format!("file{i:07}.{kind}"));
        let title = format!("Sample {i}");
        match kind {
            "docx" => write_docx(&path, &title, &c.paragraphs)?,
            "xlsx" => {
                let rows: Vec<Vec<String>> = c.paragraphs.iter().map(|p| p.split(' ').take(6).map(String::from).collect()).collect();
                write_xlsx(&path, &[("Data".into(), rows)])?
            }
            "pptx" => {
                let slides: Vec<(String, Vec<String>)> = c.paragraphs.chunks(4).enumerate().map(|(k, ch)| (format!("Slide {k}"), ch.to_vec())).collect();
                write_pptx(&path, &slides)?
            }
            "pdf" => {
                let lines: Vec<String> = c.paragraphs.iter().flat_map(|p| p.as_bytes().chunks(90).map(|b| String::from_utf8_lossy(b).into_owned()).collect::<Vec<_>>()).collect();
                let pages: Vec<Vec<String>> = lines.chunks(50).map(|c| c.to_vec()).collect();
                write_pdf(&path, &title, &pages)?
            }
            "csv" => {
                let mut w = BufWriter::new(File::create(&path)?);
                writeln!(w, "id,name,amount,notes")?;
                for (k, p) in c.paragraphs.iter().enumerate() {
                    writeln!(w, "{k},{},{},\"{}\"", p.split(' ').next().unwrap_or(""), k * 17 % 1000, p.replace('"', "'"))?;
                }
            }
            "json" => {
                let v: Vec<serde_json::Value> = c.paragraphs.iter().enumerate().map(|(k, p)| serde_json::json!({ "id": k, "text": p })).collect();
                fs::write(&path, serde_json::to_vec_pretty(&serde_json::json!({ "title": title, "items": v }))?)?;
            }
            "html" => {
                let body: String = c.paragraphs.iter().map(|p| format!("<p>{}</p>\n", xml_escape(p))).collect();
                fs::write(&path, format!("<!DOCTYPE html><html><head><title>{title}</title><style>p{{margin:0}}</style></head><body>{body}</body></html>"))?;
            }
            "xml" => {
                let body: String = c.paragraphs.iter().map(|p| format!("  <entry>{}</entry>\n", xml_escape(p))).collect();
                fs::write(&path, format!("<?xml version=\"1.0\"?>\n<entries title=\"{title}\">\n{body}</entries>\n"))?;
            }
            "md" => fs::write(&path, format!("# {title}\n\n{}\n", c.paragraphs.join("\n\n")))?,
            _ => {
                let mut w = BufWriter::new(File::create(&path)?);
                for p in &c.paragraphs {
                    writeln!(w, "{p}")?;
                }
            }
        }
        stats.bytes += fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        stats.files += 1;
    }
    Ok(stats)
}
