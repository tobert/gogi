//! Small media files that a served model can decode: a PNG of 8 by 8 pixels and a WAV of a short tone. A model that
//! reads `image/png` or `audio/wav` decodes the object of a part, so bytes that are not a file of that type are a 400
//! that names the part. Each label gives other pixels or another tone, so two labels give two objects.

/// The bytes of a file of `mime` for `label`: a PNG for `image/png`, a WAV for `audio/wav`, and `None` for another type.
pub(crate) fn media_file(mime: &str, label: &str) -> Option<Vec<u8>> {
    match mime {
        "image/png" => Some(png(label)),
        "audio/wav" => Some(wav(label)),
        _ => None,
    }
}

/// The FNV-1a hash of the label: the seed of the pixels and of the tone.
fn seed(label: &str) -> u32 {
    label.bytes().fold(0x811c_9dc5u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for b in bytes {
        crc ^= u32::from(*b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for x in bytes {
        a = (a + u32::from(*x)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend((data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend(kind);
    out.extend(data);
    let crc = crc32(&out[start..]);
    out.extend(crc.to_be_bytes());
}

/// A PNG of 8 by 8 RGB pixels in 8 bits, with no filter, in one stored block of zlib.
fn png(label: &str) -> Vec<u8> {
    const SIDE: u32 = 8;
    let mut state = seed(label);
    let mut next = || {
        state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        (state >> 16) as u8
    };
    // Each row is the filter byte 0, none, then its RGB values.
    let raw: Vec<u8> = (0..SIDE)
        .flat_map(|_| {
            std::iter::once(0)
                .chain((0..SIDE * 3).map(|_| next()))
                .collect::<Vec<u8>>()
        })
        .collect();
    let mut zlib = vec![0x78, 0x01, 0x01];
    zlib.extend((raw.len() as u16).to_le_bytes());
    zlib.extend((!(raw.len() as u16)).to_le_bytes());
    zlib.extend(&raw);
    zlib.extend(adler32(&raw).to_be_bytes());
    let mut header = Vec::new();
    header.extend(SIDE.to_be_bytes());
    header.extend(SIDE.to_be_bytes());
    header.extend([8, 2, 0, 0, 0]); // 8 bits, RGB, deflate, no filter set, no interlace
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &zlib);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// The rate, the channels and the length of the WAV.
const RATE: u32 = 16_000;
const SAMPLES: u32 = 4_000;

/// A WAV of 0.25 s of one tone: PCM in 16 bits, mono, at 16 kHz. The label sets the tone, from 200 to 1,223 Hz.
fn wav(label: &str) -> Vec<u8> {
    let hz = 200.0 + f64::from(seed(label) % 1_024);
    let data: Vec<u8> = (0..SAMPLES)
        .flat_map(|i| {
            {
                let t = f64::from(i) / f64::from(RATE);
                ((t * hz * std::f64::consts::TAU).sin() * 8_000.0) as i16
            }
            .to_le_bytes()
        })
        .collect();
    let mut out = b"RIFF".to_vec();
    out.extend((36 + data.len() as u32).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes()); // PCM
    out.extend(1u16.to_le_bytes()); // mono
    out.extend(RATE.to_le_bytes());
    out.extend((RATE * 2).to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(data);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_png_has_the_signature_and_chunks_whose_crcs_check() {
        let bytes = png("media-a-1");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let mut at = 8;
        let mut kinds = Vec::new();
        while at < bytes.len() {
            let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            let body = &bytes[at + 4..at + 8 + len];
            let crc = u32::from_be_bytes(bytes[at + 8 + len..at + 12 + len].try_into().unwrap());
            assert_eq!(crc32(body), crc, "the crc of {:?}", &body[..4]);
            kinds.push(String::from_utf8(body[..4].to_vec()).unwrap());
            at += 12 + len;
        }
        assert_eq!(kinds, ["IHDR", "IDAT", "IEND"]);
        // The IDAT is one stored block of 8 rows of a filter byte and 24 values, with its adler32.
        let idat = &bytes[8 + 25 + 8..];
        assert_eq!(&idat[..3], [0x78, 0x01, 0x01]);
        assert_eq!(u16::from_le_bytes([idat[3], idat[4]]), 200);
        assert_eq!(adler32(&[]), 1);
        assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn a_wav_is_pcm_16_bits_mono_at_16_khz() {
        let bytes = wav("media-a-1");
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(u16::from_le_bytes([bytes[22], bytes[23]]), 1);
        assert_eq!(
            u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            16_000
        );
        assert_eq!(u16::from_le_bytes([bytes[34], bytes[35]]), 16);
        assert_eq!(bytes.len(), 44 + 8_000);
    }

    #[test]
    fn two_labels_give_two_files_and_another_type_gives_none() {
        for mime in ["image/png", "audio/wav"] {
            assert_ne!(
                media_file(mime, "media-a-1"),
                media_file(mime, "media-b-1"),
                "{mime}"
            );
            assert_eq!(
                media_file(mime, "media-a-1"),
                media_file(mime, "media-a-1"),
                "{mime}"
            );
        }
        assert_eq!(&media_file("image/png", "a").unwrap()[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&media_file("audio/wav", "a").unwrap()[..4], b"RIFF");
        assert_eq!(media_file("image/x-gogi-conformance", "media-a-1"), None);
    }
}
