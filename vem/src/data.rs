//! 外部数据容器（琴体 IR / 弓噪）。
//!
//! 走 C ABI 的 `vem_set_patch(blob, len)` 传入，格式：
//!
//! ```text
//! offset  size  field
//! 0       4     magic "VEMD"
//! 4       2     version (u16 LE) = 1
//! 6       1     instrument (u8)
//! 7       1     flags (u8): bit0 = 有琴体 IR，bit1 = 有弓噪
//! 8       4     body_len  (u32 LE, 采样数)
//! 12      4     noise_len (u32 LE, 采样数)
//! 16      ...   body:  f32[body_len]  (单声道 @48k)
//! ...     ...   noise: f32[noise_len] (单声道 @48k)
//! ```
//!
//! 用 f32 是为了开发期与参考实现逐采样对拍；交付版可换 i16（见 M6）。

pub const MAGIC: &[u8; 4] = b"VEMD";
pub const VERSION: u16 = 1;

pub struct VemData {
    pub body: Vec<f32>,
    pub noise: Vec<f32>,
}

impl VemData {
    pub fn parse(blob: &[u8]) -> Option<Self> {
        if blob.len() < 16 || &blob[0..4] != MAGIC {
            return None;
        }
        let version = u16::from_le_bytes([blob[4], blob[5]]);
        if version != VERSION {
            return None;
        }
        let flags = blob[7];
        let body_len = u32::from_le_bytes([blob[8], blob[9], blob[10], blob[11]]) as usize;
        let noise_len = u32::from_le_bytes([blob[12], blob[13], blob[14], blob[15]]) as usize;
        let need = 16 + 4 * (body_len + noise_len);
        if blob.len() < need {
            return None;
        }
        let mut body = Vec::with_capacity(body_len);
        let mut noise = Vec::with_capacity(noise_len);
        if flags & 1 != 0 {
            for i in 0..body_len {
                let o = 16 + i * 4;
                body.push(f32::from_le_bytes([blob[o], blob[o + 1], blob[o + 2], blob[o + 3]]));
            }
        }
        if flags & 2 != 0 {
            let base = 16 + 4 * body_len;
            for i in 0..noise_len {
                let o = base + i * 4;
                noise.push(f32::from_le_bytes([blob[o], blob[o + 1], blob[o + 2], blob[o + 3]]));
            }
        }
        Some(Self { body, noise })
    }

    pub fn write(body: &[f32], noise: &[f32], instrument: u8) -> Vec<u8> {
        let mut out = Vec::with_capacity(16 + 4 * (body.len() + noise.len()));
        let mut flags = 0u8;
        if !body.is_empty() {
            flags |= 1;
        }
        if !noise.is_empty() {
            flags |= 2;
        }
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.push(instrument);
        out.push(flags);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&(noise.len() as u32).to_le_bytes());
        for v in body {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in noise {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let body: Vec<f32> = (0..32).map(|i| i as f32 * 0.01).collect();
        let noise: Vec<f32> = (0..17).map(|i| -(i as f32) * 0.02).collect();
        let blob = VemData::write(&body, &noise, 2);
        let parsed = VemData::parse(&blob).expect("应能解析");
        assert_eq!(parsed.body, body);
        assert_eq!(parsed.noise, noise);
    }

    #[test]
    fn rejects_bad_blobs() {
        assert!(VemData::parse(b"").is_none());
        assert!(VemData::parse(b"VEMD").is_none());
        let mut blob = VemData::write(&[1.0], &[], 0);
        blob[4] = 99; // 版本不对
        assert!(VemData::parse(&blob).is_none());
    }
}
