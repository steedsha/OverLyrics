//! Cover art as a data URI.
//!
//! The SMTC thumbnail is a `IRandomAccessStreamReference`; reading it yields
//! encoded bytes (PNG/JPEG/…), which the renderer decodes itself. This module
//! only sniffs the MIME type and base64-encodes the bytes — decoding here would
//! add an image codec to a process that already has one.

/// Covers are capped so a pathological embed cannot balloon every packet.
pub const MAX_COVER_BYTES: usize = 8 * 1024 * 1024;

/// `data:<mime>;base64,<payload>` from raw bytes, or `None` when empty.
pub fn data_uri(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    let mime = sniff_mime(bytes);
    Some(format!("data:{mime};base64,{}", base64(bytes)))
}

/// Magic-number sniffing. Unknown formats are labelled `image/jpeg`, which is
/// what the reference bridge does: the renderer sniffs again, and the label only
/// has to be plausible for a webview that no longer exists.
pub fn sniff_mime(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else if bytes.starts_with(b"GIF8") {
        "image/gif"
    } else if bytes.starts_with(b"BM") {
        "image/bmp"
    } else {
        "image/jpeg"
    }
}

/// Standard alphabet, padded base64. Small enough that a dependency would cost
/// more than the twenty lines it replaces.
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18) as usize & 0x3F] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3F] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3F] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3F] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(windows)]
pub fn read_cover(
    reference: &windows::Media::Control::GlobalSystemMediaTransportControlsSessionMediaProperties,
) -> Option<String> {
    let thumbnail = reference.Thumbnail().ok()?;
    let stream = thumbnail.OpenReadAsync().and_then(|op| op.join()).ok()?;
    let size = stream.Size().ok()?;
    if size == 0 || size > MAX_COVER_BYTES as u64 {
        return None;
    }
    let input = stream.GetInputStreamAt(0).ok()?;
    let reader = windows::Storage::Streams::DataReader::CreateDataReader(&input).ok()?;
    let loaded = reader
        .LoadAsync(size as u32)
        .and_then(|op| op.join())
        .ok()?;
    if loaded == 0 {
        return None;
    }
    let mut bytes = vec![0u8; loaded as usize];
    reader.ReadBytes(&mut bytes).ok()?;
    data_uri(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet_with_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn mime_sniffing_prefers_the_magic_number_over_any_extension() {
        assert_eq!(sniff_mime(&[0x89, b'P', b'N', b'G', 1]), "image/png");
        assert_eq!(sniff_mime(&[0xFF, 0xD8, 0xFF, 0]), "image/jpeg");
        assert_eq!(sniff_mime(b"RIFF0000WEBPxx"), "image/webp");
        assert_eq!(sniff_mime(b"GIF89a"), "image/gif");
        assert_eq!(sniff_mime(&[0, 1, 2]), "image/jpeg");
        assert_eq!(data_uri(&[]), None);
        assert!(data_uri(&[0x89, b'P', b'N', b'G']).unwrap().starts_with("data:image/png;base64,"));
    }
}
