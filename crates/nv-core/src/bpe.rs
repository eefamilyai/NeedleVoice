//! Minimal SentencePiece-BPE encoder, used to turn the wake phrase ("hey
//! nova") into the sub-word tokens the keyword-spotting model expects
//! ("▁HE Y ▁NO VA"). Reads the `bpe.model` protobuf directly.

use std::collections::HashMap;
use std::path::Path;

pub struct Bpe {
    scores: HashMap<String, f32>,
}

fn varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let b = *buf.get(*pos)?;
        *pos += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some(v);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

/// Skip one protobuf field of the given wire type.
fn skip(buf: &[u8], pos: &mut usize, wire: u64) -> Option<()> {
    match wire {
        0 => {
            varint(buf, pos)?;
        }
        1 => *pos += 8,
        2 => {
            let len = varint(buf, pos)? as usize;
            *pos += len;
        }
        5 => *pos += 4,
        _ => return None,
    }
    (*pos <= buf.len()).then_some(())
}

impl Bpe {
    pub fn load(path: &Path) -> Option<Bpe> {
        let buf = std::fs::read(path).ok()?;
        let mut scores = HashMap::new();
        let mut pos = 0;
        while pos < buf.len() {
            let key = varint(&buf, &mut pos)?;
            let (field, wire) = (key >> 3, key & 7);
            if field == 1 && wire == 2 {
                // SentencePiece { 1: piece, 2: score, 3: type }
                let len = varint(&buf, &mut pos)? as usize;
                let end = pos + len;
                let (mut piece, mut score, mut kind) = (String::new(), 0f32, 1u64);
                while pos < end {
                    let k = varint(&buf, &mut pos)?;
                    match (k >> 3, k & 7) {
                        (1, 2) => {
                            let l = varint(&buf, &mut pos)? as usize;
                            piece = String::from_utf8_lossy(buf.get(pos..pos + l)?).into_owned();
                            pos += l;
                        }
                        (2, 5) => {
                            score = f32::from_le_bytes(buf.get(pos..pos + 4)?.try_into().ok()?);
                            pos += 4;
                        }
                        (3, 0) => kind = varint(&buf, &mut pos)?,
                        (_, w) => skip(&buf, &mut pos, w)?,
                    }
                }
                pos = end;
                // type 1 = normal piece; skip control/unknown pieces.
                if kind == 1 || kind == 4 {
                    scores.insert(piece, score);
                }
            } else {
                skip(&buf, &mut pos, wire)?;
            }
        }
        (!scores.is_empty()).then_some(Bpe { scores })
    }

    /// Encode text into pieces, e.g. "hey siri" → ["▁HE", "Y", "▁S", "I", "RI"].
    pub fn encode(&self, text: &str) -> Vec<String> {
        let mut out = Vec::new();
        for word in text.split_whitespace() {
            // Unigram (Viterbi): the segmentation with the highest total score.
            let chars: Vec<char> = format!("▁{}", word.to_uppercase()).chars().collect();
            let n = chars.len();
            let mut best: Vec<Option<(f32, usize)>> = vec![None; n + 1];
            best[0] = Some((0.0, 0));
            for end in 1..=n {
                for start in 0..end {
                    let Some((s0, _)) = best[start] else { continue };
                    let piece: String = chars[start..end].iter().collect();
                    // Single characters missing from the vocab still get through, heavily penalised.
                    let score = match self.scores.get(&piece) {
                        Some(&s) => s,
                        None if end - start == 1 => -100.0,
                        None => continue,
                    };
                    if best[end].map_or(true, |(b, _)| s0 + score > b) {
                        best[end] = Some((s0 + score, start));
                    }
                }
            }
            let mut pieces = Vec::new();
            let mut end = n;
            while end > 0 {
                let (_, start) = best[end].unwrap();
                pieces.push(chars[start..end].iter().collect::<String>());
                end = start;
            }
            pieces.reverse();
            out.extend(pieces);
        }
        out
    }

    pub fn has(&self, piece: &str) -> bool {
        self.scores.contains_key(piece)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_reference_keywords() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../models/kws/sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01");
        let Some(bpe) = Bpe::load(&dir.join("bpe.model")) else {
            eprintln!("kws model not present; skipping");
            return;
        };
        let (Ok(raw), Ok(tok)) = (
            std::fs::read_to_string(dir.join("keywords_raw.txt")),
            std::fs::read_to_string(dir.join("keywords.txt")),
        ) else {
            eprintln!("reference keywords not installed; skipping");
            return;
        };
        for (r, t) in raw.lines().zip(tok.lines()) {
            assert_eq!(bpe.encode(r).join(" "), t.trim(), "{r}");
        }
    }
}
