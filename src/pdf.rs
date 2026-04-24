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
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return None;
    }
    let mut dpi = 300.0_f64;
    let mut dims: Option<(u32, u32, u8)> = None;
    let mut pos = 2;

    while pos + 4 <= data.len() {
        if data[pos] != 0xFF {
            break;
        }
        let marker = data[pos + 1];

        // JFIF APP0 — contains DPI
        if marker == 0xE0 && pos + 16 <= data.len() && &data[pos + 4..pos + 9] == b"JFIF\0" {
            let units = data[pos + 11];
            let xd = u16::from_be_bytes([data[pos + 12], data[pos + 13]]) as f64;
            if xd > 0.0 {
                dpi = match units {
                    1 => xd,
                    2 => xd * 2.54,
                    _ => dpi,
                };
            }
        }

        // SOF markers — contain width, height, components
        if matches!(
            marker,
            0xC0 | 0xC1 | 0xC2 | 0xC3 | 0xC5 | 0xC6 | 0xC7 | 0xC9 | 0xCA | 0xCB | 0xCD | 0xCE
                | 0xCF
        ) && pos + 9 < data.len()
        {
            let h = u16::from_be_bytes([data[pos + 5], data[pos + 6]]) as u32;
            let w = u16::from_be_bytes([data[pos + 7], data[pos + 8]]) as u32;
            let comps = data[pos + 9];
            dims = Some((w, h, comps));
            break;
        }

        if marker == 0xD9 {
            break;
        }
        let seg_len = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize;
        if seg_len < 2 {
            break;
        }
        pos += 2 + seg_len;
    }

    let (width, height, comps) = dims?;
    let color_space = if comps == 1 { "DeviceGray" } else { "DeviceRGB" };
    Some(JpegInfo { width, height, color_space, dpi })
}

/// Assemble JPEG pages into a single PDF, embedding them without re-encoding.
pub fn assemble_pdf(jpeg_pages: &[Vec<u8>]) -> Result<Vec<u8>> {
    if jpeg_pages.is_empty() {
        bail!("no pages to assemble");
    }

    let mut doc = Document::with_version("1.5");
    let mut page_ids = Vec::new();

    for (i, jpeg) in jpeg_pages.iter().enumerate() {
        let info = parse_jpeg(jpeg)
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
        img_dict.set("ColorSpace", Object::Name(info.color_space.as_bytes().to_vec()));
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
        let mut v = vec![
            0xFF, 0xD8, // SOI
            // APP0 (JFIF), length=16
            0xFF, 0xE0, 0x00, 0x10,
            0x4A, 0x46, 0x49, 0x46, 0x00, // "JFIF\0"
            0x01, 0x01, // version 1.1
            0x01, // units = DPI
        ];
        v.extend_from_slice(&dpi.to_be_bytes()); // Xdensity
        v.extend_from_slice(&dpi.to_be_bytes()); // Ydensity
        v.extend_from_slice(&[0x00, 0x00]); // no thumbnail
        // SOF0, length=11 (1 component = grayscale)
        v.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
        v.extend_from_slice(&height.to_be_bytes());
        v.extend_from_slice(&width.to_be_bytes());
        v.extend_from_slice(&[0x01, 0x01, 0x11, 0x00]); // 1 component
        v.extend_from_slice(&[0xFF, 0xD9]); // EOI
        v
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
}
