//! What can be pasted on a terminal. gpui reads text and some image formats, and only the first
//! one it recognizes; screenshot tools may offer just a Windows bitmap, which it does not read.
use gpui::{App, ClipboardEntry, ImageFormat};

pub enum Clip {
    Text(String),
    Image { name: &'static str, data: Vec<u8> },
}

pub fn read(cx: &App) -> Option<Clip> {
    let Some(item) = cx.read_from_clipboard() else {
        return read_bitmap();
    };
    if let Some(text) = item.text() {
        return Some(Clip::Text(text));
    }
    // Spreadsheets offer a picture of the cells too: copied text stays text.
    if let Some(text) = platform::text() {
        return Some(Clip::Text(text));
    }
    item.entries().iter().find_map(|entry| match entry {
        ClipboardEntry::Image(image) => Some(Clip::Image {
            name: match image.format {
                ImageFormat::Png => "image.png",
                ImageFormat::Jpeg => "image.jpg",
                ImageFormat::Webp => "image.webp",
                ImageFormat::Gif => "image.gif",
                ImageFormat::Svg => "image.svg",
                ImageFormat::Bmp => "image.bmp",
                _ => "image",
            },
            data: image.bytes.clone(),
        }),
        _ => None,
    })
}

fn read_bitmap() -> Option<Clip> {
    let data = dib_to_png(&platform::bitmap()?)?;
    Some(Clip::Image {
        name: "screenshot.png",
        data,
    })
}

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

/// Converts a Windows device-independent bitmap (24 or 32 bits per pixel) to a PNG file.
fn dib_to_png(dib: &[u8]) -> Option<Vec<u8>> {
    const BI_RGB: u32 = 0;
    const BI_BITFIELDS: u32 = 3;
    let header = u32_at(dib, 0)? as usize;
    let width = u32_at(dib, 4)? as i32;
    let height = u32_at(dib, 8)? as i32;
    let bits = u16_at(dib, 14)? as usize;
    let compression = u32_at(dib, 16)?;
    if header < 40 || width <= 0 || height == 0 || !matches!(bits, 24 | 32) {
        return None;
    }
    let mut pixels_at = header;
    match compression {
        BI_RGB => {}
        BI_BITFIELDS if bits == 32 => {
            // Only the usual blue, green, red byte order. The masks follow a 40 byte header and
            // are part of the longer ones.
            let masks = (u32_at(dib, 40)?, u32_at(dib, 44)?, u32_at(dib, 48)?);
            if masks != (0x00FF_0000, 0x0000_FF00, 0x0000_00FF) {
                return None;
            }
            if header == 40 {
                pixels_at += 12;
            }
        }
        _ => return None,
    }
    let (width, rows) = (width as usize, height.unsigned_abs() as usize);
    let bytes_per_pixel = bits / 8;
    // Rows are padded to 4 bytes, and go from the bottom up when the height is positive.
    let stride = width.checked_mul(bytes_per_pixel)?.checked_add(3)? / 4 * 4;
    let size = stride.checked_mul(rows)?;
    let pixels = dib.get(pixels_at..pixels_at.checked_add(size)?)?;
    let row_at = |row: usize| {
        let row = if height > 0 { rows - 1 - row } else { row };
        &pixels[row * stride..row * stride + width * bytes_per_pixel]
    };
    // Most programs leave alpha at zero in a 32 bit bitmap, which does not mean transparent.
    let has_alpha = bits == 32
        && (0..rows).any(|row| {
            row_at(row)
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] != 0)
        });
    let mut rgba = Vec::with_capacity(width.checked_mul(rows)?.checked_mul(4)?);
    for row in 0..rows {
        for pixel in row_at(row).chunks_exact(bytes_per_pixel) {
            let alpha = if has_alpha { pixel[3] } else { 255 };
            rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], alpha]);
        }
    }
    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, width as u32, rows as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(&rgba).ok()?;
    writer.finish().ok()?;
    Some(png)
}

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;

    const CF_DIB: u32 = 8;
    const CF_UNICODETEXT: u32 = 13;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn OpenClipboard(owner: *mut c_void) -> i32;
        fn CloseClipboard() -> i32;
        fn IsClipboardFormatAvailable(format: u32) -> i32;
        fn GetClipboardData(format: u32) -> *mut c_void;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalLock(memory: *mut c_void) -> *mut c_void;
        fn GlobalUnlock(memory: *mut c_void) -> i32;
        fn GlobalSize(memory: *mut c_void) -> usize;
    }

    /// A copy of the clipboard contents in one format. Windows also serves plain bitmaps as DIB.
    fn read(format: u32) -> Option<Vec<u8>> {
        // SAFETY: the clipboard is open while its memory is used, and the memory is locked and
        // valid for `GlobalSize` bytes while it is copied.
        unsafe {
            if IsClipboardFormatAvailable(format) == 0 || OpenClipboard(std::ptr::null_mut()) == 0 {
                return None;
            }
            let handle = GetClipboardData(format);
            let mut bytes = None;
            if !handle.is_null() {
                let data = GlobalLock(handle);
                if !data.is_null() {
                    let size = GlobalSize(handle);
                    bytes = Some(std::slice::from_raw_parts(data as *const u8, size).to_vec());
                    GlobalUnlock(handle);
                }
            }
            CloseClipboard();
            bytes
        }
    }

    pub fn bitmap() -> Option<Vec<u8>> {
        read(CF_DIB)
    }

    pub fn text() -> Option<String> {
        let bytes = read(CF_UNICODETEXT)?;
        let wide: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .take_while(|unit| *unit != 0)
            .collect();
        Some(String::from_utf16_lossy(&wide))
    }
}

#[cfg(not(windows))]
mod platform {
    pub fn bitmap() -> Option<Vec<u8>> {
        None
    }

    pub fn text() -> Option<String> {
        None
    }
}
