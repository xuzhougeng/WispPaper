//! Pictures that go with a question: whatever was pasted or picked is made
//! one a model takes. The services read PNG, JPEG, GIF and WebP; the
//! clipboard also hands over TIFF and BMP, and a screenshot of a large
//! display is far bigger than a model has any use for.

use gpui::{Image, ImageFormat};
use std::io::Cursor;

/// The longest edge a picture is sent with, in pixels: beyond this a model
/// sees no more, and only pays for it.
const MAX_EDGE: u32 = 2048;

/// The most bytes a picture is sent with. The services stop at a few
/// megabytes for one image.
const MAX_BYTES: usize = 3_500_000;

/// The format behind a media type a picture was kept under.
pub fn format_of(media_type: &str) -> Option<ImageFormat> {
    [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::Gif, ImageFormat::Webp].into_iter().find(|format| format.mime_type() == media_type)
}

/// The picture as it will be sent and drawn: unchanged when it is already
/// in a format the services read and small enough, otherwise scaled down
/// to `MAX_EDGE` and written as PNG, or as JPEG when that is still too big.
pub fn prepare(bytes: Vec<u8>) -> Result<Image, String> {
    let unreadable = |err: image::ImageError| tf!("无法读取这张图片：{err}", "Cannot read image: {err}");
    let direct = match image::guess_format(&bytes) {
        Ok(image::ImageFormat::Png) => Some(ImageFormat::Png),
        Ok(image::ImageFormat::Jpeg) => Some(ImageFormat::Jpeg),
        Ok(image::ImageFormat::Gif) => Some(ImageFormat::Gif),
        Ok(image::ImageFormat::WebP) => Some(ImageFormat::Webp),
        _ => None,
    };
    let picture = image::load_from_memory(&bytes).map_err(unreadable)?;
    let large = picture.width().max(picture.height()) > MAX_EDGE;
    if let Some(format) = direct && !large && bytes.len() <= MAX_BYTES { return Ok(Image::from_bytes(format, bytes)) }
    let picture = if large { picture.resize(MAX_EDGE, MAX_EDGE, image::imageops::FilterType::Triangle) } else { picture };
    let mut out = vec![];
    picture.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).map_err(unreadable)?;
    if out.len() <= MAX_BYTES { return Ok(Image::from_bytes(ImageFormat::Png, out)) }
    // A photograph: JPEG, which has no transparency to keep.
    let mut out = vec![];
    image::DynamicImage::ImageRgb8(picture.to_rgb8()).write_to(&mut Cursor::new(&mut out), image::ImageFormat::Jpeg).map_err(unreadable)?;
    Ok(Image::from_bytes(ImageFormat::Jpeg, out))
}

#[cfg(test)]
mod tests {
    use super::{MAX_EDGE, format_of, prepare};
    use gpui::ImageFormat;
    use std::io::Cursor;

    fn encoded(width: u32, height: u32, format: image::ImageFormat) -> Vec<u8> {
        let mut out = vec![];
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(width, height, image::Rgb([200, 40, 40]))).write_to(&mut Cursor::new(&mut out), format).unwrap();
        out
    }

    #[test]
    fn pictures_are_made_sendable() {
        // A small PNG goes as it is.
        let small = encoded(40, 30, image::ImageFormat::Png);
        let kept = prepare(small.clone()).unwrap();
        assert_eq!((kept.format, &kept.bytes), (ImageFormat::Png, &small));
        // What the services do not read becomes PNG; what is too large is scaled down, keeping its shape.
        let bmp = prepare(encoded(40, 30, image::ImageFormat::Bmp)).unwrap();
        assert_eq!(bmp.format, ImageFormat::Png);
        let wide = prepare(encoded(MAX_EDGE * 2, 100, image::ImageFormat::Png)).unwrap();
        let size = image::load_from_memory(&wide.bytes).unwrap();
        assert_eq!((wide.format, size.width(), size.height()), (ImageFormat::Png, MAX_EDGE, 50));
        assert!(prepare(b"not a picture".to_vec()).unwrap_err().contains("无法读取"));
        assert_eq!((format_of("image/jpeg"), format_of("image/tiff")), (Some(ImageFormat::Jpeg), None));
    }
}
