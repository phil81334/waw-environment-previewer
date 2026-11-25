//! Original installed WaW shader programs for the isolated D3D9 comparison.
//! Bytecode is read from the user's installation; it is never embedded/distributed.
//! Named runtime shaders are bounded records in zlib-compressed IWffu100 zones.
//! Post shaders with shared-name pointers use freshly verified shader_bin aliases.
use flate2::read::ZlibDecoder;
use sha2::{Digest, Sha256};
use std::{collections::HashMap, fs, io::Read, path::Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterKind {
    Bool,
    Int,
    Float,
    Sampler,
}
#[derive(Clone, Debug)]
pub struct ShaderConstant {
    pub name: String,
    pub register: u32,
    pub count: u32,
    pub kind: RegisterKind,
}
#[derive(Clone, Debug)]
pub struct ShaderProgram {
    pub words: Vec<u32>,
    pub constants: Vec<ShaderConstant>,
    pub sha256: String,
    pub source: String,
}
impl ShaderProgram {
    pub fn constant(&self, name: &str) -> Option<&ShaderConstant> {
        self.constants.iter().find(|value| value.name == name)
    }
}
pub struct ShaderLibrary {
    vertex: HashMap<String, ShaderProgram>,
    pixel: HashMap<String, ShaderProgram>,
}
fn word(bytes: &[u8], at: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        bytes
            .get(at..at + 4)
            .ok_or("Truncated shader word")?
            .try_into()
            .unwrap(),
    ))
}
fn half(bytes: &[u8], at: usize) -> Result<u16, String> {
    Ok(u16::from_le_bytes(
        bytes
            .get(at..at + 2)
            .ok_or("Truncated shader half word")?
            .try_into()
            .unwrap(),
    ))
}
fn text(bytes: &[u8], at: usize) -> Result<String, String> {
    let tail = bytes.get(at..).ok_or("Invalid shader string offset")?;
    let length = tail
        .iter()
        .position(|b| *b == 0)
        .ok_or("Unterminated shader string")?;
    if length > 256 || !tail[..length].is_ascii() {
        return Err("Unsupported shader constant name".into());
    }
    String::from_utf8(tail[..length].to_vec()).map_err(|e| e.to_string())
}
fn parse_constants(bytes: &[u8]) -> Result<Vec<ShaderConstant>, String> {
    let mut cursor = 4;
    while cursor + 4 <= bytes.len() {
        let token = word(bytes, cursor)?;
        if token == 0xffff {
            break;
        }
        if token & 0xffff == 0xfffe {
            let count = ((token >> 16) & 0x7fff) as usize;
            let end = cursor
                .checked_add(4 + count * 4)
                .ok_or("Shader comment overflow")?;
            let body = bytes
                .get(cursor + 4..end)
                .ok_or("Truncated shader comment")?;
            if body.starts_with(b"CTAB") {
                let table = &body[4..];
                if word(table, 0)? != 28 {
                    return Err("Unsupported shader constant table header".into());
                }
                let count = word(table, 12)? as usize;
                if count > 256 {
                    return Err("Too many shader constants".into());
                }
                let start = word(table, 16)? as usize;
                let mut constants = Vec::with_capacity(count);
                for index in 0..count {
                    let offset = start + index * 20;
                    let kind = match half(table, offset + 4)? {
                        0 => RegisterKind::Bool,
                        1 => RegisterKind::Int,
                        2 => RegisterKind::Float,
                        3 => RegisterKind::Sampler,
                        _ => return Err("Invalid shader register set".into()),
                    };
                    let register = half(table, offset + 6)? as u32;
                    let registers = half(table, offset + 8)? as u32;
                    if registers == 0 || register + registers > 256 {
                        return Err("Invalid shader register span".into());
                    }
                    constants.push(ShaderConstant {
                        name: text(table, word(table, offset)? as usize)?,
                        register,
                        count: registers,
                        kind,
                    });
                }
                return Ok(constants);
            }
            cursor = end;
        } else {
            // Shader model 2/3 instruction token encodes following argument count.
            let args = ((token >> 24) & 15) as usize;
            cursor = cursor
                .checked_add(4 + args * 4)
                .ok_or("Shader instruction overflow")?;
        }
    }
    Ok(Vec::new())
}
fn parse_program(bytes: &[u8], source: String) -> Result<(bool, ShaderProgram), String> {
    if bytes.len() < 8
        || bytes.len() > 1024 * 1024
        || bytes.len() % 4 != 0
        || word(bytes, bytes.len() - 4)? != 0xffff
    {
        return Err("Invalid original shader bytecode length/end token".into());
    }
    let token = word(bytes, 0)?;
    let vertex = match token {
        0xfffe0200 | 0xfffe0300 => true,
        0xffff0200 | 0xffff0300 => false,
        _ => return Err("Unsupported original shader version".into()),
    };
    Ok((
        vertex,
        ShaderProgram {
            words: bytes
                .chunks_exact(4)
                .map(|v| u32::from_le_bytes(v.try_into().unwrap()))
                .collect(),
            constants: parse_constants(bytes)?,
            sha256: format!("{:x}", Sha256::digest(bytes)),
            source,
        },
    ))
}
impl ShaderLibrary {
    pub fn load(game: &Path) -> Result<Self, String> {
        let mut result = Self {
            vertex: HashMap::new(),
            pixel: HashMap::new(),
        };
        // Bounded, known named shaders only; a program is selected at draw time.
        for leaf in [
            "common.ff",
            "nazi_zombie_factory.ff",
            "nazi_zombie_prototype.ff",
            "code_post_gfx.ff",
        ] {
            let file = game.join("zone/english").join(leaf);
            if !file.exists() {
                continue;
            }
            let compressed = fs::read(&file)
                .map_err(|e| format!("Cannot read original shader zone {leaf}: {e}"))?;
            if compressed.len() > 256 * 1024 * 1024 || compressed.get(..8) != Some(b"IWffu100") {
                return Err(format!("Unsupported shader zone {leaf}"));
            }
            let mut decoder =
                ZlibDecoder::new(compressed.get(12..).ok_or("Truncated zone header")?);
            let mut zone = Vec::new();
            decoder
                .by_ref()
                .take(384 * 1024 * 1024 + 1)
                .read_to_end(&mut zone)
                .map_err(|e| format!("Cannot decompress shader zone {leaf}: {e}"))?;
            if zone.len() > 384 * 1024 * 1024 {
                return Err("Shader zone exceeds supported size".into());
            }
            result.read_zone(&zone, leaf)?;
        }
        for &(name, vertex, file, expected) in POST_CACHE {
            let raw = fs::read(game.join("raw/shader_bin").join(file))
                .map_err(|e| format!("Missing original post shader {file}: {e}"))?;
            let code = raw.get(4..).ok_or("Truncated shader cache wrapper")?;
            let (actual_vertex, program) = parse_program(code, format!("raw/shader_bin/{file}"))?;
            if actual_vertex != vertex || program.sha256 != expected {
                return Err(format!(
                    "Original post shader {file} differs from the verified supported program"
                ));
            }
            if vertex {
                result.vertex.insert(name.into(), program);
            } else {
                result.pixel.insert(name.into(), program);
            }
        }
        result.vertex("lm_s_tc0n0_sm3.hlsl")?;
        result.pixel("lm_r0c0n0s0_sm3.hlsl")?;
        result.vertex("lp_s_tc0n0_dtex_sm3.hlsl")?;
        result.pixel("lp_r0c0n0s0_sm3.hlsl")?;
        Ok(result)
    }
    fn read_zone(&mut self, zone: &[u8], leaf: &str) -> Result<(), String> {
        let suffix = b".hlsl\0";
        let mut cursor = 4;
        while cursor + suffix.len() <= zone.len() {
            if zone.get(cursor..cursor + suffix.len()) != Some(suffix.as_slice()) {
                cursor += 1;
                continue;
            }
            let end = cursor + suffix.len();
            let mut begin = cursor;
            while begin > 0
                && cursor - begin < 128
                && (zone[begin - 1].is_ascii_alphanumeric() || b"_/".contains(&zone[begin - 1]))
            {
                begin -= 1;
            }
            cursor = end;
            if begin < 4 {
                continue;
            }
            let name = std::str::from_utf8(&zone[begin..end - 1]).map_err(|e| e.to_string())?;
            if !name_allowed(name) {
                continue;
            }
            let count = half(zone, begin - 4)? as usize;
            if count == 0 || count > 32768 {
                continue;
            }
            let Some(bytes) = zone.get(end..end + count * 4) else {
                continue;
            };
            // Names may occur in ordinary data; only complete genuine programs qualify.
            let Ok((vertex, program)) =
                parse_program(bytes, format!("zone/english/{leaf}@{begin}"))
            else {
                continue;
            };
            let table = if vertex {
                &mut self.vertex
            } else {
                &mut self.pixel
            };
            table.entry(name.into()).or_insert(program);
            if self.vertex.len() + self.pixel.len() > 8192 {
                return Err("Too many original shader programs".into());
            }
        }
        Ok(())
    }
    pub fn vertex(&self, name: &str) -> Result<&ShaderProgram, String> {
        self.vertex
            .get(name)
            .ok_or_else(|| format!("Original vertex shader not available: {name}"))
    }
    pub fn pixel(&self, name: &str) -> Result<&ShaderProgram, String> {
        self.pixel
            .get(name)
            .ok_or_else(|| format!("Original pixel shader not available: {name}"))
    }
}
fn name_allowed(name: &str) -> bool {
    name.len() <= 128
        && (name.starts_with("lm_")
            || name.starts_with("lp_")
            || name.starts_with("l_hsm_")
            || name.starts_with("sky_")
            || name.starts_with("vertcol_")
            || name.starts_with("textured_")
            || name.starts_with("filter_symmetric_")
            || name.starts_with("glow_")
            || name.starts_with("objective_")
            || [
                "lib/revivefx.hlsl",
                "transform_only.hlsl",
                "null.hlsl",
                "vertalpha_simple_atest.hlsl",
            ]
            .contains(&name))
}

// Cache aliases are necessary where the zone serializes a shared-name pointer
// rather than a second string. Hashes are of bytecode after the 4-byte wrapper.
const POST_CACHE: &[(&str, bool, &str, &str)] = &[
    (
        "floatz_build.hlsl",
        true,
        "vs_3_0_b4b2e8cf",
        "44c6c7de4e6e81eab5fd551e3491c1e0aec0c2460adbdc9390109f17a597aeaf",
    ),
    (
        "floatz_build.hlsl",
        false,
        "ps_3_0_add7ff09",
        "3f71992f7ca6c0e289a4a2385653b115b7196e2daebd94a1f5e463f529614b16",
    ),
    (
        "floatz_clear.hlsl",
        false,
        "ps_3_0_688f1be6",
        "223b6fb61bfa08a3129140fe90a8f2c9c71ea5d5e10ac21d9e5b49b17bfa9df2",
    ),
    (
        "transform_only.hlsl",
        true,
        "vs_3_0_742a900b",
        "136a6302e6b7734d903db084c468b0ce4c92a67e66b307bc8033e162b1e91211",
    ),
    (
        "null.hlsl",
        false,
        "ps_3_0_0da9af51",
        "c25028dd372557facffd8f0e4d28480285eb1511662723b03107b75ca93f880a",
    ),
    (
        "sky_noncubemap_transition_dtex.hlsl",
        true,
        "vs_3_0_c098d8f2",
        "45c082cd63f88d7eb464979acabd16cfc6f1e07cd7ff87f8b57908e691bf5e07",
    ),
    (
        "sky_noncubemap_transition.hlsl",
        false,
        "ps_3_0_bb7ac466",
        "eb338723ca17c83b070a2955bf1c42e66b4ed6b74aae0a60acf76794c5f14eae",
    ),
    (
        "sky_noncubemap_dtex_add.hlsl",
        true,
        "vs_3_0_7bb018bc",
        "45c082cd63f88d7eb464979acabd16cfc6f1e07cd7ff87f8b57908e691bf5e07",
    ),
    (
        "sky_noncubemap_add.hlsl",
        false,
        "ps_3_0_d5c17368",
        "7dc00245b94f1e43d777bfdf909f12a9118191a5a15ef662e510d9af7fd6aeff",
    ),
    (
        "textured_simple.hlsl",
        true,
        "vs_3_0_5afa700d",
        "bb7360f2413c64e5c03907a45ed422be1dfdeddb2ea5910732470d22386a19cd",
    ),
    (
        "textured_simple.hlsl",
        false,
        "ps_3_0_03c9e712",
        "7b6f06f69a788f20e55260b587cf3a91cfd3a436c0ee464dd3a439778346be33",
    ),
    (
        "glow_consistent_setup.hlsl",
        true,
        "vs_3_0_2b811b38",
        "7b90425bfcbe4d521a1d44479fe25e1f704839ff374f16e76a44d6d2af79c54e",
    ),
    (
        "glow_consistent_setup.hlsl",
        false,
        "ps_3_0_0864653e",
        "bdc444fb4b4f92440a4a6b26ea0a3a4e10167bbd26db26cf84f9b0573ef89bae",
    ),
    (
        "glow_apply_bloom.hlsl",
        true,
        "vs_3_0_5afa700d",
        "bb7360f2413c64e5c03907a45ed422be1dfdeddb2ea5910732470d22386a19cd",
    ),
    (
        "glow_apply_bloom.hlsl",
        false,
        "ps_3_0_4586800e",
        "df6fd1c374366a6bce60c0fb66a9247be37c94f1b368a64dc4b8a1fd8edbb216",
    ),
    (
        "lib/revivefx.hlsl",
        true,
        "vs_3_0_a6f8821e",
        "c7f0ba1750e9c04d4c5a69bb990c9c62d434f91591f2fd8a2e9542a31ecccb11",
    ),
    (
        "lib/revivefx.hlsl",
        false,
        "ps_3_0_ac3b51c7",
        "6cd134c89a863da3b08f28b2741e368141314e375229da1a6beb9dd3b9e938b6",
    ),
    (
        "glow_godrays.hlsl",
        true,
        "vs_3_0_34210895",
        "3afb10f78eefd6284b5311da7fd8318ed7c0e636626f4899e48a0145f49aa5e3",
    ),
    (
        "glow_godrays.hlsl",
        false,
        "ps_3_0_7e704153",
        "881d78b7a932d0459751b0fc8ba1c4898036c878772398504f648b6224e0fdc6",
    ),
    (
        "filter_symmetric_1.hlsl",
        true,
        "vs_3_0_238ef1fe",
        "12349b133527f4121a5adf2ef902b518282f55eb8f91d9ce128f574bb11bdffd",
    ),
    (
        "filter_symmetric_1.hlsl",
        false,
        "ps_3_0_c85d2f38",
        "3fb736ec1f27e97e2b53633b60aff54167e3e9d4c58162413d8bbdcce5c23ba4",
    ),
    (
        "filter_symmetric_2.hlsl",
        true,
        "vs_3_0_21612a1d",
        "a66a3e59687d7f012f3ecd1a6a89f8a694f0ec11c1af338a3961ddfdb5d06aab",
    ),
    (
        "filter_symmetric_2.hlsl",
        false,
        "ps_3_0_ceef06db",
        "d16fc5d8594e10f78ea85526d0b232961c36de7090f6343baa2f91a86a59cd26",
    ),
    (
        "filter_symmetric_3.hlsl",
        true,
        "vs_3_0_1f2f223c",
        "d7f4743ccd2bb83778b1fd1176a543c902da8baf5d45d47be9bee73b3e5a8b04",
    ),
    (
        "filter_symmetric_3.hlsl",
        false,
        "ps_3_0_ccbcfefa",
        "175f690b7f9887bafdb9338caadb97fc7ec27149899062f7bb4710a0b6c9c936",
    ),
    (
        "filter_symmetric_4.hlsl",
        true,
        "vs_3_0_2e84fa5b",
        "7dda26d1ac82a803d3b6ffeb60d7e2b69c1315b5fe4e756c653f5b9d661e5044",
    ),
    (
        "filter_symmetric_4.hlsl",
        false,
        "ps_3_0_c1cb369d",
        "f09973a67d7852944ed4a25f1be5a753033260182023f51c3c16be223655414a",
    ),
    (
        "filter_symmetric_5.hlsl",
        true,
        "vs_3_0_2c52f27a",
        "5a734443b15479c9be547c98c17958a7efe9e392f4002927a6577bc6b6f9c462",
    ),
    (
        "filter_symmetric_5.hlsl",
        false,
        "ps_3_0_bf992ebc",
        "877ae86e9d26aff3d22b375149b2bebf8c8e7199fd6e796c1b3a8f092fe48afa",
    ),
    (
        "filter_symmetric_6.hlsl",
        true,
        "vs_3_0_2a252a99",
        "68bf01c7043cb8b9a6578ba59c1b650905eae82da48f85a503deec130fbf2842",
    ),
    (
        "filter_symmetric_6.hlsl",
        false,
        "ps_3_0_c62b065f",
        "5ccc7ff6b4336b51d642ef9cbe81616b1e0fb9aff169589b34aa2d198d284fdb",
    ),
    (
        "filter_symmetric_7.hlsl",
        true,
        "vs_3_0_27f322b8",
        "f32b88da9bf22b10aaaac4eb1e373388800665086a1841397892e3695d8fea23",
    ),
    (
        "filter_symmetric_7.hlsl",
        false,
        "ps_3_0_c3f8fe7e",
        "b7799bc1e39dd64dbaec67a1a97bf5d7e88b280d6c37982adda862ab50e4d7b1",
    ),
    (
        "filter_symmetric_8.hlsl",
        true,
        "vs_3_0_1430bad7",
        "5a818b28adf5e433ee2b2fcc6d9dc2ddf5c5af6bcc826c22d8af4c6567ad6e80",
    ),
    (
        "filter_symmetric_8.hlsl",
        false,
        "ps_3_0_b8fef811",
        "dfb88d603be69b604e1cfb6d1e286a679e0623ca4e6083bc9bb2d905c2bd8483",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_programs_are_rejected() {
        assert!(parse_program(&[], "fixture".into()).is_err());
        assert!(parse_program(&[0; 12], "fixture".into()).is_err());
        assert!(parse_program(&[0, 3, 254, 255, 0, 0, 0, 0], "fixture".into()).is_err());
    }
    #[test]
    fn minimal_original_program_and_no_constants() {
        let (vertex, p) =
            parse_program(&[0, 3, 254, 255, 255, 255, 0, 0], "fixture".into()).unwrap();
        assert!(vertex);
        assert!(p.constants.is_empty());
        assert_eq!(p.words.len(), 2);
    }
    #[test]
    fn names_stay_in_the_render_profile() {
        assert!(name_allowed("lm_s_tc0n0_sm3.hlsl"));
        assert!(!name_allowed("../outside.hlsl"));
        assert!(!name_allowed("some_unrelated_gameplay.hlsl"));
    }
}
