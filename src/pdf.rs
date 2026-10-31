use anyhow::{Result, bail};
use lopdf::{
    Dictionary, Document, Object, Stream,
    content::{Content, Operation},
};

struct JpegInfo {
    width: u32,
    height: u32,
    color_space: &'static str,
    dpi: f64,
}

/// Parse JPEG dimensions, component count and DPI from JFIF/SOF markers.
fn parse_jpeg(data: &[u8]) -> Option<JpegInfo> {
    parse_jpeg_with_dpi(data, 300)
}

pub(crate) fn valid_jpeg(data: &[u8]) -> bool {
    parse_jpeg(data).is_some()
}

fn parse_jpeg_with_dpi(data: &[u8], fallback_dpi: u32) -> Option<JpegInfo> {
    if !data.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let mut pos = 2;
    let mut dpi = if fallback_dpi > 0 {
        f64::from(fallback_dpi)
    } else {
        300.0
    };
    while pos < data.len() {
        if data[pos] != 0xff {
            return None;
        }
        while data.get(pos) == Some(&0xff) {
            pos += 1;
        }
        let marker = *data.get(pos)?;
        pos += 1;
        if matches!(marker, 0xd9 | 0xda) {
            return None;
        }
        if matches!(marker, 0xd0..=0xd7 | 0x01) {
            continue;
        }
        let length = usize::from(u16::from_be_bytes([*data.get(pos)?, *data.get(pos + 1)?]));
        if length < 2 || pos.checked_add(length)? > data.len() {
            return None;
        }
        let segment = &data[pos + 2..pos + length];
        if marker == 0xe0 && segment.len() >= 12 && segment.starts_with(b"JFIF\0") {
            let density = f64::from(u16::from_be_bytes([segment[8], segment[9]]));
            if density > 0.0 {
                dpi = match segment[7] {
                    1 => density,
                    2 => density * 2.54,
                    _ => dpi,
                };
            }
        }
        if matches!(marker,0xc0..=0xc3|0xc5..=0xc7|0xc9..=0xcb|0xcd..=0xcf) {
            if !matches!(marker, 0xc0..=0xc2) || segment.len() < 6 || segment[0] != 8 {
                return None;
            }
            let height = u32::from(u16::from_be_bytes([segment[1], segment[2]]));
            let width = u32::from(u16::from_be_bytes([segment[3], segment[4]]));
            let components = segment[5];
            if width == 0 || height == 0 || segment.len() < 6 + usize::from(components) * 3 {
                return None;
            }
            let color_space = match components {
                1 => "DeviceGray",
                3 => "DeviceRGB",
                4 => "DeviceCMYK",
                _ => return None,
            };
            return Some(JpegInfo {
                width,
                height,
                color_space,
                dpi,
            });
        }
        pos += length;
    }
    None
}

/// Assemble JPEG pages into a single PDF, embedding them without re-encoding.
#[cfg(test)]
pub fn assemble_pdf(jpeg_pages: &[Vec<u8>]) -> Result<Vec<u8>> {
    assemble_pdf_with_dpi(jpeg_pages, 300)
}

pub fn assemble_pdf_with_dpi(jpeg_pages: &[Vec<u8>], fallback_dpi: u32) -> Result<Vec<u8>> {
    if jpeg_pages.is_empty() {
        bail!("no pages to assemble");
    }

    let mut doc = Document::with_version("1.5");
    let mut page_ids = Vec::new();

    for (i, jpeg) in jpeg_pages.iter().enumerate() {
        let info = parse_jpeg_with_dpi(jpeg, fallback_dpi)
            .ok_or_else(|| anyhow::anyhow!("cannot parse JPEG header for page {i}"))?;

        // Page size in PDF points (72 pt = 1 inch)
        let pw = (info.width as f64 * 72.0 / info.dpi) as f32;
        let ph = (info.height as f64 * 72.0 / info.dpi) as f32;
        let img_name = format!("Im{i}");

        // Embed raw JPEG bytes via DCTDecode — no quality loss
        let mut img_dict = Dictionary::new();
        img_dict.set("Type", Object::Name(b"XObject".to_vec()));
        img_dict.set("Subtype", Object::Name(b"Image".to_vec()));
        img_dict.set("Width", Object::Integer(info.width as i64));
        img_dict.set("Height", Object::Integer(info.height as i64));
        img_dict.set(
            "ColorSpace",
            Object::Name(info.color_space.as_bytes().to_vec()),
        );
        img_dict.set("BitsPerComponent", Object::Integer(8));
        img_dict.set("Filter", Object::Name(b"DCTDecode".to_vec()));
        let img_id = doc.add_object(Stream::new(img_dict, jpeg.clone()));

        // Content stream: transform unit square → page size, draw image
        let content = Content {
            operations: vec![
                Operation::new("q", vec![]),
                Operation::new(
                    "cm",
                    vec![
                        Object::Real(pw),
                        Object::Integer(0),
                        Object::Integer(0),
                        Object::Real(ph),
                        Object::Integer(0),
                        Object::Integer(0),
                    ],
                ),
                Operation::new("Do", vec![Object::Name(img_name.as_bytes().to_vec())]),
                Operation::new("Q", vec![]),
            ],
        };
        let content_id = doc.add_object(Stream::new(Dictionary::new(), content.encode()?));

        let mut xobjects = Dictionary::new();
        xobjects.set(img_name.as_bytes(), Object::Reference(img_id));
        let mut resources = Dictionary::new();
        resources.set("XObject", Object::Dictionary(xobjects));

        let mut page_dict = Dictionary::new();
        page_dict.set("Type", Object::Name(b"Page".to_vec()));
        page_dict.set(
            "MediaBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Real(pw),
                Object::Real(ph),
            ]),
        );
        page_dict.set("Contents", Object::Reference(content_id));
        page_dict.set("Resources", Object::Dictionary(resources));
        page_ids.push(doc.add_object(Object::Dictionary(page_dict)));
    }

    // Pages node (forward-ref problem solved by backfilling Parent after insertion)
    let pages_id = doc.add_object(Object::Dictionary({
        let mut d = Dictionary::new();
        d.set("Type", Object::Name(b"Pages".to_vec()));
        d.set(
            "Kids",
            Object::Array(page_ids.iter().map(|id| Object::Reference(*id)).collect()),
        );
        d.set("Count", Object::Integer(page_ids.len() as i64));
        d
    }));
    for page_id in &page_ids {
        if let Some(Object::Dictionary(d)) = doc.objects.get_mut(page_id) {
            d.set("Parent", Object::Reference(pages_id));
        }
    }

    let catalog_id = doc.add_object(Object::Dictionary({
        let mut d = Dictionary::new();
        d.set("Type", Object::Name(b"Catalog".to_vec()));
        d.set("Pages", Object::Reference(pages_id));
        d
    }));
    doc.trailer.set("Root", Object::Reference(catalog_id));

    let mut buf = Vec::new();
    doc.save_to(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal but structurally valid JPEG for unit tests.
    fn make_test_jpeg(width: u16, height: u16, dpi: u16) -> Vec<u8> {
        make_test_jpeg_units(width, height, dpi, 1)
    }

    /// Like make_test_jpeg but with a configurable JFIF density units field.
    /// units: 0 = aspect ratio only, 1 = DPI, 2 = dots/cm
    fn make_test_jpeg_units(width: u16, height: u16, density: u16, units: u8) -> Vec<u8> {
        let mut v = vec![
            0xFF, 0xD8, // SOI
            // APP0 (JFIF), length=16
            0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, // "JFIF\0"
            0x01, 0x01, // version 1.1
            units,
        ];
        v.extend_from_slice(&density.to_be_bytes()); // Xdensity
        v.extend_from_slice(&density.to_be_bytes()); // Ydensity
        v.extend_from_slice(&[0x00, 0x00]); // no thumbnail
        // SOF0, length=11 (1 component = grayscale): precision, height, width, components
        v.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
        v.extend_from_slice(&height.to_be_bytes());
        v.extend_from_slice(&width.to_be_bytes());
        v.extend_from_slice(&[0x01, 0x01, 0x11, 0x00]); // 1 component
        v.extend_from_slice(&[0xFF, 0xD9]); // EOI
        v
    }

    /// JPEG with no APP0 marker — just SOI + SOF0 + EOI.
    fn make_jpeg_no_jfif(width: u16, height: u16) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8]; // SOI
        // SOF0, length=11
        v.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
        v.extend_from_slice(&height.to_be_bytes());
        v.extend_from_slice(&width.to_be_bytes());
        v.extend_from_slice(&[0x01, 0x01, 0x11, 0x00]); // 1 component
        v.extend_from_slice(&[0xFF, 0xD9]); // EOI
        v
    }

    #[test]
    fn job_resolution_is_used_when_jpeg_has_no_density() {
        let jpeg = make_jpeg_no_jfif(150, 150);
        assert_eq!(parse_jpeg_with_dpi(&jpeg, 150).unwrap().dpi, 150.0);
        let pdf = assemble_pdf_with_dpi(&[jpeg], 150).unwrap();
        let doc = lopdf::Document::load_mem(&pdf).unwrap();
        let page = *doc.get_pages().get(&1).unwrap();
        let values = doc
            .get_object(page)
            .unwrap()
            .as_dict()
            .unwrap()
            .get(b"MediaBox")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(values[2].as_float().unwrap(), 72.0);
    }

    #[test]
    fn jpeg_fill_bytes_and_cmyk_are_supported() {
        let mut fill = make_jpeg_no_jfif(100, 100);
        fill.insert(2, 0xff);
        assert!(parse_jpeg(&fill).is_some());
        let mut cmyk = vec![0xff, 0xd8, 0xff, 0xc0, 0, 20, 8, 0, 100, 0, 100, 4];
        cmyk.extend_from_slice(&[1, 0x11, 0, 2, 0x11, 0, 3, 0x11, 0, 4, 0x11, 0, 0xff, 0xd9]);
        assert_eq!(parse_jpeg(&cmyk).unwrap().color_space, "DeviceCMYK");
    }

    #[test]
    fn test_parse_jpeg_dimensions() {
        let jpeg = make_test_jpeg(800, 1000, 300);
        let info = parse_jpeg(&jpeg).unwrap();
        assert_eq!(info.width, 800);
        assert_eq!(info.height, 1000);
        assert_eq!(info.dpi, 300.0);
        assert_eq!(info.color_space, "DeviceGray");
    }

    #[test]
    fn test_parse_jpeg_dpi_600() {
        let jpeg = make_test_jpeg(100, 100, 600);
        let info = parse_jpeg(&jpeg).unwrap();
        assert_eq!(info.dpi, 600.0);
    }

    #[test]
    fn test_assemble_pdf_produces_bytes() {
        let jpeg = make_test_jpeg(100, 100, 300);
        let pdf = assemble_pdf(&[jpeg]).unwrap();
        assert!(pdf.starts_with(b"%PDF-1.5"));
        assert!(pdf.len() > 200);
    }

    #[test]
    fn test_assemble_pdf_two_pages() {
        let jpeg = make_test_jpeg(200, 300, 300);
        let pdf = assemble_pdf(&[jpeg.clone(), jpeg]).unwrap();
        // PDF should contain both page references
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/Count 2"));
    }

    #[test]
    fn test_assemble_pdf_empty_fails() {
        assert!(assemble_pdf(&[]).is_err());
    }

    #[test]
    fn test_assemble_pdf_with_real_jpeg_fixture() {
        // Uses the public-domain fixture JPEG (1291×101 px, 96 DPI) to exercise
        // the full encode path: JPEG parsing → page-size calculation → lopdf embed.
        let jpeg = include_bytes!("../tests/fixtures/scan_page.jpg").to_vec();
        let pdf = assemble_pdf(&[jpeg.clone(), jpeg]).expect("PDF assembly must succeed");

        assert!(pdf.starts_with(b"%PDF-1.5"));

        // lopdf must parse the output and report 2 pages
        let doc = lopdf::Document::load_mem(&pdf).expect("PDF must be lopdf-parseable");
        let pages = doc.get_pages();
        assert_eq!(pages.len(), 2, "assembled PDF must have 2 pages");

        // MediaBox on page 1 must reflect the JPEG's pixel/DPI ratio:
        // 1291px * 72pt/96dpi ≈ 967.5pt wide  /  101px * 72/96 ≈ 75.75pt tall
        let page1_id = *pages.get(&1).unwrap();
        let page1 = doc.get_object(page1_id).unwrap();
        let dict = page1.as_dict().unwrap();
        let media_box = dict.get(b"MediaBox").unwrap().as_array().unwrap();
        let width_pt = match &media_box[2] {
            lopdf::Object::Real(f) => *f as f64,
            lopdf::Object::Integer(i) => *i as f64,
            _ => panic!("unexpected MediaBox width type"),
        };
        let height_pt = match &media_box[3] {
            lopdf::Object::Real(f) => *f as f64,
            lopdf::Object::Integer(i) => *i as f64,
            _ => panic!("unexpected MediaBox height type"),
        };
        // Allow ±2pt rounding tolerance
        assert!(
            (width_pt - 967.5).abs() < 2.0,
            "page width should be ~967.5pt, got {width_pt}"
        );
        assert!(
            (height_pt - 75.75).abs() < 2.0,
            "page height should be ~75.75pt, got {height_pt}"
        );
    }

    #[test]
    fn test_parse_jpeg_dpi_units_cm() {
        // units=2 means dots/cm; 38 dots/cm * 2.54 ≈ 96.52 DPI
        let jpeg = make_test_jpeg_units(100, 100, 38, 2);
        let info = parse_jpeg(&jpeg).unwrap();
        let expected = 38.0 * 2.54;
        assert!(
            (info.dpi - expected).abs() < 0.01,
            "expected ~{expected} DPI, got {}",
            info.dpi
        );
    }

    #[test]
    fn test_parse_jpeg_dpi_units_zero_keeps_default() {
        // units=0 means pixel aspect ratio only — density value has no physical meaning.
        // parse_jpeg must leave DPI at the 300 default.
        let jpeg = make_test_jpeg_units(100, 100, 72, 0);
        let info = parse_jpeg(&jpeg).unwrap();
        assert_eq!(info.dpi, 300.0, "units=0 must not change the DPI default");
    }

    #[test]
    fn test_parse_jpeg_no_jfif_defaults_to_300_dpi() {
        // A JPEG without any APP0/JFIF marker has no embedded DPI.
        // parse_jpeg must default to 300 DPI so assemble_pdf still produces valid output.
        let jpeg = make_jpeg_no_jfif(200, 150);
        let info = parse_jpeg(&jpeg).unwrap();
        assert_eq!(info.dpi, 300.0, "missing JFIF must default to 300 DPI");
        assert_eq!(info.width, 200);
        assert_eq!(info.height, 150);
    }

    #[test]
    fn test_assemble_pdf_no_jfif_jpeg_succeeds() {
        // assemble_pdf must not fail on a JPEG without a JFIF APP0 marker.
        let jpeg = make_jpeg_no_jfif(400, 600);
        let pdf = assemble_pdf(&[jpeg]).expect("assemble must succeed without JFIF");
        assert!(pdf.starts_with(b"%PDF-1.5"));
        let doc = lopdf::Document::load_mem(&pdf).expect("PDF must be lopdf-parseable");
        assert_eq!(doc.get_pages().len(), 1);
    }
}
