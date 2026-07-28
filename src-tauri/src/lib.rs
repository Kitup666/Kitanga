use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::fs;
use std::sync::{Mutex, OnceLock};

use base64::Engine;
use ::image::DynamicImage;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zip::write::SimpleFileOptions;

fn thumb_cache() -> &'static Mutex<HashMap<(String, u32), String>> {
    static CACHE: OnceLock<Mutex<HashMap<(String, u32), String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn file_cache() -> &'static Mutex<HashMap<String, Vec<u8>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Vec<u8>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn read_file_cached(path: &str) -> Result<Vec<u8>, String> {
    {
        let cache = file_cache().lock().unwrap();
        if let Some(bytes) = cache.get(path) {
            return Ok(bytes.clone());
        }
    }
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    {
        let mut cache = file_cache().lock().unwrap();
        cache.insert(path.to_string(), bytes.clone());
    }
    Ok(bytes)
}

const SUPPORTED: &[&str] = &["jpg","jpeg","png","webp","bmp","gif","tiff","tif"];

fn is_supported(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str())
        .map(|e| SUPPORTED.contains(&e.to_lowercase().as_str())).unwrap_or(false)
}

#[tauri::command]
fn scan_directory(path: String) -> Result<Vec<String>, String> {
    let dir = Path::new(&path);
    if !dir.is_dir() { return Err("Not a directory".into()); }
    let mut entries: Vec<_> = fs::read_dir(dir).map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file() && is_supported(&e.path()))
        .collect();
    entries.sort_by_key(|e| e.file_name());
    Ok(entries.into_iter().filter_map(|e| e.path().to_str().map(String::from)).collect())
}

#[tauri::command]
fn get_thumbnail(path: String, max_size: u32) -> Result<String, String> {
    {
        let cache = thumb_cache().lock().unwrap();
        if let Some(b64) = cache.get(&(path.clone(), max_size)) {
            return Ok(b64.clone());
        }
    }
    let bytes = read_file_cached(&path)?;
    let img = ::image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    let thumb = if img.width() > max_size || img.height() > max_size {
        img.thumbnail(max_size, max_size)
    } else { img };
    let mut buf = std::io::Cursor::new(Vec::new());
    thumb.write_to(&mut buf, ::image::ImageFormat::Jpeg).map_err(|e| e.to_string())?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(buf.into_inner());
    {
        let mut cache = thumb_cache().lock().unwrap();
        cache.insert((path, max_size), b64.clone());
    }
    Ok(b64)
}

fn parse_hex_color(hex: &str) -> Result<[u8; 3], String> {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 { return Err("Invalid color".into()); }
    let r = u8::from_str_radix(&hex[0..2], 16).map_err(|_| "Invalid color".to_string())?;
    let g = u8::from_str_radix(&hex[2..4], 16).map_err(|_| "Invalid color".to_string())?;
    let b = u8::from_str_radix(&hex[4..6], 16).map_err(|_| "Invalid color".to_string())?;
    Ok([r, g, b])
}

#[tauri::command]
fn export_images(
    images: Vec<String>,
    format: String,
    title: String,
    outdir: String,
    stitch_mode: String,
    uniform_width: u32,
    border_width: u32,
    border_color: String,
) -> Result<String, String> {
    if images.is_empty() { return Err("No images to process".into()); }
    let ext = match format.as_str() {
        "PDF" => "pdf", "EPUB" => "epub",
        "LongPNG" => "png", "LongJPG" => "jpg", "LongWEBP" => "webp",
        _ => return Err("Unsupported format".into()),
    };
    let safe: String = title.chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-' || *c == '_').collect();
    let safe = if safe.trim().is_empty() { "Comic".to_string() } else { safe.trim().to_string() };
    let out_path = PathBuf::from(&outdir).join(format!("{}.{}", safe, ext));
    fs::create_dir_all(Path::new(&outdir)).map_err(|e| format!("Cannot create output dir: {}", e))?;
    let border_rgb = parse_hex_color(&border_color)?;
    match format.as_str() {
        "PDF" => export_pdf(&images, &out_path),
        "EPUB" => export_epub(&images, &out_path, &safe),
        "LongPNG" => export_long_image(&images, &out_path, ::image::ImageFormat::Png, &stitch_mode, uniform_width, border_width, border_rgb),
        "LongJPG" => export_long_image(&images, &out_path, ::image::ImageFormat::Jpeg, &stitch_mode, uniform_width, border_width, border_rgb),
        "LongWEBP" => export_long_image(&images, &out_path, ::image::ImageFormat::WebP, &stitch_mode, uniform_width, border_width, border_rgb),
        _ => unreachable!(),
    }?;
    Ok(out_path.to_string_lossy().to_string())
}

// ─── Manual PDF generator (no external PDF lib) ───────────────────────────────

fn img_to_jpeg_bytes(img: &DynamicImage) -> Result<Vec<u8>, String> {
    let rgb = img.to_rgb8();
    let mut buf = std::io::Cursor::new(Vec::new());
    ::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 95)
        .encode(&rgb, rgb.width(), rgb.height(), ::image::ColorType::Rgb8.into())
        .map_err(|e| e.to_string())?;
    Ok(buf.into_inner())
}

fn generate_pdf(images: &[String]) -> Result<Vec<u8>, String> {
    struct PageData { w: u32, h: u32, jpeg: Vec<u8> }
    let mut pages = Vec::new();
    for path in images {
        let img = ::image::open(path).map_err(|e| e.to_string())?;
        let jpeg = img_to_jpeg_bytes(&img)?;
        pages.push(PageData { w: img.width(), h: img.height(), jpeg });
    }

    // Object layout per page: Page Dict, Content stream, Image XObject
    // Total: 2 (Catalog + Pages) + N*3
    let np = pages.len();
    let total = 2 + np * 3;
    let mut objs: Vec<Vec<u8>> = vec![Vec::new(); total + 1]; // 1-indexed

    for (i, p) in pages.iter().enumerate() {
        let base = 3 + i * 3;

        // Image XObject: dict + stream (base+2)
        let img_dict = format!(
            "<< /Type /XObject /Subtype /Image /Width {} /Height {} \
             /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>\n",
            p.w, p.h, p.jpeg.len()
        );
        let mut img_obj = img_dict.into_bytes();
        img_obj.extend_from_slice(b"stream\n");
        img_obj.extend_from_slice(&p.jpeg);
        img_obj.extend_from_slice(b"\nendstream");
        objs[base + 2] = img_obj;

        // Content stream (base+1)
        let content_body = format!("q\n{} 0 0 {} 0 0 cm\n/Im{} Do\nQ\n", p.w, p.h, base + 2);
        let mut content_obj = format!("<< /Length {} >>\n", content_body.len()).into_bytes();
        content_obj.extend_from_slice(b"stream\n");
        content_obj.extend_from_slice(content_body.as_bytes());
        content_obj.extend_from_slice(b"\nendstream");
        objs[base + 1] = content_obj;

        // Page dict (base)
        objs[base] = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] \
             /Contents {} 0 R \
             /Resources << /XObject << /Im{} {} 0 R >> >> >>\n",
            p.w, p.h, base + 1, base + 2, base + 2
        ).into_bytes();
    }

    // Pages object (obj 2)
    let kids: Vec<String> = (0..np).map(|i| format!("{} 0 R", 3 + i * 3)).collect();
    objs[2] = format!("<< /Type /Pages /Kids [{}] /Count {} >>\n", kids.join(" "), np).into_bytes();

    // Catalog (obj 1)
    objs[1] = b"<< /Type /Catalog /Pages 2 0 R >>\n".to_vec();

    // Assemble PDF
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n%\xFF\xFF\xFF\xFF\n");
    let mut offsets = vec![0u64; total + 1];
    for i in 1..=total {
        offsets[i] = pdf.len() as u64;
        pdf.extend_from_slice(format!("{} 0 obj\n", i).as_bytes());
        pdf.extend_from_slice(&objs[i]);
        pdf.extend_from_slice(b"\nendobj\n");
    }

    let xref_offset = pdf.len() as u64;
    pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", total + 1).as_bytes());
    for i in 1..=total {
        pdf.extend_from_slice(format!("{:010} 00000 n \n", offsets[i]).as_bytes());
    }
    pdf.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n", total + 1, xref_offset).as_bytes());

    Ok(pdf)
}

fn export_pdf(images: &[String], out_path: &Path) -> Result<(), String> {
    fs::write(out_path, &generate_pdf(images)?).map_err(|e| e.to_string())
}

// ─── EPUB Export ──────────────────────────────────────────────────────────────

fn export_epub(images: &[String], out_path: &Path, title: &str) -> Result<(), String> {
    let buf = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(buf);

    let opts = || SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(9))
        .unix_permissions(0o644);

    zip.start_file("mimetype", SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored))
        .map_err(|e| e.to_string())?;
    zip.write_all(b"application/epub+zip").map_err(|e| e.to_string())?;

    zip.start_file("META-INF/container.xml", opts()).map_err(|e| e.to_string())?;
    zip.write_all(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\">\n  <rootfiles>\n    <rootfile full-path=\"OEBPS/content.opf\" media-type=\"application/oebps-package+xml\"/>\n  </rootfiles>\n</container>").map_err(|e| e.to_string())?;

    let uid = Uuid::new_v4().to_string();

    for (i, path_str) in images.iter().enumerate() {
        let src = Path::new(path_str);
        let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("jpg").to_lowercase();
        zip.start_file(format!("OEBPS/images/page_{:04}.{}", i + 1, ext), opts())
            .map_err(|e| e.to_string())?;
        let data = fs::read(src).map_err(|e| e.to_string())?;
        zip.write_all(&data).map_err(|e| e.to_string())?;
    }

    for (i, path_str) in images.iter().enumerate() {
        let ext = Path::new(path_str).extension().and_then(|e| e.to_str()).unwrap_or("jpg");
        let html = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<!DOCTYPE html>\n<html xmlns=\"http://www.w3.org/1999/xhtml\">\n<head><title>Page {}</title></head>\n<body style=\"margin:0;padding:0;text-align:center;\">\n<img src=\"images/page_{:04}.{}\" alt=\"Page {}\" style=\"max-width:100%;height:auto;\"/>\n</body>\n</html>", i + 1, i + 1, ext, i + 1);
        zip.start_file(format!("OEBPS/page_{:04}.xhtml", i + 1), opts())
            .map_err(|e| e.to_string())?;
        zip.write_all(html.as_bytes()).map_err(|e| e.to_string())?;
    }

    let mut manifest = String::new();
    manifest.push_str("<item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/>\n");
    for i in 0..images.len() {
        manifest.push_str(&format!("<item id=\"page_{:04}\" href=\"page_{:04}.xhtml\" media-type=\"application/xhtml+xml\"/>\n", i+1, i+1));
    }
    for (i, path_str) in images.iter().enumerate() {
        let ext = Path::new(path_str).extension().and_then(|e| e.to_str()).unwrap_or("jpg");
        let mime = match ext { "png" => "image/png", "webp" => "image/webp", "gif" => "image/gif", "bmp" => "image/bmp", _ => "image/jpeg" };
        manifest.push_str(&format!("<item id=\"img_{:04}\" href=\"images/page_{:04}.{}\" media-type=\"{}\"/>\n", i+1, i+1, ext, mime));
    }
    let mut spine = String::new();
    for i in 0..images.len() { spine.push_str(&format!("<itemref idref=\"page_{:04}\"/>\n", i + 1)); }

    let opf = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <package xmlns=\"http://www.idpf.org/2007/opf\" unique-identifier=\"book-id\" version=\"3.0\">\n\
         <metadata>\n\
         <dc:identifier id=\"book-id\">urn:uuid:{}</dc:identifier>\n\
         <dc:title>{}</dc:title>\n\
         <dc:language>zh-CN</dc:language>\n\
         <meta property=\"dcterms:modified\">2026-01-01T00:00:00Z</meta>\n\
         </metadata>\n\
         <manifest>\n{}</manifest>\n\
         <spine>\n{}</spine>\n\
         </package>", uid, title, manifest, spine);

    zip.start_file("OEBPS/content.opf", opts()).map_err(|e| e.to_string())?;
    zip.write_all(opf.as_bytes()).map_err(|e| e.to_string())?;

    let mut nav_items = String::new();
    for i in 0..images.len() { nav_items.push_str(&format!("<li><a href=\"page_{:04}.xhtml\">Page {}</a></li>\n", i+1, i+1)); }
    let nav = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\">\n\
         <head><title>Navigation</title></head>\n\
         <body>\n<nav epub:type=\"toc\">\n<h1>Table of Contents</h1>\n<ol>\n{}</ol>\n</nav>\n</body>\n</html>", nav_items);

    zip.start_file("OEBPS/nav.xhtml", opts()).map_err(|e| e.to_string())?;
    zip.write_all(nav.as_bytes()).map_err(|e| e.to_string())?;

    let data = zip.finish().map_err(|e| e.to_string())?;
    fs::write(out_path, data.into_inner()).map_err(|e| e.to_string())
}

// ─── Long Image Export ────────────────────────────────────────────────────────

fn export_long_image(
    images: &[String], out_path: &Path, _fmt: ::image::ImageFormat,
    stitch_mode: &str, uniform_width: u32, border_width: u32, border_color: [u8; 3],
) -> Result<(), String> {
    if images.is_empty() { return Err("No images".into()); }
    let decoded: Vec<DynamicImage> = images.iter()
        .map(|p| ::image::open(p).map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?;

    let processed: Vec<DynamicImage> = if stitch_mode == "uniform" {
        let w = if uniform_width > 0 { uniform_width } else { 1200 };
        decoded.into_iter().map(|img| {
            let h = (img.height() as f64 * w as f64 / img.width() as f64).round() as u32;
            img.resize_exact(w, h, ::image::imageops::FilterType::Lanczos3)
        }).collect()
    } else {
        decoded
    };

    let max_w = processed.iter().map(|d| d.width()).max().unwrap_or(1);
    let total_h: u32 = processed.iter().map(|d| d.height()).sum();
    let border_total = border_width * (processed.len() as u32 - 1);
    let canvas_h = total_h + border_total;

    let mut canvas = ::image::RgbImage::new(max_w, canvas_h);
    for pixel in canvas.pixels_mut() {
        *pixel = ::image::Rgb(border_color);
    }

    let mut y_offset = 0u32;
    for d in &processed {
        let rgb = d.to_rgb8();
        let x_off = (max_w - d.width()) / 2;
        for (x, y, pixel) in rgb.enumerate_pixels() {
            let px = x_off + x;
            let py = y_offset + y;
            if px < max_w && py < canvas_h {
                canvas.put_pixel(px, py, *pixel);
            }
        }
        y_offset += d.height() + border_width;
    }
    canvas.save(out_path).map_err(|e| e.to_string())
}

// ─── .kag Project Format (lightweight: paths + thumbnails) ─────────────────────

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportConfig {
    format: String,
    stitch_mode: String,
    uniform_width: u32,
    border_width: u32,
    border_color: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestFileEntry {
    path: String,
    name: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    version: u32,
    title: String,
    created: String,
    files: Vec<ManifestFileEntry>,
    export: Option<ExportConfig>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LoadResult {
    title: String,
    images: Vec<String>,
    thumb_dir: String,
    export_settings: Option<ExportConfig>,
}

fn sanitize_title(raw: &str) -> String {
    let s: String = raw.chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-' || *c == '_')
        .collect();
    let s = s.trim();
    if s.is_empty() { "Project".into() } else { s.to_string() }
}

fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs();
    let days = secs / 86400;
    let t = secs % 86400;
    let h = t / 3600;
    let m = (t % 3600) / 60;
    let s = t % 60;
    let year = 1970 + (days as f64 / 365.25) as u64;
    let month = ((days % 365) as f64 / 30.44) as u64 + 1;
    let day = (days % 365) as u64 - ((month - 1) as f64 * 30.44) as u64 + 1;
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", year, month.min(12), day.min(31), h, m, s)
}

fn generate_thumb_jpeg(data: &[u8]) -> Result<Vec<u8>, String> {
    let img = ::image::load_from_memory(data).map_err(|e| e.to_string())?;
    let thumb = if img.width() > 200 || img.height() > 200 {
        img.thumbnail(200, 200)
    } else {
        img
    };
    let mut buf = std::io::Cursor::new(Vec::new());
    thumb.write_to(&mut buf, ::image::ImageFormat::Jpeg).map_err(|e| e.to_string())?;
    Ok(buf.into_inner())
}

#[tauri::command]
async fn save_project(
    app_handle: tauri::AppHandle,
    title: String,
    image_paths: Vec<String>,
    outdir: String,
    export_config: Option<ExportConfig>,
) -> Result<String, String> {
    use tauri::Emitter;

    let safe = sanitize_title(&title);
    let out_path = PathBuf::from(&outdir).join(format!("{}.kag", safe));
    fs::create_dir_all(Path::new(&outdir)).map_err(|e| e.to_string())?;

    let file = fs::File::create(&out_path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored);

    let created = now_iso();
    let mut files_meta: Vec<ManifestFileEntry> = Vec::new();
    let total = image_paths.len();

    for (i, path_str) in image_paths.iter().enumerate() {
        let src = Path::new(path_str);
        let name = src.file_name().and_then(|n| n.to_str()).unwrap_or("unknown").to_string();

        let thumb_path = format!("thumbs/{}.jpg", i);

        // Read original file and generate thumbnail on blocking thread
        let raw = fs::read(src).map_err(|e| format!("Cannot read {}: {}", path_str, e))?;
        let thumb_data = tokio::task::spawn_blocking(move || {
            generate_thumb_jpeg(&raw)
        })
        .await
        .map_err(|e| format!("Thumbnail thread failed: {}", e))??;

        // Write thumbnail to zip
        zip.start_file(&thumb_path, opts).map_err(|e| e.to_string())?;
        zip.write_all(&thumb_data).map_err(|e| e.to_string())?;

        files_meta.push(ManifestFileEntry { path: path_str.clone(), name });

        let _ = app_handle.emit("save-progress", serde_json::json!({
            "current": i + 1, "total": total, "file": "", "stage": "writing",
        }));

        tokio::task::yield_now().await;
    }

    let manifest = Manifest {
        version: 2,
        title: safe,
        created,
        files: files_meta,
        export: export_config,
    };
    let json = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    zip.start_file("manifest.json", opts).map_err(|e| e.to_string())?;
    zip.write_all(json.as_bytes()).map_err(|e| e.to_string())?;
    zip.finish().map_err(|e| e.to_string())?;

    Ok(out_path.to_string_lossy().to_string())
}

#[tauri::command]
async fn load_project(kag_path: String) -> Result<LoadResult, String> {
    tokio::task::spawn_blocking(move || {
        let file = fs::File::open(&kag_path).map_err(|e| format!("Cannot open .kag: {}", e))?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("Invalid .kag: {}", e))?;

        // Read manifest
        let mut manifest_str = String::new();
        {
            let mut mf = archive.by_name("manifest.json")
                .map_err(|_| "Missing manifest.json".to_string())?;
            mf.read_to_string(&mut manifest_str)
                .map_err(|e| format!("Cannot read manifest: {}", e))?;
        }
        let manifest: Manifest = serde_json::from_str(&manifest_str)
            .map_err(|e| format!("Invalid manifest: {}", e))?;

        // Extract thumbnails to temp dir
        let temp_base = std::env::temp_dir().join("kitanga");
        let temp_dir = temp_base.join(Uuid::new_v4().to_string());
        fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;

        for i in 0..manifest.files.len() {
            let thumb_name = format!("thumbs/{}.jpg", i);
            if let Ok(mut reader) = archive.by_name(&thumb_name) {
                let mut buf = Vec::new();
                reader.read_to_end(&mut buf).ok();
                if !buf.is_empty() {
                    let _ = fs::write(temp_dir.join(format!("{}.jpg", i)), &buf);
                }
            }
        }

        let images: Vec<String> = manifest.files.iter().map(|e| e.path.clone()).collect();

        Ok(LoadResult {
            title: manifest.title,
            images,
            thumb_dir: temp_dir.to_string_lossy().to_string(),
            export_settings: manifest.export,
        })
    })
    .await
    .map_err(|e| format!("Load thread failed: {}", e))?
}

#[tauri::command]
fn read_project_thumb(thumb_dir: String, index: usize) -> Result<String, String> {
    let path = Path::new(&thumb_dir).join(format!("{}.jpg", index));
    let bytes = fs::read(&path).map_err(|_| "Thumbnail not found".to_string())?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(b64)
}

// ─── App Entry ────────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            scan_directory,
            get_thumbnail,
            export_images,
            save_project,
            load_project,
            read_project_thumb
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
