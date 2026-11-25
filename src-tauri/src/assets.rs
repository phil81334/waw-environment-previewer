use crate::{Material, Texture, TextureBinding, TextureMip};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

pub(crate) fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn verify_tools(game: &Path) -> Result<(), String> {
    verify_tool_directory(&game.join("bin"))
}

pub(crate) fn verify_tool_directory(directory: &Path) -> Result<(), String> {
    for (name, hash) in [
        (
            "cod2map.exe",
            "4e7bafb4750232974396d38644c2ef2c4eefdf3d4a41cf20de64e7a21c682a9d",
        ),
        (
            "cod2rad.exe",
            "bed142a50162420846eb0f6683af34dc2183d37e27400c1a3f215c5d99f90c4a",
        ),
        (
            "MSVCR71.dll",
            "8094af5ee310714caebccaeee7769ffb08048503ba478b879edfef5f1a24fefe",
        ),
    ] {
        let bytes = fs::read(directory.join(name))
            .map_err(|_| format!("Missing Modtools dependency bin/{name}."))?;
        if sha(&bytes) != hash {
            return Err(format!("bin/{name} differs from the verified stock Modtools 1.2 tool. Install the original supported Modtools files before baking."));
        }
    }
    Ok(())
}

pub(crate) fn name(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 160
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.~$&".contains(&b))
    {
        Err(format!("Invalid asset name: {value}"))
    } else {
        Ok(())
    }
}
pub(crate) fn u16_at(bytes: &[u8], offset: usize) -> Result<u16, String> {
    Ok(u16::from_le_bytes(
        bytes
            .get(offset..offset + 2)
            .ok_or("Truncated asset field.")?
            .try_into()
            .unwrap(),
    ))
}
pub(crate) fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or("Truncated asset field.")?
            .try_into()
            .unwrap(),
    ))
}
pub(crate) fn f32_at(bytes: &[u8], offset: usize) -> Result<f32, String> {
    let value = f32::from_bits(u32_at(bytes, offset)?);
    if !value.is_finite() {
        return Err("A numeric asset field is not finite.".into());
    }
    Ok(value)
}
pub(crate) fn string_at(bytes: &[u8], offset: usize) -> Result<String, String> {
    let tail = bytes.get(offset..).ok_or("Invalid asset string offset.")?;
    let end = tail
        .iter()
        .position(|b| *b == 0)
        .ok_or("Unterminated asset string.")?;
    if end > 256 {
        return Err("Asset string exceeds its supported size.".into());
    }
    String::from_utf8(tail[..end].to_vec()).map_err(|_| "Asset string is not UTF-8.".into())
}

pub(crate) struct AssetReader {
    game: PathBuf,
    archives: Vec<zip::ZipArchive<File>>,
    index: HashMap<String, (usize, usize)>,
    cache: HashMap<String, Vec<u8>>,
    iwd_first: bool,
}

impl AssetReader {
    pub(crate) fn new(game: &Path) -> Result<Self, String> {
        Self::with_policy(game, false)
    }

    // Scene assets use IWD precedence. Vision assets prefer loose Modtools files.
    pub(crate) fn new_entities(game: &Path) -> Result<Self, String> {
        Self::with_policy(game, true)
    }

    fn with_policy(game: &Path, iwd_first: bool) -> Result<Self, String> {
        let mut paths: Vec<PathBuf> = fs::read_dir(game.join("main"))
            .map_err(|e| format!("Cannot read game archives: {e}"))?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.eq_ignore_ascii_case("iwd"))
            })
            .collect();
        paths.sort();
        if iwd_first {
            paths.sort_by(|a, b| {
                let a = a
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_ascii_lowercase();
                let b = b
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_ascii_lowercase();
                a.starts_with("localized_")
                    .cmp(&b.starts_with("localized_"))
                    .then_with(|| b.cmp(&a))
            });
        }
        let mut index = HashMap::new();
        let mut archives = Vec::with_capacity(paths.len());
        for (archive_index, path) in paths.into_iter().enumerate() {
            let file = File::open(&path).map_err(|e| e.to_string())?;
            let contents = zip::ZipArchive::new(file)
                .map_err(|e| format!("Cannot inspect {}: {e}", path.display()))?;
            // The central directory already has every name in member order.
            // Opening each payload here adds thousands of seeks before the first frame.
            for (member_index, name) in contents.file_names().enumerate() {
                let key = name.replace('\\', "/").to_ascii_lowercase();
                index.entry(key).or_insert((archive_index, member_index));
            }
            archives.push(contents);
        }
        Ok(Self {
            game: game.to_owned(),
            archives,
            index,
            cache: HashMap::new(),
            iwd_first,
        })
    }

    pub(crate) fn installed_sky_models(&self) -> Result<Vec<String>, String> {
        let mut models = BTreeSet::new();
        let is_sky = |model: &str| {
            model
                .get(.."skybox_".len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("skybox_"))
                && !model.contains('.')
                && name(model).is_ok()
        };
        // Model names are extensionless. Only their names are needed for the picker;
        // the selected model's geometry and materials are loaded during the bake.
        for path in self.index.keys() {
            if let Some(model) = path.strip_prefix("xmodel/").filter(|model| is_sky(model)) {
                models.insert(model.to_owned());
            }
        }
        match fs::read_dir(self.game.join("raw/xmodel")) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(|error| error.to_string())?;
                    let filename = entry.file_name();
                    let Some(model) = filename.to_str() else {
                        continue;
                    };
                    if is_sky(model)
                        && entry
                            .file_type()
                            .map_err(|error| error.to_string())?
                            .is_file()
                    {
                        models.insert(model.to_ascii_lowercase());
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Cannot list raw/xmodel: {error}")),
        }
        Ok(models.into_iter().collect())
    }

    pub(crate) fn read(&mut self, relative: &str) -> Result<Vec<u8>, String> {
        if relative.starts_with('/')
            || relative.contains("..")
            || relative.contains('\\')
            || relative.contains(':')
        {
            return Err("Unsafe asset path.".into());
        }
        if let Some(bytes) = self.cache.get(relative) {
            return Ok(bytes.clone());
        }
        let loose = self.game.join("raw").join(relative);
        let bytes = if loose.is_file()
            && (!self.iwd_first || !self.index.contains_key(&relative.to_ascii_lowercase()))
        {
            let metadata = loose.metadata().map_err(|e| e.to_string())?;
            if metadata.len() > 64 * 1024 * 1024 {
                return Err(format!("Asset is too large: {relative}"));
            }
            fs::read(loose).map_err(|e| e.to_string())?
        } else {
            let (archive_index, member_index) = *self
                .index
                .get(&relative.to_ascii_lowercase())
                .ok_or_else(|| format!("Missing game/Modtools asset: {relative}"))?;
            let archive = &mut self.archives[archive_index];
            let mut member = archive.by_index(member_index).map_err(|e| e.to_string())?;
            if member.size() > 64 * 1024 * 1024 {
                return Err(format!("Archive asset is too large: {relative}"));
            }
            let mut bytes = Vec::with_capacity(member.size() as usize);
            member.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            bytes
        };
        self.cache.insert(relative.to_owned(), bytes.clone());
        Ok(bytes)
    }

    pub(crate) fn copy_to(&mut self, relative: &str, raw: &Path) -> Result<Vec<u8>, String> {
        let bytes = self.read(relative)?;
        let destination = raw.join(relative);
        fs::create_dir_all(destination.parent().ok_or("Invalid asset destination.")?)
            .map_err(|e| e.to_string())?;
        fs::write(destination, &bytes).map_err(|e| e.to_string())?;
        Ok(bytes)
    }

    pub(crate) fn material(&mut self, asset_name: &str) -> Result<Material, String> {
        name(asset_name)?;
        let bytes = self.read(&format!("materials/{asset_name}"))?;
        let texture_count = u16_at(&bytes, 48)? as usize;
        let constant_count = u16_at(&bytes, 50)? as usize;
        let techset = string_at(&bytes, u32_at(&bytes, 52)? as usize)?;
        let texture_table = u32_at(&bytes, 56)? as usize;
        let constant_table = u32_at(&bytes, 60)? as usize;
        if texture_count > 32 || constant_count > 128 {
            return Err(format!("Unsupported material table: {asset_name}"));
        }
        let mut textures = Vec::new();
        for index in 0..texture_count {
            let offset = texture_table + index * 12;
            let semantic = string_at(&bytes, u32_at(&bytes, offset)? as usize)?;
            let flags = u32_at(&bytes, offset + 4)?;
            let image = string_at(&bytes, u32_at(&bytes, offset + 8)? as usize)?;
            name(&image)?;
            textures.push(TextureBinding {
                semantic,
                texture_id: image,
                flags,
            });
        }
        let mut env_map_parms = [0.0; 4];
        let mut detail_scale = None;
        let mut color_tint = None;
        for index in 0..constant_count {
            let offset = constant_table + index * 20;
            let constant = string_at(&bytes, u32_at(&bytes, offset)? as usize)?;
            if constant == "envMapParms"
                || (self.iwd_first && matches!(constant.as_str(), "detailScale" | "colorTint"))
            {
                let mut value = [0.0; 4];
                for channel in 0..4 {
                    value[channel] = f32_at(&bytes, offset + 4 + channel * 4)?;
                }
                match constant.as_str() {
                    "envMapParms" => env_map_parms = value,
                    "detailScale" => detail_scale = Some(value),
                    "colorTint" => color_tint = Some(value),
                    _ => {}
                }
            }
        }
        Ok(Material {
            name: asset_name.to_owned(),
            techset,
            textures,
            env_map_parms,
            detail_scale,
            color_tint,
            states: [u32_at(&bytes, 40)?, u32_at(&bytes, 44)?],
        })
    }

    pub(crate) fn texture(
        &mut self,
        image: &str,
        buffers: &mut HashMap<String, Vec<u8>>,
    ) -> Result<Texture, String> {
        name(image)?;
        let bytes = self.read(&format!("images/{image}.iwi"))?;
        if bytes.len() < 28 || &bytes[..3] != b"IWi" || bytes[3] != 6 {
            return Err(format!("Invalid IWI image: {image}"));
        }
        let format = match bytes[4] {
            1 => "BGRA8",
            2 => "BGR8",
            4 => "L8",
            5 => "A8",
            11 => "BC1",
            12 => "BC2",
            13 => "BC3",
            value => return Err(format!("Unsupported IWI format {value} for {image}")),
        };
        let width = u16_at(&bytes, 6)? as u32;
        let height = u16_at(&bytes, 8)? as u32;
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            return Err("Invalid IWI dimensions.".into());
        }
        let (mut w, mut h, mut end) = (width, height, bytes.len());
        let mut output = Vec::new();
        let mut mips = Vec::new();
        loop {
            let length = match format {
                "BC1" => w.div_ceil(4) as usize * h.div_ceil(4) as usize * 8,
                "BC2" | "BC3" => w.div_ceil(4) as usize * h.div_ceil(4) as usize * 16,
                "BGR8" => w as usize * h as usize * 3,
                "BGRA8" => w as usize * h as usize * 4,
                _ => w as usize * h as usize,
            };
            if end < 28 + length {
                return Err(format!("Truncated IWI mip chain: {image}"));
            }
            let start = end - length;
            let offset = output.len();
            output.extend_from_slice(&bytes[start..end]);
            mips.push(TextureMip {
                offset,
                length,
                width: w,
                height: h,
            });
            end = start;
            if end == 28 || (w == 1 && h == 1) {
                break;
            }
            w = (w / 2).max(1);
            h = (h / 2).max(1);
        }
        if end != 28 {
            return Err(format!("Unexpected IWI payload: {image}"));
        }
        let buffer = format!("texture-{image}");
        buffers.insert(buffer.clone(), output);
        Ok(Texture {
            id: image.to_owned(),
            name: image.to_owned(),
            buffer,
            format: format.to_owned(),
            width,
            height,
            mips,
        })
    }
}
