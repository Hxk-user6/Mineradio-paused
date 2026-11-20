//! 汽水音乐加密音频流（MP4/FLAC AES-128-CTR）与 `spade_a` 密钥解密器。
//!
//! 精确移植自原 Node 实现 `qishui-audio-decryptor/`。

use aes::Aes128;
use base64::engine::general_purpose::{STANDARD as BASE64_STD, URL_SAFE as BASE64_URL};
use base64::Engine;
use cipher::{KeyIvInit, StreamCipher};
use serde::{Deserialize, Serialize};
use std::ops::Range;

type Aes128Ctr = ctr::Ctr128BE<Aes128>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecryptResult {
    pub buffer: Vec<u8>,
    pub extension: String,
    pub is_flac: bool,
    pub sample_count: usize,
    pub chunk_count: usize,
}

impl DecryptResult {
    /// 生成安全的文件名。
    pub fn create_file_name(&self, title: Option<&str>, artist: Option<&str>) -> String {
        let safe_title = sanitize_filename_part(title, "track");
        let safe_artist = sanitize_filename_part(artist, "unknown");
        format!("{safe_title} - {safe_artist}{}", self.extension)
    }
}

#[derive(Debug, Clone)]
struct Mp4Box {
    size: usize,
    offset: usize,
    data_range: Range<usize>,
}

impl Mp4Box {
    fn from_slice(buffer: &[u8], offset: usize) -> Option<Self> {
        if offset.checked_add(8)? > buffer.len() {
            return None;
        }
        let size = u32_be(buffer, offset) as usize;
        let end = if size >= 8 && offset.saturating_add(size) <= buffer.len() {
            offset + size
        } else {
            buffer.len()
        };
        let actual_size = end.saturating_sub(offset);
        if actual_size < 8 {
            return None;
        }
        Some(Self {
            size: actual_size,
            offset,
            data_range: (offset + 8)..end,
        })
    }

    fn find(buffer: &[u8], box_type: &[u8; 4], offset: usize, end: usize) -> Option<Self> {
        let limit = end.min(buffer.len());
        let mut pos = offset;
        while pos.saturating_add(8) <= limit {
            let size = u32_be(buffer, pos) as usize;
            if size < 8 || pos.saturating_add(size) > limit {
                break;
            }
            if &buffer[pos + 4..pos + 8] == box_type {
                return Self::from_slice(buffer, pos);
            }
            pos += size;
        }
        None
    }

    fn children_range(&self) -> (usize, usize) {
        (self.offset + 8, self.offset + self.size)
    }
}

#[inline]
fn u32_be(buf: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ])
}

#[inline]
fn bit_count(value: u32) -> i32 {
    value.count_ones() as i32
}

#[inline]
fn decode_base36(ch: u8) -> usize {
    if ch.is_ascii_digit() {
        (ch - b'0') as usize
    } else if (b'a'..=b'z').contains(&ch) {
        (ch - b'a' + 10) as usize
    } else {
        0xff
    }
}

fn decrypt_spade_inner(spade_key: &[u8]) -> Vec<u8> {
    let mut result = spade_key.to_vec();
    let mut working = Vec::with_capacity(spade_key.len() + 2);
    working.push(0xfa);
    working.push(0x55);
    working.extend_from_slice(spade_key);

    for (index, byte) in spade_key.iter().enumerate() {
        let mut value = (*byte ^ working[index]) as i32 - bit_count(index as u32) - 21;
        while value < 0 {
            value += 0xff;
        }
        result[index] = (value & 0xff) as u8;
    }
    result
}

fn decrypt_spade(spade_key_bytes: &[u8]) -> String {
    if spade_key_bytes.len() < 3 {
        return String::new();
    }
    let padding_len = ((spade_key_bytes[0] ^ spade_key_bytes[1] ^ spade_key_bytes[2]) as i32) - 48;
    if padding_len < 0 {
        return String::new();
    }
    let padding_len = padding_len as usize;
    if spade_key_bytes.len() < padding_len + 2 {
        return String::new();
    }
    let inner_end = spade_key_bytes.len() - padding_len;
    if inner_end <= 1 {
        return String::new();
    }
    let temp_buffer = decrypt_spade_inner(&spade_key_bytes[1..inner_end]);
    if temp_buffer.is_empty() {
        return String::new();
    }
    let skip_bytes = decode_base36(temp_buffer[0]);
    let decoded_message_length = spade_key_bytes.len() - padding_len - 2;
    let Some(end_index) = (1 + decoded_message_length).checked_sub(skip_bytes) else {
        return String::new();
    };
    if end_index > temp_buffer.len() || end_index < 1 {
        return String::new();
    }
    String::from_utf8(temp_buffer[1..end_index].to_vec()).unwrap_or_default()
}

/// 解密汽水音频响应中的 Base64 `spade_a` 密钥字段，返回十六进制明文密钥字符串。
pub fn decrypt_spade_a(spade_a: &str) -> String {
    let trimmed = spade_a.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let bytes = BASE64_STD
        .decode(trimmed)
        .or_else(|_| BASE64_URL.decode(trimmed));
    match bytes {
        Ok(raw) => decrypt_spade(&raw),
        Err(_) => String::new(),
    }
}

fn resolve_key(spade_a: &str) -> Result<[u8; 16], String> {
    let trimmed = spade_a.trim();
    if trimmed.is_empty() {
        return Err("spade_a is required for decryption.".to_string());
    }
    let is_hex = trimmed.bytes().all(|b| b.is_ascii_hexdigit());
    let key_hex = if is_hex {
        trimmed.to_string()
    } else {
        decrypt_spade_a(trimmed)
    };
    if key_hex.is_empty() {
        return Err("Failed to resolve decryption key from spade_a.".to_string());
    }
    if key_hex.len() % 2 != 0 {
        return Err("Hex string length must be even.".to_string());
    }
    let decoded = hex::decode(&key_hex).map_err(|e| format!("Invalid key hex: {e}"))?;
    if decoded.len() != 16 {
        return Err(format!(
            "Unexpected AES-128 key length: expected 16 bytes, got {}",
            decoded.len()
        ));
    }
    let mut key = [0u8; 16];
    key.copy_from_slice(&decoded);
    Ok(key)
}

fn parse_stsz(data: &[u8]) -> Result<Vec<usize>, String> {
    if data.len() < 12 {
        return Err("Invalid 'stsz' atom: truncated header.".to_string());
    }
    let sample_size = u32_be(data, 4) as usize;
    let count = u32_be(data, 8) as usize;
    if sample_size != 0 {
        return Ok(vec![sample_size; count]);
    }
    let needed = 12usize
        .checked_add(count.saturating_mul(4))
        .ok_or_else(|| "Invalid 'stsz' sample count overflow.".to_string())?;
    if data.len() < needed {
        return Err("Invalid 'stsz' atom: sample table truncated.".to_string());
    }
    let mut sizes = Vec::with_capacity(count);
    for index in 0..count {
        sizes.push(u32_be(data, 12 + index * 4) as usize);
    }
    Ok(sizes)
}

fn parse_senc(data: &[u8]) -> Result<Vec<[u8; 16]>, String> {
    if data.len() < 8 {
        return Err("Invalid 'senc' atom: truncated header.".to_string());
    }
    let count = u32_be(data, 4) as usize;
    let needed = 8usize
        .checked_add(count.saturating_mul(8))
        .ok_or_else(|| "Invalid 'senc' sample count overflow.".to_string())?;
    if data.len() < needed {
        return Err("Invalid 'senc' atom: IV table truncated.".to_string());
    }
    let mut ivs = Vec::with_capacity(count);
    let mut pos = 8;
    for _ in 0..count {
        let mut iv = [0u8; 16];
        iv[..8].copy_from_slice(&data[pos..pos + 8]);
        ivs.push(iv);
        pos += 8;
    }
    Ok(ivs)
}

fn scan_for_flac_metadata(stsd_data: &[u8]) -> &[u8] {
    let Some(index) = stsd_data.windows(4).position(|w| w == b"dfLa") else {
        return &[];
    };
    if index < 4 {
        return &[];
    }
    let box_size = u32_be(stsd_data, index - 4) as usize;
    let content_start = index + 4;
    let content_end = (index - 4).saturating_add(box_size).min(stsd_data.len());
    if content_end <= content_start {
        return &[];
    }
    &stsd_data[content_start..content_end]
}

fn replace_enca_with_mp4a(buffer: &mut [u8], search_start: usize, search_end: usize) {
    let end = search_end.min(buffer.len());
    if search_start + 4 > end {
        return;
    }
    if let Some(rel) = buffer[search_start..end]
        .windows(4)
        .position(|w| w == b"enca")
    {
        let idx = search_start + rel;
        buffer[idx..idx + 4].copy_from_slice(b"mp4a");
    }
}

fn sanitize_filename_part(value: Option<&str>, fallback: &str) -> String {
    let raw = value.unwrap_or(fallback);
    let normalized: String = raw
        .chars()
        .map(|c| {
            if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

fn aes_ctr_decrypt(key: &[u8; 16], iv: &[u8; 16], encrypted: &[u8]) -> Vec<u8> {
    let mut buf = encrypted.to_vec();
    let mut cipher = Aes128Ctr::new(key.into(), iv.into());
    cipher.apply_keystream(&mut buf);
    buf
}

/// 解密汽水音乐加密音频容器（MP4/FLAC）并还原为可直接播放的 `.m4a` 或 `.flac` 字节流。
pub fn decrypt_qishui_track(encrypted: &[u8], spade_a: &str) -> Result<DecryptResult, String> {
    if encrypted.is_empty() {
        return Err("encryptedBuffer must be non-empty.".to_string());
    }
    let key = resolve_key(spade_a)?;

    let moov = Mp4Box::find(encrypted, b"moov", 0, encrypted.len())
        .ok_or_else(|| "Decrypt failed: 'moov' atom not found.".to_string())?;
    let (moov_start, moov_end) = moov.children_range();

    let trak = Mp4Box::find(encrypted, b"trak", moov_start, moov_end)
        .ok_or_else(|| "Decrypt failed: 'trak' atom not found.".to_string())?;
    let (trak_start, trak_end) = trak.children_range();

    let mdia = Mp4Box::find(encrypted, b"mdia", trak_start, trak_end)
        .ok_or_else(|| "Decrypt failed: 'mdia' atom not found.".to_string())?;
    let (mdia_start, mdia_end) = mdia.children_range();

    let minf = Mp4Box::find(encrypted, b"minf", mdia_start, mdia_end)
        .ok_or_else(|| "Decrypt failed: 'minf' atom not found.".to_string())?;
    let (minf_start, minf_end) = minf.children_range();

    let stbl = Mp4Box::find(encrypted, b"stbl", minf_start, minf_end)
        .ok_or_else(|| "Decrypt failed: 'stbl' atom not found.".to_string())?;
    let (stbl_start, stbl_end) = stbl.children_range();

    let stsd = Mp4Box::find(encrypted, b"stsd", stbl_start, stbl_end)
        .ok_or_else(|| "Decrypt failed: 'stsd' atom not found.".to_string())?;
    let stsz = Mp4Box::find(encrypted, b"stsz", stbl_start, stbl_end)
        .ok_or_else(|| "Decrypt failed: 'stsz' atom not found.".to_string())?;
    let stco = Mp4Box::find(encrypted, b"stco", stbl_start, stbl_end);

    let senc = Mp4Box::find(encrypted, b"senc", moov_start, moov_end)
        .or_else(|| Mp4Box::find(encrypted, b"senc", stbl_start, stbl_end))
        .ok_or_else(|| "Decrypt failed: 'senc' atom not found.".to_string())?;

    let mdat = Mp4Box::find(encrypted, b"mdat", 0, encrypted.len())
        .ok_or_else(|| "Decrypt failed: 'mdat' atom not found.".to_string())?;

    let flac_metadata = scan_for_flac_metadata(&encrypted[stsd.data_range.clone()]);
    let is_flac = !flac_metadata.is_empty();

    let sample_sizes = parse_stsz(&encrypted[stsz.data_range.clone()])?;
    let chunk_count = stco
        .and_then(|box_info| {
            let data = &encrypted[box_info.data_range];
            (data.len() >= 8).then(|| u32_be(data, 4) as usize)
        })
        .unwrap_or(0);
    let ivs = parse_senc(&encrypted[senc.data_range.clone()])?;

    if sample_sizes.len() != ivs.len() {
        return Err(format!(
            "Decrypt failed: sample count {} does not match iv count {}.",
            sample_sizes.len(),
            ivs.len()
        ));
    }

    let mut decrypted_samples = Vec::with_capacity(sample_sizes.len());
    let mut sample_offset = mdat.offset + 8;
    for (index, &size) in sample_sizes.iter().enumerate() {
        let sample_end = sample_offset
            .checked_add(size)
            .ok_or_else(|| format!("Sample {index} offset overflow."))?;
        if sample_end > encrypted.len() {
            return Err(format!("Sample {index} exceeds encrypted buffer bounds."));
        }
        decrypted_samples.push(aes_ctr_decrypt(
            &key,
            &ivs[index],
            &encrypted[sample_offset..sample_end],
        ));
        sample_offset = sample_end;
    }

    let output = if is_flac {
        let meta_body = if flac_metadata.len() > 4 {
            &flac_metadata[4..]
        } else {
            flac_metadata
        };
        let total_audio: usize = decrypted_samples.iter().map(|s| s.len()).sum();
        let mut out = Vec::with_capacity(4 + meta_body.len() + total_audio);
        out.extend_from_slice(b"fLaC");
        out.extend_from_slice(meta_body);
        for sample in &decrypted_samples {
            out.extend_from_slice(sample);
        }
        out
    } else {
        let mut out = encrypted.to_vec();
        let mut write_ptr = mdat.offset + 8;
        for sample in &decrypted_samples {
            out[write_ptr..write_ptr + sample.len()].copy_from_slice(sample);
            write_ptr += sample.len();
        }
        replace_enca_with_mp4a(&mut out, stsd.offset, stsd.offset + stsd.size);
        out
    };

    Ok(DecryptResult {
        buffer: output,
        extension: if is_flac { ".flac" } else { ".m4a" }.to_string(),
        is_flac,
        sample_count: sample_sizes.len(),
        chunk_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_box(box_type: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = (8 + payload.len()) as u32;
        let mut out = Vec::with_capacity(size as usize);
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(box_type);
        out.extend_from_slice(payload);
        out
    }

    fn encode_spade_a_for_test(hex_key: &str, padding_len: usize, skip_bytes: usize) -> String {
        assert!(skip_bytes < 36);
        let skip_char = if skip_bytes < 10 {
            b'0' + skip_bytes as u8
        } else {
            b'a' + (skip_bytes as u8 - 10)
        };
        let mut plain = Vec::new();
        plain.push(skip_char);
        plain.extend_from_slice(hex_key.as_bytes());
        plain.extend(std::iter::repeat_n(b'x', skip_bytes));

        let mut enc_inner = vec![0u8; plain.len()];
        let prev_0 = 0xfa_u8;
        let prev_1 = 0x55_u8;
        for (i, &p) in plain.iter().enumerate() {
            let work = match i {
                0 => prev_0,
                1 => prev_1,
                _ => enc_inner[i - 2],
            };
            let target = (p as i32 + bit_count(i as u32) + 21).rem_euclid(255) as u8;
            enc_inner[i] = target ^ work;
        }

        // First byte of full buffer + first two bytes of enc_inner determine padding_len:
        // (b0 ^ b1 ^ b2) - 48 == padding_len
        let b0 = ((padding_len + 48) as u8) ^ enc_inner[0] ^ enc_inner[1];
        let mut full = Vec::new();
        full.push(b0);
        full.extend_from_slice(&enc_inner);
        full.extend(std::iter::repeat_n(0x42, padding_len));
        BASE64_STD.encode(full)
    }

    fn build_encrypted_mp4(
        key: &[u8; 16],
        samples: &[&[u8]],
        ivs_8byte: &[[u8; 8]],
        flac_dfla_payload: Option<&[u8]>,
    ) -> Vec<u8> {
        // stsd
        let stsd_payload = if let Some(dfla) = flac_dfla_payload {
            let dfla_box = make_box(b"dfLa", dfla);
            let mut p = vec![0u8; 8];
            p.extend_from_slice(&dfla_box);
            p
        } else {
            let mut p = vec![0u8; 8];
            p.extend_from_slice(b"enca\x00\x00\x00\x01");
            p
        };
        let stsd = make_box(b"stsd", &stsd_payload);

        // stsz (variable sizes)
        let mut stsz_payload = Vec::new();
        stsz_payload.extend_from_slice(&[0u8; 4]); // version + flags
        stsz_payload.extend_from_slice(&0u32.to_be_bytes()); // uniform sample size = 0
        stsz_payload.extend_from_slice(&(samples.len() as u32).to_be_bytes());
        for s in samples {
            stsz_payload.extend_from_slice(&(s.len() as u32).to_be_bytes());
        }
        let stsz = make_box(b"stsz", &stsz_payload);

        // stsc
        let mut stsc_payload = vec![0u8; 8];
        stsc_payload[7] = 1; // 1 entry
        stsc_payload.extend_from_slice(&1u32.to_be_bytes());
        stsc_payload.extend_from_slice(&(samples.len() as u32).to_be_bytes());
        stsc_payload.extend_from_slice(&1u32.to_be_bytes());
        let stsc = make_box(b"stsc", &stsc_payload);

        // stco
        let mut stco_payload = vec![0u8; 8];
        stco_payload[4..8].copy_from_slice(&1u32.to_be_bytes()); // 1 chunk
        stco_payload.extend_from_slice(&0u32.to_be_bytes());
        let stco = make_box(b"stco", &stco_payload);

        // senc
        let mut senc_payload = Vec::new();
        senc_payload.extend_from_slice(&[0u8; 4]); // version + flags
        senc_payload.extend_from_slice(&(ivs_8byte.len() as u32).to_be_bytes());
        for iv8 in ivs_8byte {
            senc_payload.extend_from_slice(iv8);
        }
        let senc = make_box(b"senc", &senc_payload);

        let mut stbl_payload = Vec::new();
        stbl_payload.extend_from_slice(&stsd);
        stbl_payload.extend_from_slice(&stsz);
        stbl_payload.extend_from_slice(&stsc);
        stbl_payload.extend_from_slice(&stco);
        stbl_payload.extend_from_slice(&senc);
        let stbl = make_box(b"stbl", &stbl_payload);
        let minf = make_box(b"minf", &stbl);
        let mdia = make_box(b"mdia", &minf);
        let trak = make_box(b"trak", &mdia);
        let moov = make_box(b"moov", &trak);

        // Encrypt samples using AES-128-CTR
        let mut mdat_payload = Vec::new();
        for (idx, plain) in samples.iter().enumerate() {
            let mut iv16 = [0u8; 16];
            iv16[..8].copy_from_slice(&ivs_8byte[idx]);
            let cipher = aes_ctr_decrypt(key, &iv16, plain);
            mdat_payload.extend_from_slice(&cipher);
        }
        let mdat = make_box(b"mdat", &mdat_payload);

        let mut container = make_box(b"ftyp", b"M4A \x00\x00\x00\x00");
        container.extend_from_slice(&moov);
        container.extend_from_slice(&mdat);
        container
    }

    #[test]
    fn spade_a_roundtrip_and_invalid_inputs() {
        let hex_key = "00112233445566778899aabbccddeeff";
        let encoded = encode_spade_a_for_test(hex_key, 3, 2);
        assert_eq!(decrypt_spade_a(&encoded), hex_key);
        assert_eq!(decrypt_spade_a(""), "");
        assert_eq!(decrypt_spade_a("invalid-not-base64!!!"), "");
    }

    #[test]
    fn decrypts_encrypted_m4a_container_and_rewrites_enca_to_mp4a() {
        let key = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        let spade_a = encode_spade_a_for_test("00112233445566778899aabbccddeeff", 4, 1);
        let sample1 = b"aac-frame-one-plaintext";
        let sample2 = b"aac-frame-two-plaintext-extended";
        let ivs = [[1, 2, 3, 4, 5, 6, 7, 8], [9, 10, 11, 12, 13, 14, 15, 16]];
        let encrypted = build_encrypted_mp4(&key, &[sample1, sample2], &ivs, None);

        let res = decrypt_qishui_track(&encrypted, &spade_a).expect("m4a decrypt should succeed");
        assert!(!res.is_flac);
        assert_eq!(res.extension, ".m4a");
        assert_eq!(res.sample_count, 2);
        assert_eq!(res.chunk_count, 1);

        // Verify enca -> mp4a replacement in stsd and no enca remains
        assert!(res.buffer.windows(4).any(|w| w == b"mp4a"));
        assert!(!res.buffer.windows(4).any(|w| w == b"enca"));

        // Verify decrypted mdat payload matches plaintext concatenation
        let mdat = Mp4Box::find(&res.buffer, b"mdat", 0, res.buffer.len()).unwrap();
        let expected_mdat: Vec<u8> = [sample1.as_slice(), sample2.as_slice()].concat();
        assert_eq!(&res.buffer[mdat.data_range], expected_mdat.as_slice());
    }

    #[test]
    fn decrypts_encrypted_flac_container_with_dfla_metadata() {
        let key = [0xab; 16];
        let spade_a_hex = hex::encode(key);
        let dfla_payload = b"\x00\x00\x00\x00\x80\x00\x00\x22STREAMINFO_BYTES";
        let frame1 = b"\xff\xf8\x69\x00flac-frame-1";
        let frame2 = b"\xff\xf8\x69\x00flac-frame-2";
        let ivs = [[0x11; 8], [0x22; 8]];
        let encrypted = build_encrypted_mp4(&key, &[frame1, frame2], &ivs, Some(dfla_payload));

        let res =
            decrypt_qishui_track(&encrypted, &spade_a_hex).expect("flac decrypt should succeed");
        assert!(res.is_flac);
        assert_eq!(res.extension, ".flac");
        assert_eq!(res.sample_count, 2);
        assert!(res.buffer.starts_with(b"fLaC\x80\x00\x00\x22STREAMINFO_BYTES"));
        assert!(res.buffer.ends_with(b"\xff\xf8\x69\x00flac-frame-1\xff\xf8\x69\x00flac-frame-2"));
    }
}
