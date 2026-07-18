use base64::Engine as _;
use thiserror::Error;

use crate::config::ProviderRequestLimits;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatedImageMetadata {
    pub mime_type: &'static str,
    pub decoded_bytes: u64,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ImageValidationError {
    #[error("image MIME type is not allowed")]
    MimeNotAllowed,
    #[error("image content does not match its declared MIME type")]
    MimeMismatch,
    #[error("image base64 payload exceeds the per-image limit")]
    EncodedPayloadTooLarge,
    #[error("image base64 payload is invalid")]
    InvalidBase64,
    #[error("decoded image exceeds the per-image byte limit")]
    DecodedPayloadTooLarge,
    #[error("image content is malformed or uses an unsupported encoding")]
    MalformedImage,
    #[error("image dimensions exceed the configured limit")]
    DimensionsTooLarge,
    #[error("image pixel count exceeds the configured limit")]
    PixelCountTooLarge,
}

pub fn validate_image_payload(
    declared_mime_type: &str,
    data_base64: &str,
    limits: ProviderRequestLimits,
) -> Result<ValidatedImageMetadata, ImageValidationError> {
    if !limits.allows_image_mime_type(declared_mime_type) {
        return Err(ImageValidationError::MimeNotAllowed);
    }
    let max_encoded_chars =
        4_u64.saturating_mul(limits.max_single_image_decoded_bytes.saturating_add(2) / 3);
    if data_base64.len() as u64 > max_encoded_chars {
        return Err(ImageValidationError::EncodedPayloadTooLarge);
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_base64)
        .map_err(|_| ImageValidationError::InvalidBase64)?;
    validate_image_bytes(declared_mime_type, &bytes, limits)
}

pub fn validate_image_bytes(
    declared_mime_type: &str,
    bytes: &[u8],
    limits: ProviderRequestLimits,
) -> Result<ValidatedImageMetadata, ImageValidationError> {
    if !limits.allows_image_mime_type(declared_mime_type) {
        return Err(ImageValidationError::MimeNotAllowed);
    }
    if bytes.len() as u64 > limits.max_single_image_decoded_bytes {
        return Err(ImageValidationError::DecodedPayloadTooLarge);
    }
    let (actual_mime_type, width, height) = sniff_image_dimensions(bytes)?;
    if actual_mime_type != declared_mime_type {
        return Err(ImageValidationError::MimeMismatch);
    }
    if width == 0
        || height == 0
        || width > limits.max_image_width
        || height > limits.max_image_height
    {
        return Err(ImageValidationError::DimensionsTooLarge);
    }
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if pixels > limits.max_image_pixels {
        return Err(ImageValidationError::PixelCountTooLarge);
    }
    Ok(ValidatedImageMetadata {
        mime_type: actual_mime_type,
        decoded_bytes: bytes.len() as u64,
        width,
        height,
    })
}

fn sniff_image_dimensions(bytes: &[u8]) -> Result<(&'static str, u32, u32), ImageValidationError> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        if bytes.len() < 24 || &bytes[12..16] != b"IHDR" {
            return Err(ImageValidationError::MalformedImage);
        }
        return Ok((
            "image/png",
            u32::from_be_bytes(bytes[16..20].try_into().unwrap()),
            u32::from_be_bytes(bytes[20..24].try_into().unwrap()),
        ));
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        if bytes.len() < 10 {
            return Err(ImageValidationError::MalformedImage);
        }
        return Ok((
            "image/gif",
            u16::from_le_bytes(bytes[6..8].try_into().unwrap()).into(),
            u16::from_le_bytes(bytes[8..10].try_into().unwrap()).into(),
        ));
    }
    if bytes.starts_with(&[0xff, 0xd8]) {
        let (width, height) = jpeg_dimensions(bytes)?;
        return Ok(("image/jpeg", width, height));
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        let (width, height) = webp_dimensions(bytes)?;
        return Ok(("image/webp", width, height));
    }
    Err(ImageValidationError::MalformedImage)
}

fn jpeg_dimensions(bytes: &[u8]) -> Result<(u32, u32), ImageValidationError> {
    let mut offset = 2_usize;
    while offset < bytes.len() {
        while offset < bytes.len() && bytes[offset] == 0xff {
            offset += 1;
        }
        let Some(&marker) = bytes.get(offset) else {
            break;
        };
        offset += 1;
        if matches!(marker, 0x01 | 0xd0..=0xd9) {
            if matches!(marker, 0xd9) {
                break;
            }
            continue;
        }
        let length_bytes: [u8; 2] = bytes
            .get(offset..offset + 2)
            .ok_or(ImageValidationError::MalformedImage)?
            .try_into()
            .unwrap();
        let length = usize::from(u16::from_be_bytes(length_bytes));
        if length < 2 || offset.saturating_add(length) > bytes.len() {
            return Err(ImageValidationError::MalformedImage);
        }
        if matches!(
            marker,
            0xc0 | 0xc1
                | 0xc2
                | 0xc3
                | 0xc5
                | 0xc6
                | 0xc7
                | 0xc9
                | 0xca
                | 0xcb
                | 0xcd
                | 0xce
                | 0xcf
        ) {
            if length < 7 {
                return Err(ImageValidationError::MalformedImage);
            }
            let height = u16::from_be_bytes(bytes[offset + 3..offset + 5].try_into().unwrap());
            let width = u16::from_be_bytes(bytes[offset + 5..offset + 7].try_into().unwrap());
            return Ok((width.into(), height.into()));
        }
        if marker == 0xda {
            break;
        }
        offset += length;
    }
    Err(ImageValidationError::MalformedImage)
}

fn webp_dimensions(bytes: &[u8]) -> Result<(u32, u32), ImageValidationError> {
    let mut offset = 12_usize;
    while offset.saturating_add(8) <= bytes.len() {
        let chunk_type = &bytes[offset..offset + 4];
        let chunk_size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        let data_start = offset + 8;
        let data_end = data_start
            .checked_add(chunk_size as usize)
            .ok_or(ImageValidationError::MalformedImage)?;
        if data_end > bytes.len() {
            return Err(ImageValidationError::MalformedImage);
        }
        let data = &bytes[data_start..data_end];
        if chunk_type == b"VP8X" && data.len() >= 10 {
            let width = 1 + u32::from_le_bytes([data[4], data[5], data[6], 0]);
            let height = 1 + u32::from_le_bytes([data[7], data[8], data[9], 0]);
            return Ok((width, height));
        }
        if chunk_type == b"VP8L" && data.len() >= 5 && data[0] == 0x2f {
            let bits = u32::from_le_bytes(data[1..5].try_into().unwrap());
            return Ok((1 + (bits & 0x3fff), 1 + ((bits >> 14) & 0x3fff)));
        }
        if chunk_type == b"VP8 " && data.len() >= 10 && data[3..6] == [0x9d, 0x01, 0x2a] {
            let width = u16::from_le_bytes(data[6..8].try_into().unwrap()) & 0x3fff;
            let height = u16::from_le_bytes(data[8..10].try_into().unwrap()) & 0x3fff;
            return Ok((width.into(), height.into()));
        }
        offset = data_end.saturating_add((chunk_size & 1) as usize);
    }
    Err(ImageValidationError::MalformedImage)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend(width.to_be_bytes());
        bytes.extend(height.to_be_bytes());
        bytes
    }

    #[test]
    fn validates_bounded_png_payload_and_reports_metadata() {
        let payload = base64::engine::general_purpose::STANDARD.encode(png(640, 480));
        let metadata = validate_image_payload(
            "image/png",
            &payload,
            ProviderRequestLimits::product_default(),
        )
        .expect("valid PNG");

        assert_eq!(metadata.mime_type, "image/png");
        assert_eq!((metadata.width, metadata.height), (640, 480));
    }

    #[test]
    fn rejects_declared_mime_that_disagrees_with_magic_bytes() {
        assert_eq!(
            validate_image_bytes(
                "image/jpeg",
                &png(1, 1),
                ProviderRequestLimits::product_default(),
            ),
            Err(ImageValidationError::MimeMismatch)
        );
    }

    #[test]
    fn rejects_dimensions_before_the_payload_can_enter_a_request() {
        assert_eq!(
            validate_image_bytes(
                "image/png",
                &png(16_385, 1),
                ProviderRequestLimits::product_default(),
            ),
            Err(ImageValidationError::DimensionsTooLarge)
        );
    }
}
