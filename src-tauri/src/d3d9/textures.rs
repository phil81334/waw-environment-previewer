use crate::Texture;
use std::{
    collections::HashMap,
    ptr::{copy_nonoverlapping, null, null_mut},
};
use windows::Win32::Graphics::Direct3D9::*;
pub(super) type Result<T> = std::result::Result<T, String>;
fn error(e: windows::core::Error) -> String {
    e.to_string()
}

/// Expand one authored falloff row to the secondary atlas's BGRA layout.
/// Most installed falloffs are L8/BGR8, not four-channel images.
pub(super) fn falloff_row(format: &str, width: usize, bytes: &[u8]) -> Result<Vec<u8>> {
    let channels = match format {
        "L8" | "R8" | "A8" => 1,
        "BGR8" => 3,
        "BGRA8" | "RGBA8" => 4,
        _ => return Err(format!("Unsupported falloff row format {format}")),
    };
    let source = bytes
        .get(..width.checked_mul(channels).ok_or("Falloff row overflow")?)
        .ok_or("Truncated falloff row")?;
    Ok(source
        .chunks_exact(channels)
        .flat_map(|p| match format {
            "L8" | "R8" => [p[0], p[0], p[0], 255],
            "A8" => [255, 255, 255, p[0]],
            "BGR8" => [p[0], p[1], p[2], 255],
            "RGBA8" => [p[2], p[1], p[0], p[3]],
            _ => [p[0], p[1], p[2], p[3]],
        })
        .collect())
}

#[cfg(test)]
mod row_tests {
    use super::*;
    #[test]
    fn authored_single_and_three_channel_falloffs_reach_rgb_atlas() {
        assert_eq!(
            falloff_row("L8", 3, &[0, 128, 255]).unwrap(),
            [0, 0, 0, 255, 128, 128, 128, 255, 255, 255, 255, 255]
        );
        assert_eq!(
            falloff_row("BGR8", 1, &[12, 34, 56]).unwrap(),
            [12, 34, 56, 255]
        );
        assert_eq!(
            falloff_row("RGBA8", 1, &[12, 34, 56, 78]).unwrap(),
            [56, 34, 12, 78]
        );
        assert!(falloff_row("L8", 3, &[0, 128]).is_err());
        assert!(falloff_row("BC1", 1, &[0; 8]).is_err());
    }
}

pub(super) unsafe fn upload(
    device: &IDirect3DDevice9,
    meta: &Texture,
    buffers: &HashMap<String, Vec<u8>>,
) -> Result<IDirect3DTexture9> {
    let bytes = buffers
        .get(&meta.buffer)
        .ok_or_else(|| format!("Missing texture buffer {}", meta.buffer))?;
    let (format, block, channels) = match meta.format.as_str() {
        "BC1" => (D3DFMT_DXT1, 8, 0),
        "BC2" => (D3DFMT_DXT3, 16, 0),
        "BC3" => (D3DFMT_DXT5, 16, 0),
        "BGRA8" => (D3DFMT_A8R8G8B8, 0, 4),
        "RGBA8" => (D3DFMT_A8R8G8B8, 0, 4),
        "BGR8" => (D3DFMT_X8R8G8B8, 0, 3),
        "L8" | "R8" => (D3DFMT_L8, 0, 1),
        "A8" => (D3DFMT_A8, 0, 1),
        other => return Err(format!("Unsupported native texture format {other}")),
    };
    if meta.width == 0
        || meta.height == 0
        || meta.width > 8192
        || meta.height > 8192
        || meta.mips.is_empty()
        || meta.mips.len() > (meta.width.max(meta.height).ilog2() + 1) as usize
    {
        return Err("Invalid texture dimensions/mips".into());
    }
    let mut out = None;
    device
        .CreateTexture(
            meta.width,
            meta.height,
            meta.mips.len() as u32,
            0,
            format,
            D3DPOOL_MANAGED,
            &mut out,
            null_mut(),
        )
        .map_err(error)?;
    let out = out.ok_or("Native texture creation returned no resource")?;
    for (level, mip) in meta.mips.iter().enumerate() {
        // The destination is allocated from the base dimensions. Validate each
        // level against that allocation before copying rows into a locked mip.
        if mip.width != (meta.width >> level).max(1) || mip.height != (meta.height >> level).max(1)
        {
            return Err("Texture mip dimensions disagree with the base level".into());
        }
        let src = bytes
            .get(
                mip.offset
                    ..mip
                        .offset
                        .checked_add(mip.length)
                        .ok_or("Texture range overflow")?,
            )
            .ok_or("Truncated texture mip")?;
        let rows = if block > 0 {
            mip.height.div_ceil(4)
        } else {
            mip.height
        } as usize;
        let source_pitch = if block > 0 {
            mip.width.div_ceil(4) as usize * block
        } else {
            mip.width as usize * channels
        };
        if rows * source_pitch != src.len() {
            return Err("Texture mip size differs from dimensions".into());
        }
        let mut lock = D3DLOCKED_RECT::default();
        out.LockRect(level as u32, &mut lock, null(), 0)
            .map_err(error)?;
        let output_pitch = if channels == 3 {
            mip.width as usize * 4
        } else {
            source_pitch
        };
        if lock.Pitch < output_pitch as i32 {
            out.UnlockRect(level as u32).map_err(error)?;
            return Err("Texture pitch too small".into());
        }
        for y in 0..rows {
            let input = &src[y * source_pitch..(y + 1) * source_pitch];
            let expanded;
            let pixels = if channels == 3 {
                expanded = input
                    .chunks_exact(3)
                    .flat_map(|p| [p[0], p[1], p[2], 255])
                    .collect::<Vec<_>>();
                expanded.as_slice()
            } else if meta.format == "RGBA8" {
                expanded = input
                    .chunks_exact(4)
                    .flat_map(|p| [p[2], p[1], p[0], p[3]])
                    .collect::<Vec<_>>();
                expanded.as_slice()
            } else {
                input
            };
            copy_nonoverlapping(
                pixels.as_ptr(),
                lock.pBits.cast::<u8>().add(y * lock.Pitch as usize),
                pixels.len(),
            );
        }
        out.UnlockRect(level as u32).map_err(error)?;
    }
    Ok(out)
}
pub(super) unsafe fn raw(
    device: &IDirect3DDevice9,
    width: u32,
    height: u32,
    format: &str,
    pixels: &[u8],
) -> Result<IDirect3DTexture9> {
    let key = "raw".to_owned();
    let meta = Texture {
        id: key.clone(),
        name: key.clone(),
        buffer: key.clone(),
        format: format.into(),
        width,
        height,
        mips: vec![crate::TextureMip {
            offset: 0,
            length: pixels.len(),
            width,
            height,
        }],
    };
    upload(device, &meta, &HashMap::from([(key, pixels.to_vec())]))
}
pub(super) unsafe fn volume(
    device: &IDirect3DDevice9,
    pixels: &[u8],
) -> Result<IDirect3DVolumeTexture9> {
    if pixels.len() != 256 {
        return Err("Directional model volume must have256 RGBA bytes".into());
    }
    let mut out = None;
    device
        .CreateVolumeTexture(
            4,
            4,
            4,
            1,
            0,
            D3DFMT_A8R8G8B8,
            D3DPOOL_MANAGED,
            &mut out,
            null_mut(),
        )
        .map_err(error)?;
    let out = out.ok_or("Native volume creation returned no resource")?;
    let mut lock = D3DLOCKED_BOX::default();
    out.LockBox(0, &mut lock, null(), 0).map_err(error)?;
    for z in 0..4 {
        for y in 0..4 {
            for x in 0..4 {
                let p = &pixels[(z * 16 + y * 4 + x) * 4..][..4];
                let b = [p[2], p[1], p[0], p[3]];
                copy_nonoverlapping(
                    b.as_ptr(),
                    lock.pBits
                        .cast::<u8>()
                        .add(z * lock.SlicePitch as usize + y * lock.RowPitch as usize + x * 4),
                    4,
                )
            }
        }
    }
    out.UnlockBox(0).map_err(error)?;
    Ok(out)
}
pub(super) unsafe fn black_cube(device: &IDirect3DDevice9) -> Result<IDirect3DCubeTexture9> {
    let mut out = None;
    device
        .CreateCubeTexture(
            1,
            1,
            0,
            D3DFMT_A8R8G8B8,
            D3DPOOL_MANAGED,
            &mut out,
            null_mut(),
        )
        .map_err(error)?;
    let out = out.ok_or("No native cube")?;
    for face in 0..6 {
        let mut lock = D3DLOCKED_RECT::default();
        out.LockRect(D3DCUBEMAP_FACES(face), 0, &mut lock, null(), 0)
            .map_err(error)?;
        copy_nonoverlapping([0u8, 0, 0, 255].as_ptr(), lock.pBits.cast(), 4);
        out.UnlockRect(D3DCUBEMAP_FACES(face), 0).map_err(error)?;
    }
    Ok(out)
}
pub(super) unsafe fn cube(
    device: &IDirect3DDevice9,
    value: &serde_json::Value,
) -> Result<IDirect3DCubeTexture9> {
    // Legacy probe captures store the levels directly and always use 64px faces.
    let (size, levels) = if let Some(levels) = value.as_array() {
        (64, levels)
    } else {
        (
            value["size"].as_u64().unwrap_or(256),
            value["levels"].as_array().ok_or("Missing cube levels")?,
        )
    };
    if size == 0 || size > 1024 || levels.is_empty() || levels.len() > 11 {
        return Err("Invalid native cube dimensions".into());
    }
    let mut out = None;
    device
        .CreateCubeTexture(
            size as u32,
            levels.len() as u32,
            0,
            D3DFMT_A8R8G8B8,
            D3DPOOL_MANAGED,
            &mut out,
            null_mut(),
        )
        .map_err(error)?;
    let out = out.ok_or("No native cube")?;
    for (level, item) in levels.iter().enumerate() {
        let width = (size >> level).max(1) as usize;
        let faces = item
            .as_array()
            .or_else(|| item["faces"].as_array())
            .ok_or("Invalid cube face list")?;
        if faces.len() != 6 {
            return Err("Cube requires six faces".into());
        }
        for (face, pixels) in faces.iter().enumerate() {
            let values = pixels.as_array().ok_or("Cube pixels must be bytes")?;
            if values.len() != width * width * 4 {
                return Err("Cube mip byte count invalid".into());
            };
            let mut lock = D3DLOCKED_RECT::default();
            out.LockRect(
                D3DCUBEMAP_FACES(face as i32),
                level as u32,
                &mut lock,
                null(),
                0,
            )
            .map_err(error)?;
            for y in 0..width {
                for x in 0..width {
                    let p = (y * width + x) * 4;
                    let mut b = [0; 4];
                    for (o, i) in [2, 1, 0, 3].into_iter().enumerate() {
                        b[o] = values[p + i]
                            .as_u64()
                            .filter(|v| *v <= 255)
                            .ok_or("Invalid cube byte")? as u8;
                    }
                    copy_nonoverlapping(
                        b.as_ptr(),
                        lock.pBits.cast::<u8>().add(y * lock.Pitch as usize + x * 4),
                        4,
                    )
                }
            }
            out.UnlockRect(D3DCUBEMAP_FACES(face as i32), level as u32)
                .map_err(error)?;
        }
    }
    Ok(out)
}
