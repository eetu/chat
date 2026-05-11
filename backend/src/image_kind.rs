//! Magic-byte sniff for uploaded image bytes.
//!
//! Stored MIME and the Content-Type used on outbound requests (ComfyUI
//! upload, `/api/conversations/{cid}/messages/{mid}/image/{idx}`) should
//! reflect what the bytes actually are — the client claim is unsafe to
//! trust on its own and was, until recently, simply being overwritten
//! with `image/png` for every attachment regardless of source format.

/// Detect a supported image MIME from the first bytes of the buffer.
/// Returns `None` for everything we don't accept.
pub fn detect(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else {
        None
    }
}

/// Filename extension corresponding to a detected MIME. Used when we hand
/// bytes off to an upstream that infers format from the filename.
pub fn extension(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "bin",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_png() {
        let png = b"\x89PNG\r\n\x1a\nrest";
        assert_eq!(detect(png), Some("image/png"));
        assert_eq!(extension("image/png"), "png");
    }

    #[test]
    fn detects_jpeg() {
        let jpeg = b"\xff\xd8\xff\xe0...";
        assert_eq!(detect(jpeg), Some("image/jpeg"));
    }

    #[test]
    fn detects_webp() {
        let mut webp = Vec::from(*b"RIFF\0\0\0\0WEBPVP8 ");
        webp.extend_from_slice(b"...");
        assert_eq!(detect(&webp), Some("image/webp"));
    }

    #[test]
    fn rejects_random_bytes() {
        assert_eq!(detect(b"hello world"), None);
        assert_eq!(detect(&[0u8; 4]), None);
        assert_eq!(detect(&[]), None);
    }

    #[test]
    fn rejects_riff_but_not_webp() {
        // RIFF .... AVI  → would be a video, not an image
        let avi = b"RIFF\0\0\0\0AVI rest";
        assert_eq!(detect(avi), None);
    }
}
