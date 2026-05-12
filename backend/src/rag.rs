//! Helpers for the retrieval-augmented generation path.
//!
//! Keeping this lean for the first slice: in-memory cosine over
//! BLOB-encoded f32 embeddings stored alongside the chunk text in
//! SQLite. Scales fine to hundreds of chunks; switch to sqlite-vec or
//! a proper ANN index when the doc set outgrows it.

const CHUNK_CHARS: usize = 800;
const CHUNK_OVERLAP: usize = 120;

/// Split a document into roughly-equal character-windowed chunks with
/// a fixed overlap. Paragraph-aware splitting would be better but
/// overlap covers most of the loss for free, and chars are cheap to
/// reason about across multibyte input.
pub fn chunk_text(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let end = (start + CHUNK_CHARS).min(chars.len());
        let slice: String = chars[start..end].iter().collect();
        let trimmed = slice.trim().to_string();
        if !trimmed.is_empty() {
            out.push(trimmed);
        }
        if end == chars.len() {
            break;
        }
        start = end.saturating_sub(CHUNK_OVERLAP);
    }
    out
}

/// Cosine similarity. Returns 0.0 for either zero-length vector to
/// keep the caller's ranking math defined.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Pack a Vec<f32> into a little-endian byte buffer for BLOB storage.
pub fn embedding_to_bytes(values: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for v in values {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Reverse of `embedding_to_bytes`. Truncates trailing bytes that
/// don't fit a full f32 — defensive against partial / corrupt rows.
pub fn embedding_from_bytes(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_text_short_input_returns_one() {
        let chunks = chunk_text("hello world");
        assert_eq!(chunks, vec!["hello world".to_string()]);
    }

    #[test]
    fn chunk_text_long_input_overlaps() {
        let long = "a".repeat(2000);
        let chunks = chunk_text(&long);
        assert!(chunks.len() >= 2);
        // Each chunk shouldn't exceed the soft cap.
        for c in &chunks {
            assert!(c.chars().count() <= CHUNK_CHARS);
        }
    }

    #[test]
    fn cosine_orthogonal_zero() {
        let a = [1.0, 0.0];
        let b = [0.0, 1.0];
        assert!((cosine(&a, &b) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_identical_one() {
        let a = [0.1, 0.2, 0.3];
        let v = cosine(&a, &a);
        assert!((v - 1.0).abs() < 1e-6);
    }

    #[test]
    fn embedding_roundtrip() {
        let v = vec![0.1f32, -2.5, 3.14, 1e-3];
        let bytes = embedding_to_bytes(&v);
        let back = embedding_from_bytes(&bytes);
        assert_eq!(v.len(), back.len());
        for (a, b) in v.iter().zip(back) {
            assert!((a - b).abs() < 1e-7);
        }
    }
}
