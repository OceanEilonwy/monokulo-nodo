//! QR codes for "Manage via web" and the one-time web password link, as images one pixel per
//! module that Slint scales up pixelated. The white card around them is the quiet zone.

/// The dark and light modules of `data`'s QR code, row by row, and its side.
pub fn modules(data: &str) -> Option<(usize, Vec<bool>)> {
    let code = qrcodegen::QrCode::encode_text(data, qrcodegen::QrCodeEcc::Medium).ok()?;
    let size = code.size();
    let mut out = Vec::with_capacity((size * size) as usize);
    for y in 0..size {
        for x in 0..size {
            out.push(code.get_module(x, y));
        }
    }
    Some((size as usize, out))
}

/// `data` as a black-on-white image; an empty image when there is nothing to encode.
pub fn image(data: &str) -> slint::Image {
    let Some((size, dark)) = modules(data).filter(|_| !data.is_empty()) else { return slint::Image::default() };
    let mut buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(size as u32, size as u32);
    for (pixel, dark) in buffer.make_mut_slice().iter_mut().zip(dark) {
        let v = if dark { 0 } else { 255 };
        *pixel = slint::Rgb8Pixel { r: v, g: v, b: v };
    }
    slint::Image::from_rgb8(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_makes_a_square_code() {
        let (size, dark) = modules("https://192.168.1.155").expect("it fits");
        assert_eq!(dark.len(), size * size);
        assert!(size >= 21, "version 1 or more");
        // The finder pattern's corner is dark.
        assert!(dark[0]);
        assert_eq!(image("").size().width, 0);
    }
}
