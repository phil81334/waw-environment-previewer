//! Read the installed revive mask. No proprietary image is bundled with an app.
use crate::{assets::AssetReader, Texture, TextureMip};
use serde::Serialize;
use std::{collections::HashMap, path::Path};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VisionMask {
    pub texture: Texture,
    pub bytes: Vec<u8>,
}

fn colour565(value: u16) -> [u8; 4] {
    let red = ((value >> 11) & 31) as u8;
    let green = ((value >> 5) & 63) as u8;
    let blue = (value & 31) as u8;
    [
        (red << 3) | (red >> 2),
        (green << 2) | (green >> 4),
        (blue << 3) | (blue >> 2),
        255,
    ]
}

fn decode_bc1(bytes: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let expected = width.div_ceil(4) as usize * height.div_ceil(4) as usize * 8;
    if bytes.len() != expected || width > 512 || height > 512 {
        return Err("The revive mask has an unsupported BC1 mip.".into());
    }
    let mut rgba = vec![0; width as usize * height as usize * 4];
    for block_y in 0..height.div_ceil(4) {
        for block_x in 0..width.div_ceil(4) {
            let offset = (block_y * width.div_ceil(4) + block_x) as usize * 8;
            let block = &bytes[offset..offset + 8];
            let first = u16::from_le_bytes([block[0], block[1]]);
            let second = u16::from_le_bytes([block[2], block[3]]);
            let mut palette = [colour565(first), colour565(second), [0; 4], [0; 4]];
            for channel in 0..3 {
                let a = palette[0][channel] as u16;
                let b = palette[1][channel] as u16;
                if first > second {
                    palette[2][channel] = ((2 * a + b) / 3) as u8;
                    palette[3][channel] = ((a + 2 * b) / 3) as u8;
                } else {
                    palette[2][channel] = ((a + b) / 2) as u8;
                }
            }
            palette[2][3] = 255;
            palette[3][3] = if first > second { 255 } else { 0 };
            let selectors = u32::from_le_bytes(block[4..8].try_into().unwrap());
            for y in 0..4 {
                for x in 0..4 {
                    let image_x = block_x * 4 + x;
                    let image_y = block_y * 4 + y;
                    if image_x >= width || image_y >= height {
                        continue;
                    }
                    let selector = ((selectors >> (2 * (y * 4 + x))) & 3) as usize;
                    let target = (image_y * width + image_x) as usize * 4;
                    rgba[target..target + 4].copy_from_slice(&palette[selector]);
                }
            }
        }
    }
    Ok(rgba)
}

pub fn vision_assets(game: &Path) -> Result<VisionMask, String> {
    let mut reader = AssetReader::new(game)?;
    let mut buffers = HashMap::new();
    let original = reader.texture("gfx_fullscreen_revive_m", &mut buffers)?;
    if original.format != "BC1" || original.width != 256 || original.height != 256 {
        return Err("This preview requires the installed 256×256 BC1 revive mask.".into());
    }
    let compressed = buffers
        .get(&original.buffer)
        .ok_or("Missing revive mask payload.")?;
    let mut bytes = Vec::new();
    let mut mips = Vec::new();
    for mip in original.mips {
        let source = compressed
            .get(mip.offset..mip.offset + mip.length)
            .ok_or("The revive mask mip is truncated.")?;
        let decoded = decode_bc1(source, mip.width, mip.height)?;
        mips.push(TextureMip {
            offset: bytes.len(),
            length: decoded.len(),
            width: mip.width,
            height: mip.height,
        });
        bytes.extend(decoded);
    }
    Ok(VisionMask {
        texture: Texture {
            id: original.id,
            name: original.name,
            buffer: "revive-mask-rgba8".into(),
            format: "RGBA8".into(),
            width: original.width,
            height: original.height,
            mips,
        },
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bc1_palette_and_small_mip_bounds() {
        let block = [0, 248, 224, 7, 228, 228, 228, 228];
        let rgba = decode_bc1(&block, 4, 4).unwrap();
        assert_eq!(
            &rgba[0..16],
            &[255, 0, 0, 255, 0, 255, 0, 255, 170, 85, 0, 255, 85, 170, 0, 255]
        );
        assert_eq!(decode_bc1(&block, 1, 1).unwrap(), [255, 0, 0, 255]);
        assert!(decode_bc1(&block[..7], 4, 4).is_err());
        let transparent = [0, 0, 255, 255, 255, 255, 255, 255];
        assert_eq!(decode_bc1(&transparent, 1, 1).unwrap(), [0, 0, 0, 0]);
    }

    #[test]
    #[ignore = "Requires an explicitly supplied WAW_VISION_TEST_GAME installation"]
    fn installed_mask_retains_all_original_mips() {
        let game = std::env::var_os("WAW_VISION_TEST_GAME")
            .expect("Set WAW_VISION_TEST_GAME to the installation to inspect read-only.");
        let mask = vision_assets(Path::new(&game)).unwrap();
        assert_eq!(mask.texture.mips.len(), 9);
        assert_eq!(mask.bytes.len(), 349_524);
        let mut expected = 256;
        for mip in &mask.texture.mips {
            assert_eq!((mip.width, mip.height), (expected, expected));
            assert_eq!(mip.length, expected as usize * expected as usize * 4);
            expected = (expected / 2).max(1);
        }
        println!(
            "Decoded installed mask SHA256: {}",
            crate::assets::sha(&mask.bytes)
        );
    }
}
